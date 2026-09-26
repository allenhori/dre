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

/// Path to a workspace binary, building its package first if it isn't there yet.
/// Plugin packages are named after their binary; the fixture plugin lives in `dre-protocol`.
pub fn workspace_bin(bin: &str) -> PathBuf {
    let path = bin_dir().join(format!("{bin}{}", std::env::consts::EXE_SUFFIX));
    let package = if bin == "dre-source-fixture" {
        "dre-protocol"
    } else {
        bin
    };
    let status = Command::new(env!("CARGO"))
        .args(["build", "--quiet", "-p", package, "--bin", bin])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .status()
        .expect("cargo build");
    assert!(status.success(), "building {bin} failed");
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
