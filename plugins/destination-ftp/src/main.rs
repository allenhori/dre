//! DRE destination plugin `ftp`: plain FTP or explicit FTPS (`AUTH TLS`).
//!
//! Profile target fields: `host`, `port` (21), `username`, `password`, `passive` (default true),
//! `tls`: `none` (default) or `explicit`, and `tls_accept_invalid_certs` (default false, for
//! servers with self-signed certificates). Missing remote directories are created.
//!
//! FTPS data connections resume the control connection's TLS session, as most servers require
//! (vsftpd's `require_ssl_reuse`). A failed upload removes the partial remote file when it can.

use std::net::ToSocketAddrs;
use std::path::Path;
use std::time::Duration;

use dre_protocol::msg::ConnectionField;
use dre_protocol::plugin::{
    About, Destination, Result, conn_bool, conn_required, conn_str, serve_destination,
};
use serde_json::{Map, Value};
use std::sync::Arc;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, SignatureScheme};
use rustls_platform_verifier::BuilderVerifierExt;
use suppaftp::{Mode, RustlsConnector, RustlsFtpStream};

struct Ftp;

/// Connects to the first of `host`'s addresses that answers, like `TcpStream::connect` does:
/// `localhost` resolves to both `::1` and `127.0.0.1`, and a server may listen on only one.
fn connect(host: &str, port: u16) -> std::result::Result<RustlsFtpStream, String> {
    let mut last = None;
    for addr in (host, port)
        .to_socket_addrs()
        .map_err(|e| format!("can't resolve {host}: {e}"))?
    {
        match RustlsFtpStream::connect_timeout(addr, Duration::from_secs(30)) {
            Ok(ftp) => return Ok(ftp),
            Err(e) => last = Some(e),
        }
    }
    Err(match last {
        Some(e) => format!("can't connect to {host}:{port}: {e}"),
        None => format!("can't resolve {host}"),
    })
}

/// The TLS setup for control and data connections. One config is shared by both, so its
/// session cache lets the data connections resume the control connection's session.
///
/// TLS 1.2 first: under TLS 1.3 the server sends session tickets on every data connection,
/// which nothing reads, so closing it resets the connection and vsftpd fails the upload.
/// `tls13` is the fallback for servers that only speak TLS 1.3.
fn tls_config(accept_invalid: bool, tls13: bool) -> std::result::Result<Arc<ClientConfig>, String> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let versions: &[&rustls::SupportedProtocolVersion] = if tls13 {
        &[&rustls::version::TLS13]
    } else {
        &[&rustls::version::TLS12]
    };
    let builder = ClientConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(versions)
        .map_err(|e| e.to_string())?;
    let config = if accept_invalid {
        builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AcceptAny(provider)))
            .with_no_client_auth()
    } else {
        builder
            .with_platform_verifier()
            .map_err(|e| format!("can't load the system's certificates: {e}"))?
            .with_no_client_auth()
    };
    Ok(Arc::new(config))
}

/// `tls_accept_invalid_certs: true`: any certificate and host name, signatures still checked.
#[derive(Debug)]
struct AcceptAny(Arc<rustls::crypto::CryptoProvider>);

impl ServerCertVerifier for AcceptAny {
    fn verify_server_cert(
        &self,
        _: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.0.signature_verification_algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.0.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
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
            let accept_invalid = conn_bool(c, "tls_accept_invalid_certs").unwrap_or(false);
            ftp = match ftp.into_secure(RustlsConnector::from(tls_config(accept_invalid, false)?), host) {
                Ok(f) => f,
                Err(e12) => {
                    // Maybe a TLS 1.3-only server: once more, on a new connection.
                    let again = connect(host, port)?;
                    again
                        .into_secure(RustlsConnector::from(tls_config(accept_invalid, true)?), host)
                        .map_err(|_| format!("{host}:{port} didn't accept explicit FTPS (AUTH TLS): {e12}"))?
                }
            };
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
    if let Err(e) = ftp.put_file(name, &mut f) {
        // Don't leave a partial (often empty) file behind.
        let cleanup = match ftp.rm(name) {
            Ok(()) => "the partial remote file was removed",
            Err(_) => "the partial remote file may be left on the server",
        };
        return Err(format!("upload to {remote} failed: {e} ({cleanup})").into());
    }
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
