//! `dre-destination-slack`: uploads an output's files to a Slack channel, or to a person's DM,
//! as one post with a message.
//!
//! Profile output (`profiles.yml`): `token` (a bot token, `xoxb-...`), an optional default
//! `channel`, and `api_url` (default `https://slack.com/api`, for a proxy or a test fake). Destination options: exactly one of `channel` (an ID such as `C0123`, or `#name`)
//! or `user` (a user ID such as `U0123`), and `message`.
//!
//! Uses Slack's external upload flow: `files.getUploadURLExternal` per file, the bytes to the
//! returned URL, then one `files.completeUploadExternal` that shares every file in one post.

use std::time::Duration;

use dre_protocol::CAP_MULTI_FILE;
use dre_protocol::msg::ConnectionField;
use dre_protocol::plugin::{
    About, Delivery, Destination, Result, conn_required, conn_str, serve_destination,
};
use serde_json::{Map, Value, json};

const OPTIONS: &[&str] = &["channel", "user", "message"];
const DEFAULT_API: &str = "https://slack.com/api";
/// Longest `Retry-After` honoured before giving up on a rate-limited call.
const MAX_RETRY_WAIT: u64 = 60;

struct Slack;

impl Destination for Slack {
    fn connection_fields(&self) -> Vec<ConnectionField> {
        vec![
            ConnectionField::new("token", "bot token (xoxb-...)")
                .required()
                .secret(),
            ConnectionField::new("channel", "default channel ID (C0123) or #name"),
        ]
    }

    fn deliver_files(&mut self, d: &Delivery) -> Result<String> {
        if let Some(k) = d.options.keys().find(|k| !OPTIONS.contains(&k.as_str())) {
            return Err(format!(
                "unknown slack option `{k}`; expected one of {}",
                OPTIONS.join(", ")
            )
            .into());
        }
        let target = Target::from(&d.options, &d.connection)?;
        let message = match d.options.get("message") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) => Some(s.clone()),
            Some(v) => return Err(format!("slack option `message` must be a string, got {v}").into()),
        };
        let api = Api::new(&d.connection)?;
        let channel_id = match &target {
            Target::Channel(c) => api.resolve_channel(c)?,
            Target::User(u) => api.open_dm(u)?,
        };

        let mut uploaded = Vec::new();
        for f in &d.files {
            let bytes =
                std::fs::read(&f.local).map_err(|e| format!("can't read {}: {e}", f.local.display()))?;
            let name = f
                .local
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            let r = api.call(
                "files.getUploadURLExternal",
                &[("filename", name.clone()), ("length", bytes.len().to_string())],
                &target,
            )?;
            let (Some(url), Some(id)) = (r["upload_url"].as_str(), r["file_id"].as_str()) else {
                return Err("Slack's upload reply had no `upload_url`/`file_id`".into());
            };
            api.upload(url, bytes)
                .map_err(|e| format!("uploading {name} to Slack failed: {e}"))?;
            uploaded.push(json!({"id": id, "title": name}));
        }
        let mut form = vec![
            ("files", Value::Array(uploaded).to_string()),
            ("channel_id", channel_id.clone()),
        ];
        if let Some(m) = message {
            form.push(("initial_comment", m));
        }
        let r = api.call("files.completeUploadExternal", &form, &target)?;
        let links: Vec<&str> = r["files"]
            .as_array()
            .map(|a| a.iter().filter_map(|f| f["permalink"].as_str()).collect())
            .unwrap_or_default();
        let place = match &target {
            Target::Channel(c) if c.starts_with('#') => format!("{c} ({channel_id})"),
            Target::Channel(_) => channel_id,
            Target::User(u) => format!("DM with {u}"),
        };
        Ok(if links.is_empty() {
            format!("slack {place}")
        } else {
            format!("slack {place}: {}", links.join(", "))
        })
    }
}

enum Target {
    Channel(String),
    User(String),
}

impl Target {
    fn from(o: &Map<String, Value>, c: &Map<String, Value>) -> Result<Target> {
        let s = |m: &Map<String, Value>, k: &str| -> Result<Option<String>> {
            match m.get(k) {
                None | Some(Value::Null) => Ok(None),
                Some(Value::String(s)) if s.trim().is_empty() => Ok(None),
                Some(Value::String(s)) => Ok(Some(s.trim().to_string())),
                Some(v) => Err(format!("slack option `{k}` must be a string, got {v}").into()),
            }
        };
        match (s(o, "channel")?, s(o, "user")?) {
            (Some(_), Some(_)) => {
                Err("give the slack destination either `channel` or `user`, not both".into())
            }
            (Some(ch), None) => Ok(Target::Channel(ch)),
            (None, Some(u)) => Ok(Target::User(u)),
            (None, None) => match conn_str(c, "channel") {
                Some(ch) => Ok(Target::Channel(ch.trim().to_string())),
                None => Err(
                    "the slack destination needs `channel:` (or `user:`), or a default `channel` in the profile"
                        .into(),
                ),
            },
        }
    }

    fn describe(&self) -> String {
        match self {
            Target::Channel(c) => format!("channel {c}"),
            Target::User(u) => format!("user {u}"),
        }
    }
}

struct Api {
    base: String,
    token: String,
    agent: ureq::Agent,
}

impl Api {
    fn new(c: &Map<String, Value>) -> Result<Api> {
        let token = conn_required(c, "token")?.to_string();
        // `api_url` points the plugin at a proxy or, in tests, a fake Slack.
        let base = conn_str(c, "api_url")
            .unwrap_or(DEFAULT_API)
            .trim_end_matches('/')
            .to_string();
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(300)))
            .build()
            .into();
        Ok(Api { base, token, agent })
    }

    /// Call a Web API method with form parameters. A rate-limited call is retried once, after
    /// Slack's `Retry-After`.
    fn call(&self, method: &str, form: &[(&str, String)], target: &Target) -> Result<Value> {
        let url = format!("{}/{method}", self.base);
        for attempt in 0..2 {
            let mut resp = self
                .agent
                .post(&url)
                .header("Authorization", &format!("Bearer {}", self.token))
                .send_form(form.iter().map(|(k, v)| (*k, v.as_str())))
                .map_err(|e| format!("can't reach Slack ({method}): {e}"))?;
            let status = resp.status().as_u16();
            if status == 429 {
                let wait = resp
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.trim().parse::<u64>().ok())
                    .unwrap_or(1);
                if attempt == 0 && wait <= MAX_RETRY_WAIT {
                    eprintln!("Slack rate-limited {method}; retrying in {wait}s");
                    std::thread::sleep(Duration::from_secs(wait));
                    continue;
                }
                return Err(format!(
                    "Slack is rate-limiting {method} (retry after {wait}s); try again later"
                )
                .into());
            }
            let body = resp.body_mut().read_to_string().unwrap_or_default();
            if !(200..300).contains(&status) {
                return Err(format!("Slack {method} returned HTTP {status}").into());
            }
            let v: Value = serde_json::from_str(&body)
                .map_err(|e| format!("Slack {method} returned something that isn't JSON: {e}"))?;
            if v["ok"] == Value::Bool(true) {
                return Ok(v);
            }
            return Err(explain(method, &v, target).into());
        }
        unreachable!("the loop returns on its second attempt")
    }

    fn upload(&self, url: &str, bytes: Vec<u8>) -> std::result::Result<(), String> {
        let resp = self
            .agent
            .post(url)
            .header("Content-Type", "application/octet-stream")
            .send(&bytes[..])
            .map_err(|e| e.to_string())?;
        match resp.status().as_u16() {
            200..=299 => Ok(()),
            s => Err(format!("HTTP {s}")),
        }
    }

    /// A channel ID as given, or `#name` looked up among the channels the bot can see.
    fn resolve_channel(&self, channel: &str) -> Result<String> {
        let Some(name) = channel.strip_prefix('#') else {
            return Ok(channel.to_string());
        };
        let target = Target::Channel(channel.to_string());
        let mut cursor = String::new();
        loop {
            let mut form = vec![
                ("types", "public_channel,private_channel".to_string()),
                ("exclude_archived", "true".to_string()),
                ("limit", "1000".to_string()),
            ];
            if !cursor.is_empty() {
                form.push(("cursor", cursor.clone()));
            }
            let r = self.call("conversations.list", &form, &target)?;
            if let Some(id) = r["channels"].as_array().and_then(|cs| {
                cs.iter()
                    .find(|c| c["name"].as_str() == Some(name))
                    .and_then(|c| c["id"].as_str())
            }) {
                return Ok(id.to_string());
            }
            cursor = r["response_metadata"]["next_cursor"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            if cursor.is_empty() {
                return Err(format!(
                    "no Slack channel named {channel} is visible to the bot; check the name, invite the bot to a private channel, or use the channel ID"
                )
                .into());
            }
        }
    }

    /// The DM channel with a user.
    fn open_dm(&self, user: &str) -> Result<String> {
        let target = Target::User(user.to_string());
        let r = self.call("conversations.open", &[("users", user.to_string())], &target)?;
        r["channel"]["id"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| "Slack's conversations.open reply had no channel ID".into())
    }
}

/// Turn a Slack `ok: false` reply into an actionable message. Never includes the token.
fn explain(method: &str, v: &Value, target: &Target) -> String {
    let error = v["error"].as_str().unwrap_or("unknown_error");
    let to = target.describe();
    match error {
        "invalid_auth" | "not_authed" | "token_revoked" | "token_expired" | "account_inactive" => {
            format!("Slack rejected the bot token ({error}); check `token` in the profile")
        }
        "missing_scope" => format!(
            "the bot token lacks the `{}` scope that {method} needs; add it in the Slack app's OAuth settings and reinstall the app",
            v["needed"].as_str().unwrap_or("?")
        ),
        "not_in_channel" => {
            format!("the bot isn't a member of {to}; invite it there (/invite @your-bot) first")
        }
        "channel_not_found" => format!("Slack can't find {to}, or the bot can't see it"),
        "user_not_found" | "users_not_found" => format!("Slack can't find {to}"),
        "is_archived" => format!("{to} is archived"),
        _ => format!("Slack {method} failed for {to}: {error}"),
    }
}

fn main() {
    serve_destination(
        About::new("slack", env!("CARGO_PKG_VERSION")).capabilities(&[CAP_MULTI_FILE]),
        Slack,
    )
}
