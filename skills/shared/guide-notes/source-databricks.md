- The plugin's `describe` reply gives `auth_type`'s default as `pat`, but the plugin defaults to
  `auto` (see the docs above). Don't write `auth_type: pat` unless the user chose a token.
- Recommended sign-in (SEC-2), in order:
  1. a person at a laptop: `auth_type: oauth`, a browser sign-in; `dre` keeps the session in
     `~/.dre/oauth_sessions.json`, readable only by the user;
  2. someone already signed in with the Databricks CLI (`databricks auth login`), or with a
     `~/.databrickscfg` profile: leave `auth_type` out (`auto`), with `profile:` if it isn't the
     default one;
  3. a scheduler, CI or a Databricks job: a service principal, `auth_type: oauth` with
     `client_id` and `client_secret` from `env_var()` (SEC-3);
  4. a personal access token only if none of these is possible: `token` from `env_var()`.
- `host` and `http_path` are on the SQL warehouse's Connection details tab. They aren't secret.
- `catalog` and `schema` save writing them in every query; `target.catalog` and `target.schema`
  make SQL follow the environment (SET-2).
- A stopped warehouse starts on the first query; DRE waits up to `retry_timeout` seconds and
  says so every 30 seconds.
- With no one at the terminal, browser sign-in fails at once and lists what would work.
