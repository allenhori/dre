//! Statement splitting: lexing `;`-separated SQL, not parsing it.
//!
//! Understands single/double/backtick quotes, `--` and nested `/* */` comments, and Postgres
//! dollar-quoted blocks (`$$ ... $$`, `$tag$ ... $tag$`). Shared by `dre validate`'s best-effort
//! unmanaged-report check and by the run path.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Statement {
    /// The statement text, trimmed, without its terminating `;`.
    pub text: String,
    /// 1-based line where the statement's first non-whitespace character sits.
    pub line: usize,
}

pub fn split(sql: &str) -> Vec<Statement> {
    let b = sql.as_bytes();
    let mut out = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'\'' | b'"' | b'`' => i = skip_quoted(b, i),
            b'-' if b.get(i + 1) == Some(&b'-') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'*') => i = skip_block_comment(b, i),
            b'$' => i = skip_dollar_quoted(sql, i),
            b';' => {
                push_statement(sql, start, i, &mut out);
                i += 1;
                start = i;
            }
            _ => i += 1,
        }
    }
    push_statement(sql, start, b.len(), &mut out);
    out
}

fn push_statement(sql: &str, start: usize, end: usize, out: &mut Vec<Statement>) {
    let raw = &sql[start..end];
    let text = raw.trim();
    if text.is_empty() || strip_leading_comments(text).is_empty() {
        return;
    }
    let lead = raw.len() - raw.trim_start().len();
    let line = sql[..start + lead].matches('\n').count() + 1;
    out.push(Statement {
        text: text.to_string(),
        line,
    });
}

fn skip_quoted(b: &[u8], i: usize) -> usize {
    let q = b[i];
    let mut j = i + 1;
    while j < b.len() {
        if b[j] == q {
            // A doubled quote is an escaped quote.
            if b.get(j + 1) == Some(&q) {
                j += 2;
                continue;
            }
            return j + 1;
        }
        j += 1;
    }
    b.len()
}

fn skip_block_comment(b: &[u8], i: usize) -> usize {
    let mut depth = 0;
    let mut j = i;
    while j < b.len() {
        if b[j] == b'/' && b.get(j + 1) == Some(&b'*') {
            depth += 1;
            j += 2;
        } else if b[j] == b'*' && b.get(j + 1) == Some(&b'/') {
            depth -= 1;
            j += 2;
            if depth == 0 {
                return j;
            }
        } else {
            j += 1;
        }
    }
    b.len()
}

fn skip_dollar_quoted(sql: &str, i: usize) -> usize {
    let b = sql.as_bytes();
    // A tag is `$` [A-Za-z_][A-Za-z0-9_]* `$`, or just `$$`. `$1` parameters are not tags.
    let mut j = i + 1;
    if j < b.len() && (b[j].is_ascii_alphabetic() || b[j] == b'_') {
        while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'_') {
            j += 1;
        }
    }
    if b.get(j) != Some(&b'$') {
        return i + 1;
    }
    let tag = &sql[i..=j];
    match sql[j + 1..].find(tag) {
        Some(pos) => j + 1 + pos + tag.len(),
        None => b.len(),
    }
}

/// Remove leading whitespace and comments, returning the rest.
pub fn strip_leading_comments(mut s: &str) -> &str {
    loop {
        s = s.trim_start();
        if let Some(rest) = s.strip_prefix("--") {
            s = rest.find('\n').map_or("", |p| &rest[p..]);
        } else if s.starts_with("/*") {
            let end = skip_block_comment(s.as_bytes(), 0);
            s = &s[end..];
        } else {
            return s;
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatementKind {
    /// `SELECT` or `WITH`.
    Read,
    /// `CREATE [OR REPLACE] TEMP|TEMPORARY TABLE|VIEW`.
    TempCreate,
    /// Anything else.
    Other,
}

impl StatementKind {
    /// Whether an unmanaged report may run this statement.
    pub fn is_read_only_safe(self) -> bool {
        matches!(self, StatementKind::Read | StatementKind::TempCreate)
    }
}

/// Conservative keyword classification. A false rejection is fixed by adding a YAML; a false
/// acceptance would run a side effect, so anything unrecognised is `Other`.
pub fn classify(statement: &str) -> StatementKind {
    let s = strip_leading_comments(statement).trim_start_matches(|c: char| c == '(' || c.is_whitespace());
    let words: Vec<String> = s
        .split_whitespace()
        .take(5)
        .map(|w| w.trim_matches('(').to_ascii_lowercase())
        .collect();
    let w = |n: usize| words.get(n).map(String::as_str).unwrap_or("");
    match w(0) {
        "select" | "with" => StatementKind::Read,
        "create" => {
            let mut k = 1;
            if w(1) == "or" && w(2) == "replace" {
                k = 3;
            }
            if matches!(w(k), "temp" | "temporary") && matches!(w(k + 1), "table" | "view") {
                StatementKind::TempCreate
            } else {
                StatementKind::Other
            }
        }
        _ => StatementKind::Other,
    }
}
