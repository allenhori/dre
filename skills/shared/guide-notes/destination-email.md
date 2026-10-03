- `password` always comes from `env_var()` (SEC-3); many providers need an app password rather
  than the account's own.
- Recipients, subject and body belong to the report's destination entry; the profile's `to`,
  `cc` and `bcc` are only defaults.
- Email always attaches the file: DRE can't send a link instead and creates no download links.
  Most mail servers cap a message at 20–25 MB. For a larger output, don't offer email: deliver to
  object storage or another destination, and say plainly that the user must tell recipients where
  the file is (and that they need access to that location).
- Sending to real people needs a confirmation before the run (RUN-2).
