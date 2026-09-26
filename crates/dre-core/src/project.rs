//! Loading a DRE project directory into a resolved project.

use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_yaml_ng::Value;

use crate::diag::Diagnostics;
use crate::yaml::YamlFile;

pub const PROJECT_FILE: &str = "dre_project.yml";

#[derive(Debug, Clone, Serialize)]
pub struct Project {
    pub name: String,
}

#[derive(Debug, Clone, Default)]
pub struct LoadOptions {}

/// Parse and validate the project at `root`. Returns the resolved project when it could be
/// built at all, plus every diagnostic found along the way.
pub fn load(root: &Path, _opts: &LoadOptions) -> (Option<Project>, Diagnostics) {
    let mut diags = Diagnostics::default();
    let file = root.join(PROJECT_FILE);
    if !file.is_file() {
        diags.error(
            "project-file-missing",
            Some(PathBuf::from(PROJECT_FILE)),
            None,
            format!("no {PROJECT_FILE} found in {}", root.display()),
        );
        return (None, diags);
    }
    let Some(yf) = YamlFile::load(&file, PathBuf::from(PROJECT_FILE), &mut diags) else {
        return (None, diags);
    };
    let name = match yf.value.get("name") {
        Some(Value::String(s)) if !s.trim().is_empty() => Some(s.clone()),
        Some(_) => {
            diags.error(
                "invalid-field",
                Some(yf.display.clone()),
                yf.line_of("name", None),
                "`name` must be a non-empty string",
            );
            None
        }
        None => {
            diags.error(
                "missing-field",
                Some(yf.display.clone()),
                None,
                "missing required field `name`",
            );
            None
        }
    };
    (name.map(|name| Project { name }), diags)
}
