package main

// How the plugin authenticates to the workspace: a static token, or OAuth.
//
// auth_type: oauth without a client_secret signs a person in through the browser (authorization
// code with PKCE, redirected to a listener on localhost). The session is saved in
// ~/.dre/oauth_sessions.json under databricks/<host>/<client_id>, the same file and format the
// Rust databricks_volumes destination uses (crates/dre-protocol/src/sessions.rs), so both share
// one sign-in per workspace. The refresh token renews it, so the browser only opens when there
// is no usable refresh token. With a client_secret it signs in as a service principal (client
// credentials); those tokens stay in memory. Either way the access token is renewed shortly
// before it expires, so a long session keeps working.

import (
	"context"
	"crypto/rand"
	"crypto/sha256"
	"encoding/base64"
	"encoding/json"
	"errors"
	"fmt"
	"html"
	"io"
	"net"
	"net/http"
	"net/url"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strings"
	"sync"
	"time"
)

const (
	defaultClientID     = "databricks-cli"
	defaultRedirectPort = 8020
	renewBefore         = 5 * time.Minute
	signInTimeout       = 5 * time.Minute
)

type auth struct {
	token string // auth_type pat
	oauth *oauth
}

type oauth struct {
	mu           sync.Mutex
	base         string
	clientID     string
	clientSecret string // set for a service principal
	scopes       string
	redirectPort int
	sessionKey   string // empty for a service principal
	tokens       *tokens
	http         *http.Client
}

type tokens struct {
	access    string
	refresh   string
	expiresAt int64 // unix seconds
}

func authFromConn(conn map[string]any, base string) (*auth, error) {
	t := optional(conn, "auth_type")
	if t == "" {
		t = "pat"
	}
	switch t {
	case "pat", "token":
		tok, err := required(conn, "token")
		if err != nil {
			return nil, err
		}
		return &auth{token: tok}, nil
	case "oauth":
		secret := optional(conn, "client_secret")
		id := optional(conn, "client_id")
		if id == "" {
			if secret != "" {
				return nil, errors.New("`auth_type: oauth` with a `client_secret` also needs the service principal's `client_id`")
			}
			id = defaultClientID
		}
		scopes := optional(conn, "scopes")
		if scopes == "" {
			scopes = "all-apis offline_access"
			if secret != "" {
				scopes = "all-apis"
			}
		}
		port := defaultRedirectPort
		if v, ok := conn["redirect_port"]; ok && v != nil {
			p, ok := number(v)
			if !ok || p <= 0 || p > 65535 {
				return nil, errors.New("`redirect_port` must be a port number")
			}
			port = p
		}
		o := &oauth{
			base: base, clientID: id, clientSecret: secret, scopes: scopes, redirectPort: port,
			http: &http.Client{Timeout: 60 * time.Second},
		}
		if secret == "" {
			o.sessionKey = sessionKey(base, id)
		}
		return &auth{oauth: o}, nil
	default:
		return nil, fmt.Errorf("unknown `auth_type` `%s`; use `pat` or `oauth`", t)
	}
}

// bearer returns the token for the next request, renewed first when it's about to expire.
func (a *auth) bearer() (string, error) {
	if a.oauth == nil {
		return a.token, nil
	}
	return a.oauth.bearer()
}

func (o *oauth) bearer() (string, error) {
	o.mu.Lock()
	defer o.mu.Unlock()
	if o.tokens == nil && o.sessionKey != "" {
		o.tokens = loadSession(o.sessionKey)
	}
	if o.tokens != nil && time.Unix(o.tokens.expiresAt, 0).After(time.Now().Add(renewBefore)) {
		return o.tokens.access, nil
	}
	var fresh *tokens
	var err error
	if o.clientSecret != "" {
		fresh, err = o.clientCredentials()
	} else {
		// A refresh token can be revoked or expire; fall back to signing in again.
		if o.tokens != nil && o.tokens.refresh != "" {
			fresh, _ = o.refresh(o.tokens.refresh)
		}
		if fresh == nil {
			fresh, err = o.signIn()
		}
	}
	if err != nil {
		return "", err
	}
	o.tokens = fresh
	if o.sessionKey != "" {
		// Best effort: a session that can't be saved only means signing in again next time.
		if err := storeSession(o.sessionKey, fresh); err != nil {
			fmt.Fprintf(os.Stderr, "couldn't save the Databricks session in %s: %v\n", sessionsPath(), err)
		}
	}
	return fresh.access, nil
}

func (o *oauth) tokenURL() string    { return o.base + "/oidc/v1/token" }
func (o *oauth) redirectURI() string { return fmt.Sprintf("http://localhost:%d", o.redirectPort) }

func (o *oauth) clientCredentials() (*tokens, error) {
	basic := base64.StdEncoding.EncodeToString([]byte(o.clientID + ":" + o.clientSecret))
	return o.tokenRequest(url.Values{"grant_type": {"client_credentials"}, "scope": {o.scopes}}, basic)
}

func (o *oauth) refresh(refresh string) (*tokens, error) {
	t, err := o.tokenRequest(url.Values{
		"grant_type": {"refresh_token"}, "client_id": {o.clientID}, "refresh_token": {refresh},
	}, "")
	if err != nil {
		return nil, err
	}
	// Not every response rotates the refresh token; keep the old one when it doesn't.
	if t.refresh == "" {
		t.refresh = refresh
	}
	return t, nil
}

// signIn runs the authorization code flow with PKCE: open the browser, wait for the redirect,
// exchange the code.
func (o *oauth) signIn() (*tokens, error) {
	ln, err := net.Listen("tcp", fmt.Sprintf("127.0.0.1:%d", o.redirectPort))
	if err != nil {
		return nil, fmt.Errorf("can't listen on localhost:%d for the Databricks sign-in redirect: %v", o.redirectPort, err)
	}
	lns := []net.Listener{ln}
	// Browsers may try IPv6 first for "localhost"; listen there too when the machine has it.
	if ln6, err := net.Listen("tcp", fmt.Sprintf("[::1]:%d", o.redirectPort)); err == nil {
		lns = append(lns, ln6)
	}
	verifier, err := randomToken(32)
	if err != nil {
		return nil, err
	}
	sum := sha256.Sum256([]byte(verifier))
	challenge := base64.RawURLEncoding.EncodeToString(sum[:])
	state, err := randomToken(16)
	if err != nil {
		return nil, err
	}
	q := url.Values{
		"client_id": {o.clientID}, "response_type": {"code"}, "redirect_uri": {o.redirectURI()},
		"scope": {o.scopes}, "state": {state}, "code_challenge": {challenge},
		"code_challenge_method": {"S256"},
	}
	authURL := o.base + "/oidc/v1/authorize?" + encodeForm(q)
	announce(authURL)
	code, err := waitForCode(lns, state)
	if err != nil {
		return nil, err
	}
	t, err := o.tokenRequest(url.Values{
		"grant_type": {"authorization_code"}, "client_id": {o.clientID}, "code": {code},
		"redirect_uri": {o.redirectURI()}, "code_verifier": {verifier},
	}, "")
	if err != nil {
		return nil, err
	}
	fmt.Fprintln(os.Stderr, "Signed in to Databricks.")
	return t, nil
}

func (o *oauth) tokenRequest(form url.Values, basic string) (*tokens, error) {
	req, err := http.NewRequest("POST", o.tokenURL(), strings.NewReader(encodeForm(form)))
	if err != nil {
		return nil, err
	}
	req.Header.Set("Content-Type", "application/x-www-form-urlencoded")
	req.Header.Set("Accept", "application/json")
	req.Header.Set("User-Agent", "dre")
	if basic != "" {
		req.Header.Set("Authorization", "Basic "+basic)
	}
	resp, err := o.http.Do(req)
	if err != nil {
		return nil, fmt.Errorf("can't reach the Databricks token endpoint %s: %v", o.tokenURL(), err)
	}
	defer resp.Body.Close()
	text, _ := io.ReadAll(resp.Body)
	var body struct {
		Access    string `json:"access_token"`
		Refresh   string `json:"refresh_token"`
		ExpiresIn *int64 `json:"expires_in"`
		Error     string `json:"error"`
		Desc      string `json:"error_description"`
	}
	_ = json.Unmarshal(text, &body)
	if resp.StatusCode != 200 {
		detail := body.Error
		switch {
		case body.Error != "" && body.Desc != "":
			detail = body.Error + ": " + body.Desc
		case body.Error == "":
			detail = truncate(string(text), 300)
		}
		hint := ""
		if o.clientSecret != "" {
			hint = " (check `client_id` and `client_secret`, and that the service principal can use this workspace)"
		}
		return nil, fmt.Errorf("Databricks OAuth failed (HTTP %d)%s: %s", resp.StatusCode, hint, detail)
	}
	if body.Access == "" {
		return nil, errors.New("the Databricks token endpoint returned no access_token")
	}
	expires := int64(3600)
	if body.ExpiresIn != nil {
		expires = *body.ExpiresIn
	}
	return &tokens{access: body.Access, refresh: body.Refresh, expiresAt: time.Now().Unix() + expires}, nil
}

// waitForCode accepts connections on the redirect listener until one carries the code or an
// error. A browser also asks for /favicon.ico; that isn't the redirect.
func waitForCode(lns []net.Listener, state string) (string, error) {
	type outcome struct {
		code string
		err  error
	}
	done := make(chan outcome, 1)
	srv := &http.Server{ReadHeaderTimeout: 10 * time.Second}
	srv.Handler = http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		q := r.URL.Query()
		code, e := q.Get("code"), q.Get("error")
		if code == "" && e == "" {
			http.NotFound(w, r)
			return
		}
		var out outcome
		switch {
		case e != "":
			msg := "Databricks sign-in failed: " + e
			if d := q.Get("error_description"); d != "" {
				msg += ": " + d
			}
			out.err = errors.New(msg)
		case q.Get("state") != state:
			out.err = errors.New("the Databricks sign-in redirect didn't match this sign-in (state mismatch)")
		default:
			out.code = code
		}
		w.Header().Set("Content-Type", "text/html; charset=utf-8")
		w.Header().Set("Connection", "close")
		if out.err == nil {
			io.WriteString(w, "<!doctype html><title>DRE</title><h1>Signed in</h1><p>You can close this tab and return to DRE.</p>")
		} else {
			io.WriteString(w, "<!doctype html><title>DRE</title><h1>Sign-in failed</h1><p>"+html.EscapeString(out.err.Error())+"</p>")
		}
		select {
		case done <- out:
		default:
		}
	})
	for _, ln := range lns {
		go srv.Serve(ln)
	}
	// Let the browser get its page before the listener goes away.
	defer func() {
		ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
		defer cancel()
		srv.Shutdown(ctx)
	}()
	select {
	case out := <-done:
		return out.code, out.err
	case <-time.After(signInTimeout):
		return "", errors.New("timed out waiting for the Databricks sign-in in the browser")
	}
}

// announce shows the sign-in URL and opens the browser; tests replace it to play the browser.
var announce = func(authURL string) {
	fmt.Fprintf(os.Stderr, "Sign in to Databricks in your browser. If it doesn't open, visit:\n%s\n", authURL)
	openBrowser(authURL)
}

// openBrowser opens url in the default browser. DRE_NO_BROWSER=1 only prints it.
func openBrowser(u string) {
	if os.Getenv("DRE_NO_BROWSER") != "" {
		return
	}
	var cmd *exec.Cmd
	switch runtime.GOOS {
	case "darwin":
		cmd = exec.Command("open", u)
	case "windows":
		cmd = exec.Command("rundll32", "url.dll,FileProtocolHandler", u)
	default:
		cmd = exec.Command("xdg-open", u)
	}
	_ = cmd.Start()
}

func randomToken(n int) (string, error) {
	b := make([]byte, n)
	if _, err := rand.Read(b); err != nil {
		return "", fmt.Errorf("no randomness for the OAuth sign-in: %v", err)
	}
	return base64.RawURLEncoding.EncodeToString(b), nil
}

// encodeForm is application/x-www-form-urlencoded with %20 for spaces and a stable key order.
func encodeForm(v url.Values) string {
	return strings.ReplaceAll(v.Encode(), "+", "%20")
}

func truncate(s string, n int) string {
	if r := []rune(s); len(r) > n {
		return string(r[:n])
	}
	return s
}

// --- ~/.dre/oauth_sessions.json ---------------------------------------------------------------
//
// One JSON object with an entry per login. Writes take oauth_sessions.json.lock (create-new; a
// lock older than 10 s was left by a dead process), re-read the file, and replace it atomically
// with owner-only permissions, so plugins saving different logins at once don't lose either.
// This matches dre_protocol::sessions in the Rust plugins.

func sessionKey(base, clientID string) string {
	host := base
	if i := strings.Index(base, "://"); i >= 0 {
		host = base[i+3:]
	}
	return "databricks/" + host + "/" + clientID
}

func sessionsPath() string {
	home, _ := os.UserHomeDir()
	return filepath.Join(home, ".dre", "oauth_sessions.json")
}

func readSessions(path string) map[string]any {
	all := map[string]any{}
	if b, err := os.ReadFile(path); err == nil {
		_ = json.Unmarshal(b, &all)
	}
	return all
}

func loadSession(key string) *tokens {
	v, ok := readSessions(sessionsPath())[key].(map[string]any)
	if !ok {
		return nil
	}
	access, _ := v["access_token"].(string)
	if access == "" {
		return nil
	}
	refresh, _ := v["refresh_token"].(string)
	exp, _ := v["expires_at"].(float64)
	return &tokens{access: access, refresh: refresh, expiresAt: int64(exp)}
}

func storeSession(key string, t *tokens) error {
	var refresh any
	if t.refresh != "" {
		refresh = t.refresh
	}
	return storeSessionAt(sessionsPath(), key, map[string]any{
		"access_token": t.access, "refresh_token": refresh, "expires_at": t.expiresAt,
	})
}

// storeSessionAt saves (or with a nil session, forgets) the entry under key.
func storeSessionAt(path, key string, session map[string]any) error {
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		return err
	}
	unlock := lockFile(strings.TrimSuffix(path, ".json") + ".json.lock")
	defer unlock()
	all := readSessions(path)
	if session == nil {
		delete(all, key)
	} else {
		all[key] = session
	}
	b, err := json.MarshalIndent(all, "", "  ")
	if err != nil {
		return err
	}
	tmp := fmt.Sprintf("%s.%d.tmp", path, os.Getpid())
	f, err := os.OpenFile(tmp, os.O_WRONLY|os.O_CREATE|os.O_TRUNC, 0o600)
	if err != nil {
		return err
	}
	if _, err := f.Write(b); err != nil {
		f.Close()
		return err
	}
	if err := f.Close(); err != nil {
		return err
	}
	return os.Rename(tmp, path)
}

// lockFile takes the lock file and returns its release. Better to write unlocked than to fail a
// run over a stuck lock, so after 10 s it gives up waiting.
func lockFile(path string) func() {
	start := time.Now()
	for {
		f, err := os.OpenFile(path, os.O_WRONLY|os.O_CREATE|os.O_EXCL, 0o600)
		if err == nil {
			f.Close()
			return func() { os.Remove(path) }
		}
		if st, err := os.Stat(path); err == nil && time.Since(st.ModTime()) > 10*time.Second {
			os.Remove(path)
			continue
		}
		if time.Since(start) > 10*time.Second {
			return func() {}
		}
		time.Sleep(20 * time.Millisecond)
	}
}
