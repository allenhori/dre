- One sign-in serves the source and the destination for the same workspace: reuse the source's
  `host` and sign-in fields (the source's guide notes give the order to recommend).
- The `describe` reply gives `auth_type`'s default as `pat`, but the plugin defaults to `auto`.
  Don't write `auth_type: pat` unless the user chose a token.
- Recommend a Unity Catalog Volume path (`/Volumes/<catalog>/<schema>/<volume>/...`) for any
  file; workspace files (`/Workspace/...`) only for small files people open in the workspace.
- The destination entry takes a `path` and no other options.
