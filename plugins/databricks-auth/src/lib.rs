//! How DRE's Databricks plugins authenticate to a workspace: a static token, or OAuth.
//!
//! `auth_type: oauth` without a `client_secret` signs a person in through the browser
//! (authorization code with PKCE, redirected to a listener on localhost). The session is saved
//! in `~/.dre/oauth_sessions.json` (see [`dre_protocol::sessions`]) under
//! `databricks/<host>/<client_id>`, so the source and destination plugins share one sign-in per
//! workspace. The refresh token renews it, so the browser only opens when there is no usable
//! refresh token. With a `client_secret` it signs in as a service principal
//! (client credentials); those tokens are only kept in memory. Either way the access token is
//! renewed shortly before it expires, so a long session keeps working.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use dre_protocol::msg::ConnectionField;
use dre_protocol::plugin::{Result, conn_required, conn_str};
use dre_protocol::sessions;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

/// The public client every Databricks workspace has registered for user sign-in, with
/// `http://localhost:8020` as its redirect.
pub const DEFAULT_CLIENT_ID: &str = "databricks-cli";
const DEFAULT_REDIRECT_PORT: u16 = 8020;
/// Renew an access token this long before it expires.
const RENEW_BEFORE: u64 = 300;
const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(300);

pub enum Auth {
    Token(String),
    OAuth(OAuth),
}

pub struct OAuth {
    /// `https://<host>`, without a trailing slash.
    base: String,
    client_id: String,
    /// Set for a service principal (machine-to-machine).
    client_secret: Option<String>,
    scopes: String,
    redirect_port: u16,
    /// The user sign-in's key in the sessions file; `None` for a service principal.
    session_key: Option<String>,
    tokens: Option<Tokens>,
}

#[derive(Debug, Clone)]
struct Tokens {
    access: String,
    refresh: Option<String>,
    /// Unix seconds.
    expires_at: u64,
}

impl Auth {
    /// Read `auth_type` and its fields from a profile target. `base` is `https://<host>`.
    pub fn from_conn(c: &Map<String, Value>, base: &str) -> Result<Auth> {
        match conn_str(c, "auth_type").unwrap_or("pat") {
            "pat" | "token" => Ok(Auth::Token(conn_required(c, "token")?.to_string())),
            "oauth" => {
                let client_secret = conn_str(c, "client_secret").map(str::to_string);
                let client_id = match (conn_str(c, "client_id"), &client_secret) {
                    (Some(id), _) => id.to_string(),
                    (None, None) => DEFAULT_CLIENT_ID.to_string(),
                    (None, Some(_)) => {
                        return Err("`auth_type: oauth` with a `client_secret` also needs the service principal's `client_id`".into());
                    }
                };
                let m2m = client_secret.is_some();
                let scopes = conn_str(c, "scopes")
                    .unwrap_or(if m2m {
                        "all-apis"
                    } else {
                        "all-apis offline_access"
                    })
                    .to_string();
                let redirect_port = match c.get("redirect_port") {
                    None | Some(Value::Null) => DEFAULT_REDIRECT_PORT,
                    Some(v) => v
                        .as_u64()
                        .or_else(|| v.as_str()?.parse().ok())
                        .and_then(|p| u16::try_from(p).ok())
                        .ok_or("`redirect_port` must be a port number")?,
                };
                let session_key = (!m2m).then(|| session_key(base, &client_id));
                Ok(Auth::OAuth(OAuth {
                    base: base.to_string(),
                    client_id,
                    client_secret,
                    scopes,
                    redirect_port,
                    session_key,
                    tokens: None,
                }))
            }
            other => Err(format!("unknown `auth_type` `{other}`; use `pat` or `oauth`").into()),
        }
    }

    /// The bearer token for the next request, renewed first when it's about to expire.
    pub fn bearer(&mut self, agent: &ureq::Agent) -> Result<String> {
        match self {
            Auth::Token(t) => Ok(t.clone()),
            Auth::OAuth(o) => o.bearer(agent),
        }
    }
}

impl OAuth {
    fn bearer(&mut self, agent: &ureq::Agent) -> Result<String> {
        if self.tokens.is_none() {
            self.tokens = self.load_session();
        }
        if let Some(t) = &self.tokens
            && t.expires_at > now() + RENEW_BEFORE
        {
            return Ok(t.access.clone());
        }
        let fresh = if self.client_secret.is_some() {
            self.client_credentials(agent)?
        } else {
            let refreshed = match self.tokens.as_ref().and_then(|t| t.refresh.clone()) {
                // A refresh token can be revoked or expire; fall back to signing in again.
                Some(r) => self.refresh(agent, &r).ok(),
                None => None,
            };
            match refreshed {
                Some(t) => t,
                None => self.sign_in(agent)?,
            }
        };
        let access = fresh.access.clone();
        self.tokens = Some(fresh);
        self.save_session();
        Ok(access)
    }

    fn token_url(&self) -> String {
        format!("{}/oidc/v1/token", self.base)
    }

    fn redirect_uri(&self) -> String {
        format!("http://localhost:{}", self.redirect_port)
    }

    fn client_credentials(&self, agent: &ureq::Agent) -> Result<Tokens> {
        let secret = self.client_secret.as_deref().unwrap_or_default();
        let basic = STANDARD.encode(format!("{}:{secret}", self.client_id));
        self.token_request(
            agent,
            &[("grant_type", "client_credentials"), ("scope", &self.scopes)],
            Some(&basic),
        )
    }

    fn refresh(&self, agent: &ureq::Agent, refresh: &str) -> Result<Tokens> {
        let mut t = self.token_request(
            agent,
            &[
                ("grant_type", "refresh_token"),
                ("client_id", &self.client_id),
                ("refresh_token", refresh),
            ],
            None,
        )?;
        // Not every response rotates the refresh token; keep the old one when it doesn't.
        t.refresh.get_or_insert_with(|| refresh.to_string());
        Ok(t)
    }

    /// Authorization code with PKCE: open the browser, wait for the redirect, exchange the code.
    fn sign_in(&self, agent: &ureq::Agent) -> Result<Tokens> {
        let listener = TcpListener::bind(("127.0.0.1", self.redirect_port)).map_err(|e| {
            format!(
                "can't listen on localhost:{} for the Databricks sign-in redirect: {e}",
                self.redirect_port
            )
        })?;
        let verifier = random_token(32)?;
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let state = random_token(16)?;
        let url = format!(
            "{}/oidc/v1/authorize?{}",
            self.base,
            form(&[
                ("client_id", &self.client_id),
                ("response_type", "code"),
                ("redirect_uri", &self.redirect_uri()),
                ("scope", &self.scopes),
                ("state", &state),
                ("code_challenge", &challenge),
                ("code_challenge_method", "S256"),
            ])
        );
        eprintln!("Sign in to Databricks in your browser. If it doesn't open, visit:\n{url}");
        open_browser(&url);
        let code = wait_for_code(&listener, &state)?;
        let t = self.token_request(
            agent,
            &[
                ("grant_type", "authorization_code"),
                ("client_id", &self.client_id),
                ("code", &code),
                ("redirect_uri", &self.redirect_uri()),
                ("code_verifier", &verifier),
            ],
            None,
        )?;
        eprintln!("Signed in to Databricks.");
        Ok(t)
    }

    fn token_request(
        &self,
        agent: &ureq::Agent,
        fields: &[(&str, &str)],
        basic: Option<&str>,
    ) -> Result<Tokens> {
        let mut req = agent
            .post(&self.token_url())
            .header("Content-Type", "application/x-www-form-urlencoded")
            .header("Accept", "application/json")
            .header("User-Agent", "dre");
        if let Some(b) = basic {
            req = req.header("Authorization", &format!("Basic {b}"));
        }
        let mut resp = req.send(form(fields).as_bytes()).map_err(|e| {
            format!(
                "can't reach the Databricks token endpoint {}: {e}",
                self.token_url()
            )
        })?;
        let status = resp.status().as_u16();
        let text = resp.body_mut().read_to_string().unwrap_or_default();
        let body: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        if status != 200 {
            let detail = match (body["error"].as_str(), body["error_description"].as_str()) {
                (Some(e), Some(d)) => format!("{e}: {d}"),
                (Some(e), None) => e.to_string(),
                _ => text.chars().take(300).collect(),
            };
            let hint = if self.client_secret.is_some() {
                " (check `client_id` and `client_secret`, and that the service principal can use this workspace)"
            } else {
                ""
            };
            return Err(format!("Databricks OAuth failed (HTTP {status}){hint}: {detail}").into());
        }
        let access = body["access_token"]
            .as_str()
            .ok_or("the Databricks token endpoint returned no access_token")?;
        Ok(Tokens {
            access: access.to_string(),
            refresh: body["refresh_token"].as_str().map(str::to_string),
            expires_at: now() + body["expires_in"].as_u64().unwrap_or(3600),
        })
    }

    fn load_session(&self) -> Option<Tokens> {
        let v = sessions::load(self.session_key.as_ref()?)?;
        Some(Tokens {
            access: v["access_token"].as_str()?.to_string(),
            refresh: v["refresh_token"].as_str().map(str::to_string),
            expires_at: v["expires_at"].as_u64().unwrap_or(0),
        })
    }

    /// Best effort: a session that can't be saved only means signing in again next time.
    fn save_session(&self) {
        let (Some(key), Some(t)) = (&self.session_key, &self.tokens) else {
            return;
        };
        let session = json!({
            "access_token": t.access,
            "refresh_token": t.refresh,
            "expires_at": t.expires_at,
        });
        if let Err(e) = sessions::store(key, Some(session)) {
            eprintln!(
                "couldn't save the Databricks session in {}: {e}",
                sessions::path().display()
            );
        }
    }
}

/// `databricks/<host>/<client_id>`.
fn session_key(base: &str, client_id: &str) -> String {
    let host = base.split("://").nth(1).unwrap_or(base);
    format!("databricks/{host}/{client_id}")
}

/// `https://<host>` for a bare host; a URL with a scheme is kept (tests use `http://`).
pub fn base_url(host: &str) -> String {
    let host = host.trim_end_matches('/');
    if host.starts_with("http://") || host.starts_with("https://") {
        host.to_string()
    } else {
        format!("https://{host}")
    }
}

/// The profile fields for signing in, shared by the source and destination plugins. `dre init`
/// offers a destination the values entered for a Databricks source.
pub fn connection_fields() -> Vec<ConnectionField> {
    [
        ConnectionField::new(
            "auth_type",
            "pat (a token) or oauth (browser sign-in; with client_id and client_secret, a service principal)",
        )
        .default("pat"),
        ConnectionField::new("token", "personal access token, for auth_type pat").secret(),
        ConnectionField::new(
            "client_id",
            "OAuth client; a service principal's application ID (browser sign-in defaults to databricks-cli)",
        ),
        ConnectionField::new("client_secret", "service principal OAuth secret").secret(),
    ]
    .into_iter()
    .map(|f| f.same_as_source("databricks"))
    .collect()
}

/// Accept connections on the redirect listener until one carries the code (or an error).
fn wait_for_code(listener: &TcpListener, state: &str) -> Result<String> {
    listener.set_nonblocking(true)?;
    let started = Instant::now();
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream.set_nonblocking(false)?;
                if let Some(result) = handle_redirect(stream, state) {
                    return result;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if started.elapsed() > SIGN_IN_TIMEOUT {
                    return Err("timed out waiting for the Databricks sign-in in the browser".into());
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => return Err(e.into()),
        }
    }
}

/// Answer one request to the redirect listener. `None` for a request that isn't the redirect
/// (a browser also asks for `/favicon.ico`).
fn handle_redirect(mut stream: TcpStream, state: &str) -> Option<Result<String>> {
    stream.set_read_timeout(Some(Duration::from_secs(10))).ok()?;
    let mut line = String::new();
    BufReader::new(&stream).read_line(&mut line).ok()?;
    let target = line.split_whitespace().nth(1).unwrap_or_default();
    let query = target.split_once('?').map(|(_, q)| q).unwrap_or_default();
    let params: Vec<(String, String)> = query
        .split('&')
        .filter_map(|kv| kv.split_once('='))
        .map(|(k, v)| (decode(k), decode(v)))
        .collect();
    let get = |k: &str| params.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
    let result: Result<String> = match (get("code"), get("error")) {
        (None, None) => {
            let _ = respond(&mut stream, "404 Not Found", "");
            return None;
        }
        (_, Some(e)) => Err(format!(
            "Databricks sign-in failed: {e}{}",
            get("error_description")
                .map(|d| format!(": {d}"))
                .unwrap_or_default()
        )
        .into()),
        (Some(_), None) if get("state").as_deref() != Some(state) => {
            Err("the Databricks sign-in redirect didn't match this sign-in (state mismatch)".into())
        }
        (Some(code), None) => Ok(code),
    };
    let page = match &result {
        Ok(_) => "<h1>Signed in</h1><p>You can close this tab and return to DRE.</p>".to_string(),
        Err(e) => format!("<h1>Sign-in failed</h1><p>{}</p>", html_escape(&e.to_string())),
    };
    let _ = respond(&mut stream, "200 OK", &page);
    Some(result)
}

fn respond(stream: &mut TcpStream, status: &str, html: &str) -> std::io::Result<()> {
    let body = format!("<!doctype html><title>DRE</title>{html}");
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

/// Open `url` in the default browser. `DRE_NO_BROWSER=1` only prints it (for headless use, where
/// the printed URL can be opened elsewhere as long as it redirects back to this machine).
fn open_browser(url: &str) {
    if std::env::var_os("DRE_NO_BROWSER").is_some() {
        return;
    }
    let mut cmd = if cfg!(target_os = "macos") {
        std::process::Command::new("open")
    } else if cfg!(windows) {
        let mut c = std::process::Command::new("rundll32");
        c.arg("url.dll,FileProtocolHandler");
        c
    } else {
        std::process::Command::new("xdg-open")
    };
    let _ = cmd
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

fn random_token(bytes: usize) -> Result<String> {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf).map_err(|e| format!("no randomness for the OAuth sign-in: {e}"))?;
    Ok(URL_SAFE_NO_PAD.encode(buf))
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// `application/x-www-form-urlencoded` (also used for the authorize URL's query).
pub fn form(fields: &[(&str, &str)]) -> String {
    fields
        .iter()
        .map(|(k, v)| format!("{}={}", encode(k), encode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

fn encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

pub fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => {
                match u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("zz"), 16) {
                    Ok(v) => {
                        out.push(v);
                        i += 2;
                    }
                    Err(_) => out.push(b'%'),
                }
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn form_encoding_round_trips() {
        let f = form(&[
            ("scope", "all-apis offline_access"),
            ("redirect_uri", "http://localhost:8020"),
        ]);
        assert_eq!(
            f,
            "scope=all-apis%20offline_access&redirect_uri=http%3A%2F%2Flocalhost%3A8020"
        );
        assert_eq!(decode("a%2Fb+c%3d%"), "a/b c=%");
    }

    #[test]
    fn auth_type_picks_the_flow() {
        let conn = |v: Value| v.as_object().unwrap().clone();
        let base = "https://dbc-1.cloud.databricks.com";
        assert!(
            matches!(Auth::from_conn(&conn(json!({"token": "t"})), base).unwrap(), Auth::Token(t) if t == "t")
        );
        assert!(Auth::from_conn(&conn(json!({})), base).is_err());
        let Auth::OAuth(u2m) = Auth::from_conn(&conn(json!({"auth_type": "oauth"})), base).unwrap() else {
            panic!()
        };
        assert_eq!(u2m.client_id, DEFAULT_CLIENT_ID);
        assert_eq!(u2m.scopes, "all-apis offline_access");
        assert_eq!(
            u2m.session_key.unwrap(),
            "databricks/dbc-1.cloud.databricks.com/databricks-cli"
        );
        let Auth::OAuth(m2m) = Auth::from_conn(
            &conn(json!({"auth_type": "oauth", "client_id": "sp", "client_secret": "s"})),
            base,
        )
        .unwrap() else {
            panic!()
        };
        assert!(m2m.session_key.is_none() && m2m.scopes == "all-apis");
        let err = Auth::from_conn(&conn(json!({"auth_type": "oauth", "client_secret": "s"})), base)
            .err()
            .unwrap()
            .to_string();
        assert!(err.contains("client_id"), "{err}");
        assert!(Auth::from_conn(&conn(json!({"auth_type": "saml"})), base).is_err());
    }
}
