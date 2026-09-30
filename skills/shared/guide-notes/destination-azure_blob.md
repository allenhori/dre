- Recommended sign-in (SEC-2): `use_azure_cli: true` after `az login` for a person,
  `use_managed_identity: true` on Azure. `connection_string`, `sas_token` or `access_key` from
  `env_var()` only if there's no alternative (SEC-3).
- The destination entry takes a `path` and no other options.
