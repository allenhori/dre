//! FTP delivery against a server, when `DRE_TEST_FTP=host:port` is set (user `dre`,
//! password `dre-pass`, passive ports reachable from the test).

use std::path::Path;
use std::sync::Arc;

use dre_protocol::conformance;
use dre_protocol::host::{LogSink, PluginProcess};
use serde_json::{Value, json};

fn bin() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_dre-plugin-ftp"))
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

#[test]
fn conforms_to_the_protocol() {
    conformance::assert_conforms(bin());
}

#[test]
fn uploads_in_passive_mode_creating_directories() {
    let Ok(server) = std::env::var("DRE_TEST_FTP") else {
        eprintln!("skipped: set DRE_TEST_FTP=host:port");
        return;
    };
    let (host, port) = server.split_once(':').unwrap();
    let conn = json!({"host": host, "port": port, "username": "dre", "password": "dre-pass"});
    let data: Vec<u8> = (0..2_000_000u32).map(|i| (i % 253) as u8).collect();
    let loc = deliver("reports/2026/monthly.csv", conn.clone(), &data).unwrap();
    assert_eq!(loc, format!("ftp://dre@{host}:{port}/reports/2026/monthly.csv"));
    // Again, into the same (now existing) directories.
    deliver("reports/2026/monthly.csv", conn.clone(), b"again").unwrap();

    let mut bad = conn.clone();
    bad["password"] = json!("wrong");
    assert!(
        deliver("x.csv", bad, b"x")
            .unwrap_err()
            .contains("refused the credentials")
    );
    // This server doesn't offer TLS: asking for FTPS fails clearly instead of sending in clear.
    let mut tls = conn;
    tls["tls"] = json!("explicit");
    assert!(
        deliver("x.csv", tls, b"x")
            .unwrap_err()
            .contains("didn't accept explicit FTPS")
    );
}

#[test]
fn explicit_ftps_reuses_the_tls_session_for_data() {
    // vsftpd with its defaults (`require_ssl_reuse=YES`) and a self-signed certificate.
    let Ok(server) = std::env::var("DRE_TEST_FTPS") else {
        eprintln!("skipped: set DRE_TEST_FTPS=host:port");
        return;
    };
    let (host, port) = server.split_once(':').unwrap();
    let conn = json!({"host": host, "port": port, "username": "dre", "password": "dre-pass",
                      "tls": "explicit", "tls_accept_invalid_certs": true});
    let data: Vec<u8> = (0..500_000u32).map(|i| (i % 251) as u8).collect();
    let loc = deliver("tls/reports/monthly.csv", conn.clone(), &data).unwrap();
    assert_eq!(loc, format!("ftp://dre@{host}:{port}/tls/reports/monthly.csv"));
    deliver("tls/reports/monthly.csv", conn.clone(), b"again").unwrap();

    // Without accepting it, the self-signed certificate is refused.
    let mut strict = conn;
    strict["tls_accept_invalid_certs"] = json!(false);
    let e = deliver("tls/x.csv", strict, b"x").unwrap_err();
    assert!(e.contains("didn't accept explicit FTPS"), "{e}");
}
