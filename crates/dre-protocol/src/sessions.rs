//! `~/.dre/oauth_sessions.json`: OAuth sessions plugins keep between runs.
//!
//! One JSON object with an entry per login, keyed by the plugin as `<platform>/<host>/<client>`,
//! so each workspace or account has its own entry and a source and destination on the same one
//! share it. Plugins read their entry when they connect and write it after signing in or
//! renewing a token. Only sessions go here: long-lived secrets stay in the environment.
//!
//! The file is only readable by its owner: mode 0600 on Unix, and on Windows the home folder's
//! access list already limits it to the user. Writes lock the file, re-read it and replace it
//! atomically, so two plugins saving different logins at once don't lose either.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{Map, Value};

pub const FILE: &str = "oauth_sessions.json";

/// `~/.dre/oauth_sessions.json`.
pub fn path() -> PathBuf {
    std::env::home_dir().unwrap_or_default().join(".dre").join(FILE)
}

/// The session saved under `key`, if any.
pub fn load(key: &str) -> Option<Value> {
    read(&path()).remove(key)
}

/// Save (or with `None`, forget) the session under `key`.
pub fn store(key: &str, session: Option<Value>) -> std::io::Result<()> {
    store_at(&path(), key, session)
}

fn store_at(path: &Path, key: &str, session: Option<Value>) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let _lock = Lock::acquire(&path.with_extension("json.lock"));
    let mut all = read(path);
    match session {
        Some(s) => all.insert(key.to_string(), s),
        None => all.remove(key),
    };
    let bytes = serde_json::to_vec_pretty(&Value::Object(all)).unwrap_or_default();
    let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
    let mut opts = OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    opts.open(&tmp)?.write_all(&bytes)?;
    std::fs::rename(&tmp, path)
}

/// Every saved session; an unreadable or missing file is empty.
fn read(path: &Path) -> Map<String, Value> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// A lock file held while the sessions file is rewritten. A lock older than a few seconds was
/// left by a process that died, so it's taken over rather than waited on forever.
struct Lock(Option<PathBuf>);

impl Lock {
    fn acquire(path: &Path) -> Lock {
        let started = Instant::now();
        loop {
            if OpenOptions::new().write(true).create_new(true).open(path).is_ok() {
                return Lock(Some(path.to_path_buf()));
            }
            let stale = std::fs::metadata(path)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age > Duration::from_secs(10));
            if stale {
                let _ = std::fs::remove_file(path);
                continue;
            }
            if started.elapsed() > Duration::from_secs(10) {
                // Better to write unlocked than to fail a run over a stuck lock.
                return Lock(None);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        if let Some(p) = &self.0 {
            let _ = std::fs::remove_file(p);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn concurrent_logins_keep_every_entry_and_the_file_is_private() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".dre").join(FILE);
        std::thread::scope(|s| {
            for i in 0..8 {
                let path = &path;
                s.spawn(move || {
                    store_at(path, &format!("databricks/ws-{i}/c"), Some(json!({"n": i}))).unwrap()
                });
            }
        });
        let all = read(&path);
        assert_eq!(all.len(), 8);
        assert_eq!(all["databricks/ws-3/c"], json!({"n": 3}));
        store_at(&path, "databricks/ws-3/c", None).unwrap();
        assert!(!read(&path).contains_key("databricks/ws-3/c"));
        assert!(!path.with_extension("json.lock").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }
}
