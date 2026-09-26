//! Selector resolution, shared by `dre validate` (schedules.yml) and `dre run`.
//!
//! A bare token is tried as (1) an exact report name, (2) a tag, (3) a folder leaf name anywhere
//! under `reports/`. `tag:x` is an explicit tag; a dotted token is a folder path from `reports/`.

use std::fmt;

use crate::project::{Project, REPORTS_DIR, Report, dotted};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectorError {
    Ambiguous {
        token: String,
        candidates: Vec<Vec<String>>,
    },
    NoMatch {
        token: String,
    },
}

impl SelectorError {
    pub fn code(&self) -> &'static str {
        match self {
            SelectorError::Ambiguous { .. } => "ambiguous-selector",
            SelectorError::NoMatch { .. } => "selector-matches-nothing",
        }
    }
}

impl fmt::Display for SelectorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SelectorError::Ambiguous { token, candidates } => {
                writeln!(
                    f,
                    "\"{token}\" matches more than one location — use the dotted form to disambiguate:"
                )?;
                let width = candidates.iter().map(|c| dotted(c).len()).max().unwrap_or(0);
                for (i, c) in candidates.iter().enumerate() {
                    let d = dotted(c);
                    write!(f, "  {d:<width$}   ({REPORTS_DIR}/{}/)", c.join("/"))?;
                    if i + 1 < candidates.len() {
                        writeln!(f)?;
                    }
                }
                Ok(())
            }
            SelectorError::NoMatch { token } => {
                write!(f, "selector `{token}` matches no report name, tag or folder")
            }
        }
    }
}

impl std::error::Error for SelectorError {}

/// Resolve one selector token to reports, in project order. An explicit `tag:` or dotted path
/// that matches nothing returns an empty list; an unknown bare token is `NoMatch`.
pub fn resolve<'p>(project: &'p Project, token: &str) -> Result<Vec<&'p Report>, SelectorError> {
    let token = token.trim();
    let under = |folder: &[String]| -> Vec<&'p Report> {
        project
            .reports
            .iter()
            .filter(|r| r.folder.starts_with(folder))
            .collect()
    };
    if let Some(tag) = token.strip_prefix("tag:") {
        return Ok(project
            .reports
            .iter()
            .filter(|r| r.tags.iter().any(|t| t == tag))
            .collect());
    }
    if let Some(r) = project.report(token) {
        return Ok(vec![r]);
    }
    if token.contains('.') {
        let path: Vec<String> = token.split('.').map(str::to_string).collect();
        return Ok(under(&path));
    }
    let tagged: Vec<&Report> = project
        .reports
        .iter()
        .filter(|r| r.tags.iter().any(|t| t == token))
        .collect();
    if !tagged.is_empty() {
        return Ok(tagged);
    }
    let folders: Vec<&Vec<String>> = project
        .folders
        .iter()
        .filter(|f| f.last().is_some_and(|l| l == token))
        .collect();
    match folders.len() {
        0 => Err(SelectorError::NoMatch {
            token: token.to_string(),
        }),
        1 => Ok(under(folders[0])),
        _ => Err(SelectorError::Ambiguous {
            token: token.to_string(),
            candidates: folders.into_iter().cloned().collect(),
        }),
    }
}
