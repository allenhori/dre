//! Shared helpers for CLI tests.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;

/// The cargo target directory holding the `dre` binary under test.
pub fn bin_dir() -> PathBuf {
    assert_cmd::cargo::cargo_bin("dre")
        .parent()
        .unwrap()
        .to_path_buf()
}

/// The workspace package that builds `bin`.
fn package_of(bin: &str) -> &str {
    match bin {
        "dre-source-fixture" => "dre-protocol",
        "dre-format-delimited" => "dre-format-csv",
        b => b,
    }
}

/// Build the packages of `bins` in one cargo invocation.
pub fn build_bins(bins: &[&str]) {
    let mut cmd = Command::new(env!("CARGO"));
    cmd.args(["build", "--quiet", "--bins"])
        .current_dir(env!("CARGO_MANIFEST_DIR"));
    let mut pkgs: Vec<&str> = bins.iter().map(|b| package_of(b)).collect();
    pkgs.dedup();
    for p in pkgs {
        cmd.args(["-p", p]);
    }
    assert!(
        cmd.status().expect("cargo build").success(),
        "building {bins:?} failed"
    );
}

/// Path to a workspace binary, building its package first.
pub fn workspace_bin(bin: &str) -> PathBuf {
    build_bins(&[bin]);
    let path = bin_dir().join(format!("{bin}{}", std::env::consts::EXE_SUFFIX));
    assert!(path.exists(), "{} missing after build", path.display());
    path
}

/// Copy `bin` into `dir` (flat layout) under its own file name.
pub fn place_plugin(dir: &Path, bin: &str) -> PathBuf {
    let src = workspace_bin(bin);
    std::fs::create_dir_all(dir).unwrap();
    let dst = dir.join(src.file_name().unwrap());
    std::fs::copy(&src, &dst).unwrap();
    dst
}

/// A shared plugin directory for run tests holding the given first-party plugins (flat layout).
pub fn test_plugins(bins: &[&str]) -> PathBuf {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = bin_dir().join("test-plugins");
    std::fs::create_dir_all(&dir).unwrap();
    build_bins(bins);
    for bin in bins {
        let src = bin_dir().join(format!("{bin}{}", std::env::consts::EXE_SUFFIX));
        let dst = dir.join(src.file_name().unwrap());
        let stale = match (std::fs::metadata(&src), std::fs::metadata(&dst)) {
            (Ok(s), Ok(d)) => s.modified().unwrap() > d.modified().unwrap(),
            _ => true,
        };
        if stale {
            let tmp = dir.join(format!(".{}.tmp{}", bin, std::process::id()));
            std::fs::copy(&src, &tmp).unwrap();
            std::fs::rename(&tmp, &dst).unwrap();
        }
    }
    dir
}

/// A throwaway DRE project for end-to-end runs.
pub struct TestProject {
    pub dir: tempfile::TempDir,
    pub plugins: PathBuf,
}

pub const ALL_PLUGINS: &[&str] = &["dre-source-duckdb", "dre-format-csv", "dre-format-delimited"];

impl TestProject {
    /// `files` are written under the project root; `profiles.yml` goes to a separate dir.
    pub fn new(files: &[(&str, &str)], profiles: &str) -> TestProject {
        let dir = tempfile::tempdir().unwrap();
        for (rel, content) in files {
            let p = dir.path().join("project").join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, content).unwrap();
        }
        std::fs::create_dir_all(dir.path().join("project")).unwrap();
        std::fs::create_dir_all(dir.path().join("profiles")).unwrap();
        std::fs::write(dir.path().join("profiles/profiles.yml"), profiles).unwrap();
        TestProject {
            dir,
            plugins: test_plugins(ALL_PLUGINS),
        }
    }

    pub fn root(&self) -> PathBuf {
        self.dir.path().join("project")
    }

    pub fn path(&self, rel: &str) -> PathBuf {
        self.root().join(rel)
    }

    pub fn read(&self, rel: &str) -> String {
        std::fs::read_to_string(self.path(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
    }

    pub fn write(&self, rel: &str, content: &str) {
        let p = self.path(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, content).unwrap();
    }

    pub fn json(&self, rel: &str) -> serde_json::Value {
        serde_json::from_str(&self.read(rel)).unwrap()
    }

    /// Create a DuckDB database file in the project by running `sql`.
    pub fn duckdb(&self, rel: &str, sql: &str) {
        let c = duckdb::Connection::open(self.path(rel)).unwrap();
        c.execute_batch(sql).unwrap();
    }

    /// Run `dre <cmd> <args…>` against this project.
    pub fn dre(&self, cmd: &str, args: &[&str]) -> Run {
        let mut c = assert_cmd::Command::cargo_bin("dre").unwrap();
        c.arg(cmd).args(args);
        c.arg("--project-dir").arg(self.root());
        if cmd != "clean" {
            c.arg("--profiles-dir").arg(self.dir.path().join("profiles"));
        }
        c.env("DRE_PLUGINS_DIR", &self.plugins)
            .env("DRE_RUN_DATE", "2026-01-25")
            .env("HOME", self.dir.path().join("home"))
            .env_remove("DRE_PROFILES_DIR");
        let out = c.output().unwrap();
        Run {
            code: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).to_string(),
        }
    }
}

#[derive(Debug)]
pub struct Run {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Run {
    pub fn ok(&self) -> &Self {
        assert_eq!(
            self.code, 0,
            "expected success\nstdout:\n{}\nstderr:\n{}",
            self.stdout, self.stderr
        );
        self
    }

    pub fn failed(&self) -> &Self {
        assert_ne!(
            self.code, 0,
            "expected failure\nstdout:\n{}\nstderr:\n{}",
            self.stdout, self.stderr
        );
        self
    }

    pub fn says(&self, needle: &str) -> &Self {
        assert!(
            self.stderr.contains(needle) || self.stdout.contains(needle),
            "expected output to contain {needle:?}\nstdout:\n{}\nstderr:\n{}",
            self.stdout,
            self.stderr
        );
        self
    }
}

pub const DUCK_PROFILES: &str = "warehouse:\n  target: dev\n  outputs:\n    dev: {type: duckdb, path: data.duckdb}\n    prod: {type: duckdb, path: prod.duckdb}\n";
pub const PLUGINS_YML: &str = "sources:\n  - duckdb\nformats:\n  - csv\n  - delimited\n";
