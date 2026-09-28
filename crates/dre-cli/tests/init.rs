//! `dre init` (scripted answers on stdin) and `dre new`.

mod common;

use std::path::Path;

use common::test_plugins;
use sha2::{Digest, Sha256};

fn registry(dir: &Path) {
    let exe = format!("dre-source-fixture{}", std::env::consts::EXE_SUFFIX);
    let bin = std::fs::read(test_plugins(&["dre-source-fixture"]).join(&exe)).unwrap();
    let reg = dir.join("registry");
    std::fs::create_dir_all(&reg).unwrap();
    std::fs::write(reg.join("fixture"), &bin).unwrap();
    let sha: String = Sha256::digest(&bin).iter().map(|b| format!("{b:02x}")).collect();
    let plat = format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH);
    let art = serde_json::json!({ plat: {"url": reg.join("fixture").to_string_lossy(), "sha256": sha} });
    // One package, `fixture`, with a source and a destination (`inbox`).
    let index = serde_json::json!({"schema": 2, "plugins": [
        {"name": "fixture", "description": "a test package",
         "provides": ["source/fixture", "destination/inbox"],
         "versions": [{"version": "1.0.0", "protocol": 0, "artifacts": art}]},
    ]});
    std::fs::write(reg.join("index.json"), index.to_string()).unwrap();
}

fn dre(dir: &Path, args: &[&str], stdin: &str) -> common::Run {
    dre_in(dir, dir, args, stdin)
}

/// Run with `dir`'s plugins/registry/profiles, from working directory `cwd`.
fn dre_in(dir: &Path, cwd: &Path, args: &[&str], stdin: &str) -> common::Run {
    let out = assert_cmd::Command::cargo_bin("dre")
        .unwrap()
        .args(args)
        .current_dir(cwd)
        .env("DRE_PLUGINS_DIR", dir.join("plugins"))
        .env("DRE_REGISTRY_URL", dir.join("registry/index.json"))
        .env("DRE_PROFILES_DIR", dir.join("dot-dre"))
        .env("HOME", dir.join("home"))
        .write_stdin(stdin)
        .output()
        .unwrap();
    common::Run {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into(),
        stderr: String::from_utf8_lossy(&out.stderr).into(),
    }
}

#[test]
fn init_installs_the_source_writes_profiles_and_scaffolds_a_project() {
    let d = tempfile::tempdir().unwrap();
    registry(d.path());
    // source: fixture; profile name; target; path (required); token (secret → accept env_var);
    // destinations: inbox; its profile name; its path; its token; scaffold: yes; directory.
    let answers = "fixture\nwarehouse\n\ndata.duckdb\n\n1\n\nout\n\ny\nmy_reports\n";
    let r = dre(d.path(), &["init"], answers);
    r.ok()
        .says("Installed")
        .says("plugin package `fixture` 1.0.0")
        .says("Created");
    assert_eq!(
        r.stderr.matches("Installed").count() + r.stdout.matches("Installed").count(),
        1,
        "installed once: {}",
        r.stderr
    );

    let profiles = std::fs::read_to_string(d.path().join("dot-dre/profiles.yml")).unwrap();
    assert_eq!(
        profiles,
        "sources:\n  warehouse:\n    target: dev\n    targets:\n      dev:\n        type: fixture\n        path: data.duckdb\n        token: '{{ env_var(''WAREHOUSE_TOKEN'') }}'\n\n\
         destinations:\n  inbox_out:\n    target: dev\n    targets:\n      dev:\n        type: inbox\n        path: out\n        token: '{{ env_var(''INBOX_OUT_TOKEN'') }}'\n"
    );
    let p = d.path().join("my_reports");
    for f in [
        "dre_project.yml",
        "dependencies.yml",
        "reports/examples/hello/hello.yml",
        "reports/examples/hello/hello.sql",
        ".gitignore",
    ] {
        assert!(p.join(f).exists(), "{f} missing");
    }
    assert_eq!(
        std::fs::read_to_string(p.join(".gitignore")).unwrap(),
        "target/\nlogs/\ndre_deps/\n"
    );
    let plugins = std::fs::read_to_string(p.join("dependencies.yml")).unwrap();
    assert!(plugins.ends_with("plugins:\n  - fixture\n  - csv\n"), "{plugins}");
    assert!(
        std::fs::read_to_string(p.join("dre_project.yml"))
            .unwrap()
            .contains("default_profile: warehouse")
    );
    // The scaffold is a valid project against the profiles init wrote.
    let v = dre_in(d.path(), &p, &["validate", "--no-auto-install"], "");
    v.ok();
}

#[test]
fn init_offers_the_source_values_to_a_destination_on_the_same_platform() {
    let d = tempfile::tempdir().unwrap();
    registry(d.path());
    // Profile `my-wh`; the destination's `path` is left blank, so it takes the source's.
    let answers = "fixture\nmy-wh\n\ndata.duckdb\n\n1\n\n\n\nn\n";
    let r = dre(d.path(), &["init"], answers);
    r.ok().says("Enter keeps the value from `my-wh`");
    let profiles = std::fs::read_to_string(d.path().join("dot-dre/profiles.yml")).unwrap();
    assert_eq!(
        profiles,
        "sources:\n  my-wh:\n    target: dev\n    targets:\n      dev:\n        type: fixture\n        path: data.duckdb\n        token: '{{ env_var(''MY_WH_TOKEN'') }}'\n\n\
         destinations:\n  inbox_out:\n    target: dev\n    targets:\n      dev:\n        type: inbox\n        path: data.duckdb\n        token: '{{ env_var(''INBOX_OUT_TOKEN'') }}'\n"
    );
}

#[test]
fn init_refuses_to_overwrite_an_existing_profile() {
    let d = tempfile::tempdir().unwrap();
    registry(d.path());
    std::fs::create_dir_all(d.path().join("dot-dre")).unwrap();
    std::fs::write(
        d.path().join("dot-dre/profiles.yml"),
        "sources:\n  warehouse:\n    target: dev\n    targets:\n      dev: {type: duckdb}\n",
    )
    .unwrap();
    dre(d.path(), &["init"], "fixture\nwarehouse\n\ndata.duckdb\n\n\nn\n")
        .failed()
        .says("source profile `warehouse` already exists");
}

#[test]
fn init_adds_to_the_right_section_of_an_existing_file_keeping_comments() {
    let d = tempfile::tempdir().unwrap();
    registry(d.path());
    std::fs::create_dir_all(d.path().join("dot-dre")).unwrap();
    std::fs::write(
        d.path().join("dot-dre/profiles.yml"),
        "# my connections\nsources:\n  old:  # keep me\n    target: dev\n    targets:\n      dev: {type: duckdb}\n\n\
         destinations:\n  box:\n    target: dev\n    targets:\n      dev: {type: local}\n",
    )
    .unwrap();
    // A destination profile may share a source profile's name: each section is its own namespace.
    dre(
        d.path(),
        &["init"],
        "fixture\nnew\n\ndata.duckdb\n\n1\nold\n\n\nn\n",
    )
    .ok();
    assert_eq!(
        std::fs::read_to_string(d.path().join("dot-dre/profiles.yml")).unwrap(),
        "# my connections\nsources:\n  old:  # keep me\n    target: dev\n    targets:\n      dev: {type: duckdb}\n\n\
         \x20 new:\n    target: dev\n    targets:\n      dev:\n        type: fixture\n        path: data.duckdb\n        token: '{{ env_var(''NEW_TOKEN'') }}'\n\n\
         destinations:\n  box:\n    target: dev\n    targets:\n      dev: {type: local}\n\n\
         \x20 old:\n    target: dev\n    targets:\n      dev:\n        type: inbox\n        path: data.duckdb\n        token: '{{ env_var(''OLD_TOKEN'') }}'\n"
    );
}

#[test]
fn new_scaffolds_without_prompts_and_never_overwrites() {
    let d = tempfile::tempdir().unwrap();
    dre(d.path(), &["new", "acme reports"], "").ok().says("Created");
    let p = d.path().join("acme reports");
    assert_eq!(
        std::fs::read_to_string(p.join("dre_project.yml"))
            .unwrap()
            .lines()
            .next(),
        Some("name: acme_reports")
    );
    assert!(
        std::fs::read_to_string(p.join("dependencies.yml"))
            .unwrap()
            .contains("  - duckdb\n")
    );
    dre(d.path(), &["new", "acme reports"], "")
        .failed()
        .says("isn't empty");
}

#[test]
fn a_new_project_runs_end_to_end() {
    let d = tempfile::tempdir().unwrap();
    dre(d.path(), &["new", "proj"], "").ok();
    std::fs::create_dir_all(d.path().join("dot-dre")).unwrap();
    std::fs::write(
        d.path().join("dot-dre/profiles.yml"),
        "sources:\n  warehouse:\n    target: dev\n    targets:\n      dev: {type: duckdb, path: dev.duckdb}\n",
    )
    .unwrap();
    let p = d.path().join("proj");
    let out = assert_cmd::Command::cargo_bin("dre")
        .unwrap()
        .args(["run"])
        .current_dir(&p)
        .env("DRE_PLUGINS_DIR", test_plugins(common::ALL_PLUGINS))
        .env("DRE_PROFILES_DIR", d.path().join("dot-dre"))
        .env("DRE_RUN_DATE", "2026-01-25")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stdout));
    assert_eq!(
        std::fs::read_to_string(p.join("target/run/hello/default/hello.csv")).unwrap(),
        "report,run_date,message\r\nhello,2026-01-25,Hello from DRE\r\n"
    );
}
