- Recommended sign-in (SEC-2): leave `access_key_id` and `secret_access_key` out and use AWS's
  credential chain, e.g. `aws sso login` (with `profile:` or `AWS_PROFILE` for a named profile),
  or an instance or container role. Keys from `env_var()` only if there's no alternative (SEC-3).
- Set `region` to the bucket's region.
- Name paths with dates and variables so runs don't overwrite each other.
- The destination entry takes a `path` and no other options.
