//! Secret masking. The value of every environment variable named `DRE_SECRET_*` is replaced by
//! `*****` in everything DRE writes for people to read: the console, `logs/dre.log`, JSON events,
//! `run_results.json` and the compiled SQL in `target/compiled/`. The SQL actually sent to the
//! database is unchanged. `mask_secrets: false` in `dre_project.yml` turns masking off.
//!
//! JSON is redacted structurally, before it is serialized: serializing escapes `"`, `\` and
//! control characters, so masking the finished text would miss a secret that contains them. Use
//! [`to_json_pretty`] / [`to_json_line`] for every JSON surface, never `mask` on JSON text.
//!
//! Values shorter than 4 characters aren't masked: replacing every `a` or `12` in a log would
//! make it unreadable, and a value that short isn't a secret anyway.

use std::borrow::Cow;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicBool, Ordering};

pub const PREFIX: &str = "DRE_SECRET_";
pub const MASK: &str = "*****";
const MIN_LEN: usize = 4;

static ENABLED: AtomicBool = AtomicBool::new(true);

/// Secret values, longest first so a secret containing another is masked whole.
static SECRETS: LazyLock<Vec<String>> = LazyLock::new(|| {
    let mut v: Vec<String> = std::env::vars()
        .filter(|(k, v)| k.starts_with(PREFIX) && v.chars().count() >= MIN_LEN)
        .map(|(_, v)| v)
        .collect();
    v.sort_by_key(|s| std::cmp::Reverse(s.len()));
    v.dedup();
    v
});

pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
}

/// `s` with every secret value replaced by `*****`.
pub fn mask(s: &str) -> Cow<'_, str> {
    if !ENABLED.load(Ordering::Relaxed) || !SECRETS.iter().any(|x| s.contains(x.as_str())) {
        return Cow::Borrowed(s);
    }
    let mut out = s.to_string();
    for x in SECRETS.iter() {
        out = out.replace(x.as_str(), MASK);
    }
    Cow::Owned(out)
}

/// `v` with every secret value replaced by `*****` in all strings and object keys.
pub fn redact_json(v: &mut serde_json::Value) {
    use serde_json::Value;
    match v {
        Value::String(s) => {
            if let Cow::Owned(m) = mask(s) {
                *s = m;
            }
        }
        Value::Array(a) => a.iter_mut().for_each(redact_json),
        Value::Object(o) => {
            let entries = std::mem::take(o);
            for (k, mut val) in entries {
                redact_json(&mut val);
                o.insert(mask(&k).into_owned(), val);
            }
        }
        _ => {}
    }
}

fn redacted<T: serde::Serialize>(v: &T) -> serde_json::Result<serde_json::Value> {
    let mut v = serde_json::to_value(v)?;
    redact_json(&mut v);
    Ok(v)
}

/// Pretty JSON of `v` with secrets redacted first.
pub fn to_json_pretty<T: serde::Serialize>(v: &T) -> serde_json::Result<String> {
    serde_json::to_string_pretty(&redacted(v)?)
}

/// One-line JSON of `v` with secrets redacted first.
pub fn to_json_line<T: serde::Serialize>(v: &T) -> serde_json::Result<String> {
    serde_json::to_string(&redacted(v)?)
}
