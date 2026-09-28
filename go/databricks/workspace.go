package main

// The databricks destination's workspace paths: writes the output as a workspace file
// (/Workspace/Users/..., /Workspace/Shared/..., /Workspace/Repos/...) through the Workspace API,
// creating missing folders. On Databricks compute, where /Workspace is mounted, it copies the
// file there instead, with the job or cluster's own access. Sign-in is the same as the source's.
//
// Files are imported as plain files (format RAW), never converted to notebooks, and replace
// an existing file at the path.

import (
	"bytes"
	"encoding/json"
	"fmt"
	"io"
	"mime/multipart"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"time"
)

// workspacePath checks remote and returns the path as the Workspace API takes it (without the
// /Workspace prefix) and as shown to the person (with it).
func workspacePath(remote string) (api, shown string, err error) {
	p := "/" + strings.Trim(remote, "/")
	api = strings.TrimPrefix(p, "/Workspace")
	parts := strings.Split(strings.TrimPrefix(api, "/"), "/")
	top := map[string]bool{"Users": true, "Shared": true, "Repos": true}
	bad := len(parts) < 2 || !top[parts[0]] || !strings.HasPrefix(p, "/Workspace/") && !strings.HasPrefix(p, "/"+parts[0]+"/")
	for _, s := range parts {
		bad = bad || s == "" || s == "." || s == ".."
	}
	if bad {
		return "", "", fmt.Errorf("`%s` must be a workspace file path: /Workspace/Users/<user>/..., /Workspace/Shared/... or /Workspace/Repos/...", remote)
	}
	return api, "/Workspace" + api, nil
}

func deliverToWorkspace(local, remote string, conn map[string]any) (string, error) {
	if remote == "" {
		return "", fmt.Errorf("the databricks destination needs `output.destination.path`")
	}
	api, shown, err := workspacePath(remote)
	if err != nil {
		return "", err
	}
	if loc, ok, err := copyToMountedWorkspace(local, shown); ok || err != nil {
		return loc, err
	}
	host, err := required(conn, "host")
	if err != nil {
		return "", err
	}
	base := baseURL(host)
	if err := checkAuthType(conn); err != nil {
		return "", err
	}
	if err := reachable(base); err != nil {
		return "", err
	}
	a, err := authFromConn(conn, base)
	if err != nil {
		return "", err
	}
	client := &http.Client{}
	dir := api[:strings.LastIndex(api, "/")]
	mkdirs, _ := json.Marshal(map[string]string{"path": dir})
	if err := workspaceRequest(client, base+"/api/2.0/workspace/mkdirs", a, func() (io.Reader, string, error) {
		return bytes.NewReader(mkdirs), "application/json", nil
	}); err != nil {
		return "", fmt.Errorf("can't create /Workspace%s: %v", dir, err)
	}
	err = workspaceRequest(client, base+"/api/2.0/workspace/import", a, func() (io.Reader, string, error) {
		return importForm(local, api)
	})
	if err != nil {
		return "", fmt.Errorf("upload to %s failed: %v; the output is still in target/", shown, err)
	}
	return shown, nil
}

// importForm is the multipart body of a workspace import: the file as is, replacing any file
// already there. The file is streamed, so large outputs don't sit in memory.
func importForm(local, api string) (io.Reader, string, error) {
	f, err := os.Open(local)
	if err != nil {
		return nil, "", fmt.Errorf("can't read %s: %v", local, err)
	}
	pr, pw := io.Pipe()
	mw := multipart.NewWriter(pw)
	go func() {
		defer f.Close()
		fields := [][2]string{{"path", api}, {"format", "RAW"}, {"overwrite", "true"}}
		for _, kv := range fields {
			if err := mw.WriteField(kv[0], kv[1]); err != nil {
				pw.CloseWithError(err)
				return
			}
		}
		part, err := mw.CreateFormFile("content", filepath.Base(local))
		if err == nil {
			_, err = io.Copy(part, f)
		}
		if err == nil {
			err = mw.Close()
		}
		pw.CloseWithError(err)
	}()
	return pr, mw.FormDataContentType(), nil
}

// workspaceRequest POSTs the body from mk (made afresh for each attempt), retrying while the
// workspace answers 429/503.
func workspaceRequest(client *http.Client, url string, a *auth, mk func() (io.Reader, string, error)) error {
	start := time.Now()
	wait := time.Second
	for {
		bearer, err := a.bearer()
		if err != nil {
			return err
		}
		body, ctype, err := mk()
		if err != nil {
			return err
		}
		req, err := http.NewRequest("POST", url, body)
		if err != nil {
			return err
		}
		req.Header.Set("Authorization", "Bearer "+bearer)
		req.Header.Set("User-Agent", "dre")
		req.Header.Set("Content-Type", ctype)
		resp, err := client.Do(req)
		if c, ok := body.(io.Closer); ok {
			c.Close()
		}
		if err != nil {
			return fmt.Errorf("can't reach Databricks: %v", err)
		}
		text, _ := io.ReadAll(resp.Body)
		resp.Body.Close()
		if resp.StatusCode >= 200 && resp.StatusCode < 300 {
			return nil
		}
		if (resp.StatusCode == 429 || resp.StatusCode == 503) && time.Since(start) < 300*time.Second {
			time.Sleep(wait)
			wait = min(wait*2, 30*time.Second)
			continue
		}
		hint := ""
		switch resp.StatusCode {
		case 401, 403:
			hint = " (check the token and its permissions on the folder)"
			if a.oauth != nil {
				hint = " (check the signed-in identity's permissions on the folder)"
			}
		case 404:
			hint = " (check the user or repo folder exists)"
		}
		return fmt.Errorf("HTTP %d%s: %s", resp.StatusCode, hint, apiError(text))
	}
}

// copyToMountedWorkspace writes to /Workspace/... directly on Databricks compute, where the
// workspace is mounted. ok is false when it doesn't apply. DRE_WORKSPACE_ROOT stands in for /
// in tests.
func copyToMountedWorkspace(local, shown string) (loc string, ok bool, err error) {
	if os.Getenv("DATABRICKS_RUNTIME_VERSION") == "" {
		return "", false, nil
	}
	root := os.Getenv("DRE_WORKSPACE_ROOT")
	if root == "" {
		root = "/"
	}
	if st, err := os.Stat(filepath.Join(root, "Workspace")); err != nil || !st.IsDir() {
		return "", false, nil
	}
	dest := filepath.Join(root, filepath.FromSlash(strings.TrimPrefix(shown, "/")))
	fail := func(err error) (string, bool, error) {
		return "", true, fmt.Errorf("copy to %s failed: %v; the output is still in target/", shown, err)
	}
	if err := os.MkdirAll(filepath.Dir(dest), 0o755); err != nil {
		return fail(err)
	}
	in, err := os.Open(local)
	if err != nil {
		return fail(err)
	}
	defer in.Close()
	out, err := os.Create(dest)
	if err != nil {
		return fail(err)
	}
	if _, err := io.Copy(out, in); err != nil {
		out.Close()
		return fail(err)
	}
	if err := out.Close(); err != nil {
		return fail(err)
	}
	return shown, true, nil
}
