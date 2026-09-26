//! Email delivery against a minimal in-process SMTP server, and against Mailpit when
//! `DRE_TEST_SMTP=host:port` and `DRE_TEST_MAILPIT_API=http://host:port` are set.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

use dre_protocol::host::{LogSink, PluginProcess};
use dre_protocol::msg::DeliveryFile;
use dre_protocol::{CAP_MULTI_FILE, conformance};
use mail_parser::{MessageParser, MimeHeaders};
use serde_json::{Map, Value, json};

fn bin() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_dre-destination-email"))
}

/// One message the fake server received.
struct Captured {
    rcpt: Vec<String>,
    data: Vec<u8>,
}

/// A plain-SMTP server accepting every message; each is sent down the channel.
fn fake_smtp() -> (u16, Receiver<Captured>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut w) = stream else { return };
            let mut r = BufReader::new(w.try_clone().unwrap());
            let _ = w.write_all(b"220 fake ESMTP\r\n");
            let mut rcpt = Vec::new();
            let mut line = String::new();
            loop {
                line.clear();
                if r.read_line(&mut line).unwrap_or(0) == 0 {
                    break;
                }
                let cmd = line.trim_end().to_ascii_uppercase();
                let reply: &[u8] = if cmd.starts_with("EHLO") {
                    b"250-fake\r\n250 8BITMIME\r\n"
                } else if cmd.starts_with("RCPT TO:") {
                    rcpt.push(line.trim_end()[8..].trim_matches(['<', '>']).to_string());
                    b"250 ok\r\n"
                } else if cmd == "DATA" {
                    let _ = w.write_all(b"354 go\r\n");
                    let mut data = Vec::new();
                    loop {
                        let mut l = Vec::new();
                        if r.read_until(b'\n', &mut l).unwrap_or(0) == 0 || l == b".\r\n" {
                            break;
                        }
                        let l = if l.starts_with(b"..") { l[1..].to_vec() } else { l };
                        data.extend(l);
                    }
                    let _ = tx.send(Captured {
                        rcpt: std::mem::take(&mut rcpt),
                        data,
                    });
                    b"250 queued\r\n"
                } else if cmd == "QUIT" {
                    let _ = w.write_all(b"221 bye\r\n");
                    break;
                } else {
                    b"250 ok\r\n"
                };
                let _ = w.write_all(reply);
            }
        }
    });
    (port, rx)
}

fn quiet() -> LogSink {
    Arc::new(|_, _| {})
}

fn obj(v: Value) -> Map<String, Value> {
    v.as_object().unwrap().clone()
}

struct Files(tempfile::TempDir);

impl Files {
    fn new() -> Files {
        Files(tempfile::tempdir().unwrap())
    }
    fn file(&self, name: &str, bytes: &[u8]) -> DeliveryFile {
        let p: PathBuf = self.0.path().join(name);
        std::fs::write(&p, bytes).unwrap();
        DeliveryFile {
            local_path: p.to_string_lossy().to_string(),
            remote_path: None,
        }
    }
}

fn send(files: &[DeliveryFile], conn: Value, options: Value) -> Result<String, String> {
    let mut p = PluginProcess::start(bin(), quiet()).unwrap();
    let r = p
        .deliver_files(files, obj(conn), obj(options))
        .map_err(|e| e.to_string());
    let _ = p.close();
    r
}

fn local(port: u16) -> Value {
    json!({"host": "127.0.0.1", "port": port, "tls": "none", "from": "DRE Reports <reports@example.com>"})
}

fn received(rx: &Receiver<Captured>) -> Captured {
    rx.recv_timeout(Duration::from_secs(10))
        .expect("no message arrived")
}

#[test]
fn conforms_to_the_protocol() {
    conformance::assert_conforms(bin());
}

#[test]
fn advertises_multi_file() {
    let p = PluginProcess::start(bin(), quiet()).unwrap();
    assert!(p.has(CAP_MULTI_FILE));
}

#[test]
fn sends_one_email_with_the_file_attached() {
    let (port, rx) = fake_smtp();
    let f = Files::new();
    let loc = send(
        &[f.file("daily.csv", b"n\r\n1\r\n")],
        local(port),
        json!({"to": "finance@example.com", "cc": ["a@example.com", "b@example.com"],
               "subject": "Daily report 2026-01-25", "body": "Here it is."}),
    )
    .unwrap();
    assert!(loc.starts_with("email <"), "{loc}");
    assert!(loc.ends_with("to 3 recipients"), "{loc}");

    let m = received(&rx);
    assert_eq!(m.rcpt, ["finance@example.com", "a@example.com", "b@example.com"]);
    let msg = MessageParser::default().parse(&m.data).unwrap();
    assert_eq!(msg.subject(), Some("Daily report 2026-01-25"));
    assert_eq!(
        msg.from().unwrap().first().unwrap().address(),
        Some("reports@example.com")
    );
    assert_eq!(
        msg.to().unwrap().first().unwrap().address(),
        Some("finance@example.com")
    );
    assert_eq!(msg.body_text(0).unwrap().trim(), "Here it is.");
    let a = msg.attachment(0).unwrap();
    assert_eq!(a.attachment_name(), Some("daily.csv"));
    assert_eq!(a.contents(), b"n\r\n1\r\n");
    assert_eq!(msg.attachment_count(), 1);
}

#[test]
fn several_files_arrive_on_one_email() {
    let (port, rx) = fake_smtp();
    let f = Files::new();
    send(
        &[
            f.file("daily_Orders.csv", b"o"),
            f.file("daily_Refunds.csv", b"r"),
        ],
        local(port),
        json!({"to": "finance@example.com"}),
    )
    .unwrap();
    let m = received(&rx);
    let msg = MessageParser::default().parse(&m.data).unwrap();
    assert_eq!(msg.attachment_count(), 2);
    assert_eq!(
        msg.attachment(1).unwrap().attachment_name(),
        Some("daily_Refunds.csv")
    );
    assert_eq!(msg.attachment(1).unwrap().contents(), b"r");
    // No subject given: one is made from the file names.
    assert_eq!(msg.subject(), Some("Report: daily_Orders.csv, daily_Refunds.csv"));
    assert!(rx.try_recv().is_err(), "expected exactly one message");
}

#[test]
fn profile_recipients_are_the_default_and_options_replace_them() {
    let (port, rx) = fake_smtp();
    let f = Files::new();
    let mut conn = local(port);
    conn["to"] = json!("ops@example.com");
    conn["bcc"] = json!("archive@example.com");
    send(&[f.file("a.csv", b"1")], conn.clone(), json!({})).unwrap();
    assert_eq!(received(&rx).rcpt, ["ops@example.com", "archive@example.com"]);
    send(
        &[f.file("a.csv", b"1")],
        conn,
        json!({"to": "client_a@example.com, client_b@example.com"}),
    )
    .unwrap();
    assert_eq!(
        received(&rx).rcpt,
        [
            "client_a@example.com",
            "client_b@example.com",
            "archive@example.com"
        ]
    );
}

#[test]
fn attachment_name_renames_a_single_file() {
    let (port, rx) = fake_smtp();
    let f = Files::new();
    send(
        &[f.file("daily.csv", b"1")],
        local(port),
        json!({"to": "x@example.com", "attachment_name": "Daily Report.csv"}),
    )
    .unwrap();
    let m = received(&rx);
    let msg = MessageParser::default().parse(&m.data).unwrap();
    assert_eq!(
        msg.attachment(0).unwrap().attachment_name(),
        Some("Daily Report.csv")
    );

    let err = send(
        &[f.file("a.csv", b"1"), f.file("b.csv", b"2")],
        local(port),
        json!({"to": "x@example.com", "attachment_name": "x.csv"}),
    )
    .unwrap_err();
    assert!(err.contains("needs a single file"), "{err}");
}

/// A port with nothing listening: any attempt to connect would fail with a connection error.
fn dead_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

#[test]
fn no_recipients_fails_before_connecting() {
    let f = Files::new();
    let err = send(&[f.file("a.csv", b"1")], local(dead_port()), json!({})).unwrap_err();
    assert!(err.contains("no recipients"), "{err}");
}

#[test]
fn attachments_over_the_limit_fail_before_connecting() {
    let f = Files::new();
    let mut conn = local(dead_port());
    conn["max_attachment_mb"] = json!(1);
    let big = vec![b'x'; 1_200_000];
    let err = send(&[f.file("big.csv", &big)], conn, json!({"to": "x@example.com"})).unwrap_err();
    assert!(err.contains("over the 1 MB limit"), "{err}");
    assert!(err.contains("nothing was sent"), "{err}");
}

#[test]
fn unknown_options_and_bad_addresses_are_clear_errors() {
    let f = Files::new();
    let err = send(
        &[f.file("a.csv", b"1")],
        local(dead_port()),
        json!({"to": "x@example.com", "subjet": "typo"}),
    )
    .unwrap_err();
    assert!(err.contains("unknown email option `subjet`"), "{err}");
    let err = send(
        &[f.file("a.csv", b"1")],
        local(dead_port()),
        json!({"to": "not an address"}),
    )
    .unwrap_err();
    assert!(err.contains("isn't a valid email address"), "{err}");
}

#[test]
fn a_connection_failure_names_the_server_but_not_the_password() {
    let f = Files::new();
    let port = dead_port();
    let mut conn = local(port);
    conn["username"] = json!("reports");
    conn["password"] = json!("s3cret-pass");
    let err = send(&[f.file("a.csv", b"1")], conn, json!({"to": "x@example.com"})).unwrap_err();
    assert!(err.contains(&format!("127.0.0.1:{port}")), "{err}");
    assert!(!err.contains("s3cret-pass"), "{err}");
}

#[test]
fn delivers_to_mailpit() {
    let (Ok(smtp), Ok(api)) = (
        std::env::var("DRE_TEST_SMTP"),
        std::env::var("DRE_TEST_MAILPIT_API"),
    ) else {
        eprintln!("skipped: set DRE_TEST_SMTP=host:port and DRE_TEST_MAILPIT_API=http://host:port");
        return;
    };
    let (host, port) = smtp.split_once(':').unwrap();
    let subject = format!("dre-mailpit-{}", std::process::id());
    let f = Files::new();
    send(
        &[
            f.file("monthly.csv", b"a,b\r\n1,2\r\n"),
            f.file("notes.txt", b"hello"),
        ],
        json!({"host": host, "port": port, "tls": "none", "username": "dre", "password": "dre-pass",
               "from": "reports@example.com"}),
        json!({"to": "finance@example.com", "cc": "audit@example.com", "subject": subject,
               "body": "Monthly figures attached."}),
    )
    .unwrap();
    let get = |path: &str| -> Vec<u8> {
        ureq::get(&format!("{api}{path}"))
            .call()
            .unwrap()
            .body_mut()
            .read_to_vec()
            .unwrap()
    };
    let json = |path: &str| -> Value { serde_json::from_slice(&get(path)).unwrap() };
    let query: String = format!("subject:\"{subject}\"")
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect();
    let search = json(&format!("/api/v1/search?query={query}"));
    let id = search["messages"][0]["ID"]
        .as_str()
        .expect("message not found in Mailpit")
        .to_string();
    let m = json(&format!("/api/v1/message/{id}"));
    assert_eq!(m["Subject"], subject.as_str());
    assert_eq!(m["To"][0]["Address"], "finance@example.com");
    assert_eq!(m["Cc"][0]["Address"], "audit@example.com");
    assert_eq!(m["Text"].as_str().unwrap().trim(), "Monthly figures attached.");
    let atts = m["Attachments"].as_array().unwrap();
    assert_eq!(atts.len(), 2);
    let part = |name: &str| {
        let a = atts.iter().find(|a| a["FileName"] == name).unwrap();
        get(&format!(
            "/api/v1/message/{id}/part/{}",
            a["PartID"].as_str().unwrap()
        ))
    };
    assert_eq!(part("monthly.csv"), b"a,b\r\n1,2\r\n");
    assert_eq!(part("notes.txt"), b"hello");
}
