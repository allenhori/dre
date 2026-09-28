//! SFTP delivery against a server, when `DRE_TEST_SFTP=host:port` is set (user `dre`, password
//! `dre-pass`, writable `upload/`; `DRE_TEST_SFTP_KEY` is a private key the server accepts).

use std::path::Path;
use std::sync::{Arc, OnceLock};

use dre_protocol::conformance;
use dre_protocol::host::{LogSink, PluginProcess};
use serde_json::{Value, json};

fn bin() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_dre-plugin-sftp"))
}

fn server() -> Option<(String, u16)> {
    let v = std::env::var("DRE_TEST_SFTP").ok()?;
    let (h, p) = v.split_once(':')?;
    Some((h.to_string(), p.parse().ok()?))
}

fn deliver(remote: &str, conn: Value, bytes: &[u8]) -> Result<String, String> {
    let dir = tempfile::tempdir().unwrap();
    let local = dir.path().join("report.csv");
    std::fs::write(&local, bytes).unwrap();
    let log: LogSink = Arc::new(|_, _| {});
    let mut p = PluginProcess::start(bin(), log).unwrap();
    let Value::Object(c) = conn else { panic!() };
    p.deliver(local.to_str().unwrap(), Some(remote), c)
        .map_err(|e| e.to_string())
}

/// Scan the server's host keys into a known_hosts file. The scan runs once for all tests:
/// `ssh-keyscan` opens a connection per key type, and several scans at once from the parallel
/// tests pass sshd's `MaxStartups` (10 unauthenticated connections), so it drops some at random.
fn known_hosts(dir: &Path, host: &str, port: u16) -> String {
    static SCAN: OnceLock<Vec<u8>> = OnceLock::new();
    let keys = SCAN.get_or_init(|| {
        std::process::Command::new("ssh-keyscan")
            .args(["-p", &port.to_string(), host])
            .output()
            .unwrap()
            .stdout
    });
    let path = dir.join("known_hosts");
    std::fs::write(&path, keys).unwrap();
    path.to_string_lossy().to_string()
}

fn base(extra: Value) -> Value {
    let (host, port) = server().unwrap();
    let mut v = json!({"host": host, "port": port, "username": "dre", "password": "dre-pass"});
    for (k, x) in extra.as_object().unwrap() {
        v[k] = x.clone();
    }
    v
}

#[test]
fn conforms_to_the_protocol() {
    conformance::assert_conforms(bin());
}

#[test]
fn unknown_hosts_are_refused_until_trusted() {
    let Some((host, port)) = server() else {
        eprintln!("skipped: set DRE_TEST_SFTP=host:port");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let empty = dir.path().join("empty_known_hosts");
    std::fs::write(&empty, "").unwrap();
    let err = deliver("upload/x.csv", base(json!({"known_hosts_path": empty})), b"x").unwrap_err();
    assert!(err.contains("isn't in") && err.contains("SHA256:"), "{err}");

    // Pinning the fingerprint the error reported is enough.
    let fp = err
        .split("pin `host_key_fingerprint: ")
        .nth(1)
        .unwrap()
        .trim_end_matches('`')
        .to_string();
    deliver(
        "upload/pinned.csv",
        base(json!({"known_hosts_path": empty, "host_key_fingerprint": fp})),
        b"x",
    )
    .unwrap();
    let err = deliver(
        "upload/x.csv",
        base(json!({"host_key_fingerprint": "SHA256:AAAAnotthekey"})),
        b"x",
    )
    .unwrap_err();
    assert!(err.contains("not the pinned"), "{err}");

    // A known_hosts entry is trusted too.
    let kh = known_hosts(dir.path(), &host, port);
    deliver("upload/known.csv", base(json!({"known_hosts_path": kh})), b"x").unwrap();
}

#[test]
fn uploads_with_a_password_creating_directories() {
    let Some((host, port)) = server() else { return };
    let dir = tempfile::tempdir().unwrap();
    let kh = known_hosts(dir.path(), &host, port);
    let data: Vec<u8> = (0..3_000_000u32).map(|i| (i % 251) as u8).collect();
    let loc = deliver(
        "upload/2026/01/monthly.csv",
        base(json!({"known_hosts_path": kh})),
        &data,
    )
    .unwrap();
    assert_eq!(
        loc,
        format!("sftp://dre@{host}:{port}/upload/2026/01/monthly.csv")
    );
    let err = deliver(
        "upload/x.csv",
        base(json!({"known_hosts_path": kh, "password": "wrong"})),
        b"x",
    )
    .unwrap_err();
    assert!(err.contains("refused the credentials"), "{err}");
}

#[test]
fn uploads_with_a_private_key() {
    let (Some((host, port)), Ok(key)) = (server(), std::env::var("DRE_TEST_SFTP_KEY")) else {
        eprintln!("skipped: set DRE_TEST_SFTP and DRE_TEST_SFTP_KEY");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let kh = known_hosts(dir.path(), &host, port);
    let conn = json!({"host": host, "port": port, "username": "dre", "private_key_path": key, "known_hosts_path": kh});
    deliver("upload/key.csv", conn, b"a,b\r\n").unwrap();
}
