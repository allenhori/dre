//! Selector resolution, shared by `dre validate` (schedules.yml) and `dre run`.
//!
//! A bare token is tried as (1) an exact report name, (2) a tag, (3) a folder leaf name anywhere
//! under `reports/`. `tag:x` is an explicit tag; a dotted token is a folder path from `reports/`.
//! `source:sales` picks every report a query of which reads a table of source `sales`, and
//! `source:sales.orders` those reading that table (as the parse pass found them).

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
    /// `source:` names a source or table that isn't declared.
    UnknownSource {
        token: String,
        message: String,
    },
}

impl SelectorError {
    pub fn code(&self) -> &'static str {
        match self {
            SelectorError::Ambiguous { .. } => "ambiguous-selector",
            SelectorError::NoMatch { .. } => "selector-matches-nothing",
            SelectorError::UnknownSource { .. } => "unknown-source",
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
            SelectorError::UnknownSource { token, message } => write!(f, "selector `{token}`: {message}"),
        }
    }
}

impl std::error::Error for SelectorError {}

/// Resolve one selector token to reports, in project order. An explicit `tag:` or dotted path
/// that matches nothing returns an empty list; an unknown bare token is `NoMatch`.
/// Resolve a selection: one or more selectors separated by spaces, commas or semicolons
/// (`daily tag:regulatory`, `daily,monthly`, `daily;monthly`), matching any of them. Reports come
/// back in project order, each once.
pub fn resolve<'p>(project: &'p Project, selection: &str) -> Result<Vec<&'p Report>, SelectorError> {
    let mut picked: Vec<&'p Report> = Vec::new();
    for term in selection.split(|c: char| c.is_whitespace() || c == ',' || c == ';') {
        if term.is_empty() {
            continue;
        }
        for r in resolve_one(project, term)? {
            if !picked.iter().any(|x| x.name == r.name) {
                picked.push(r);
            }
        }
    }
    picked.sort_by_key(|r| project.reports.iter().position(|x| x.name == r.name));
    Ok(picked)
}

fn resolve_one<'p>(project: &'p Project, token: &str) -> Result<Vec<&'p Report>, SelectorError> {
    let token = token.trim();
    let under = |folder: &[String]| -> Vec<&'p Report> {
        project
            .reports
            .iter()
            .filter(|r| r.folder.starts_with(folder))
            .collect()
    };
    if let Some(sel) = token.strip_prefix("source:") {
        return by_source(project, token, sel);
    }
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

/// `source:<source>` or `source:<source>.<table>`: the reports some query of which reads it.
fn by_source<'p>(project: &'p Project, token: &str, sel: &str) -> Result<Vec<&'p Report>, SelectorError> {
    let unknown = |message: String| SelectorError::UnknownSource {
        token: token.to_string(),
        message,
    };
    let (source, table) = match sel.split_once('.') {
        Some((s, t)) => (s, Some(t)),
        None => (sel, None),
    };
    let Some(def) = project.sources.get(source) else {
        let known: Vec<&str> = project.sources.keys().map(String::as_str).collect();
        return Err(unknown(if known.is_empty() {
            format!("no source `{source}`: the project declares no `sources:`")
        } else {
            format!("no source `{source}` (declared: {})", known.join(", "))
        }));
    };
    if let Some(t) = table
        && def.table(t).is_none()
    {
        return Err(unknown(format!(
            "source `{source}` has no table `{t}` (it has: {})",
            def.tables
                .iter()
                .map(|t| t.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )));
    }
    let reads = |key: &str| match table {
        Some(t) => key == format!("{source}.{t}"),
        None => key.split_once('.').is_some_and(|(s, _)| s == source),
    };
    Ok(project
        .reports
        .iter()
        .filter(|r| {
            r.bindings
                .iter()
                .filter_map(|b| b.parsed.as_ref())
                .any(|p| p.source_keys().into_iter().any(reads))
        })
        .collect())
}
