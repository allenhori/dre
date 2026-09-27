package main

// The databricks_volumes destination against a fake Files API (there's no emulator).

import (
	"encoding/json"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
)

type filesCall struct {
	method, path string
	size         int
}

// fakeFiles records PUTs; answers 503 once, then 204 (401 without the token `good`).
func fakeFiles(t *testing.T) (*httptest.Server, func() []filesCall) {
	var mu sync.Mutex
	var calls []filesCall
	first := true
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		b, _ := io.ReadAll(r.Body)
		mu.Lock()
		defer mu.Unlock()
		switch {
		case r.Header.Get("Authorization") != "Bearer good":
			w.WriteHeader(401)
			io.WriteString(w, "invalid token")
		case first:
			first = false
			w.WriteHeader(503)
		default:
			calls = append(calls, filesCall{r.Method, r.URL.RequestURI(), len(b)})
			w.WriteHeader(204)
		}
	}))
	t.Cleanup(srv.Close)
	return srv, func() []filesCall {
		mu.Lock()
		defer mu.Unlock()
		return append([]filesCall(nil), calls...)
	}
}

func localFile(t *testing.T, body string) string {
	p := filepath.Join(t.TempDir(), "report.csv")
	os.WriteFile(p, []byte(body), 0o600)
	return p
}

func TestCreatesDirectoriesThenUploadsTheFile(t *testing.T) {
	srv, calls := fakeFiles(t)
	loc, err := deliverToVolume(localFile(t, "a,b\r\n1,2\r\n"), "/Volumes/main/client_a/reports/2026/Jan report.csv",
		map[string]any{"host": srv.URL, "token": "good"})
	if err != nil || loc != "dbfs:/Volumes/main/client_a/reports/2026/Jan report.csv" {
		t.Fatalf("%q %v", loc, err)
	}
	want := []filesCall{
		{"PUT", "/api/2.0/fs/directories/Volumes/main/client_a/reports/2026", 0},
		{"PUT", "/api/2.0/fs/files/Volumes/main/client_a/reports/2026/Jan%20report.csv?overwrite=true", 10},
	}
	if got := calls(); len(got) != 2 || got[0] != want[0] || got[1] != want[1] {
		t.Fatalf("%v", got)
	}
}

func TestPathsOutsideAVolumeAndBadTokensAreClearErrors(t *testing.T) {
	srv, _ := fakeFiles(t)
	_, err := deliverToVolume(localFile(t, "x"), "/tmp/x.csv", map[string]any{"host": srv.URL, "token": "good"})
	if err == nil || !strings.Contains(err.Error(), "must be /Volumes/<catalog>/<schema>/<volume>/<file>") {
		t.Fatal(err)
	}
	_, err = deliverToVolume(localFile(t, "x"), "/Volumes/c/s/v/x.csv", map[string]any{"host": srv.URL, "token": "bad"})
	if err == nil || !strings.Contains(err.Error(), "HTTP 401") || !strings.Contains(err.Error(), "still in target/") {
		t.Fatal(err)
	}
}

func TestOAuthUsesTheSessionTheSourceSavedForTheWorkspace(t *testing.T) {
	home := isolatedHome(t)
	srv, calls := fakeFiles(t)
	host := strings.TrimPrefix(srv.URL, "http://")
	b, _ := json.Marshal(map[string]any{
		"databricks/" + host + "/databricks-cli":    map[string]any{"access_token": "good", "refresh_token": "r", "expires_at": 4_000_000_000},
		"databricks/other-workspace/databricks-cli": map[string]any{"access_token": "bad", "expires_at": 4_000_000_000},
	})
	os.MkdirAll(filepath.Join(home, ".dre"), 0o700)
	os.WriteFile(filepath.Join(home, ".dre", "oauth_sessions.json"), b, 0o600)
	old := announce
	t.Cleanup(func() { announce = old })
	announce = func(string) { t.Fatal("browser opened") }
	if _, err := deliverToVolume(localFile(t, "x"), "/Volumes/c/s/v/x.csv", map[string]any{"host": srv.URL, "auth_type": "oauth"}); err != nil {
		t.Fatal(err)
	}
	if len(calls()) != 1 {
		t.Fatalf("%v", calls())
	}
}

func TestOnDatabricksComputeAMountedVolumeIsWrittenDirectly(t *testing.T) {
	isolatedHome(t)
	root := t.TempDir()
	os.MkdirAll(filepath.Join(root, "Volumes", "main", "fin", "out"), 0o755)
	t.Setenv("DATABRICKS_RUNTIME_VERSION", "16.4")
	t.Setenv("DRE_VOLUMES_ROOT", root)
	// No host and no login: none are needed on the mount.
	loc, err := deliverToVolume(localFile(t, "a,b\r\n"), "/Volumes/main/fin/out/2026/daily.csv", map[string]any{})
	if err != nil || loc != "/Volumes/main/fin/out/2026/daily.csv" {
		t.Fatalf("%q %v", loc, err)
	}
	if b, _ := os.ReadFile(filepath.Join(root, "Volumes", "main", "fin", "out", "2026", "daily.csv")); string(b) != "a,b\r\n" {
		t.Fatalf("%q", b)
	}
	// A volume that isn't mounted here goes through the Files API as usual.
	srv, calls := fakeFiles(t)
	if _, err := deliverToVolume(localFile(t, "x"), "/Volumes/other/s/v/x.csv", map[string]any{"host": srv.URL, "token": "good"}); err != nil || len(calls()) != 1 {
		t.Fatalf("%v %v", err, calls())
	}
}

func TestRESTErrorsShowTheCodeAndMessage(t *testing.T) {
	body := []byte("{\n  \"error_code\" : \"NOT_FOUND\",\n  \"message\" : \"Volume 'w.s.v' does not exist.\",\n  \"details\" : [ ]\n}")
	if got := apiError(body); got != "NOT_FOUND: Volume 'w.s.v' does not exist." {
		t.Fatal(got)
	}
	if got := apiError([]byte("invalid token")); got != "invalid token" {
		t.Fatal(got)
	}
}

func TestTheDestinationRoleSpeaksTheProtocol(t *testing.T) {
	if roleOf("/x/dre-destination-databricks_volumes.exe").name != "databricks_volumes" || roleOf("dre-source-databricks").name != "databricks" {
		t.Fatal("roleOf")
	}
	c := startAs(t, destinationRole)
	c.send(map[string]any{"type": "hello", "min_version": 0, "max_version": 0, "core_version": "t"})
	if r := c.reply(); r["kind"] != "destination" || r["name"] != "databricks_volumes" {
		t.Fatalf("%v", r)
	}
	c.send(map[string]any{"type": "describe"})
	var names []string
	for _, f := range c.reply()["connection_fields"].([]any) {
		names = append(names, f.(map[string]any)["name"].(string))
	}
	if strings.Join(names, ",") != "host,auth_type,token,client_id,client_secret" {
		t.Fatalf("%v", names)
	}
	c.send(map[string]any{"type": "deliver", "local_path": "/x", "remote_path": "/Volumes/c/s/v/x", "connection": map[string]any{}, "options": map[string]any{"to": "x"}})
	expectError(t, c.reply(), "takes no options, but the destination entry has `to`")
	c.send(map[string]any{"type": "execute", "sql": "select 1"})
	expectError(t, c.reply(), "a destination plugin doesn't handle execute requests")
	srv, _ := fakeFiles(t)
	c.send(map[string]any{"type": "deliver", "local_path": localFile(t, "x"), "remote_path": "/Volumes/c/s/v/x.csv",
		"connection": map[string]any{"host": srv.URL, "token": "good"}, "options": map[string]any{}})
	if r := c.reply(); r["type"] != "delivered" || r["location"] != "dbfs:/Volumes/c/s/v/x.csv" {
		t.Fatalf("%v", r)
	}
}
