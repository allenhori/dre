- `password` always comes from `env_var()` (SEC-3); many providers need an app password rather
  than the account's own.
- Recipients, subject and body belong to the report's destination entry; the profile's `to`,
  `cc` and `bcc` are only defaults.
- Most mail servers cap a message at 20–25 MB; for larger outputs, deliver to object storage and
  put a link in `body`.
- Sending to real people needs a confirmation before the run (RUN-2).
