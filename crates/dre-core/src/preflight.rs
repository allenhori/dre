//! Jinja pre-flight: syntax checks and static scans of `var()`, `env_var()` and `run.*` call
//! sites. Nothing is rendered with live values here.

use std::sync::LazyLock;

use regex::Regex;

/// `run.*` attributes the runtime context provides.
pub const RUN_ATTRS: &[&str] = &["report", "set", "target", "profile", "date", "date_format"];
/// Pre-built formats of `run.date`.
pub const DATE_ATTRS: &[&str] = &["yyyymmdd", "ddmmyyyy", "yyyy", "mm", "dd", "iso"];

static SEGMENT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)\{\{.*?\}\}|\{%.*?%\}").unwrap());
static VAR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?:^|[^\w.])(env_var|var)\s*\(\s*(['"])([^'"]+)['"]\s*([,)])"#).unwrap());
static RUN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:^|[^\w.])run\.([A-Za-z_]\w*)(?:\.([A-Za-z_]\w*))?").unwrap());

/// Whether a string contains Jinja at all.
pub fn is_templated(s: &str) -> bool {
    s.contains("{{") || s.contains("{%")
}

/// Compile-check a template; returns `(line, message)` on a syntax error.
pub fn check_syntax(name: &str, src: &str) -> Result<(), (Option<usize>, String)> {
    let env = minijinja::Environment::new();
    match env.template_from_named_str(name, src) {
        Ok(_) => Ok(()),
        Err(e) => Err((e.line(), e.detail().unwrap_or("syntax error").to_string())),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Call {
    /// `var` or `env_var`.
    pub func: &'static str,
    pub name: String,
    pub has_default: bool,
    /// 1-based line within the scanned source.
    pub line: usize,
}

fn line_at(src: &str, offset: usize) -> usize {
    src[..offset].matches('\n').count() + 1
}

/// Every `var('x')` / `env_var('X')` call inside Jinja blocks.
pub fn calls(src: &str) -> Vec<Call> {
    let mut out = Vec::new();
    for seg in SEGMENT.find_iter(src) {
        for c in VAR.captures_iter(seg.as_str()) {
            let m = c.get(1).unwrap();
            out.push(Call {
                func: if m.as_str() == "var" { "var" } else { "env_var" },
                name: c[3].to_string(),
                has_default: &c[4] == ",",
                line: line_at(src, seg.start() + m.start()),
            });
        }
    }
    out
}

/// `run.*` references inside Jinja blocks that aren't part of the runtime context.
pub fn unknown_run_refs(src: &str) -> Vec<(String, usize)> {
    let mut out = Vec::new();
    for seg in SEGMENT.find_iter(src) {
        for c in RUN.captures_iter(seg.as_str()) {
            let attr = &c[1];
            let line = line_at(src, seg.start() + c.get(0).unwrap().start());
            if !RUN_ATTRS.contains(&attr) {
                out.push((format!("run.{attr}"), line));
            } else if attr == "date"
                && let Some(sub) = c.get(2)
                && !DATE_ATTRS.contains(&sub.as_str())
            {
                out.push((format!("run.date.{}", sub.as_str()), line));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_var_calls_with_and_without_defaults() {
        let src = "select {{ var('a') }},\n {{ var(\"b\", 1) }}, {{ env_var('HOME') }} from t";
        let c = calls(src);
        assert_eq!(c.len(), 3);
        assert_eq!((c[0].name.as_str(), c[0].has_default, c[0].line), ("a", false, 1));
        assert_eq!((c[1].name.as_str(), c[1].has_default, c[1].line), ("b", true, 2));
        assert_eq!((c[2].func, c[2].name.as_str()), ("env_var", "HOME"));
    }

    #[test]
    fn ignores_sql_outside_jinja_and_method_like_names() {
        assert!(calls("select var('x') from t").is_empty());
        assert!(calls("{{ my.var('x') }} {{ env_var_x('y') }}").is_empty());
        assert!(unknown_run_refs("select run.dat from runs run").is_empty());
    }

    #[test]
    fn flags_unknown_run_attributes() {
        let src = "{{ run.report }} {{ run.dat }}\n{{ run.date.yyyymmdd }} {{ run.date.yymm }}";
        assert_eq!(
            unknown_run_refs(src),
            vec![("run.dat".to_string(), 1), ("run.date.yymm".to_string(), 2)]
        );
    }
}
