//! Slack delivery against a local fake of the Web API endpoints the plugin uses (there's no
//! official emulator). Against a real workspace when `DRE_TEST_SLACK_TOKEN` and
//! `DRE_TEST_SLACK_CHANNEL` are set (and a DM with `DRE_TEST_SLACK_USER`): a manual smoke test,
//! never run in CI.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::sync::{Arc, Mutex};

use dre_protocol::host::{LogSink, PluginProcess};
use dre_protocol::msg::DeliveryFile;
use dre_protocol::{CAP_MULTI_FILE, conformance};
use serde_json::{Map, Value, json};

fn bin() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_dre-plugin-slack"))
}

/// One request the fake received: path, form fields (or raw body length for uploads), token.
#[derive(Debug, Clone)]
struct Call {
    path: String,
    form: BTreeMap<String, String>,
    body_len: usize,
    token: String,
}

type Calls = Arc<Mutex<Vec<Call>>>;

/// Tokens change the fake's behaviour: `bad` → invalid_auth everywhere, `noscope` → missing_scope
/// on conversations.list, `slow` → the first upload-URL call is rate limited, `busy` → every
/// upload-URL call is, `nochat` → missing_scope on chat.postMessage. Channel `C_OUT` →
/// not_in_channel on completion; user `U_OFF` has the app's Messages tab off.
fn fake_slack() -> (String, Calls) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let calls: Calls = Arc::default();
    let (c, b) = (calls.clone(), base.clone());
    std::thread::spawn(move || {
        let mut next_file = 0;
        let mut slow_once = true;
        for stream in listener.incoming().flatten() {
            let mut r = BufReader::new(stream.try_clone().unwrap());
            let mut s = stream;
            let mut line = String::new();
            if r.read_line(&mut line).unwrap_or(0) == 0 {
                continue;
            }
            let path = line.split_whitespace().nth(1).unwrap().to_string();
            let (mut len, mut token) = (0usize, String::new());
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
                    "authorization" => token = v.trim().trim_start_matches("Bearer ").to_string(),
                    _ => {}
                }
            }
            let mut body = vec![0; len];
            r.read_exact(&mut body).unwrap();
            let form: BTreeMap<String, String> = if path.starts_with("/api/") {
                form_urlencoded::parse(&body).into_owned().collect()
            } else {
                BTreeMap::new()
            };
            c.lock().unwrap().push(Call {
                path: path.clone(),
                form: form.clone(),
                body_len: len,
                token: token.clone(),
            });
            let method = path.trim_start_matches("/api/");
            let (status, extra, reply) = if path.starts_with("/upload/") {
                ("200 OK", "", "OK".to_string())
            } else if token == "bad" {
                (
                    "200 OK",
                    "",
                    json!({"ok": false, "error": "invalid_auth"}).to_string(),
                )
            } else if method == "files.getUploadURLExternal"
                && (token == "busy" || (token == "slow" && std::mem::take(&mut slow_once)))
            {
                ("429 Too Many Requests", "Retry-After: 1\r\n", String::new())
            } else {
                let v = match method {
                    "files.getUploadURLExternal" => {
                        next_file += 1;
                        json!({"ok": true, "upload_url": format!("{b}/upload/F{next_file}"), "file_id": format!("F{next_file}")})
                    }
                    "files.completeUploadExternal" if form["channel_id"] == "C_OUT" => {
                        json!({"ok": false, "error": "not_in_channel"})
                    }
                    "files.completeUploadExternal" => {
                        let files: Vec<Value> = serde_json::from_str(&form["files"]).unwrap();
                        let out: Vec<Value> = files
                            .iter()
                            .map(|f| json!({"id": f["id"], "title": f["title"], "permalink": format!("https://acme.slack.com/files/{}", f["id"].as_str().unwrap())}))
                            .collect();
                        json!({"ok": true, "files": out})
                    }
                    "conversations.list" if token == "noscope" => {
                        json!({"ok": false, "error": "missing_scope", "needed": "channels:read"})
                    }
                    "conversations.list" if !form.contains_key("cursor") => json!({
                        "ok": true, "channels": [{"id": "C1", "name": "general"}],
                        "response_metadata": {"next_cursor": "page2"}}),
                    "conversations.list" => json!({
                        "ok": true, "channels": [{"id": "C2", "name": "finance-reports"}],
                        "response_metadata": {"next_cursor": ""}}),
                    "chat.postMessage" if token == "nochat" => {
                        json!({"ok": false, "error": "missing_scope", "needed": "chat:write"})
                    }
                    "chat.postMessage" if form["channel"] == "D_U_OFF" => {
                        json!({"ok": false, "error": "messages_tab_disabled"})
                    }
                    // The plugin only ever probes a DM with no text.
                    "chat.postMessage" if !form.contains_key("text") => {
                        json!({"ok": false, "error": "no_text"})
                    }
                    "conversations.open" => {
                        json!({"ok": true, "channel": {"id": format!("D_{}", form["users"])}})
                    }
                    _ => json!({"ok": false, "error": "unknown_method"}),
                };
                ("200 OK", "", v.to_string())
            };
            let _ = write!(
                s,
                "HTTP/1.1 {status}\r\n{extra}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                reply.len()
            );
        }
    });
    (base, calls)
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
        let p = self.0.path().join(name);
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

fn conn(base: &str, token: &str) -> Value {
    json!({"token": token, "api_url": format!("{base}/api")})
}

fn calls_to(calls: &Calls, method: &str) -> Vec<Call> {
    calls
        .lock()
        .unwrap()
        .iter()
        .filter(|c| c.path == format!("/api/{method}"))
        .cloned()
        .collect()
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
fn uploads_to_a_channel_id_with_the_message() {
    let (base, calls) = fake_slack();
    let f = Files::new();
    let loc = send(
        &[f.file("daily.csv", b"n\r\n1\r\n")],
        conn(&base, "xoxb-good"),
        json!({"channel": "C9", "message": "Daily report for client_a"}),
    )
    .unwrap();
    assert_eq!(loc, "slack C9: https://acme.slack.com/files/F1");

    let get = calls_to(&calls, "files.getUploadURLExternal");
    assert_eq!(get.len(), 1);
    assert_eq!(get[0].form["filename"], "daily.csv");
    assert_eq!(get[0].form["length"], "6");
    assert_eq!(get[0].token, "xoxb-good");
    let upload: Vec<Call> = calls
        .lock()
        .unwrap()
        .iter()
        .filter(|c| c.path == "/upload/F1")
        .cloned()
        .collect();
    assert_eq!(upload[0].body_len, 6);
    let done = calls_to(&calls, "files.completeUploadExternal");
    assert_eq!(done.len(), 1);
    assert_eq!(done[0].form["channel_id"], "C9");
    assert_eq!(done[0].form["initial_comment"], "Daily report for client_a");
    let files: Value = serde_json::from_str(&done[0].form["files"]).unwrap();
    assert_eq!(files, json!([{"id": "F1", "title": "daily.csv"}]));
}

#[test]
fn several_files_land_in_one_post() {
    let (base, calls) = fake_slack();
    let f = Files::new();
    let loc = send(
        &[
            f.file("daily_Orders.csv", b"o"),
            f.file("daily_Refunds.csv", b"r"),
        ],
        conn(&base, "xoxb-good"),
        json!({"channel": "C9"}),
    )
    .unwrap();
    assert!(
        loc.ends_with("files/F1, https://acme.slack.com/files/F2"),
        "{loc}"
    );
    assert_eq!(calls_to(&calls, "files.getUploadURLExternal").len(), 2);
    let done = calls_to(&calls, "files.completeUploadExternal");
    assert_eq!(done.len(), 1);
    let files: Vec<Value> = serde_json::from_str(&done[0].form["files"]).unwrap();
    assert_eq!(files.len(), 2);
    assert!(!done[0].form.contains_key("initial_comment"));
}

#[test]
fn a_channel_name_is_resolved_across_pages() {
    let (base, calls) = fake_slack();
    let f = Files::new();
    let loc = send(
        &[f.file("a.csv", b"1")],
        conn(&base, "xoxb-good"),
        json!({"channel": "#finance-reports"}),
    )
    .unwrap();
    assert!(loc.starts_with("slack #finance-reports (C2)"), "{loc}");
    assert_eq!(calls_to(&calls, "conversations.list").len(), 2);
    assert_eq!(
        calls_to(&calls, "files.completeUploadExternal")[0].form["channel_id"],
        "C2"
    );

    let err = send(
        &[f.file("a.csv", b"1")],
        conn(&base, "xoxb-good"),
        json!({"channel": "#nope"}),
    )
    .unwrap_err();
    assert!(err.contains("no Slack channel named #nope"), "{err}");
}

#[test]
fn a_user_gets_the_file_in_a_dm() {
    let (base, calls) = fake_slack();
    let f = Files::new();
    let loc = send(
        &[f.file("a.csv", b"1")],
        conn(&base, "xoxb-good"),
        json!({"user": "U42"}),
    )
    .unwrap();
    assert!(loc.starts_with("slack DM with U42"), "{loc}");
    assert_eq!(calls_to(&calls, "conversations.open")[0].form["users"], "U42");
    let probe = calls_to(&calls, "chat.postMessage");
    assert_eq!(probe.len(), 1);
    assert_eq!(probe[0].form["channel"], "D_U42");
    assert!(
        !probe[0].form.contains_key("text"),
        "the probe must not post anything"
    );
    assert_eq!(
        calls_to(&calls, "files.completeUploadExternal")[0].form["channel_id"],
        "D_U42"
    );
}

#[test]
fn a_dm_with_the_messages_tab_off_fails_before_uploading() {
    let (base, calls) = fake_slack();
    let f = Files::new();
    let err = send(
        &[f.file("a.csv", b"1")],
        conn(&base, "xoxb-good"),
        json!({"user": "U_OFF"}),
    )
    .unwrap_err();
    assert!(err.contains("turn on App Home > Messages Tab"), "{err}");
    assert!(calls_to(&calls, "files.getUploadURLExternal").is_empty());
}

#[test]
fn a_dm_needs_chat_write() {
    let (base, calls) = fake_slack();
    let f = Files::new();
    let err = send(
        &[f.file("a.csv", b"1")],
        conn(&base, "nochat"),
        json!({"user": "U42"}),
    )
    .unwrap_err();
    assert!(err.contains("lacks the `chat:write` scope"), "{err}");
    assert!(calls_to(&calls, "files.getUploadURLExternal").is_empty());
}

#[test]
fn a_channel_is_not_probed() {
    let (base, calls) = fake_slack();
    let f = Files::new();
    send(
        &[f.file("a.csv", b"1")],
        conn(&base, "xoxb-good"),
        json!({"channel": "C7"}),
    )
    .unwrap();
    assert!(calls_to(&calls, "chat.postMessage").is_empty());
}

#[test]
fn the_profile_channel_is_the_default() {
    let (base, calls) = fake_slack();
    let f = Files::new();
    let mut c = conn(&base, "xoxb-good");
    c["channel"] = json!("C7");
    send(&[f.file("a.csv", b"1")], c, json!({"message": "hi"})).unwrap();
    assert_eq!(
        calls_to(&calls, "files.completeUploadExternal")[0].form["channel_id"],
        "C7"
    );
}

#[test]
fn target_mistakes_fail_before_any_api_call() {
    let (base, calls) = fake_slack();
    let f = Files::new();
    let err = send(
        &[f.file("a.csv", b"1")],
        conn(&base, "xoxb-good"),
        json!({"channel": "C1", "user": "U1"}),
    )
    .unwrap_err();
    assert!(err.contains("either `channel` or `user`, not both"), "{err}");
    let err = send(&[f.file("a.csv", b"1")], conn(&base, "xoxb-good"), json!({})).unwrap_err();
    assert!(err.contains("needs `channel:`"), "{err}");
    let err = send(
        &[f.file("a.csv", b"1")],
        conn(&base, "xoxb-good"),
        json!({"chanel": "C1"}),
    )
    .unwrap_err();
    assert!(
        err.contains("unknown option `chanel` for destination `slack`"),
        "{err}"
    );
    assert!(calls.lock().unwrap().is_empty());
}

#[test]
fn slack_errors_become_clear_messages_without_the_token() {
    let (base, _calls) = fake_slack();
    let f = Files::new();
    let err = send(
        &[f.file("a.csv", b"1")],
        conn(&base, "bad"),
        json!({"channel": "C9"}),
    )
    .unwrap_err();
    assert!(
        err.contains("Slack rejected the bot token (invalid_auth)"),
        "{err}"
    );
    let err = send(
        &[f.file("a.csv", b"1")],
        conn(&base, "xoxb-secret-value"),
        json!({"channel": "C_OUT"}),
    )
    .unwrap_err();
    assert!(err.contains("the bot isn't a member of channel C_OUT"), "{err}");
    assert!(!err.contains("xoxb-secret-value"), "{err}");
    let err = send(
        &[f.file("a.csv", b"1")],
        conn(&base, "noscope"),
        json!({"channel": "#general"}),
    )
    .unwrap_err();
    assert!(err.contains("lacks the `channels:read` scope"), "{err}");
}

#[test]
fn a_rate_limited_call_is_retried_once() {
    let (base, calls) = fake_slack();
    let f = Files::new();
    send(
        &[f.file("a.csv", b"1")],
        conn(&base, "slow"),
        json!({"channel": "C9"}),
    )
    .unwrap();
    assert_eq!(calls_to(&calls, "files.getUploadURLExternal").len(), 2);

    let err = send(
        &[f.file("a.csv", b"1")],
        conn(&base, "busy"),
        json!({"channel": "C9"}),
    )
    .unwrap_err();
    assert!(err.contains("rate-limiting files.getUploadURLExternal"), "{err}");
}

#[test]
fn delivers_to_a_real_workspace() {
    let (Ok(token), Ok(channel)) = (
        std::env::var("DRE_TEST_SLACK_TOKEN"),
        std::env::var("DRE_TEST_SLACK_CHANNEL"),
    ) else {
        eprintln!("skipped: set DRE_TEST_SLACK_TOKEN and DRE_TEST_SLACK_CHANNEL for a manual smoke test");
        return;
    };
    let f = Files::new();
    let loc = send(
        &[f.file("dre-smoke.csv", b"n\r\n1\r\n")],
        json!({"token": token}),
        json!({"channel": channel, "message": "DRE Slack destination smoke test"}),
    )
    .unwrap();
    assert!(loc.starts_with("slack "), "{loc}");
    if let Ok(user) = std::env::var("DRE_TEST_SLACK_USER") {
        let loc = send(
            &[f.file("dre-smoke-dm.csv", b"n\r\n1\r\n")],
            json!({"token": std::env::var("DRE_TEST_SLACK_TOKEN").unwrap()}),
            json!({"user": user, "message": "DRE Slack DM smoke test"}),
        )
        .unwrap();
        assert!(loc.starts_with("slack DM with "), "{loc}");
    }
}
