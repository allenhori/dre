//! Plugins from somewhere other than the default registry: `github:` (GitHub Releases),
//! `local:` (a file in place) and `registry:` (another index), declared in `dependencies.yml`.

mod common;

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use common::{Run, test_plugins};
use sha2::{Digest, Sha256};

fn sha(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

fn platform() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

fn exe() -> String {
    format!("dre-source-fixture{}", std::env::consts::EXE_SUFFIX)
}

fn fixture_bin() -> Vec<u8> {
    std::fs::read(test_plugins(&["dre-source-fixture"]).join(exe())).unwrap()
}

/// Where the fake GitHub API serves a release asset.
fn asset_path(version: &str, name: &str) -> String {
    format!("/repos/acme/dre-source-fixture/releases/assets/{version}/{name}")
}

/// A tiny HTTP server: `GET <path>` answers the bytes registered for it, else 404.
#[derive(Clone)]
struct Server {
    base: String,
    routes: Arc<Mutex<BTreeMap<String, Vec<u8>>>>,
    hits: Arc<Mutex<Vec<String>>>,
}

impl Server {
    fn start() -> Server {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", l.local_addr().unwrap());
        let s = Server {
            base,
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
                let mut accept = String::new();
                loop {
                    let mut h = String::new();
                    if r.read_line(&mut h).unwrap_or(0) == 0 || h == "\r\n" {
                        break;
                    }
                    if let Some(v) = h.to_lowercase().strip_prefix("accept:") {
                        accept = v.trim().to_string();
                    }
                }
                hits.lock().unwrap().push(format!("{path} {accept}"));
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

    /// `acme/dre-source-fixture` with releases `v1.0.0` and `v1.1.0` of the fixture source.
    /// `with_sha` publishes a `.sha256` next to each asset.
    fn github_releases(&self, bin: &[u8], with_sha: bool) {
        let mut releases = Vec::new();
        for v in ["1.0.0", "1.1.0"] {
            let asset = format!("dre-source-fixture-{v}-{}", platform());
            let url = asset_path(v, &asset);
            self.route(&url, bin.to_vec());
            let mut assets = vec![serde_json::json!({"name": asset, "url": format!("{}{url}", self.base)})];
            if with_sha {
                let sum = asset_path(v, &format!("{asset}.sha256"));
                self.route(&sum, format!("{}  {asset}\n", sha(bin)).into_bytes());
                assets.push(serde_json::json!({"name": format!("{asset}.sha256"), "url": format!("{}{sum}", self.base)}));
            }
            releases.push(serde_json::json!({"tag_name": format!("v{v}"), "draft": false, "assets": assets}));
        }
        // Not a version: ignored.
        releases.push(serde_json::json!({"tag_name": "nightly", "assets": []}));
        self.route(
            "/repos/acme/dre-source-fixture/releases?per_page=100",
            serde_json::to_vec(&releases).unwrap(),
        );
    }
}

struct Env {
    dir: tempfile::TempDir,
    server: Server,
}

impl Env {
    fn new(deps: &str) -> Env {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("project");
        std::fs::create_dir_all(project.join("reports/ops/f")).unwrap();
        std::fs::write(
            project.join("dre_project.yml"),
            "name: acme_reports\ndefault_profile: fx\n",
        )
        .unwrap();
        std::fs::write(project.join("dependencies.yml"), deps).unwrap();
        std::fs::write(project.join("reports/ops/f/f.yml"), "queries: [fq]\n").unwrap();
        std::fs::write(project.join("reports/ops/f/fq.sql"), "rows 3").unwrap();
        // csv is placed by hand, so only the fixture source is resolved.
        let plugins = dir.path().join("plugins");
        std::fs::create_dir_all(&plugins).unwrap();
        let csv = format!("dre-format-csv{}", std::env::consts::EXE_SUFFIX);
        std::fs::copy(test_plugins(&["dre-format-csv"]).join(&csv), plugins.join(&csv)).unwrap();
        std::fs::create_dir_all(dir.path().join("profiles")).unwrap();
        std::fs::write(
            dir.path().join("profiles/profiles.yml"),
            "sources:\n  fx:\n    target: dev\n    targets:\n      dev: {type: fixture}\n",
        )
        .unwrap();
        Env {
            dir,
            server: Server::start(),
        }
    }

    fn p(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }

    fn dre(&self, args: &[&str]) -> Run {
        let mut c = assert_cmd::Command::cargo_bin("dre").unwrap();
        c.args(args)
            .current_dir(self.p("project"))
            .env("DRE_PLUGINS_DIR", self.p("plugins"))
            // The default registry isn't reachable: nothing here may need it.
            .env("DRE_REGISTRY_URL", self.p("no-registry/index.json"))
            .env("DRE_GITHUB_API_URL", &self.server.base)
            .env("DRE_PROFILES_DIR", self.p("profiles"))
            .env("HOME", self.p("home"))
            .env_remove("GITHUB_TOKEN");
        let out = c.output().unwrap();
        Run {
            code: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into(),
            stderr: String::from_utf8_lossy(&out.stderr).into(),
        }
    }

    fn write(&self, rel: &str, s: &str) {
        std::fs::write(self.p(rel), s).unwrap();
    }

    fn lock(&self) -> String {
        std::fs::read_to_string(self.p("project/dre.lock")).unwrap_or_default()
    }

    fn installed(&self, version: &str) -> bool {
        self.p("plugins/source/fixture")
            .join(version)
            .join(exe())
            .exists()
    }
}

const GITHUB: &str =
    "sources:\n  - {name: fixture, github: acme/dre-source-fixture, version: \">=1.0\"}\nformats:\n  - csv\n";

#[test]
fn github_releases_install_the_newest_match_and_pin_it() {
    let e = Env::new(GITHUB);
    let bin = fixture_bin();
    e.server.github_releases(&bin, true);
    e.dre(&["deps"]).ok().says("source plugin `fixture` 1.1.0");
    assert!(e.installed("1.1.0") && !e.installed("1.0.0"));
    let lock = e.lock();
    assert!(
        lock.contains(&format!(
            "sources:\n  fixture:\n    version: 1.1.0\n    sha256:\n      {}: {}\n    from: github:acme/dre-source-fixture\n",
            platform(),
            sha(&bin)
        )),
        "{lock}"
    );
    e.dre(&["run"]).ok();
    // Assets come through the API URL, asking for the file rather than its metadata.
    let hits = e.server.hits.lock().unwrap().clone();
    assert!(
        hits.iter()
            .any(|h| h.contains("/releases/assets/1.1.0/") && h.ends_with("application/octet-stream")),
        "{hits:?}"
    );
    // Narrower constraint, fresh resolve.
    e.write("project/dependencies.yml", &GITHUB.replace(">=1.0", "<1.1"));
    std::fs::remove_file(e.p("project/dre.lock")).unwrap();
    e.dre(&["deps"]).ok().says("source plugin `fixture` 1.0.0");
}

#[test]
fn github_checksum_mismatch_is_refused() {
    let e = Env::new(GITHUB);
    let bin = fixture_bin();
    e.server.github_releases(&bin, true);
    let asset = format!("dre-source-fixture-1.1.0-{}", platform());
    e.server.route(
        &asset_path("1.1.0", &format!("{asset}.sha256")),
        format!("{}  {asset}\n", "0".repeat(64)).into_bytes(),
    );
    e.dre(&["deps"])
        .failed()
        .says("checksum mismatch")
        .says("the download was discarded");
    assert!(!e.installed("1.1.0"));
}

#[test]
fn github_without_published_checksums_pins_the_first_download() {
    let e = Env::new(GITHUB);
    let bin = fixture_bin();
    e.server.github_releases(&bin, false);
    e.dre(&["deps"]).ok();
    assert!(
        e.lock().contains(&format!("{}: {}", platform(), sha(&bin))),
        "{}",
        e.lock()
    );
    // The same release now serves different bytes: the pin catches it.
    std::fs::remove_dir_all(e.p("plugins/source")).unwrap();
    std::fs::remove_dir_all(e.p("home")).ok();
    let asset = format!("dre-source-fixture-1.1.0-{}", platform());
    e.server.route(&asset_path("1.1.0", &asset), b"tampered".to_vec());
    e.dre(&["deps"]).failed().says("checksum mismatch");
}

#[test]
fn local_plugins_are_used_in_place_and_recorded() {
    let e = Env::new("sources:\n  - {name: fixture, local: bin/my-fixture}\nformats:\n  - csv\n");
    std::fs::create_dir_all(e.p("project/bin")).unwrap();
    let dst = e.p("project/bin/my-fixture");
    std::fs::write(&dst, fixture_bin()).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dst, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    e.dre(&["deps"]).ok();
    assert!(
        e.lock().contains("local:\n  source/fixture: bin/my-fixture\n"),
        "{}",
        e.lock()
    );
    assert!(!e.p("plugins/source").exists(), "nothing is installed");
    e.dre(&["run"]).ok();
    e.dre(&["plugin", "update", "fixture"])
        .ok()
        .says("used from bin/my-fixture");
    std::fs::remove_file(&dst).unwrap();
    e.dre(&["run"])
        .failed()
        .says("declared `local: bin/my-fixture`, but there's no file");
}

#[test]
fn a_per_plugin_registry_is_used_for_that_plugin_only() {
    let e = Env::new("sources:\n  - {name: fixture, registry: REG}\nformats:\n  - csv\n");
    let reg = e.p("other-registry");
    std::fs::create_dir_all(&reg).unwrap();
    let bin = fixture_bin();
    std::fs::write(reg.join("fixture-2.0.0"), &bin).unwrap();
    let index = serde_json::json!({"schema": 1, "plugins": [{"kind": "source", "name": "fixture", "versions": [
        {"version": "2.0.0", "protocol": 0, "artifacts": {platform(): {"url": reg.join("fixture-2.0.0").to_string_lossy(), "sha256": sha(&bin)}}}
    ]}]});
    std::fs::write(reg.join("index.json"), index.to_string()).unwrap();
    e.write(
        "project/dependencies.yml",
        &format!(
            "sources:\n  - {{name: fixture, registry: '{}'}}\nformats:\n  - csv\n",
            reg.join("index.json").display()
        ),
    );
    e.dre(&["deps"]).ok().says("source plugin `fixture` 2.0.0");
    assert!(e.lock().contains("from: registry:"), "{}", e.lock());
    e.dre(&["run"]).ok();
}

#[test]
fn changing_the_source_resolves_the_plugin_again() {
    let e = Env::new(GITHUB);
    let bin = fixture_bin();
    e.server.github_releases(&bin, true);
    e.dre(&["deps"]).ok();
    assert!(e.lock().contains("from: github:"));
    std::fs::create_dir_all(e.p("project/bin")).unwrap();
    std::fs::write(e.p("project/bin/fx"), &bin).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(e.p("project/bin/fx"), std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    e.write(
        "project/dependencies.yml",
        "sources:\n  - {name: fixture, local: bin/fx}\nformats:\n  - csv\n",
    );
    e.dre(&["deps"]).ok();
    let lock = e.lock();
    assert!(
        lock.contains("source/fixture: bin/fx") && !lock.contains("from: github:"),
        "{lock}"
    );
    e.write("project/dependencies.yml", GITHUB);
    e.dre(&["deps"]).ok();
    let lock = e.lock();
    assert!(
        lock.contains("from: github:") && !lock.contains("bin/fx"),
        "{lock}"
    );
}

#[test]
fn bad_entries_are_validate_errors() {
    for (deps, msg) in [
        (
            "sources:\n  - {name: fixture, github: nope}\n",
            "`github: nope` must be `owner/repo`",
        ),
        (
            "sources:\n  - {name: fixture, github: a/b, local: x}\n",
            "give only one of `github`, `local` and `registry`",
        ),
        (
            "sources:\n  - {name: fixture, gitlab: a/b}\n",
            "unknown key `gitlab`",
        ),
        (
            "sources:\n  - {name: fixture, local: x, version: '1'}\n",
            "a `local` plugin has no `version`",
        ),
        (
            "sources:\n  - {name: '', local: x}\n",
            "`name` must be a non-empty string",
        ),
        ("sources:\n  - [fixture]\n", "each `sources` entry is a name"),
    ] {
        let e = Env::new(deps);
        e.dre(&["validate", "--no-auto-install"]).failed().says(msg);
    }
    let e = Env::new("sources:\n  - {name: fixture, github: a/b}\n");
    e.write(
        "project/packages.yml",
        "sources:\n  - {name: fixture, local: bin/x}\n",
    );
    e.dre(&["validate", "--no-auto-install"])
        .failed()
        .says("declared with two sources");
}

#[test]
fn a_run_notices_a_changed_source_without_dre_deps() {
    // First from a registry, then the entry moves to GitHub: `dre run` must reinstall, not keep
    // using the registry's copy.
    let e = Env::new("sources:\n  - fixture\nformats:\n  - csv\n");
    let reg = e.p("registry");
    std::fs::create_dir_all(&reg).unwrap();
    let bin = fixture_bin();
    std::fs::write(reg.join("fixture-1.1.0"), &bin).unwrap();
    let index = serde_json::json!({"schema": 1, "plugins": [{"kind": "source", "name": "fixture", "versions": [
        {"version": "1.1.0", "protocol": 0, "artifacts": {platform(): {"url": reg.join("fixture-1.1.0").to_string_lossy(), "sha256": sha(&bin)}}}
    ]}]});
    std::fs::write(reg.join("index.json"), index.to_string()).unwrap();
    let mut c = assert_cmd::Command::cargo_bin("dre").unwrap();
    c.args(["deps"])
        .current_dir(e.p("project"))
        .env("DRE_PLUGINS_DIR", e.p("plugins"))
        .env("DRE_REGISTRY_URL", reg.join("index.json"))
        .env("HOME", e.p("home"));
    assert!(c.output().unwrap().status.success());
    assert!(!e.lock().contains("from:"), "{}", e.lock());

    e.server.github_releases(&bin, true);
    e.write("project/dependencies.yml", GITHUB);
    e.dre(&["run"])
        .ok()
        .says("Installed")
        .says("source plugin `fixture` 1.1.0");
    assert!(
        e.lock().contains("from: github:acme/dre-source-fixture"),
        "{}",
        e.lock()
    );
}
