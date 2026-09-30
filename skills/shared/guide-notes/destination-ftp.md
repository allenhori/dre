- `password` always comes from `env_var()` (SEC-3).
- Recommend `tls: explicit` (FTPS) whenever the server supports it: plain FTP sends the password
  and the file unencrypted. Prefer SFTP if the receiving side offers it.
- Paths are relative to the login folder; a leading `/` is the server's root.
- The destination entry takes a `path` and no other options.
