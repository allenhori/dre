package main

// The databricks_workspace destination against a fake Workspace API.

import (
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
)

type wsCall struct {
	path   string
	fields map[string]string
	file   string
}

func fakeWorkspace(t *testing.T) (*httptest.Server, func() []wsCall) {
	var mu sync.Mutex
	var calls []wsCall
	first := true
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		mu.Lock()
		defer mu.Unlock()
		if r.Header.Get("Authorization") != "Bearer good" {
			w.WriteHeader(401)
			io.WriteString(w, `{"error_code":"PERMISSION_DENIED","message":"no"}`)
			return
		}
		if first {
			first = false
			io.Copy(io.Discard, r.Body)
			w.WriteHeader(503)
			return
		}
		c := wsCall{path: r.URL.Path, fields: map[string]string{}}
		if strings.HasPrefix(r.Header.Get("Content-Type"), "multipart/") {
			if err := r.ParseMultipartForm(1 << 20); err != nil {
				t.Error(err)
			}
			for k, v := range r.MultipartForm.Value {
				c.fields[k] = v[0]
			}
			f, _, _ := r.FormFile("content")
			b, _ := io.ReadAll(f)
			c.file = string(b)
		} else {
			b, _ := io.ReadAll(r.Body)
			c.file = string(b)
		}
		calls = append(calls, c)
		io.WriteString(w, "{}")
	}))
	t.Cleanup(srv.Close)
	return srv, func() []wsCall {
		mu.Lock()
		defer mu.Unlock()
		return append([]wsCall(nil), calls...)
	}
}

func TestWorkspacePaths(t *testing.T) {
	for in, want := range map[string]string{
		"/Workspace/Users/a@b.com/reports/x.csv": "/Users/a@b.com/reports/x.csv",
		"/Workspace/Shared/x.xlsx":               "/Shared/x.xlsx",
		"/Users/a@b.com/x.csv":                   "/Users/a@b.com/x.csv",
	} {
		api, shown, err := workspacePath(in)
		if err != nil || api != want || shown != "/Workspace"+want {
			t.Fatalf("%s: %s %s %v", in, api, shown, err)
		}
	}
	for _, bad := range []string{"/Volumes/c/s/v/x.csv", "/Workspace/x.csv", "/Workspace/Users/../x", "/tmp/x"} {
		if _, _, err := workspacePath(bad); err == nil || !strings.Contains(err.Error(), "must be a workspace file path") {
			t.Fatalf("%s: %v", bad, err)
		}
	}
}

func TestMakesTheFolderThenImportsARawFile(t *testing.T) {
	srv, calls := fakeWorkspace(t)
	loc, err := deliverToWorkspace(localFile(t, "a,b\r\n1,2\r\n"), "/Workspace/Users/a@b.com/reports/2026/jan.csv",
		map[string]any{"host": srv.URL, "token": "good"})
	if err != nil || loc != "/Workspace/Users/a@b.com/reports/2026/jan.csv" {
		t.Fatal(loc, err)
	}
	got := calls()
	if len(got) != 2 || got[0].path != "/api/2.0/workspace/mkdirs" || !strings.Contains(got[0].file, `"/Users/a@b.com/reports/2026"`) {
		t.Fatalf("%+v", got)
	}
	imp := got[1]
	if imp.path != "/api/2.0/workspace/import" || imp.fields["path"] != "/Users/a@b.com/reports/2026/jan.csv" ||
		imp.fields["format"] != "RAW" || imp.fields["overwrite"] != "true" || imp.file != "a,b\r\n1,2\r\n" {
		t.Fatalf("%+v", imp)
	}
	_, err = deliverToWorkspace(localFile(t, "x"), "/Workspace/Shared/x.csv", map[string]any{"host": srv.URL, "token": "bad"})
	if err == nil || !strings.Contains(err.Error(), "HTTP 401") || !strings.Contains(err.Error(), "PERMISSION_DENIED: no") {
		t.Fatal(err)
	}
}

func TestOnDatabricksComputeTheFileIsCopiedToTheMountedWorkspace(t *testing.T) {
	root := t.TempDir()
	os.MkdirAll(filepath.Join(root, "Workspace", "Shared"), 0o755)
	t.Setenv("DATABRICKS_RUNTIME_VERSION", "15.4")
	t.Setenv("DRE_WORKSPACE_ROOT", root)
	loc, err := deliverToWorkspace(localFile(t, "hello"), "/Workspace/Shared/out/x.csv", map[string]any{})
	if err != nil || loc != "/Workspace/Shared/out/x.csv" {
		t.Fatal(loc, err)
	}
	b, _ := os.ReadFile(filepath.Join(root, "Workspace", "Shared", "out", "x.csv"))
	if string(b) != "hello" {
		t.Fatal(string(b))
	}
}

func TestTheWorkspaceRole(t *testing.T) {
	if roleOf("/x/dre-destination-databricks_workspace.exe").name != "databricks_workspace" ||
		roleOf("dre-destination-databricks_volumes").name != "databricks_volumes" {
		t.Fatal("roleOf")
	}
	c := startAs(t, workspaceRole)
	c.send(map[string]any{"type": "hello", "min_version": 0, "max_version": 0, "core_version": "t"})
	if r := c.reply(); r["kind"] != "destination" || r["name"] != "databricks_workspace" {
		t.Fatalf("%v", r)
	}
	srv, _ := fakeWorkspace(t)
	c.send(map[string]any{"type": "deliver", "local_path": localFile(t, "x"), "remote_path": "/Workspace/Shared/x.csv",
		"connection": map[string]any{"host": srv.URL, "token": "good"}, "options": map[string]any{}})
	if r := c.reply(); r["type"] != "delivered" || r["location"] != "/Workspace/Shared/x.csv" {
		t.Fatalf("%v", r)
	}
}
