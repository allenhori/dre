//! Databricks Volumes against a fake Files API (no emulator exists); against a real workspace
//! when `DRE_TEST_DATABRICKS_HOST`, `_TOKEN` and `_VOLUME` (`/Volumes/c/s/v`) are set.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::sync::{Arc, Mutex};

use dre_protocol::conformance;
use dre_protocol::host::{LogSink, PluginProcess};
use serde_json::{Value, json};

fn bin() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_dre-destination-databricks_volumes"))
}

type Log = Arc<Mutex<Vec<(String, String, usize)>>>;

/// Records `(method, path, body length)`; answers 503 once, then 204 (401 for a bad token).
fn fake() -> (u16, Log) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let log: Log = Arc::default();
    let l = log.clone();
    std::thread::spawn(move || {
        let mut first = true;
        for stream in listener.incoming().flatten() {
            let mut r = BufReader::new(stream.try_clone().unwrap());
            let mut s = stream;
            let mut line = String::new();
            if r.read_line(&mut line).unwrap_or(0) == 0 {
                continue;
            }
            let mut parts = line.split_whitespace();
            let (method, path) = (
                parts.next().unwrap().to_string(),
                parts.next().unwrap().to_string(),
            );
            let (mut len, mut auth) = (0usize, String::new());
            loop {
                let mut h = String::new();
                r.read_line(&mut h).unwrap();
                let h = h.trim_end().to_string();
                if h.is_empty() {
                    break;
                }
                let (k, v) = h.split_once(':').unwrap();
                match k.to_ascii_lowercase().as_str() {
                    "content-length" => len = v.trim().parse().unwrap(),
                    "authorization" => auth = v.trim().to_string(),
                    _ => {}
                }
            }
            let mut body = vec![0; len];
            r.read_exact(&mut body).unwrap();
            let status = if auth != "Bearer good" {
                "401 Unauthorized"
            } else if first {
                first = false;
                "503 Service Unavailable"
            } else {
                l.lock().unwrap().push((method, path, len));
                "204 No Content"
            };
            let _ = write!(
                s,
                "HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
        }
    });
    (port, log)
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
fn creates_directories_then_uploads_the_file() {
    let (port, log) = fake();
    let conn = json!({"host": format!("http://127.0.0.1:{port}"), "token": "good"});
    let loc = deliver(
        "/Volumes/main/client_a/reports/2026/Jan report.csv",
        conn,
        b"a,b\r\n1,2\r\n",
    )
    .unwrap();
    assert_eq!(loc, "dbfs:/Volumes/main/client_a/reports/2026/Jan report.csv");
    let log = log.lock().unwrap();
    assert_eq!(
        *log,
        vec![
            (
                "PUT".into(),
                "/api/2.0/fs/directories/Volumes/main/client_a/reports/2026".into(),
                0
            ),
            (
                "PUT".into(),
                "/api/2.0/fs/files/Volumes/main/client_a/reports/2026/Jan%20report.csv?overwrite=true".into(),
                10
            ),
        ]
    );
}

#[test]
fn paths_outside_a_volume_and_bad_tokens_are_clear_errors() {
    let (port, _) = fake();
    let host = format!("http://127.0.0.1:{port}");
    let err = deliver("/tmp/x.csv", json!({"host": host, "token": "good"}), b"x").unwrap_err();
    assert!(
        err.contains("must be /Volumes/<catalog>/<schema>/<volume>/<file>"),
        "{err}"
    );
    let err = deliver(
        "/Volumes/c/s/v/x.csv",
        json!({"host": host, "token": "bad"}),
        b"x",
    )
    .unwrap_err();
    assert!(
        err.contains("HTTP 401") && err.contains("still in target/"),
        "{err}"
    );
}

#[test]
fn real_workspace_upload() {
    let (Ok(host), Ok(token), Ok(volume)) = (
        std::env::var("DRE_TEST_DATABRICKS_HOST"),
        std::env::var("DRE_TEST_DATABRICKS_TOKEN"),
        std::env::var("DRE_TEST_DATABRICKS_VOLUME"),
    ) else {
        eprintln!("skipped: set DRE_TEST_DATABRICKS_HOST, _TOKEN and _VOLUME");
        return;
    };
    deliver(
        &format!("{volume}/dre-test/probe.csv"),
        json!({"host": host, "token": token}),
        b"a\r\n",
    )
    .unwrap();
}
