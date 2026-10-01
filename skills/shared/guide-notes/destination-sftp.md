- Recommended sign-in (SEC-2): a key pair, `private_key_path`, with `private_key_passphrase` from
  `env_var()` if the key has one. Otherwise a password from `env_var()` (SEC-3).
- Keep host key checking on: `known_hosts_path` (default `~/.ssh/known_hosts`) or a pinned
  `host_key_fingerprint`. Don't recommend `accept_unknown_host: true`, except for a first test
  against a server the user controls.
- The destination entry takes a `path` and no other options.
