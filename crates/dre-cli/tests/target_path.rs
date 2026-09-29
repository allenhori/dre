//! A configurable target path (`--target-path`, `DRE_TARGET_PATH`, `target_path:`): where DRE
//! writes compiled SQL, run outputs, schema snapshots, `run_results.json` and the manifest, and
//! a `dre clean` that only deletes folders DRE made.

mod common;

use std::path::{Path, PathBuf};

use common::TestProject;

const PROFILES: &str = "\
sources:
  warehouse:
    target: dev
    targets:
      dev: {type: duckdb, path: data.duckdb}
destinations:
  inbox:
    target: dev
    targets:
      dev: {type: local}
";

fn project() -> TestProject {
    project_with("name: acme_reports\ndefault_profile: warehouse\n")
}

fn project_with(project_yml: &str) -> TestProject {
    let p = TestProject::new(
        &[
            ("dre_project.yml", project_yml),
            ("dependencies.yml", "plugins:\n  - duckdb\n  - csv\n"),
            (
                "reports/fin/s/s.yml",
                "queries: [sq]\noutput:\n  destination: {profile: inbox, path: out/s.csv}\n",
            ),
            (
                "reports/fin/s/sq.sql",
                "select id, name from accounts where id = 1\n",
            ),
        ],
        PROFILES,
    );
    p.duckdb(
        "data.duckdb",
        "create table accounts as select range as id, 'acct ' || range as name from range(3);",
    );
    p
}

/// A folder outside the project.
fn outside(p: &TestProject, name: &str) -> PathBuf {
    p.dir.path().join(name)
}

fn arg(path: &Path) -> String {
    path.display().to_string()
}

#[test]
fn the_default_is_target_in_the_project() {
    let p = project();
    p.dre("run", &["s"]).ok();
    assert!(p.path("target/compiled/s/default/sq.sql").is_file());
    assert!(p.path("target/run/s/default/run_results.json").is_file());
    assert!(p.path("target/manifest.json").is_file());
    let results = p.json("target/run/s/default/run_results.json");
    assert_eq!(results["outputs"][0]["path"], "target/run/s/default/s.csv");
}

#[test]
fn everything_moves_to_the_flags_folder_and_delivery_still_works() {
    let p = project();
    let t = outside(&p, "build/dre");
    let r = p.dre("run", &["s", "--target-path", &arg(&t)]);
    r.ok();
    assert!(t.join("compiled/s/default/sq.sql").is_file());
    assert!(t.join("run/s/default/s.csv").is_file());
    assert!(t.join("run/s/default/run_results.json").is_file());
    assert!(t.join("schema/s/default/last_success.json").is_file());
    assert!(t.join("manifest.json").is_file());
    assert!(!p.path("target").exists(), "nothing lands in target/");
    assert!(p.path("out/s.csv").is_file(), "delivered from the moved folder");

    let results: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(t.join("run/s/default/run_results.json")).unwrap())
            .unwrap();
    assert_eq!(results["outputs"][0]["path"], "run/s/default/s.csv");
    assert_eq!(
        PathBuf::from(results["target_path"].as_str().unwrap()),
        t,
        "the run records the target path it used"
    );
    // Messages show the real path, in full.
    p.dre("validate", &["--target-path", &arg(&t)])
        .ok()
        .says(&arg(&t.join("compiled")).replace('\\', "/"));
}

#[test]
fn drift_history_persists_in_the_moved_folder() {
    let p = project();
    let t = outside(&p, "persistent");
    p.dre("run", &["s", "--target-path", &arg(&t)]).ok();
    p.write(
        "reports/fin/s/sq.sql",
        "select id, 1 as extra from accounts where id = 1\n",
    );
    p.dre("run", &["s", "--target-path", &arg(&t)])
        .failed()
        .says("column `extra` added");
}

#[test]
fn flag_beats_env_beats_project() {
    let p = project_with("name: acme_reports\ndefault_profile: warehouse\ntarget_path: from_project\n");
    p.dre("compile", &[]).ok();
    assert!(p.path("from_project/manifest.json").is_file());

    p.dre_env("compile", &[], &[("DRE_TARGET_PATH", "from_env")]).ok();
    assert!(p.path("from_env/manifest.json").is_file());

    p.dre_env(
        "compile",
        &["--target-path", "from_flag"],
        &[("DRE_TARGET_PATH", "from_env")],
    )
    .ok();
    assert!(p.path("from_flag/manifest.json").is_file());

    // An empty variable counts as unset.
    std::fs::remove_dir_all(p.path("from_project")).unwrap();
    p.dre_env("compile", &[], &[("DRE_TARGET_PATH", "")]).ok();
    assert!(p.path("from_project/manifest.json").is_file());
    assert!(!p.path("target").exists());
}

#[test]
fn a_relative_path_is_relative_to_the_project_not_the_working_directory() {
    // The tests run `dre` from the crate directory with `--project-dir`.
    let p = project();
    p.dre("compile", &["--target-path", "build/out"]).ok();
    assert!(p.path("build/out/manifest.json").is_file());
    assert!(!Path::new("build/out").exists());
}

#[test]
fn a_target_path_inside_the_project_isnt_scanned() {
    let p = project();
    p.dre("compile", &["--target-path", "build/dre"]).ok();
    // A second compile would pick up build/dre/compiled/**/*.sql as unmanaged reports.
    let r = p.dre("ls", &["--target-path", "build/dre"]);
    r.ok();
    assert_eq!(r.stdout.lines().count(), 2, "{}", r.stdout);
    p.dre("validate", &["--target-path", "build/dre"]).ok();
}

#[cfg(unix)]
#[test]
fn a_tilde_is_the_home_directory() {
    let p = project();
    p.dre("compile", &["--target-path", "~/dre-target"]).ok();
    assert!(p.dir.path().join("home/dre-target/manifest.json").is_file());
}

#[test]
fn urls_and_unsafe_folders_are_refused() {
    let p = project();
    for url in [
        "s3://bucket/dre",
        "gs://b/x",
        "abfss://c@acct.dfs.core.windows.net/x",
        "https://example.com/t",
    ] {
        p.dre("compile", &["--target-path", url])
            .failed()
            .says("--target-path")
            .says("local or mounted path")
            .says("Volume");
    }
    let root = arg(&p.root());
    let parent = arg(p.dir.path());
    for (path, why) in [
        (root.as_str(), "project root"),
        (".", "project root"),
        (parent.as_str(), "contains the project"),
        ("reports/out", "inside `reports/`"),
        ("macros", "inside `macros/`"),
        ("lookups/x", "inside `lookups/`"),
    ] {
        p.dre("compile", &["--target-path", path]).failed().says(why);
    }
    p.dre_env("compile", &[], &[("DRE_TARGET_PATH", "s3://bucket")])
        .failed()
        .says("DRE_TARGET_PATH");
    let q = project_with("name: acme_reports\ndefault_profile: warehouse\ntarget_path: reports\n");
    q.dre("validate", &[])
        .failed()
        .says("`target_path` in dre_project.yml");
}

#[test]
fn an_unwritable_path_names_its_source() {
    let p = project();
    p.write("a_file", "not a folder");
    p.dre("compile", &["--target-path", "a_file/sub"])
        .failed()
        .says("a_file")
        .says("--target-path");
    p.dre_env("run", &["s"], &[("DRE_TARGET_PATH", "a_file/sub")])
        .failed()
        .says("DRE_TARGET_PATH");
}

#[test]
fn a_misspelled_key_is_flagged() {
    let p = project_with("name: acme_reports\ndefault_profile: warehouse\ntarget_paht: out\n");
    p.dre("validate", &[]).says("target_paht");
}

// -- dre clean --------------------------------------------------------------------------------

#[test]
fn clean_removes_a_folder_dre_made() {
    let p = project();
    let t = outside(&p, "made_by_dre");
    p.dre("compile", &["--target-path", &arg(&t)]).ok();
    assert!(t.is_dir());
    p.dre("clean", &["--target-path", &arg(&t)]).ok();
    assert!(!t.exists());
    // Missing: a quiet success.
    p.dre("clean", &["--target-path", &arg(&t)])
        .ok()
        .says("Nothing to clean");
}

#[test]
fn clean_follows_the_project_setting_and_the_environment() {
    let p = project_with("name: acme_reports\ndefault_profile: warehouse\ntarget_path: gen\n");
    p.dre("compile", &[]).ok();
    p.dre("clean", &[]).ok();
    assert!(!p.path("gen").exists());

    p.dre_env("compile", &[], &[("DRE_TARGET_PATH", "env_gen")]).ok();
    p.dre_env("clean", &[], &[("DRE_TARGET_PATH", "env_gen")]).ok();
    assert!(!p.path("env_gen").exists());
}

#[test]
fn clean_refuses_a_folder_dre_didnt_make() {
    let p = project();
    let t = outside(&p, "data");
    std::fs::create_dir_all(&t).unwrap();
    std::fs::write(t.join("precious.txt"), "keep me").unwrap();
    p.dre("clean", &["--target-path", &arg(&t)])
        .failed()
        .says(&arg(&t))
        .says("--target-path")
        .says("delete the folder yourself");
    assert_eq!(
        std::fs::read_to_string(t.join("precious.txt")).unwrap(),
        "keep me"
    );

    // Pointing a command at it doesn't make it DRE's either.
    p.dre("compile", &["--target-path", &arg(&t)]).ok();
    p.dre("clean", &["--target-path", &arg(&t)]).failed();
    assert!(t.join("precious.txt").is_file());
}

#[test]
fn clean_refuses_an_existing_empty_folder_dre_wrote_into() {
    let p = project();
    let t = outside(&p, "shared_mount");
    std::fs::create_dir_all(&t).unwrap();
    p.dre("compile", &["--target-path", &arg(&t)]).ok();
    assert!(t.join("manifest.json").is_file());
    p.dre("clean", &["--target-path", &arg(&t)]).failed();
    assert!(t.join("manifest.json").is_file());
}

#[test]
fn clean_still_removes_a_legacy_target_without_the_marker() {
    let p = project();
    std::fs::create_dir_all(p.path("target/compiled")).unwrap();
    std::fs::write(p.path("target/compiled/x.sql"), "select 1").unwrap();
    p.dre("clean", &[]).ok().says("Removed target/");
    assert!(!p.path("target").exists());
}

#[test]
fn the_manifest_is_the_same_wherever_the_target_path_is() {
    let p = project();
    p.dre("compile", &[]).ok();
    let t = outside(&p, "elsewhere");
    p.dre("compile", &["--target-path", &arg(&t)]).ok();
    assert_eq!(
        std::fs::read(t.join("manifest.json")).unwrap(),
        std::fs::read(p.path("target/manifest.json")).unwrap()
    );
}
