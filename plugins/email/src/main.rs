//! The `email` destination: sends an output's files as attachments on one email, over SMTP.
//!
//! Profile target fields (`profiles.yml`): `host`, `port`, `tls` (`starttls`, `implicit` or `none`),
//! `username`/`password`, `from`, optional default `to`/`cc`/`bcc`, `max_attachment_mb` and
//! `tls_accept_invalid_certs`. Destination options (the report's `output.destination` entry):
//! `to`, `cc`, `bcc`, `subject`, `body`, `attachment_name`. An option replaces the profile's
//! default of the same name.

use std::path::Path;
use std::str::FromStr;
use std::time::Duration;

use dre_protocol::CAP_MULTI_FILE;
use dre_protocol::msg::ConnectionField;
use dre_protocol::options::{OptionField, OptionType, is_template};
use dre_protocol::plugin::{
    About, Delivery, Destination, Result, conn_bool, conn_required, conn_str, serve_destination,
};
use lettre::message::header::ContentType;
use lettre::message::{Attachment, Mailbox, MultiPart, SinglePart};
use lettre::transport::smtp::authentication::Credentials;
use lettre::transport::smtp::client::{Tls, TlsParameters};
use lettre::{Message, SmtpTransport, Transport};
use serde_json::{Map, Value};

const DEFAULT_MAX_MB: f64 = 20.0;

struct Email;

impl Destination for Email {
    fn connection_fields(&self) -> Vec<ConnectionField> {
        vec![
            ConnectionField::new("host", "SMTP server host").required(),
            ConnectionField::new("port", "SMTP port (587 for starttls, 465 for implicit TLS)"),
            ConnectionField::new("tls", "starttls, implicit or none").default("starttls"),
            ConnectionField::new("username", "SMTP username"),
            ConnectionField::new("password", "SMTP password").secret(),
            ConnectionField::new("from", "sender address, e.g. \"Reports <reports@example.com>\"").required(),
            ConnectionField::new("to", "default recipients when a report names none"),
            ConnectionField::new("cc", "default cc recipients"),
            ConnectionField::new("bcc", "default bcc recipients"),
            ConnectionField::new("max_attachment_mb", "largest total attachment size to send").default(20),
            ConnectionField::new(
                "tls_accept_invalid_certs",
                "accept a self-signed server certificate",
            )
            .default(false),
        ]
    }

    fn options(&self) -> Vec<OptionField> {
        use OptionType::*;
        vec![
            OptionField::new(
                "to",
                Strings,
                "recipients: an address, a comma-separated string or a list",
            ),
            OptionField::new("cc", Strings, "cc recipients"),
            OptionField::new("bcc", Strings, "bcc recipients"),
            OptionField::new("subject", String, "the subject line"),
            OptionField::new("body", String, "the message text"),
            OptionField::new(
                "attachment_name",
                String,
                "the attachment's file name (single-file outputs)",
            ),
        ]
    }

    /// Recipient addresses are checked unless they hold Jinja, which core renders later.
    fn validate(&self, o: &Map<String, Value>) -> Vec<std::string::String> {
        ["to", "cc", "bcc"]
            .into_iter()
            .filter_map(|k| {
                let v = o.get(k)?;
                let templated = match v {
                    Value::String(s) => is_template(s),
                    Value::Array(a) => a.iter().any(|x| x.as_str().is_some_and(is_template)),
                    _ => false,
                };
                if templated {
                    return None;
                }
                addresses(v, k).err().map(|e| e.to_string())
            })
            .collect()
    }

    fn deliver_files(&mut self, d: &Delivery) -> Result<String> {
        let plan = Plan::new(d)?;
        let message = plan.message()?;
        let message_id = message
            .headers()
            .get_raw("Message-ID")
            .unwrap_or_default()
            .to_string();
        let smtp = Smtp::from_connection(&d.connection)?;
        smtp.send(&message)?;
        let n = plan.recipients();
        Ok(format!(
            "email {message_id} to {n} recipient{}",
            if n == 1 { "" } else { "s" }
        ))
    }
}

/// What to send, checked before anything connects.
struct Plan {
    from: Mailbox,
    to: Vec<Mailbox>,
    cc: Vec<Mailbox>,
    bcc: Vec<Mailbox>,
    subject: String,
    body: String,
    attachments: Vec<(String, Vec<u8>)>,
}

impl Plan {
    fn new(d: &Delivery) -> Result<Plan> {
        let c = &d.connection;
        let o = &d.options;
        let from = parse_mailbox(conn_required(c, "from")?, "from")?;
        let field = |k: &str| -> Result<Vec<Mailbox>> {
            match o.get(k).or_else(|| c.get(k)) {
                None | Some(Value::Null) => Ok(Vec::new()),
                Some(v) => addresses(v, k),
            }
        };
        let (to, cc, bcc) = (field("to")?, field("cc")?, field("bcc")?);
        if to.is_empty() && cc.is_empty() && bcc.is_empty() {
            return Err(
                "no recipients: give the destination `to:` (or `cc:`/`bcc:`), or a default `to` in the profile"
                    .into(),
            );
        }

        let rename = option_str(o, "attachment_name")?;
        if rename.is_some() && d.files.len() > 1 {
            return Err(format!(
                "`attachment_name` needs a single file, but this output has {}",
                d.files.len()
            )
            .into());
        }
        let limit = match c.get("max_attachment_mb") {
            None | Some(Value::Null) => DEFAULT_MAX_MB,
            Some(Value::Number(n)) => n.as_f64().unwrap_or(DEFAULT_MAX_MB),
            Some(Value::String(s)) => s
                .parse()
                .map_err(|_| format!("`max_attachment_mb` must be a number, got `{s}`"))?,
            Some(v) => return Err(format!("`max_attachment_mb` must be a number, got {v}").into()),
        };
        let mut total = 0u64;
        let mut names = Vec::new();
        for f in &d.files {
            total += std::fs::metadata(&f.local)
                .map_err(|e| format!("can't read {}: {e}", f.local.display()))?
                .len();
            let name = rename.clone().unwrap_or_else(|| file_name(&f.local));
            names.push(name);
        }
        let mb = total as f64 / (1024.0 * 1024.0);
        if mb > limit {
            return Err(format!(
                "attachments total {mb:.1} MB, over the {limit} MB limit (`max_attachment_mb`); nothing was sent — deliver the file somewhere else and send a link instead, or raise the limit if your mail server allows it"
            )
            .into());
        }
        let mut attachments = Vec::new();
        for (f, name) in d.files.iter().zip(&names) {
            let bytes =
                std::fs::read(&f.local).map_err(|e| format!("can't read {}: {e}", f.local.display()))?;
            attachments.push((name.clone(), bytes));
        }

        let subject = option_str(o, "subject")?.unwrap_or_else(|| format!("Report: {}", names.join(", ")));
        let body = option_str(o, "body")?.unwrap_or_else(|| format!("Attached: {}\n", names.join(", ")));
        Ok(Plan {
            from,
            to,
            cc,
            bcc,
            subject,
            body,
            attachments,
        })
    }

    fn recipients(&self) -> usize {
        self.to.len() + self.cc.len() + self.bcc.len()
    }

    fn message(&self) -> Result<Message> {
        let mut b = Message::builder()
            .from(self.from.clone())
            .subject(&self.subject)
            .message_id(None);
        for m in &self.to {
            b = b.to(m.clone());
        }
        for m in &self.cc {
            b = b.cc(m.clone());
        }
        for m in &self.bcc {
            b = b.bcc(m.clone());
        }
        let mut parts = MultiPart::mixed().singlepart(SinglePart::plain(self.body.clone()));
        for (name, bytes) in &self.attachments {
            let ct = ContentType::parse(content_type(name)).expect("valid content type");
            parts = parts.singlepart(Attachment::new(name.clone()).body(bytes.clone(), ct));
        }
        Ok(b.multipart(parts)
            .map_err(|e| format!("can't build the email: {e}"))?)
    }
}

#[derive(Clone, Copy)]
enum TlsMode {
    StartTls,
    Implicit,
    None,
}

/// How to reach the SMTP server.
struct Smtp {
    host: String,
    port: u16,
    tls: TlsMode,
    credentials: Option<Credentials>,
    accept_invalid_certs: bool,
}

impl Smtp {
    fn from_connection(c: &Map<String, Value>) -> Result<Smtp> {
        let host = conn_required(c, "host")?.to_string();
        let (tls, default_port) = match conn_str(c, "tls").unwrap_or("starttls") {
            "starttls" => (TlsMode::StartTls, 587),
            "implicit" => (TlsMode::Implicit, 465),
            "none" => (TlsMode::None, 25),
            other => {
                return Err(format!("`tls` must be starttls, implicit or none, got `{other}`").into());
            }
        };
        let port = match c.get("port") {
            None | Some(Value::Null) => default_port,
            Some(Value::Number(n)) => n
                .as_u64()
                .and_then(|n| u16::try_from(n).ok())
                .ok_or("`port` must be a port number")?,
            Some(Value::String(s)) => s
                .parse()
                .map_err(|_| format!("`port` must be a number, got `{s}`"))?,
            Some(_) => return Err("`port` must be a number".into()),
        };
        let credentials = match (conn_str(c, "username"), conn_str(c, "password")) {
            (Some(u), Some(p)) => Some(Credentials::new(u.to_string(), p.to_string())),
            (Some(_), None) => return Err("`username` is set but `password` isn't".into()),
            (None, Some(_)) => return Err("`password` is set but `username` isn't".into()),
            (None, None) => None,
        };
        Ok(Smtp {
            host,
            port,
            tls,
            credentials,
            accept_invalid_certs: conn_bool(c, "tls_accept_invalid_certs") == Some(true),
        })
    }

    fn send(&self, message: &Message) -> Result<()> {
        let at = format!("{}:{}", self.host, self.port);
        let params = || {
            TlsParameters::builder(self.host.clone())
                .dangerous_accept_invalid_certs(self.accept_invalid_certs)
                .build()
                .map_err(|e| format!("can't set up TLS for {at}: {e}"))
        };
        let tls = match self.tls {
            TlsMode::StartTls => Tls::Required(params()?),
            TlsMode::Implicit => Tls::Wrapper(params()?),
            TlsMode::None => Tls::None,
        };
        let mut b = SmtpTransport::builder_dangerous(&self.host)
            .port(self.port)
            .tls(tls)
            .timeout(Some(Duration::from_secs(60)));
        if let Some(c) = &self.credentials {
            b = b.credentials(c.clone());
        }
        b.build()
            .send(message)
            .map_err(|e| format!("sending through SMTP server {at} failed: {e}"))?;
        Ok(())
    }
}

fn option_str(o: &Map<String, Value>, k: &str) -> Result<Option<String>> {
    match o.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(v) => Err(format!("email option `{k}` must be a string, got {v}").into()),
    }
}

/// A recipient field: one address, a comma-separated string, or a list.
fn addresses(v: &Value, field: &str) -> Result<Vec<Mailbox>> {
    let items: Vec<String> = match v {
        Value::String(s) => s.split(',').map(str::to_string).collect(),
        Value::Array(a) => a
            .iter()
            .map(|x| {
                x.as_str()
                    .map(str::to_string)
                    .ok_or_else(|| format!("`{field}` entries must be strings, got {x}"))
            })
            .collect::<std::result::Result<_, _>>()?,
        other => return Err(format!("`{field}` must be an address or a list, got {other}").into()),
    };
    items
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| parse_mailbox(s, field))
        .collect()
}

fn parse_mailbox(s: &str, field: &str) -> Result<Mailbox> {
    Mailbox::from_str(s.trim())
        .map_err(|e| format!("`{field}`: `{s}` isn't a valid email address ({e})").into())
}

fn file_name(p: &Path) -> String {
    p.file_name().unwrap_or_default().to_string_lossy().to_string()
}

fn content_type(name: &str) -> &'static str {
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase());
    match ext.as_deref() {
        Some("csv") => "text/csv",
        Some("txt" | "dat" | "tsv") => "text/plain",
        Some("xlsx") => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        Some("parquet") => "application/vnd.apache.parquet",
        Some("json") => "application/json",
        _ => "application/octet-stream",
    }
}

fn main() {
    serve_destination(
        About::new("email", env!("CARGO_PKG_VERSION")).capabilities(&[CAP_MULTI_FILE]),
        Email,
    )
}
