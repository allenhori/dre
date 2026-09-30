- Sign-in: Postgres has no sign-in that stores nothing, so `password` is always
  `"{{ env_var('<NAME>') }}"` (SEC-2, SEC-3), set by the user.
- Recommend `sslmode: require` (or `verify-full` with `sslrootcert`) for any server that isn't on
  the user's own machine; `prefer`, the default, falls back to no TLS without saying so.
- Recommend a read-only database user for reports.
- Cast unconstrained `numeric` to `numeric(p,s)` in SQL, or it arrives as text (see the docs
  above). Types DRE can't map need a cast, e.g. `::text`.
- On macOS, a `verify-ca` or `verify-full` server certificate valid for more than 825 days is
  rejected; see the docs above.
