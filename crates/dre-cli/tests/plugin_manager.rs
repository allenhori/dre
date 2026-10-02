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
    /// A registry with the `fixture` package 1.0.0 and 1.1.0 (raw executables) and 1.2.0-rc.1
    /// (tar.gz, excluded by default constraints as a pre-release), plus a project using it. The
    /// index is in schema 1 (one plugin per entry), as a third-party registry may still be, and
    /// the executable is a single plugin's `dre-source-fixture`.
    fn new(plugins_yml: &str) -> Env {
        let dir = tempfile::tempdir().unwrap();
        let reg = dir.path().join("registry");
        std::fs::create_dir_all(&reg).unwrap();
        let exe_name = format!("dre-source-fixture{}", std::env::consts::EXE_SUFFIX);
        let bin = std::fs::read(test_plugins(&["dre-source-fixture"]).join(&exe_name)).unwrap();
        std::fs::write(reg.join("fixture-1.0.0"), &bin).unwrap();
        std::fs::write(reg.join("fixture-1.1.0"), &bin).unwrap();
        // Stored, not compressed: still a .tar.gz DRE must unpack, without spending seconds per
        // test compressing a debug binary.
        let tgz = {
            let mut b = tar::Builder::new(flate2::write::GzEncoder::new(
                Vec::new(),
                flate2::Compression::none(),
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

        // The csv package is placed by hand (flat), so only the fixture comes from the registry.
        let plugins = dir.path().join("plugins");
        std::fs::create_dir_all(&plugins).unwrap();
        let csv = format!("dre-plugin-csv{}", std::env::consts::EXE_SUFFIX);
        std::fs::copy(test_plugins(&["dre-plugin-csv"]).join(&csv), plugins.join(&csv)).unwrap();

        let project = dir.path().join("project");
        std::fs::create_dir_all(project.join("reports/ops/f")).unwrap();
        std::fs::write(
            project.join("dre_project.yml"),
            "name: acme_reports\ndefault_profile: fx\n",
        )
        .unwrap();
        std::fs::write(project.join("dependencies.yml"), plugins_yml).unwrap();
        std::fs::write(project.join("reports/ops/f/f.yml"), "queries: [fq]\n").unwrap();
        std::fs::write(project.join("reports/ops/f/fq.sql"), "rows 3").unwrap();
        std::fs::create_dir_all(dir.path().join("profiles")).unwrap();
        std::fs::write(
            dir.path().join("profiles/profiles.yml"),
            "sources:\n  fx:\n    target: dev\n    targets:\n      dev: {type: fixture}\n",
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
        self.p("plugins/fixture").join(version).join(exe).exists()
    }

    fn lock(&self) -> String {
        std::fs::read_to_string(self.p("project/dre.lock")).unwrap_or_default()
    }
}

const DECLARED: &str = "plugins:\n  - fixture: \">=1.0\"\n  - csv\n";

#[test]
fn deps_installs_the_highest_matching_version_and_pins_it() {
    let e = Env::new(DECLARED);
    e.dre(&["deps"])
        .ok()
        .says("Installed")
        .says("plugin package `fixture` 1.1.0");
    assert!(e.installed("1.1.0") && !e.installed("1.0.0"));
    let lock = e.lock();
    assert!(lock.starts_with("# Generated by DRE"), "{lock}");
    assert!(
        lock.contains(&format!(
            "plugins:\n  fixture:\n    version: 1.1.0\n    sha256:\n      {}: ",
            platform()
        )),
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
    std::fs::remove_dir_all(e.p("plugins/fixture")).unwrap();
    e.dre(&["deps"]).ok().says("fixture` 1.0.0");
    assert!(e.installed("1.0.0") && !e.installed("1.1.0"));
}

#[test]
fn deps_without_a_lockfile_resolves_the_newest_allowed_version_again() {
    let e = Env::new(DECLARED);
    e.dre(&["plugin", "install", "fixture@=1.0.0"]).ok();
    assert!(e.installed("1.0.0") && !e.installed("1.1.0"));
    std::fs::remove_file(e.p("project/dre.lock")).unwrap();
    e.dre(&["deps"])
        .ok()
        .says("plugin package `fixture` 1.1.0")
        .says("dre.lock is up to date");
    assert!(e.installed("1.1.0"));
    assert!(e.lock().contains("version: 1.1.0"), "{}", e.lock());
}

#[test]
fn deps_without_a_lockfile_pins_an_installed_newest_version_without_reinstalling() {
    let e = Env::new(DECLARED);
    e.dre(&["deps"]).ok();
    let before = e.lock();
    std::fs::remove_file(e.p("project/dre.lock")).unwrap();
    let r = e.dre(&["deps"]);
    r.ok();
    assert!(!r.stdout.contains("Installed"), "{}", r.stdout);
    assert_eq!(e.lock(), before);
}

#[test]
fn deps_without_a_lockfile_reinstalls_an_installed_file_that_differs_from_the_registry() {
    let e = Env::new(DECLARED);
    e.dre(&["deps"]).ok();
    let before = e.lock();
    // The same version rebuilt locally: not the registry's artifact any more.
    let exe = e
        .p("plugins/fixture/1.1.0")
        .join(format!("dre-source-fixture{}", std::env::consts::EXE_SUFFIX));
    let original = std::fs::read(&exe).unwrap();
    let mut changed = original.clone();
    changed.extend_from_slice(b"rebuilt");
    std::fs::remove_file(&exe).unwrap();
    std::fs::write(&exe, &changed).unwrap();
    std::fs::remove_file(e.p("project/dre.lock")).unwrap();
    e.dre(&["deps"]).ok().says("plugin package `fixture` 1.1.0");
    assert_eq!(
        std::fs::read(&exe).unwrap(),
        original,
        "the registry's file is back"
    );
    assert_eq!(e.lock(), before);
}

/// Forget every installed copy, so the next `deps` downloads from the registry.
fn uninstall_everything(e: &Env) {
    std::fs::remove_dir_all(e.p("plugins/fixture")).unwrap();
    let _ = std::fs::remove_dir_all(e.p("home"));
}

#[test]
fn a_lockfile_written_on_another_platform_installs_here_and_learns_this_platform() {
    let e = Env::new(DECLARED);
    e.dre(&["deps"]).ok();
    let here = e.lock();
    // As if written on another machine: only that platform's checksum.
    let other = here.replace(&format!("{}: ", platform()), "otheros-arch: ");
    std::fs::write(e.p("project/dre.lock"), &other).unwrap();
    uninstall_everything(&e);
    e.dre(&["deps"]).ok().says("fixture` 1.1.0");
    assert!(e.installed("1.1.0"));
    let lock = e.lock();
    assert!(
        lock.contains("otheros-arch: ") && lock.contains(&format!("{}: ", platform())),
        "{lock}"
    );
}

#[test]
fn a_lockfile_from_before_per_platform_checksums_still_works() {
    let e = Env::new(DECLARED);
    e.dre(&["deps"]).ok();
    let sha = e
        .lock()
        .lines()
        .find_map(|l| {
            l.trim()
                .strip_prefix(&format!("{}: ", platform()))
                .map(str::to_string)
        })
        .unwrap();
    let old = |s: &str| format!("plugins:\n  fixture:\n    version: 1.1.0\n    sha256: {s}\n");
    // Its one checksum is one the registry publishes: installed, and rewritten per platform.
    std::fs::write(e.p("project/dre.lock"), old(&sha)).unwrap();
    uninstall_everything(&e);
    e.dre(&["deps"]).ok();
    assert!(e.installed("1.1.0"));
    assert!(
        e.lock().contains(&format!("{}: {sha}", platform())),
        "{}",
        e.lock()
    );
    // One the registry doesn't publish for any platform: refused.
    std::fs::write(e.p("project/dre.lock"), old(&"0".repeat(64))).unwrap();
    uninstall_everything(&e);
    e.dre(&["deps"])
        .failed()
        .says("the registry's checksum doesn't match dre.lock");
    assert!(!e.installed("1.1.0"));
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
fn an_undeclared_plugin_is_looked_up_in_the_registry() {
    // In the registry: declare it and run `dre deps`.
    let e = Env::new("plugins:\n  - csv\n");
    e.dre(&["validate"])
        .failed()
        .says("add `fixture` under `plugins:` in dependencies.yml, then run `dre deps`");
    // Not in the registry at all, or there as another kind.
    std::fs::write(
        e.p("project/reports/ops/f/f.yml"),
        "queries: [fq]\noutput: {format: fixture}\n",
    )
    .unwrap();
    std::fs::write(e.p("project/dependencies.yml"), DECLARED).unwrap();
    e.dre(&["validate"])
        .failed()
        .says("`fixture` in DRE's plugin registry is a source plugin, not a format");
    std::fs::write(
        e.p("project/reports/ops/f/f.yml"),
        "queries: [fq]\noutput: {format: xslx}\n",
    )
    .unwrap();
    e.dre(&["validate"])
        .failed()
        .says("DRE's plugin registry has no format plugin called `xslx`; check the spelling");
    // Offline, the registry isn't asked.
    e.dre(&["validate", "--no-auto-install"]).failed().says(
        "add the package with the `xslx` format under `plugins:` in dependencies.yml, then run `dre deps`",
    );
}

#[test]
fn format_options_are_checked_by_the_plugin_and_apply_under_every_output_of_that_format() {
    let e = Env::new(DECLARED);
    std::fs::write(
        e.p("project/dre_project.yml"),
        "name: acme_reports\ndefault_profile: fx\nformat_options:\n  csv: {delimiter: \"|\", quoting: all}\n",
    )
    .unwrap();
    e.dre(&["run", "f"]).ok();
    assert_eq!(
        std::fs::read_to_string(e.p("project/target/run/f/default/f.csv")).unwrap(),
        "\"n\"\r\n\"0\"\r\n\"1\"\r\n\"2\"\r\n"
    );
    // A report's own keys win.
    std::fs::write(
        e.p("project/reports/ops/f/f.yml"),
        "queries: [fq]\noutput: {quoting: none}\n",
    )
    .unwrap();
    e.dre(&["run", "f"]).ok();
    assert_eq!(
        std::fs::read_to_string(e.p("project/target/run/f/default/f.csv")).unwrap(),
        "n\r\n0\r\n1\r\n2\r\n"
    );
    // The plugin checks the project-wide block too, and a run refuses to start.
    std::fs::write(
        e.p("project/dre_project.yml"),
        "name: acme_reports\ndefault_profile: fx\nformat_options:\n  csv: {delimiter: \"||\", encoding: klingon}\n",
    )
    .unwrap();
    e.dre(&["validate"])
        .failed()
        .says("dre_project.yml: `format_options.csv`: `delimiter` must be a single character");
    e.dre(&["run", "f"]).failed().says("fix them before running");
    std::fs::write(
        e.p("project/dre_project.yml"),
        "name: acme_reports\ndefault_profile: fx\nformat_options:\n  csv: {encoding: klingon}\n",
    )
    .unwrap();
    e.dre(&["validate"])
        .failed()
        .says("unknown or unsupported `encoding` \"klingon\"");
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
    e.dre(&["plugin", "install", "fixture"])
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
    e.dre(&["plugin", "remove", "fixture"]).ok();
    assert!(!e.installed("1.1.0"));
    assert!(!e.lock().contains("fixture"), "{}", e.lock());
    // A constraint that contradicts the declaration is refused.
    e.dre(&["plugin", "install", "fixture@<1.0"])
        .failed()
        .says("contradicts the project's declared constraint");
}

impl Env {
    /// Like `dre`, without `DRE_PLUGINS_DIR`: plugins go to the project's dre_deps/, via the
    /// shared cache in $HOME/.dre/plugins.
    fn dre_in_project(&self, args: &[&str], registry: &str) -> Run {
        let mut c = assert_cmd::Command::cargo_bin("dre").unwrap();
        c.args(args)
            .current_dir(self.p("project"))
            .env_remove("DRE_PLUGINS_DIR")
            .env("DRE_REGISTRY_URL", self.p(registry))
            .env("DRE_PROFILES_DIR", self.p("profiles"))
            // Windows finds the home folder through USERPROFILE.
            .env("HOME", self.p("home"))
            .env("USERPROFILE", self.p("home"));
        let out = c.output().unwrap();
        Run {
            code: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into(),
            stderr: String::from_utf8_lossy(&out.stderr).into(),
        }
    }
}

#[test]
fn plugins_install_into_dre_deps_linked_from_the_shared_cache() {
    let e = Env::new(DECLARED);
    // The hand-placed csv plugin goes in the project's own plugins folder.
    let csv = format!("dre-plugin-csv{}", std::env::consts::EXE_SUFFIX);
    std::fs::create_dir_all(e.p("project/dre_deps/plugins")).unwrap();
    std::fs::copy(
        e.p("plugins").join(&csv),
        e.p("project/dre_deps/plugins").join(&csv),
    )
    .unwrap();

    e.dre_in_project(&["deps"], "registry/index.json")
        .ok()
        .says("plugin package `fixture` 1.1.0");
    let exe = format!("dre-source-fixture{}", std::env::consts::EXE_SUFFIX);
    let in_project = e.p("project/dre_deps/plugins/fixture/1.1.0").join(&exe);
    let in_cache = e.p("home/.dre/plugins/fixture/1.1.0").join(&exe);
    assert!(in_project.is_file() && in_cache.is_file());
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let (a, b) = (
            std::fs::metadata(&in_project).unwrap(),
            std::fs::metadata(&in_cache).unwrap(),
        );
        assert_eq!((a.dev(), a.ino()), (b.dev(), b.ino()), "hard-linked, not copied");
    }
    e.dre_in_project(&["run", "f"], "registry/index.json").ok();

    // A fresh checkout with the same dre.lock links the pinned version from the cache, with no
    // registry at all.
    std::fs::remove_dir_all(e.p("project/dre_deps/plugins/fixture")).unwrap();
    e.dre_in_project(&["deps"], "no-registry/index.json").ok();
    assert!(in_project.is_file());
}

/// A package of several plugins, in the current index schema: one download, one lock entry, and
/// every plugin it provides usable.
#[test]
fn a_package_installs_once_and_provides_all_its_plugins() {
    let e = Env::new("plugins:\n  - fixture\n  - csv\n");
    let exe_name = format!("dre-plugin-csv{}", std::env::consts::EXE_SUFFIX);
    std::fs::remove_file(e.p("plugins").join(&exe_name)).unwrap();
    let bin = std::fs::read(test_plugins(&["dre-plugin-csv"]).join(&exe_name)).unwrap();
    let tgz = {
        let mut b = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::none(),
        ));
        let mut h = tar::Header::new_gnu();
        h.set_size(bin.len() as u64);
        h.set_mode(0o755);
        h.set_cksum();
        b.append_data(&mut h, &exe_name, bin.as_slice()).unwrap();
        b.into_inner().unwrap().finish().unwrap()
    };
    std::fs::write(e.p("registry/csv-1.0.0.tar.gz"), &tgz).unwrap();
    let mut index: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(e.p("registry/index.json")).unwrap()).unwrap();
    index["schema"] = 2.into();
    index["plugins"].as_array_mut().unwrap().push(serde_json::json!({
        "name": "csv",
        "description": "csv and delimited",
        "provides": ["format/csv", "format/delimited"],
        "versions": [{"version": "1.0.0", "protocol": 0, "artifacts": {
            platform(): {"url": e.p("registry/csv-1.0.0.tar.gz").to_string_lossy(), "sha256": sha(&tgz)}
        }}],
    }));
    std::fs::write(e.p("registry/index.json"), index.to_string()).unwrap();
    std::fs::write(
        e.p("project/reports/ops/f/f.yml"),
        "queries: [fq]\noutput: {format: delimited, delimiter: \"|\"}\n",
    )
    .unwrap();

    e.dre(&["run", "f"])
        .ok()
        .says("plugin package `csv` 1.0.0")
        .says("Succeeded");
    assert!(e.p("plugins/csv/1.0.0").join(&exe_name).is_file());
    assert!(
        e.lock()
            .contains("provides:\n    - format/csv\n    - format/delimited\n"),
        "{}",
        e.lock()
    );
    e.dre(&["plugin", "list"])
        .ok()
        .says("format/csv, format/delimited");

    // Offline, what a package provides comes from the install and dre.lock.
    e.dre(&["validate", "--no-auto-install"]).ok();
    std::fs::write(
        e.p("project/reports/ops/f/f.yml"),
        "queries: [fq]\noutput: {format: xlsx}\n",
    )
    .unwrap();
    e.dre(&["validate", "--no-auto-install"])
        .failed()
        .says("format `xlsx` is used by")
        .says("no plugin package the project declares provides it");

    // Asking for a plugin by name points at its package.
    e.dre(&["plugin", "install", "delimited"])
        .failed()
        .says("the `delimited` plugin comes in `csv`");
}

/// The old declaration blocks are an error that says what to write instead.
#[test]
fn sources_formats_and_destinations_blocks_point_at_plugins() {
    let e = Env::new("sources:\n  - fixture\nformats:\n  - csv\n");
    e.dre(&["validate", "--no-auto-install"])
        .failed()
        .says("`sources:` no longer declares plugins: list plugin packages under `plugins:` instead")
        .says("`formats:` no longer declares plugins");
}
