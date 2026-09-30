- Recommended sign-in (SEC-2): leave the key fields out and use application default credentials
  (`gcloud auth application-default login` for a person, the metadata server on Google Cloud).
  A service account key file (`service_account_key_path`) only if there's no alternative, kept
  outside the project.
- The destination entry takes a `path` and no other options.
