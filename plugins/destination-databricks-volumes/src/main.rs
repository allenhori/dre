//! DRE destination plugin `databricks_volumes`: uploads to a Unity Catalog Volume through the
//! Databricks Files API, for runs outside Databricks (a laptop, Airflow, CI).
//!
//! Inside a Databricks job or cluster, `/Volumes/...` is a mounted path: use the built-in `local`
//! destination there instead; no plugin or token is needed.
//!
//! Profile output fields: `host` and `token`, the same fields as the Databricks source profile,
//! so one set of credentials can serve both. The remote path must be
//! `/Volumes/<catalog>/<schema>/<volume>/...`; missing directories are created. Retries while
//! the workspace answers 429/503.

use std::path::Path;
use std::time::{Duration, Instant};

use dre_protocol::msg::ConnectionField;
use dre_protocol::plugin::{About, Destination, Result, conn_required, serve_destination};
use serde_json::{Map, Value};

struct Volumes;

/// Percent-encode each path segment, keeping the slashes.
fn encode_path(p: &str) -> String {
    p.split('/')
        .map(|seg| {
            seg.bytes()
                .map(|b| match b {
                    b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                        (b as char).to_string()
                    }
                    b => format!("%{b:02X}"),
                })
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn request(agent: &ureq::Agent, url: &str, token: &str, body: Option<&Path>) -> Result<()> {
    let started = Instant::now();
    let mut wait = Duration::from_secs(1);
    loop {
        let req = agent.put(url).header("Authorization", &format!("Bearer {token}"));
        let resp = match body {
            Some(p) => {
                let f = std::fs::File::open(p).map_err(|e| format!("can't read {}: {e}", p.display()))?;
                let len = f.metadata()?.len();
                req.header("Content-Type", "application/octet-stream")
                    .header("Content-Length", &len.to_string())
                    .send(ureq::SendBody::from_reader(&mut std::io::BufReader::new(f)))
            }
            None => req.send_empty(),
        };
        let mut resp = resp.map_err(|e| format!("can't reach Databricks: {e}"))?;
        let status = resp.status().as_u16();
        if (200..300).contains(&status) {
            return Ok(());
        }
        if matches!(status, 429 | 503) && started.elapsed() < Duration::from_secs(300) {
            std::thread::sleep(wait);
            wait = (wait * 2).min(Duration::from_secs(30));
            continue;
        }
        let text = resp.body_mut().read_to_string().unwrap_or_default();
        let hint = match status {
            401 | 403 => " (check the token and its permissions on the volume)",
            404 => " (check the catalog, schema and volume exist)",
            _ => "",
        };
        return Err(format!(
            "HTTP {status}{hint}: {}",
            text.chars().take(300).collect::<String>()
        )
        .into());
    }
}

impl Destination for Volumes {
    fn connection_fields(&self) -> Vec<ConnectionField> {
        vec![
            ConnectionField::new("host", "workspace host, e.g. adb-123.4.azuredatabricks.net").required(),
            ConnectionField::new("token", "personal access token")
                .required()
                .secret(),
        ]
    }

    fn deliver(&mut self, local: &Path, remote: Option<&str>, c: &Map<String, Value>) -> Result<String> {
        let remote = remote.ok_or("databricks_volumes needs `output.destination.path`")?;
        let parts: Vec<&str> = remote.trim_start_matches('/').split('/').collect();
        if parts.first() != Some(&"Volumes") || parts.len() < 5 || parts.iter().any(|p| p.is_empty()) {
            return Err(format!("`{remote}` must be /Volumes/<catalog>/<schema>/<volume>/<file>").into());
        }
        let host = conn_required(c, "host")?.trim_end_matches('/');
        let base = if host.starts_with("http://") || host.starts_with("https://") {
            host.to_string()
        } else {
            format!("https://{host}")
        };
        let token = conn_required(c, "token")?;
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .build()
            .into();
        let path = format!("/{}", parts.join("/"));
        // Directories under the volume (the volume itself must exist).
        if parts.len() > 5 {
            let dir = format!("/{}", parts[..parts.len() - 1].join("/"));
            request(
                &agent,
                &format!("{base}/api/2.0/fs/directories{}", encode_path(&dir)),
                token,
                None,
            )
            .map_err(|e| format!("can't create {dir}: {e}"))?;
        }
        request(
            &agent,
            &format!("{base}/api/2.0/fs/files{}?overwrite=true", encode_path(&path)),
            token,
            Some(local),
        )
        .map_err(|e| format!("upload to {path} failed: {e}; the output is still in target/"))?;
        Ok(format!("dbfs:{path}"))
    }
}

fn main() {
    serve_destination(
        About::new("databricks_volumes", env!("CARGO_PKG_VERSION")),
        Volumes,
    )
}
