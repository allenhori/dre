package main

// The databricks destination: a Unity Catalog Volume or a workspace file, chosen by the path.
// Profile target fields: host plus the source's sign-in fields (auth_type, token, client_id,
// client_secret), so one set of credentials, and one OAuth session per workspace, serves both.

import (
	"fmt"
	"strings"
)

// deliver sends local to remote: /Volumes/... to a Volume (volumes.go), /Workspace/... (or
// /Users/..., /Shared/..., /Repos/...) to a workspace file (workspace.go).
func deliver(local, remote string, conn map[string]any) (string, error) {
	if remote == "" {
		return "", fmt.Errorf("the databricks destination needs `output.destination.path`: /Volumes/<catalog>/<schema>/<volume>/<file> or /Workspace/...")
	}
	switch strings.SplitN(strings.TrimLeft(remote, "/"), "/", 2)[0] {
	case "Volumes":
		return deliverToVolume(local, remote, conn)
	case "Workspace", "Users", "Shared", "Repos":
		return deliverToWorkspace(local, remote, conn)
	}
	return "", fmt.Errorf("`%s` must be a Volume path (/Volumes/<catalog>/<schema>/<volume>/<file>) or a workspace file path (/Workspace/Users/<user>/..., /Workspace/Shared/... or /Workspace/Repos/...)", remote)
}
