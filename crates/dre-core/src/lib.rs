//! DRE core: turns a project directory into a resolved, validated project, and runs it.

pub mod diag;
pub mod project;
pub mod yaml;

pub use diag::{Diagnostic, Diagnostics, Severity};

/// The version string `dre --version` prints. DRE has no released version yet.
pub fn version() -> &'static str {
    match env!("CARGO_PKG_VERSION") {
        "0.0.0" => "unreleased",
        v => v,
    }
}
