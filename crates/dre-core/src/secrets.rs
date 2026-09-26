//! Secret masking. The value of every environment variable named `DRE_SECRET_*` is replaced by
//! `*****` in everything DRE writes for people to read: the console, `logs/dre.log`, JSON events,
//! `run_results.json` and the compiled SQL in `target/compiled/`. The SQL actually sent to the
//! database is unchanged. `mask_secrets: false` in `dre_project.yml` turns masking off.
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
