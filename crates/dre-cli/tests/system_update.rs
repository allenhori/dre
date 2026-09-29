//! `dre system update`: a copy of `dre`, placed as each install method places it, against a
//! local fake GitHub Releases API (`DRE_GITHUB_API_URL`).

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use sha2::{Digest, Sha256};

const EXE: &str = std::env::consts::EXE_SUFFIX;
const RECEIPT: &str = "dre-receipt.json";

fn sha(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

fn platform() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

fn receipt(source: &str) -> String {
    format!("{{\"schema\": 1, \"source\": \"{source}\", \"version\": \"0.0.0\"}}\n")
}

/// The stand-in binary a fake release ships: recognisable bytes, never run.
fn stand_in(version: &str) -> Vec<u8> {
    format!("#!/bin/sh\necho stand-in dre {version}\n").into_bytes()
}

/// A tiny HTTP server: `GET <path>` answers the bytes registered for it, else 404. It records
/// each request's path and `Authorization` header.
#[derive(Clone)]
struct Server {
    base: String,
    routes: Arc<Mutex<BTreeMap<String, Vec<u8>>>>,
    hits: Arc<Mutex<Vec<(String, String)>>>,
}

impl Server {
    fn start() -> Server {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let s = Server {
            base: format!("http://{}", l.local_addr().unwrap()),
            routes: Arc::default(),
            hits: Arc::default(),
        };
        let (routes, hits) = (s.routes.clone(), s.hits.clone());
        std::thread::spawn(move || {
            for stream in l.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut r = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                r.read_line(&mut line).unwrap_or_default();
                let path = line.split_whitespace().nth(1).unwrap_or("/").to_string();
                let mut auth = String::new();
                loop {
                    let mut h = String::new();
                    if r.read_line(&mut h).unwrap_or(0) == 0 || h == "\r\n" {
                        break;
                    }
                    if let Some(v) = h.to_lowercase().strip_prefix("authorization:") {
                        auth = v.trim().to_string();
                    }
                }
                hits.lock().unwrap().push((path.clone(), auth));
                let body = routes.lock().unwrap().get(&path).cloned();
                let (status, body) = match body {
                    Some(b) => ("200 OK", b),
                    None => ("404 Not Found", b"not found".to_vec()),
                };
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(&body);
            }
        });
        s
    }

    fn route(&self, path: &str, body: Vec<u8>) {
        self.routes.lock().unwrap().insert(path.to_string(), body);
    }

    fn downloaded(&self) -> Vec<String> {
        self.hits
            .lock()
            .unwrap()
            .iter()
            .map(|(p, _)| p.clone())
            .filter(|p| p.contains("/assets/"))
            .collect()
    }

    /// Publish `allenhori/dre` releases, each with this platform's archive and SHA256SUMS.
    fn releases(&self, versions: &[&str]) {
        self.releases_with(versions, |_, sums| sums)
    }

    /// Like `releases`, with the SHA256SUMS text of each release passed through `sums`.
    fn releases_with(&self, versions: &[&str], sums: impl Fn(&str, String) -> String) {
        let mut listed = Vec::new();
        for v in versions {
            let ext = if cfg!(windows) { "zip" } else { "tar.gz" };
            let name = format!("dre-{v}-{}.{ext}", platform());
            let archive = archive(v);
            let asset = format!("/repos/allenhori/dre/releases/assets/{v}/{name}");
            self.route(&asset, archive.clone());
            let sums_path = format!("/repos/allenhori/dre/releases/assets/{v}/SHA256SUMS");
            let text = format!("{}  {name}\n{}  install.sh\n", sha(&archive), sha(b"x"));
            self.route(&sums_path, sums(v, text).into_bytes());
            listed.push(serde_json::json!({
                "tag_name": format!("v{v}"),
                "draft": false,
                "prerelease": v.contains('-'),
                "assets": [
                    {"name": name, "url": format!("{}{asset}", self.base)},
                    {"name": "SHA256SUMS", "url": format!("{}{sums_path}", self.base)},
                ],
            }));
        }
        listed.push(serde_json::json!({"tag_name": "v9.9.9", "draft": true, "assets": []}));
        listed.push(serde_json::json!({"tag_name": "registry", "assets": []}));
        self.route(
            "/repos/allenhori/dre/releases?per_page=100",
            serde_json::to_vec(&listed).unwrap(),
        );
    }
}

/// A release archive holding the stand-in binary and a `release_archive` receipt.
fn archive(version: &str) -> Vec<u8> {
    let bin = stand_in(version);
    let rec = format!("{{\"schema\": 1, \"source\": \"release_archive\", \"version\": \"{version}\"}}\n");
    if cfg!(windows) {
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut z = zip::ZipWriter::new(&mut buf);
            let o = zip::write::SimpleFileOptions::default();
            z.start_file("dre.exe", o).unwrap();
            z.write_all(&bin).unwrap();
            z.start_file(RECEIPT, o).unwrap();
            z.write_all(rec.as_bytes()).unwrap();
            z.finish().unwrap();
        }
        buf.into_inner()
    } else {
        let mut t = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::fast(),
        ));
        for (name, data, mode) in [("dre", bin, 0o755), (RECEIPT, rec.into_bytes(), 0o644)] {
            let mut h = tar::Header::new_gnu();
            h.set_size(data.len() as u64);
            h.set_mode(mode);
            h.set_cksum();
            t.append_data(&mut h, name, data.as_slice()).unwrap();
        }
        t.into_inner().unwrap().finish().unwrap()
    }
}

/// A copy of the `dre` under test, installed at `rel` in a temp dir.
struct Installed {
    dir: tempfile::TempDir,
    exe: PathBuf,
    original: Vec<u8>,
}

impl Installed {
    /// At `rel` (a directory), with `receipt` next to it (`None`: no receipt).
    fn at(rel: &str, receipt_source: Option<&str>) -> Installed {
        let dir = tempfile::tempdir().unwrap();
        let bin_dir = dir.path().join(rel);
        std::fs::create_dir_all(&bin_dir).unwrap();
        let exe = bin_dir.join(format!("dre{EXE}"));
        let src = assert_cmd::cargo::cargo_bin("dre");
        std::fs::copy(&src, &exe).unwrap();
        if let Some(s) = receipt_source {
            std::fs::write(bin_dir.join(RECEIPT), receipt(s)).unwrap();
        }
        let original = std::fs::read(&exe).unwrap();
        Installed { dir, exe, original }
    }

    fn direct() -> Installed {
        Installed::at("bin", Some("release_archive"))
    }

    fn update(&self, server: &Server, current: &str, args: &[&str]) -> Out {
        self.update_env(server, current, args, &[])
    }

    fn update_env(&self, server: &Server, current: &str, args: &[&str], env: &[(&str, &str)]) -> Out {
        self.run_as(&self.exe, &server.base, current, args, env)
    }

    fn run_as(&self, exe: &Path, api: &str, current: &str, args: &[&str], env: &[(&str, &str)]) -> Out {
        let out = std::process::Command::new(exe)
            .args(["system", "update"])
            .args(args)
            .env("DRE_GITHUB_API_URL", api)
            .env("DRE_TEST_CURRENT_VERSION", current)
            .env_remove("GITHUB_TOKEN")
            .env_remove("SCOOP")
            .env_remove("SCOOP_GLOBAL")
            .env_remove("UV_TOOL_DIR")
            .env_remove("PIPX_HOME")
            .envs(env.iter().copied())
            .output()
            .unwrap();
        Out {
            code: out.status.code().unwrap_or(-1),
            text: format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ),
        }
    }

    fn unchanged(&self) -> bool {
        std::fs::read(&self.exe).unwrap() == self.original
    }

    fn receipt(&self) -> String {
        std::fs::read_to_string(self.exe.with_file_name(RECEIPT)).unwrap_or_default()
    }
}

#[derive(Debug)]
struct Out {
    code: i32,
    text: String,
}

impl Out {
    fn ok(&self) -> &Self {
        assert_eq!(self.code, 0, "expected success:\n{}", self.text);
        self
    }
    fn failed(&self) -> &Self {
        assert_ne!(self.code, 0, "expected failure:\n{}", self.text);
        self
    }
    fn says(&self, s: &str) -> &Self {
        assert!(self.text.contains(s), "expected {s:?} in:\n{}", self.text);
        self
    }
}

// -- --check ----------------------------------------------------------------------------------

#[test]
fn already_latest_downloads_nothing() {
    let s = Server::start();
    s.releases(&["0.0.1-alpha-11", "0.0.1-alpha-12"]);
    let d = Installed::direct();
    d.update(&s, "0.0.1-alpha-12", &["--check"])
        .ok()
        .says("dre 0.0.1-alpha-12 is the latest version");
    d.update(&s, "0.0.1-alpha-12", &[])
        .ok()
        .says("dre 0.0.1-alpha-12 is the latest version");
    assert!(s.downloaded().is_empty());
    assert!(d.unchanged());
}

#[test]
fn check_reports_an_update_and_changes_nothing() {
    let s = Server::start();
    s.releases(&["0.0.1-alpha-11", "0.0.1-alpha-12"]);
    let d = Installed::direct();
    d.update(&s, "0.0.1-alpha-11", &["--check"])
        .ok()
        .says("Update available: 0.0.1-alpha-11 → 0.0.1-alpha-12, run `dre system update`");
    assert!(d.unchanged());
    assert!(s.downloaded().is_empty());
}

#[test]
fn alpha_10_is_newer_than_alpha_9() {
    let s = Server::start();
    s.releases(&["0.0.1-alpha-10", "0.0.1-alpha-9"]);
    Installed::direct()
        .update(&s, "0.0.1-alpha-8", &["--check"])
        .ok()
        .says("→ 0.0.1-alpha-10");
}

#[test]
fn stable_wins_unless_running_a_pre_release() {
    let s = Server::start();
    s.releases(&["0.0.1", "0.0.2", "0.0.3-alpha-1"]);
    let d = Installed::direct();
    d.update(&s, "0.0.1", &["--check"]).ok().says("0.0.1 → 0.0.2,");
    d.update(&s, "0.0.2-alpha-1", &["--check"])
        .ok()
        .says("0.0.2-alpha-1 → 0.0.3-alpha-1");
}

#[test]
fn an_install_without_a_receipt_is_refused() {
    let s = Server::start();
    s.releases(&["0.0.1-alpha-12"]);
    let d = Installed::at("target/release", None);
    for args in [&["--check"][..], &[]] {
        d.update(&s, "0.0.1-alpha-11", args)
            .failed()
            .says("wasn't installed from a DRE release")
            .says("install.sh")
            .says("pip install dre-cli");
    }
    assert!(d.unchanged());
}

#[test]
fn github_unreachable_is_a_clear_error() {
    let d = Installed::direct();
    // Nothing listens on port 9 (discard) here.
    d.run_as(&d.exe, "http://127.0.0.1:9", "0.0.1-alpha-11", &["--check"], &[])
        .failed()
        .says("can't reach GitHub Releases");
    assert!(d.unchanged());
}

#[test]
fn github_token_is_sent() {
    let s = Server::start();
    s.releases(&["0.0.1-alpha-12"]);
    Installed::direct()
        .update_env(&s, "0.0.1-alpha-12", &["--check"], &[("GITHUB_TOKEN", "t0ken")])
        .ok();
    let hits = s.hits.lock().unwrap();
    assert!(
        hits.iter()
            .any(|(p, a)| p.contains("/releases?") && a == "bearer t0ken"),
        "{hits:?}"
    );
}

// -- replacing a direct install ---------------------------------------------------------------

#[test]
fn an_update_replaces_the_binary_and_its_receipt() {
    let s = Server::start();
    s.releases(&["0.0.1-alpha-11", "0.0.1-alpha-12"]);
    let d = Installed::direct();
    d.update(&s, "0.0.1-alpha-11", &[])
        .ok()
        .says("Updated dre from 0.0.1-alpha-11 to 0.0.1-alpha-12")
        .says("https://github.com/allenhori/dre/releases/tag/v0.0.1-alpha-12")
        .says("dre plugin update");
    assert_eq!(std::fs::read(&d.exe).unwrap(), stand_in("0.0.1-alpha-12"));
    assert!(
        d.receipt().contains("\"version\": \"0.0.1-alpha-12\""),
        "{}",
        d.receipt()
    );
    // No temp files left behind.
    let left: Vec<_> = std::fs::read_dir(d.exe.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(left.len(), 2, "{left:?}");
}

#[test]
fn a_pinned_version_with_or_without_v_and_a_downgrade() {
    let s = Server::start();
    s.releases(&["0.0.1-alpha-10", "0.0.1-alpha-11", "0.0.1-alpha-12"]);
    let d = Installed::direct();
    d.update(&s, "0.0.1-alpha-12", &["v0.0.1-alpha-10"])
        .ok()
        .says("Downgraded dre from 0.0.1-alpha-12 to 0.0.1-alpha-10");
    assert_eq!(std::fs::read(&d.exe).unwrap(), stand_in("0.0.1-alpha-10"));

    let d = Installed::direct();
    d.update(&s, "0.0.1-alpha-10", &["0.0.1-alpha-11"])
        .ok()
        .says("Updated dre from 0.0.1-alpha-10 to 0.0.1-alpha-11");
    assert_eq!(std::fs::read(&d.exe).unwrap(), stand_in("0.0.1-alpha-11"));

    let d = Installed::direct();
    d.update(&s, "0.0.1-alpha-11", &["v0.0.1-alpha-11"])
        .ok()
        .says("dre 0.0.1-alpha-11 is already installed");
    assert!(d.unchanged());
}

#[test]
fn a_version_that_doesnt_exist_is_an_error() {
    let s = Server::start();
    s.releases(&["0.0.1-alpha-12"]);
    let d = Installed::direct();
    d.update(&s, "0.0.1-alpha-11", &["0.0.1-alpha-99"])
        .failed()
        .says("0.0.1-alpha-99")
        .says("https://github.com/allenhori/dre/releases");
    assert!(d.unchanged());
}

#[test]
fn a_checksum_mismatch_changes_nothing() {
    let s = Server::start();
    s.releases_with(&["0.0.1-alpha-12"], |_, sums| {
        let (_, rest) = sums.split_once(' ').unwrap();
        format!("{}{rest}", "0".repeat(64))
    });
    let d = Installed::direct();
    d.update(&s, "0.0.1-alpha-11", &[])
        .failed()
        .says("doesn't match its SHA256SUMS entry")
        .says("untouched");
    assert!(d.unchanged());
    assert!(d.receipt().contains("release_archive"));
}

#[test]
fn a_missing_checksum_entry_changes_nothing() {
    let s = Server::start();
    s.releases_with(&["0.0.1-alpha-12"], |_, _| format!("{}  install.sh\n", sha(b"x")));
    let d = Installed::direct();
    d.update(&s, "0.0.1-alpha-11", &[])
        .failed()
        .says("has no entry for")
        .says("untouched");
    assert!(d.unchanged());
}

#[cfg(unix)]
#[test]
fn a_read_only_install_directory_changes_nothing() {
    use std::os::unix::fs::PermissionsExt;
    let s = Server::start();
    s.releases(&["0.0.1-alpha-12"]);
    let d = Installed::direct();
    let dir = d.exe.parent().unwrap().to_path_buf();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
    // Root ignores directory permissions; there's nothing to check then.
    let probe = dir.join("probe");
    if std::fs::write(&probe, "x").is_ok() {
        let _ = std::fs::remove_file(&probe);
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        eprintln!("skipped: running as a user who can write to read-only directories");
        return;
    }
    let out = d.update(&s, "0.0.1-alpha-11", &[]);
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    out.failed()
        .says("permission denied")
        .says("re-run with the permissions")
        .says("untouched");
    assert!(d.unchanged());
}

// -- installs owned by a package manager ------------------------------------------------------

const SITE: &str = if cfg!(windows) {
    "Lib/site-packages/dre_cli/bin"
} else {
    "lib/python3.12/site-packages/dre_cli/bin"
};

fn managed(d: &Installed, s: &Server, command: &str) {
    let out = d.update(s, "0.0.1-alpha-11", &[]);
    out.failed()
        .says("Update available: 0.0.1-alpha-11 → 0.0.1-alpha-12")
        .says(command);
    d.update(s, "0.0.1-alpha-11", &["--check"]).ok().says(command);
    d.update(s, "0.0.1-alpha-12", &[])
        .ok()
        .says("dre 0.0.1-alpha-12 is the latest version");
    assert!(d.unchanged());
    assert!(s.downloaded().is_empty());
}

#[test]
fn pip_names_the_environments_python() {
    let s = Server::start();
    s.releases(&["0.0.1-alpha-12"]);
    let d = Installed::at(&format!("venv/{SITE}"), Some("pypi"));
    let venv = d.dir.path().canonicalize().unwrap().join("venv");
    let python = if cfg!(windows) {
        venv.join("Scripts").join("python.exe")
    } else {
        venv.join("bin").join("python")
    };
    std::fs::create_dir_all(python.parent().unwrap()).unwrap();
    std::fs::write(&python, "").unwrap();
    managed(&d, &s, &format!("{} -m pip install -U dre-cli", python.display()));
    d.update(&s, "0.0.1-alpha-11", &[])
        .says("installed with pip")
        .says("bundled")
        .says("pin");
}

#[test]
fn uv_tool_and_pipx() {
    let s = Server::start();
    s.releases(&["0.0.1-alpha-12"]);
    let d = Installed::at(&format!(".local/share/uv/tools/dre-cli/{SITE}"), Some("pypi"));
    managed(&d, &s, "uv tool upgrade dre-cli");
    let d = Installed::at(&format!(".local/share/pipx/venvs/dre-cli/{SITE}"), Some("pypi"));
    managed(&d, &s, "pipx upgrade dre-cli");
}

#[cfg(unix)]
#[test]
fn homebrew_through_its_symlink() {
    let s = Server::start();
    s.releases(&["0.0.1-alpha-12"]);
    // A formula built from a release archive keeps its receipt; the Cellar path still wins.
    let d = Installed::at("homebrew/Cellar/dre/0.0.1-alpha-11/bin", Some("release_archive"));
    let link = d.dir.path().join("homebrew/bin/dre");
    std::fs::create_dir_all(link.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&d.exe, &link).unwrap();
    d.run_as(&link, &s.base, "0.0.1-alpha-11", &[], &[])
        .failed()
        .says("Homebrew")
        .says("brew upgrade dre");
    assert!(d.unchanged());
}

#[test]
fn scoop_by_layout_or_root() {
    let s = Server::start();
    s.releases(&["0.0.1-alpha-12"]);
    let d = Installed::at("scoop/apps/dre/current", Some("release_archive"));
    managed(&d, &s, "scoop update dre");

    let d = Installed::at("tools/sc/apps2/dre", Some("release_archive"));
    let root = d.dir.path().join("tools/sc");
    d.update_env(&s, "0.0.1-alpha-11", &[], &[("SCOOP", root.to_str().unwrap())])
        .failed()
        .says("scoop update dre");
    assert!(d.unchanged());
}

#[test]
fn a_managed_install_gets_the_command_for_a_pinned_version() {
    let s = Server::start();
    s.releases(&["0.0.1-alpha-10", "0.0.1-alpha-12"]);
    let d = Installed::at(&format!(".local/share/uv/tools/dre-cli/{SITE}"), Some("pypi"));
    d.update(&s, "0.0.1-alpha-12", &["v0.0.1-alpha-10", "--check"])
        .ok()
        .says("uv tool install --force dre-cli==0.0.1a10");
    let d = Installed::at(&format!(".local/share/pipx/venvs/dre-cli/{SITE}"), Some("pypi"));
    d.update(&s, "0.0.1-alpha-12", &["0.0.1-alpha-10"])
        .failed()
        .says("pipx install --force dre-cli==0.0.1a10");
    assert!(d.unchanged());
}

#[test]
fn a_managed_build_newer_than_every_release_is_up_to_date() {
    let s = Server::start();
    s.releases(&["0.0.1-alpha-11"]);
    let d = Installed::at(&format!("venv/{SITE}"), Some("pypi"));
    d.update(&s, "0.0.1-alpha-12", &[])
        .ok()
        .says("dre 0.0.1-alpha-12 is the latest version");
}

#[cfg(unix)]
#[test]
fn a_user_install_without_its_own_python_names_python3() {
    let s = Server::start();
    s.releases(&["0.0.1-alpha-12"]);
    let d = Installed::at(&format!("home/.local/{SITE}"), Some("pypi"));
    d.update(&s, "0.0.1-alpha-11", &["--check"])
        .ok()
        .says("\n  python3 -m pip install -U dre-cli");
}
