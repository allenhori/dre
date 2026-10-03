//! DRE core: turns a project directory into a resolved, validated project, and runs it.

pub mod constraints;
pub mod dates;
pub mod diag;
pub mod lock;
pub mod lookups;
pub mod manager;
pub mod manifest;
mod mutable;
pub mod occurrences;
pub mod options;
pub mod packages;
pub mod plugins;
pub mod preflight;
pub mod profiles;
pub mod project;
pub mod render;
pub mod run;
pub mod schedule;
mod schema;
pub mod secrets;
pub mod selector;
pub mod sqlsplit;
pub mod target;
pub mod values;
pub mod yaml;

pub use diag::{Diagnostic, Diagnostics, Severity};

/// The version string `dre --version` prints. DRE has no released version yet.
pub fn version() -> &'static str {
    match env!("CARGO_PKG_VERSION") {
        "0.0.0" => "unreleased",
        v => v,
    }
}

/// DRE's home directory: `~/.dre`.
pub fn dre_home() -> std::path::PathBuf {
    std::env::home_dir().unwrap_or_default().join(".dre")
}

/// A path with `/` separators on every platform, so messages and goldens read the same everywhere.
pub fn slash(p: &std::path::Path) -> std::path::PathBuf {
    std::path::PathBuf::from(p.to_string_lossy().replace('\\', "/"))
}
