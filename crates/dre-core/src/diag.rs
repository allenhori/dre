//! Validation diagnostics: every problem found in one pass, with file and line where known.

use std::fmt;
use std::path::PathBuf;

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Diagnostic {
    pub severity: Severity,
    /// Stable, kebab-case identifier for tooling (e.g. `duplicate-report-name`).
    pub code: &'static str,
    pub message: String,
    /// Path relative to the project root, or absolute for files outside it (profiles.yml).
    pub file: Option<PathBuf>,
    pub line: Option<usize>,
    /// The plugin an `undeclared-plugin` error is about, so the CLI can look it up in DRE's
    /// registry.
    #[serde(skip)]
    pub plugin: Option<(crate::project::PluginKind, String)>,
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let sev = match self.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
        };
        write!(f, "{sev}[{}]: ", self.code)?;
        match (&self.file, self.line) {
            (Some(file), Some(line)) => write!(f, "{}:{line}: ", file.display())?,
            (Some(file), None) => write!(f, "{}: ", file.display())?,
            _ => {}
        }
        f.write_str(&self.message)
    }
}

/// An ordered collection of diagnostics; errors and warnings are reported together.
#[derive(Debug, Default, Clone, Serialize)]
#[serde(transparent)]
pub struct Diagnostics(Vec<Diagnostic>);

impl Diagnostics {
    pub fn push(&mut self, d: Diagnostic) {
        if !self.0.contains(&d) {
            self.0.push(d);
        }
    }

    pub fn error(
        &mut self,
        code: &'static str,
        file: Option<PathBuf>,
        line: Option<usize>,
        message: impl Into<String>,
    ) {
        self.push(Diagnostic {
            severity: Severity::Error,
            code,
            message: message.into(),
            file,
            line,
            plugin: None,
        });
    }

    pub fn warning(
        &mut self,
        code: &'static str,
        file: Option<PathBuf>,
        line: Option<usize>,
        message: impl Into<String>,
    ) {
        self.push(Diagnostic {
            severity: Severity::Warning,
            code,
            message: message.into(),
            file,
            line,
            plugin: None,
        });
    }

    pub fn extend(&mut self, other: Diagnostics) {
        for d in other.0 {
            self.push(d);
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = &Diagnostic> {
        self.0.iter()
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut Diagnostic> {
        self.0.iter_mut()
    }

    pub fn error_count(&self) -> usize {
        self.0.iter().filter(|d| d.severity == Severity::Error).count()
    }

    pub fn warning_count(&self) -> usize {
        self.0.iter().filter(|d| d.severity == Severity::Warning).count()
    }

    pub fn has_errors(&self) -> bool {
        self.error_count() > 0
    }

    /// Stable order for output: by file, then line, then severity, keeping insertion order otherwise.
    pub fn sorted(&self) -> Vec<&Diagnostic> {
        let mut v: Vec<&Diagnostic> = self.0.iter().collect();
        v.sort_by(|a, b| (&a.file, a.line, a.severity).cmp(&(&b.file, b.line, b.severity)));
        v
    }
}
