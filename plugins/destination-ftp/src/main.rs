//! DRE destination plugin `ftp`: plain FTP or explicit FTPS (`AUTH TLS`).
//!
//! Profile target fields: `host`, `port` (21), `username`, `password`, `passive` (default true),
//! `tls`: `none` (default) or `explicit`, and `tls_accept_invalid_certs` (default false, for
//! servers with self-signed certificates). Missing remote directories are created.

use std::net::ToSocketAddrs;
use std::path::Path;
use std::time::Duration;

use dre_protocol::msg::ConnectionField;
use dre_protocol::plugin::{
    About, Destination, Result, conn_bool, conn_required, conn_str, serve_destination,
};
use serde_json::{Map, Value};
use suppaftp::{Mode, NativeTlsConnector, NativeTlsFtpStream};

struct Ftp;

/// Connects to the first of `host`'s addresses that answers, like `TcpStream::connect` does:
/// `localhost` resolves to both `::1` and `127.0.0.1`, and a server may listen on only one.
fn connect(host: &str, port: u16) -> std::result::Result<NativeTlsFtpStream, String> {
    let mut last = None;
    for addr in (host, port)
        .to_socket_addrs()
        .map_err(|e| format!("can't resolve {host}: {e}"))?
    {
        match NativeTlsFtpStream::connect_timeout(addr, Duration::from_secs(30)) {
            Ok(ftp) => return Ok(ftp),
            Err(e) => last = Some(e),
        }
    }
    Err(match last {
        Some(e) => format!("can't connect to {host}:{port}: {e}"),
        None => format!("can't resolve {host}"),
    })
}

fn upload(local: &Path, remote: &str, c: &Map<String, Value>) -> Result<String> {
    let host = conn_required(c, "host")?;
    let port: u16 = match c.get("port") {
        Some(Value::Number(n)) => n.as_u64().unwrap_or(21) as u16,
        Some(Value::String(s)) => s.parse().map_err(|_| format!("invalid `port` `{s}`"))?,
        _ => 21,
    };
    let user = conn_required(c, "username")?;
    let password = conn_str(c, "password").unwrap_or("");
    let mut ftp = connect(host, port)?;
    match conn_str(c, "tls").unwrap_or("none") {
        "none" => {}
        "explicit" => {
            let mut b = native_tls::TlsConnector::builder();
            if conn_bool(c, "tls_accept_invalid_certs").unwrap_or(false) {
                b.danger_accept_invalid_certs(true)
                    .danger_accept_invalid_hostnames(true);
            }
            ftp = ftp
                .into_secure(NativeTlsConnector::from(b.build()?), host)
                .map_err(|e| format!("{host}:{port} didn't accept explicit FTPS (AUTH TLS): {e}"))?;
        }
        t => return Err(format!("unknown `tls` `{t}` (none or explicit)").into()),
    }
    ftp.login(user, password)
        .map_err(|e| format!("{host}:{port} refused the credentials for `{user}`: {e}"))?;
    ftp.set_mode(if conn_bool(c, "passive").unwrap_or(true) {
        Mode::Passive
    } else {
        Mode::Active
    });
    ftp.transfer_type(suppaftp::types::FileType::Binary)?;

    // Walk (and create) the directories, then upload the file there.
    let (dir, name) = match remote.rsplit_once('/') {
        Some((d, n)) => (d, n),
        None => ("", remote),
    };
    if remote.starts_with('/') {
        ftp.cwd("/")?;
    }
    for part in dir.split('/').filter(|p| !p.is_empty()) {
        if ftp.cwd(part).is_err() {
            ftp.mkdir(part)
                .map_err(|e| format!("can't create directory `{part}`: {e}"))?;
            ftp.cwd(part)?;
        }
    }
    let mut f = std::fs::File::open(local).map_err(|e| format!("can't read {}: {e}", local.display()))?;
    ftp.put_file(name, &mut f)
        .map_err(|e| format!("upload to {remote} failed: {e}"))?;
    let _ = ftp.quit();
    Ok(format!(
        "ftp://{user}@{host}:{port}/{}",
        remote.trim_start_matches('/')
    ))
}

impl Destination for Ftp {
    fn connection_fields(&self) -> Vec<ConnectionField> {
        vec![
            ConnectionField::new("host", "FTP server").required(),
            ConnectionField::new("port", "port").default(21),
            ConnectionField::new("username", "user name").required(),
            ConnectionField::new("password", "password").secret(),
            ConnectionField::new("tls", "none or explicit (FTPS)").default("none"),
        ]
    }

    fn deliver(&mut self, local: &Path, remote: Option<&str>, c: &Map<String, Value>) -> Result<String> {
        upload(local, remote.ok_or("ftp needs `output.destination.path`")?, c)
    }
}

fn main() {
    serve_destination(About::new("ftp", env!("CARGO_PKG_VERSION")), Ftp)
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::net::TcpListener;

    #[test]
    fn connect_falls_back_to_the_next_resolved_address() {
        // Listen on IPv4 only; `localhost` often resolves to `::1` first.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            s.write_all(b"220 ready\r\n").unwrap();
        });
        super::connect("localhost", port).unwrap();
        server.join().unwrap();
    }
}
