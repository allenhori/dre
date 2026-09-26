//! The plugin manager against a local registry: deps, lockfile pins, checksums, auto-install,
//! install/update/remove.

mod common;

use std::path::PathBuf;

use common::{Run, test_plugins};
use sha2::{Digest, Sha256};

struct Env {
    dir: tempfile::TempDir,
}

fn sha(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

fn platform() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

impl Env {
    /// A registry with source/fixture 1.0.0 and 1.1.0 (raw executables) and 1.2.0-rc.1
    /// (tar.gz, excluded by default constraints as a pre-release), plus a project using it.
    fn new(plugins_yml: &str) -> Env {
        let dir = tempfile::tempdir().unwrap();
        let reg = dir.path().join("registry");
        std::fs::create_dir_all(&reg).unwrap();
        let exe_name = format!("dre-source-fixture{}", std::env::consts::EXE_SUFFIX);
        let bin = std::fs::read(test_plugins(&["dre-source-fixture"]).join(&exe_name)).unwrap();
        std::fs::write(reg.join("fixture-1.0.0"), &bin).unwrap();
        std::fs::write(reg.join("fixture-1.1.0"), &bin).unwrap();
        let tgz = {
            let mut b = tar::Builder::new(flate2::write::GzEncoder::new(
                Vec::new(),
                flate2::Compression::fast(),
            ));
            let mut h = tar::Header::new_gnu();
            h.set_size(bin.len() as u64);
            h.set_mode(0o755);
            h.set_cksum();
            b.append_data(&mut h, &exe_name, bin.as_slice()).unwrap();
            b.into_inner().unwrap().finish().unwrap()
        };
        std::fs::write(reg.join("fixture-1.2.0-rc.1.tar.gz"), &tgz).unwrap();
        let art = |file: &str, bytes: &[u8]| serde_json::json!({ platform(): {"url": reg.join(file).to_string_lossy(), "sha256": sha(bytes)} });
        let index = serde_json::json!({
            "schema": 1,
            "plugins": [{
                "kind": "source", "name": "fixture", "description": "test plugin",
                "versions": [
                    {"version": "1.0.0", "protocol": 0, "artifacts": art("fixture-1.0.0", &bin)},
                    {"version": "1.1.0", "protocol": 0, "artifacts": art("fixture-1.1.0", &bin)},
                    {"version": "1.2.0-rc.1", "protocol": 0, "artifacts": art("fixture-1.2.0-rc.1.tar.gz", &tgz)},
                ]
            }]
        });
        std::fs::write(
            reg.join("index.json"),
            serde_json::to_string_pretty(&index).unwrap(),
        )
        .unwrap();

        // The csv format is placed by hand (flat), so only the fixture comes from the registry.
        let plugins = dir.path().join("plugins");
        std::fs::create_dir_all(&plugins).unwrap();
        let csv = format!("dre-format-csv{}", std::env::consts::EXE_SUFFIX);
        std::fs::copy(test_plugins(&["dre-format-csv"]).join(&csv), plugins.join(&csv)).unwrap();

        let project = dir.path().join("project");
        std::fs::create_dir_all(project.join("reports/ops/f")).unwrap();
        std::fs::write(
            project.join("dre_project.yml"),
            "name: acme_reports\ndefault_profile: fx\n",
        )
        .unwrap();
        std::fs::write(project.join("plugins.yml"), plugins_yml).unwrap();
        std::fs::write(project.join("reports/ops/f/f.yml"), "queries: [fq]\n").unwrap();
        std::fs::write(project.join("reports/ops/f/fq.sql"), "rows 3").unwrap();
        std::fs::create_dir_all(dir.path().join("profiles")).unwrap();
        std::fs::write(
            dir.path().join("profiles/profiles.yml"),
            "fx:\n  target: dev\n  outputs:\n    dev: {type: fixture}\n",
        )
        .unwrap();
        Env { dir }
    }

    fn p(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }

    fn dre(&self, args: &[&str]) -> Run {
        let mut c = assert_cmd::Command::cargo_bin("dre").unwrap();
        c.args(args)
            .current_dir(self.p("project"))
            .env("DRE_PLUGINS_DIR", self.p("plugins"))
            .env("DRE_REGISTRY_URL", self.p("registry/index.json"))
            .env("DRE_PROFILES_DIR", self.p("profiles"))
            .env("HOME", self.p("home"));
        let out = c.output().unwrap();
        Run {
            code: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into(),
            stderr: String::from_utf8_lossy(&out.stderr).into(),
        }
    }

    fn installed(&self, version: &str) -> bool {
        let exe = format!("dre-source-fixture{}", std::env::consts::EXE_SUFFIX);
        self.p("plugins/source/fixture").join(version).join(exe).exists()
    }

    fn lock(&self) -> String {
        std::fs::read_to_string(self.p("project/dre.lock")).unwrap_or_default()
    }
}

const DECLARED: &str = "sources:\n  - fixture: \">=1.0\"\nformats:\n  - csv\n";

#[test]
fn deps_installs_the_highest_matching_version_and_pins_it() {
    let e = Env::new(DECLARED);
    e.dre(&["deps"])
        .ok()
        .says("Installed")
        .says("source plugin `fixture` 1.1.0");
    assert!(e.installed("1.1.0") && !e.installed("1.0.0"));
    let lock = e.lock();
    assert!(lock.starts_with("# Generated by DRE"), "{lock}");
    assert!(
        lock.contains("sources:\n  fixture:\n    version: 1.1.0\n    sha256: "),
        "{lock}"
    );
    // Nothing to do the second time.
    let r = e.dre(&["deps"]);
    r.ok();
    assert!(!r.stdout.contains("Installed"), "{}", r.stdout);
}

#[test]
fn a_lockfile_pins_the_exact_version_on_another_machine() {
    let e = Env::new(DECLARED);
    e.dre(&["deps"]).ok();
    let lock = e.lock().replace("1.1.0", "1.0.0");
    // The pinned version's own checksum (the same binary here).
    std::fs::write(e.p("project/dre.lock"), &lock).unwrap();
    std::fs::remove_dir_all(e.p("plugins/source")).unwrap();
    e.dre(&["deps"]).ok().says("fixture` 1.0.0");
    assert!(e.installed("1.0.0") && !e.installed("1.1.0"));
}

#[test]
fn a_checksum_mismatch_aborts_the_install() {
    let e = Env::new(DECLARED);
    std::fs::write(e.p("registry/fixture-1.1.0"), b"tampered").unwrap();
    e.dre(&["deps"])
        .failed()
        .says("checksum mismatch")
        .says("discarded");
    assert!(!e.installed("1.1.0"));
    assert!(e.lock().is_empty());
}

#[test]
fn run_auto_installs_declared_plugins_and_says_so() {
    let e = Env::new(DECLARED);
    e.dre(&["run", "f"]).ok().says("Installed").says("Succeeded");
    assert!(e.installed("1.1.0"));
    assert_eq!(
        std::fs::read_to_string(e.p("project/target/run/f/default/f.csv")).unwrap(),
        "n\r\n0\r\n1\r\n2\r\n"
    );
}

#[test]
fn no_auto_install_makes_a_missing_plugin_a_hard_failure_for_run() {
    let e = Env::new(DECLARED);
    e.dre(&["run", "f", "--no-auto-install"])
        .failed()
        .says("isn't installed and auto-install is off; run `dre deps`");
    assert!(!e.installed("1.1.0"));
    // validate reports it as a warning rather than installing.
    e.dre(&["validate", "--no-auto-install"])
        .ok()
        .says("plugin-not-installed");
}

#[test]
fn install_update_and_remove_keep_dre_lock_in_step() {
    let e = Env::new(DECLARED);
    e.dre(&["plugin", "install", "fixture@=1.0.0"])
        .ok()
        .says("fixture` 1.0.0")
        .says("dre.lock pins `fixture` to 1.0.0");
    assert!(e.lock().contains("version: 1.0.0"));
    // Without a version, install respects the pin.
    e.dre(&["plugin", "install", "source/fixture"])
        .ok()
        .says("fixture` 1.0.0");
    // update moves to the newest allowed version and re-pins.
    e.dre(&["plugin", "update", "fixture"])
        .ok()
        .says("fixture` 1.1.0");
    assert!(e.lock().contains("version: 1.1.0"));
    // A pre-release is only installed when asked for explicitly (and comes from a tar.gz).
    e.dre(&["plugin", "install", "fixture@=1.2.0-rc.1"]).ok();
    assert!(e.installed("1.2.0-rc.1"));
    // Remove one version, then everything.
    e.dre(&["plugin", "remove", "fixture@1.0.0"]).ok().says("Removed");
    assert!(!e.installed("1.0.0") && e.installed("1.1.0"));
    e.dre(&["plugin", "remove", "source/fixture"]).ok();
    assert!(!e.installed("1.1.0"));
    assert!(!e.lock().contains("fixture"), "{}", e.lock());
    // A constraint that contradicts the declaration is refused.
    e.dre(&["plugin", "install", "fixture@<1.0"])
        .failed()
        .says("contradicts the project's declared constraint");
}
