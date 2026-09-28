//! DRE destination plugin `sftp`.
//!
//! Profile target fields: `host`, `port` (22), `username`, and `password` or `private_key_path`
//! (+ `private_key_passphrase`). The server's host key must be trusted: it's checked against
//! `known_hosts_path` (default `~/.ssh/known_hosts`) or a pinned `host_key_fingerprint`
//! (`SHA256:...`, as `ssh-keygen -lf` prints it). Unknown hosts are refused unless
//! `accept_unknown_host: true`. Missing remote directories are created.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dre_protocol::msg::ConnectionField;
use dre_protocol::plugin::{
    About, Destination, Result, conn_bool, conn_required, conn_str, serve_destination,
};
use russh::client;
use russh::keys::{HashAlg, PrivateKeyWithHashAlg, PublicKey};
use russh_sftp::client::SftpSession;
use serde_json::{Map, Value};
use tokio::io::AsyncWriteExt;

struct HostCheck {
    host: String,
    port: u16,
    known_hosts: PathBuf,
    fingerprint: Option<String>,
    accept_unknown: bool,
    /// Why the key was refused, for the error message.
    refused: Arc<Mutex<Option<String>>>,
}

impl HostCheck {
    fn verdict(&self, key: &PublicKey) -> std::result::Result<(), String> {
        let fp = key.fingerprint(HashAlg::Sha256).to_string();
        if let Some(pin) = &self.fingerprint {
            let want = if pin.starts_with("SHA256:") {
                pin.clone()
            } else {
                format!("SHA256:{pin}")
            };
            return if want == fp {
                Ok(())
            } else {
                Err(format!(
                    "the server's host key is {fp}, not the pinned `host_key_fingerprint` {want}"
                ))
            };
        }
        match russh::keys::check_known_hosts_path(&self.host, self.port, key, &self.known_hosts) {
            Ok(true) => Ok(()),
            Ok(false) if self.accept_unknown => Ok(()),
            Ok(false) => Err(format!(
                "the host key of {}:{} ({fp}) isn't in {}; add it (ssh-keyscan) or pin `host_key_fingerprint: {fp}`",
                self.host,
                self.port,
                self.known_hosts.display()
            )),
            Err(e) => Err(format!(
                "the host key of {}:{} doesn't match {} ({e}); refusing to connect — it may have changed or be spoofed",
                self.host,
                self.port,
                self.known_hosts.display()
            )),
        }
    }
}

impl client::Handler for HostCheck {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        key: &russh::keys::PublicKeyOrCertificate,
    ) -> std::result::Result<bool, Self::Error> {
        let russh::keys::PublicKeyOrCertificate::PublicKey { key, .. } = key else {
            *self.refused.lock().unwrap() =
                Some("the server presented a certificate; DRE only checks plain host keys".into());
            return Ok(false);
        };
        match self.verdict(key) {
            Ok(()) => Ok(true),
            Err(e) => {
                *self.refused.lock().unwrap() = Some(e);
                Ok(false)
            }
        }
    }
}

struct Sftp;

fn home_known_hosts() -> PathBuf {
    std::env::home_dir()
        .unwrap_or_default()
        .join(".ssh")
        .join("known_hosts")
}

async fn upload(local: &Path, remote: &str, c: &Map<String, Value>) -> Result<String> {
    let host = conn_required(c, "host")?.to_string();
    let port: u16 = match c.get("port") {
        Some(Value::Number(n)) => n.as_u64().unwrap_or(22) as u16,
        Some(Value::String(s)) => s.parse().map_err(|_| format!("invalid `port` `{s}`"))?,
        _ => 22,
    };
    let user = conn_required(c, "username")?.to_string();
    let refused = Arc::new(Mutex::new(None));
    let check = HostCheck {
        host: host.clone(),
        port,
        known_hosts: conn_str(c, "known_hosts_path")
            .map(PathBuf::from)
            .unwrap_or_else(home_known_hosts),
        fingerprint: conn_str(c, "host_key_fingerprint").map(str::to_string),
        accept_unknown: conn_bool(c, "accept_unknown_host").unwrap_or(false),
        refused: refused.clone(),
    };
    let config = Arc::new(client::Config {
        inactivity_timeout: Some(Duration::from_secs(300)),
        ..Default::default()
    });
    let connecting = client::connect(config, (host.as_str(), port), check);
    let mut session = match tokio::time::timeout(Duration::from_secs(30), connecting).await {
        Err(_) => return Err(format!("timed out connecting to {host}:{port}").into()),
        Ok(Err(e)) => {
            let why = refused.lock().unwrap().take();
            return Err(why
                .unwrap_or_else(|| format!("can't connect to {host}:{port}: {e}"))
                .into());
        }
        Ok(Ok(s)) => s,
    };
    let auth = if let Some(path) = conn_str(c, "private_key_path") {
        let key = russh::keys::load_secret_key(path, conn_str(c, "private_key_passphrase"))
            .map_err(|e| format!("can't load private key {path}: {e}"))?;
        let hash = session.best_supported_rsa_hash().await?.flatten();
        session
            .authenticate_publickey(&user, PrivateKeyWithHashAlg::new(Arc::new(key), hash))
            .await?
    } else if let Some(pw) = conn_str(c, "password") {
        session.authenticate_password(&user, pw).await?
    } else {
        return Err("sftp needs `password` or `private_key_path`".into());
    };
    if !auth.success() {
        return Err(format!("{host}:{port} refused the credentials for `{user}`").into());
    }
    let channel = session.channel_open_session().await?;
    channel.request_subsystem(true, "sftp").await?;
    let sftp = SftpSession::new(channel.into_stream())
        .await
        .map_err(|e| format!("can't start SFTP: {e}"))?;

    // Create missing parent directories.
    let mut dir = String::new();
    let parts: Vec<&str> = remote.split('/').collect();
    for (i, part) in parts[..parts.len().saturating_sub(1)].iter().enumerate() {
        if part.is_empty() {
            if i == 0 {
                dir.push('/');
            }
            continue;
        }
        if !dir.is_empty() && !dir.ends_with('/') {
            dir.push('/');
        }
        dir.push_str(part);
        if !sftp.try_exists(dir.clone()).await.unwrap_or(false) {
            sftp.create_dir(dir.clone())
                .await
                .map_err(|e| format!("can't create directory {dir}: {e}"))?;
        }
    }
    let mut src = tokio::fs::File::open(local)
        .await
        .map_err(|e| format!("can't read {}: {e}", local.display()))?;
    let mut dst = sftp
        .create(remote)
        .await
        .map_err(|e| format!("can't create {remote}: {e}"))?;
    tokio::io::copy(&mut src, &mut dst)
        .await
        .map_err(|e| format!("upload to {remote} failed: {e}"))?;
    dst.shutdown()
        .await
        .map_err(|e| format!("upload to {remote} failed: {e}"))?;
    let _ = sftp.close().await;
    let _ = session
        .disconnect(russh::Disconnect::ByApplication, "", "en")
        .await;
    Ok(format!(
        "sftp://{user}@{host}:{port}/{}",
        remote.trim_start_matches('/')
    ))
}

impl Destination for Sftp {
    fn connection_fields(&self) -> Vec<ConnectionField> {
        vec![
            ConnectionField::new("host", "SFTP server").required(),
            ConnectionField::new("port", "port").default(22),
            ConnectionField::new("username", "user name").required(),
            ConnectionField::new("password", "password (or set private_key_path)").secret(),
            ConnectionField::new("private_key_path", "private key file (instead of a password)"),
            ConnectionField::new(
                "host_key_fingerprint",
                "pinned host key, SHA256:... (otherwise ~/.ssh/known_hosts is used)",
            ),
        ]
    }

    fn deliver(&mut self, local: &Path, remote: Option<&str>, c: &Map<String, Value>) -> Result<String> {
        let remote = remote.ok_or("sftp needs `output.destination.path`")?;
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        rt.block_on(upload(local, remote, c))
    }
}

fn main() {
    serve_destination(About::new("sftp", env!("CARGO_PKG_VERSION")), Sftp)
}
