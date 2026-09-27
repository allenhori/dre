//! A fixture destination plugin for protocol and run tests.
//!
//! Every delivery appends one JSON line to `<dir>/deliveries.jsonl` (`dir` from the connection):
//! `{"files": [{"local": ..., "remote": ..., "content": ...}], "options": {...}}`. The reply's
//! location is `fixture:` followed by the remote paths (or file names), comma-separated.
//!
//! - It advertises `multi_file` unless `DRE_FIXTURE_SINGLE_FILE` is set.
//! - A connection with `fail: true` makes every delivery fail.
//! - Its options are `to`, `subject` and `message`, recorded as given.

use std::io::Write;

use dre_protocol::CAP_MULTI_FILE;
use dre_protocol::msg::ConnectionField;
use dre_protocol::options::{OptionField, OptionType};
use dre_protocol::plugin::{
    About, Delivery, Destination, Result, conn_bool, conn_required, serve_destination,
};
use serde_json::json;

struct Fixture;

impl Destination for Fixture {
    fn connection_fields(&self) -> Vec<ConnectionField> {
        vec![ConnectionField::new("dir", "where deliveries are recorded").required()]
    }

    fn options(&self) -> Vec<OptionField> {
        vec![
            OptionField::new("to", OptionType::Strings, "recorded"),
            OptionField::new("subject", OptionType::String, "recorded"),
            OptionField::new("message", OptionType::String, "recorded"),
        ]
    }

    fn deliver_files(&mut self, d: &Delivery) -> Result<String> {
        if conn_bool(&d.connection, "fail") == Some(true) {
            return Err("fixture destination told to fail".into());
        }
        let dir = conn_required(&d.connection, "dir")?;
        let mut files = Vec::new();
        let mut names = Vec::new();
        for f in &d.files {
            let content = std::fs::read_to_string(&f.local)
                .map_err(|e| format!("can't read {}: {e}", f.local.display()))?;
            names.push(f.remote.clone().unwrap_or_else(|| {
                f.local
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string()
            }));
            files.push(json!({"local": f.local, "remote": f.remote, "content": content}));
        }
        std::fs::create_dir_all(dir)?;
        let mut log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(std::path::Path::new(dir).join("deliveries.jsonl"))?;
        writeln!(log, "{}", json!({"files": files, "options": d.options}))?;
        Ok(format!("fixture:{}", names.join(",")))
    }
}

fn main() {
    let caps: &'static [&'static str] = if std::env::var_os("DRE_FIXTURE_SINGLE_FILE").is_some() {
        &[]
    } else {
        &[CAP_MULTI_FILE]
    };
    serve_destination(
        About::new("fixture", env!("CARGO_PKG_VERSION")).capabilities(caps),
        Fixture,
    )
}
