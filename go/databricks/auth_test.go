package main

// Sign-in against a fake workspace token endpoint; the tests play the browser.

import (
	"crypto/sha256"
	"encoding/base64"
	"encoding/json"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/http/httptest"
	"net/url"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"sync"
	"testing"
	"time"
)

// fakeIdP plays the workspace's /oidc/v1/token: service principal sp/sp-secret, the code
// the-code (checked against the PKCE challenge), and the refresh token refresh-1.
type fakeIdP struct {
	mu        sync.Mutex
	grants    []string
	challenge string
	expiresIn int
}

func (f *fakeIdP) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	if r.URL.Path != "/oidc/v1/token" {
		// Databricks' SDK also looks for discovery documents; there are none here.
		http.NotFound(w, r)
		return
	}
	r.ParseForm()
	f.mu.Lock()
	defer f.mu.Unlock()
	g := r.Form.Get("grant_type")
	f.grants = append(f.grants, g)
	ok := false
	switch g {
	case "client_credentials":
		ok = r.Header.Get("Authorization") == "Basic "+base64.StdEncoding.EncodeToString([]byte("sp:sp-secret")) && r.Form.Get("scope") == "all-apis"
	case "authorization_code":
		sum := sha256.Sum256([]byte(r.Form.Get("code_verifier")))
		ok = r.Form.Get("code") == "the-code" && r.Form.Get("client_id") == "databricks-cli" &&
			strings.HasPrefix(r.Form.Get("redirect_uri"), "http://localhost:") &&
			base64.RawURLEncoding.EncodeToString(sum[:]) == f.challenge
	case "refresh_token":
		ok = r.Form.Get("refresh_token") == "refresh-1"
	}
	w.Header().Set("Content-Type", "application/json")
	if !ok {
		w.WriteHeader(400)
		json.NewEncoder(w).Encode(map[string]string{"error": "invalid_client", "error_description": "bad credentials"})
		return
	}
	body := map[string]any{"access_token": "good-token", "token_type": "Bearer", "expires_in": f.expiresIn}
	if g != "client_credentials" {
		body["refresh_token"] = "refresh-1"
	}
	json.NewEncoder(w).Encode(body)
}

func (f *fakeIdP) seen() []string {
	f.mu.Lock()
	defer f.mu.Unlock()
	return append([]string(nil), f.grants...)
}

// isolatedHome gives the test an empty home and no Databricks login from the machine running it.
func TestMain(m *testing.M) {
	quietLibraries()
	os.Exit(m.Run())
}

func isolatedHome(t *testing.T) string {
	home := t.TempDir()
	t.Setenv("HOME", home)
	t.Setenv("USERPROFILE", home)
	for _, e := range os.Environ() {
		if k, _, _ := strings.Cut(e, "="); strings.HasPrefix(k, "DATABRICKS_") || k == "DRE_INTERACTIVE" {
			t.Setenv(k, "")
			os.Unsetenv(k)
		}
	}
	return home
}

func freePort(t *testing.T) int {
	l, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	defer l.Close()
	return l.Addr().(*net.TCPAddr).Port
}

// playBrowser replaces announce: record the PKCE challenge, then follow the redirect with query
// (`{state}` filled in). The page the listener answers with goes to pages.
func playBrowser(t *testing.T, idp *fakeIdP, query string, pages chan<- string) {
	t.Setenv("DRE_INTERACTIVE", "1")
	old := announce
	t.Cleanup(func() { announce = old })
	announce = func(authURL string) {
		u, _ := url.Parse(authURL)
		q := u.Query()
		if q.Get("response_type") != "code" || q.Get("code_challenge_method") != "S256" || q.Get("scope") != "all-apis offline_access" {
			t.Errorf("authorize URL: %s", authURL)
		}
		idp.mu.Lock()
		idp.challenge = q.Get("code_challenge")
		idp.mu.Unlock()
		go func() {
			redirect := q.Get("redirect_uri")
			fav, err := http.Get(redirect + "/favicon.ico")
			if err == nil {
				if fav.StatusCode != 404 {
					t.Errorf("favicon got %d", fav.StatusCode)
				}
				fav.Body.Close()
			}
			resp, err := http.Get(redirect + "/?" + strings.ReplaceAll(query, "{state}", url.QueryEscape(q.Get("state"))))
			if err != nil {
				t.Error(err)
				pages <- ""
				return
			}
			b, _ := io.ReadAll(resp.Body)
			resp.Body.Close()
			pages <- string(b)
		}()
	}
}

func TestBrowserSignInIsSavedReusedAndRefreshed(t *testing.T) {
	home := isolatedHome(t)
	idp := &fakeIdP{expiresIn: 3600}
	srv := httptest.NewServer(idp)
	defer srv.Close()
	// auth_type auto finds no other login here, so it falls back to the browser sign-in.
	conn := map[string]any{"redirect_port": float64(freePort(t))}
	pages := make(chan string, 1)
	playBrowser(t, idp, "code=the-code&state={state}", pages)

	a, err := authFromConn(conn, srv.URL)
	if err != nil {
		t.Fatal(err)
	}
	tok, err := a.bearer()
	if err != nil || tok != "good-token" {
		t.Fatalf("%q %v", tok, err)
	}
	if p := <-pages; !strings.Contains(p, "Signed in") {
		t.Fatalf("page: %s", p)
	}
	path := filepath.Join(home, ".dre", "oauth_sessions.json")
	if runtime.GOOS != "windows" {
		st, _ := os.Stat(path)
		if st.Mode().Perm() != 0o600 {
			t.Fatalf("mode %v", st.Mode().Perm())
		}
	}
	key := "databricks/" + strings.TrimPrefix(srv.URL, "http://") + "/databricks-cli"
	var saved map[string]map[string]any
	b, _ := os.ReadFile(path)
	json.Unmarshal(b, &saved)
	if saved[key]["refresh_token"] != "refresh-1" {
		t.Fatalf("saved: %v", saved)
	}

	// A new process reuses the saved session: no token request, no browser.
	announce = func(string) { t.Fatal("browser opened again") }
	a, _ = authFromConn(conn, srv.URL)
	if tok, err := a.bearer(); err != nil || tok != "good-token" {
		t.Fatal(err)
	}
	// Expired: renewed with the refresh token, still no browser.
	saved[key]["expires_at"] = 0
	b, _ = json.Marshal(saved)
	os.WriteFile(path, b, 0o600)
	a, _ = authFromConn(conn, srv.URL)
	if tok, err := a.bearer(); err != nil || tok != "good-token" {
		t.Fatal(err)
	}
	if g := idp.seen(); strings.Join(g, ",") != "authorization_code,refresh_token" {
		t.Fatalf("grants: %v", g)
	}
}

func TestADeclinedSignInIsAClearError(t *testing.T) {
	isolatedHome(t)
	idp := &fakeIdP{expiresIn: 3600}
	srv := httptest.NewServer(idp)
	defer srv.Close()
	pages := make(chan string, 1)
	playBrowser(t, idp, "error=access_denied&error_description=The+user+declined&state={state}", pages)
	a, _ := authFromConn(map[string]any{"auth_type": "oauth", "redirect_port": float64(freePort(t))}, srv.URL)
	_, err := a.bearer()
	if err == nil || !strings.Contains(err.Error(), "access_denied: The user declined") {
		t.Fatalf("%v", err)
	}
	if p := <-pages; !strings.Contains(p, "Sign-in failed") {
		t.Fatalf("page: %s", p)
	}
}

func TestAServicePrincipalRenewsExpiringTokensAndSavesNothing(t *testing.T) {
	home := isolatedHome(t)
	idp := &fakeIdP{expiresIn: 0}
	srv := httptest.NewServer(idp)
	defer srv.Close()
	a, err := authFromConn(map[string]any{"auth_type": "oauth", "client_id": "sp", "client_secret": "sp-secret"}, srv.URL)
	if err != nil {
		t.Fatal(err)
	}
	for range 3 {
		if tok, err := a.bearer(); err != nil || tok != "good-token" {
			t.Fatal(err)
		}
	}
	if g := idp.seen(); len(g) != 3 {
		t.Fatalf("every call should renew a token that expires at once: %v", g)
	}
	if _, err := os.Stat(filepath.Join(home, ".dre")); !os.IsNotExist(err) {
		t.Fatal("service principal tokens were saved")
	}
	a, _ = authFromConn(map[string]any{"auth_type": "oauth", "client_id": "sp", "client_secret": "wrong"}, srv.URL)
	_, err = a.bearer()
	if err == nil || !strings.Contains(err.Error(), "HTTP 400") || !strings.Contains(err.Error(), "client_secret") || !strings.Contains(err.Error(), "invalid_client") {
		t.Fatalf("%v", err)
	}
}

func TestAuthTypePicksTheFlow(t *testing.T) {
	isolatedHome(t)
	base := "https://dbc-1.cloud.databricks.com"
	if a, err := authFromConn(map[string]any{"token": "t"}, base); err != nil || a.token != "t" {
		t.Fatal(err)
	}
	if _, err := authFromConn(map[string]any{"auth_type": "pat"}, base); err == nil || !strings.Contains(err.Error(), "`token`") {
		t.Fatalf("%v", err)
	}
	// auto with nothing to find: DRE's own sign-in, which explains every option when no one
	// is at the terminal instead of waiting for a browser.
	a, _ := authFromConn(map[string]any{}, base)
	if a.oauth == nil || !a.oauth.auto || a.oauth.clientID != "databricks-cli" || a.oauth.redirectPort != 8020 ||
		a.oauth.sessionKey != "databricks/dbc-1.cloud.databricks.com/databricks-cli" {
		t.Fatalf("%+v", a)
	}
	_, err := a.bearer()
	for _, want := range []string{"no Databricks login found for dbc-1.cloud.databricks.com", "DATABRICKS_TOKEN", "databricks auth login --host dbc-1.cloud.databricks.com", "~/.databrickscfg"} {
		if err == nil || !strings.Contains(err.Error(), want) {
			t.Fatalf("missing %q in %v", want, err)
		}
	}
	// On Databricks compute a browser is never tried, even with a person's terminal flag set.
	t.Setenv("DRE_INTERACTIVE", "1")
	t.Setenv("DATABRICKS_RUNTIME_VERSION", "16.4")
	a, _ = authFromConn(map[string]any{"auth_type": "oauth"}, base)
	if _, err := a.bearer(); err == nil || !strings.Contains(err.Error(), "needs a browser") {
		t.Fatalf("%v", err)
	}
	if _, err := authFromConn(map[string]any{"auth_type": "oauth", "client_secret": "s"}, base); err == nil || !strings.Contains(err.Error(), "client_id") {
		t.Fatalf("%v", err)
	}
	if _, err := authFromConn(map[string]any{"auth_type": "saml"}, base); err == nil {
		t.Fatal("saml accepted")
	}
	if _, err := authFromConn(map[string]any{"auth_type": "oauth", "redirect_port": "x"}, base); err == nil {
		t.Fatal("bad port accepted")
	}
}

// With no DRE auth fields, Databricks' own credentials are used: here DATABRICKS_TOKEN, and a
// ~/.databrickscfg profile named in the DRE profile.
func TestAutoUsesDatabricksOwnCredentials(t *testing.T) {
	home := isolatedHome(t)
	srv := httptest.NewServer(http.NotFoundHandler())
	defer srv.Close()
	t.Setenv("DATABRICKS_TOKEN", "from-env")
	a, err := authFromConn(map[string]any{}, srv.URL)
	if err != nil || a.sdk == nil {
		t.Fatalf("%v %+v", err, a)
	}
	if tok, err := a.bearer(); err != nil || tok != "from-env" {
		t.Fatalf("%q %v", tok, err)
	}
	os.Unsetenv("DATABRICKS_TOKEN")
	cfg := "[reports]\nhost = " + srv.URL + "\ntoken = from-profile\n"
	os.WriteFile(filepath.Join(home, ".databrickscfg"), []byte(cfg), 0o600)
	a, err = authFromConn(map[string]any{"profile": "reports"}, srv.URL)
	if err != nil || a.sdk == nil {
		t.Fatalf("%v %+v", err, a)
	}
	if tok, err := a.bearer(); err != nil || tok != "from-profile" {
		t.Fatalf("%q %v", tok, err)
	}
	// A token in the DRE profile still wins.
	if a, _ := authFromConn(map[string]any{"token": "mine", "profile": "reports"}, srv.URL); a.token != "mine" {
		t.Fatal("profile token ignored")
	}
}

func TestConcurrentSaversKeepEveryEntry(t *testing.T) {
	path := filepath.Join(t.TempDir(), ".dre", "oauth_sessions.json")
	var wg sync.WaitGroup
	for i := range 8 {
		wg.Add(1)
		go func() {
			defer wg.Done()
			if err := storeSessionAt(path, fmt.Sprintf("databricks/ws-%d/c", i), map[string]any{"n": i}); err != nil {
				t.Error(err)
			}
		}()
	}
	wg.Wait()
	all := readSessions(path)
	if len(all) != 8 {
		t.Fatalf("%d entries", len(all))
	}
	storeSessionAt(path, "databricks/ws-3/c", nil)
	if _, ok := readSessions(path)["databricks/ws-3/c"]; ok {
		t.Fatal("entry not removed")
	}
	if _, err := os.Stat(path + ".lock"); !os.IsNotExist(err) {
		t.Fatal("lock left behind")
	}
}

func TestAStaleLockIsTakenOver(t *testing.T) {
	path := filepath.Join(t.TempDir(), "oauth_sessions.json")
	lock := path + ".lock"
	os.WriteFile(lock, nil, 0o600)
	old := time.Now().Add(-time.Minute)
	os.Chtimes(lock, old, old)
	start := time.Now()
	if err := storeSessionAt(path, "k", map[string]any{"a": 1}); err != nil {
		t.Fatal(err)
	}
	if time.Since(start) > 5*time.Second {
		t.Fatal("waited on a stale lock")
	}
}
