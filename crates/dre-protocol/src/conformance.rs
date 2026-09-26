//! The protocol conformance suite. Every first-party plugin runs it in its tests; third-party
//! plugin authors can run it too. It only checks protocol behaviour common to every plugin kind;
//! each plugin's own tests cover what it does with real data.

// Each check is an immediately-invoked closure so `?` works inside it.
#![allow(clippy::redundant_closure_call)]

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use crate::host::{HostError, Incoming, LogSink, PluginProcess};
use crate::msg::{Request, Response};
use crate::{MAX_VERSION, MIN_VERSION, parse_executable_name};

const TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug)]
pub struct Check {
    pub name: &'static str,
    pub passed: bool,
    pub detail: String,
}

fn quiet() -> LogSink {
    Arc::new(|_, _| {})
}

fn start(path: &Path) -> Result<PluginProcess, HostError> {
    PluginProcess::start_with(path, quiet(), (MIN_VERSION, MAX_VERSION), TIMEOUT)
}

/// Run every check against the plugin executable at `path`.
pub fn run(path: &Path) -> Vec<Check> {
    let mut out = Vec::new();
    let mut check = |name: &'static str, r: Result<(), String>| {
        out.push(Check {
            name,
            passed: r.is_ok(),
            detail: r.err().unwrap_or_default(),
        });
    };

    check(
        "handshake negotiates a supported version and reports its identity",
        (|| {
            let p = start(path).map_err(|e| e.to_string())?;
            let info = p.info().clone();
            let file = path.file_name().unwrap().to_string_lossy().to_string();
            let (kind, name) =
                parse_executable_name(&file).ok_or(format!("`{file}` isn't named dre-<kind>-<name>"))?;
            if info.kind != kind || info.name != name {
                return Err(format!(
                    "file says {kind}/{name}, handshake says {}/{}",
                    info.kind, info.name
                ));
            }
            if info.version.is_empty() {
                return Err("empty plugin version".into());
            }
            p.close().map_err(|e| e.to_string())
        })(),
    );

    check(
        "an unsupported protocol range is refused, not hung on",
        (|| {
            let far = MAX_VERSION + 1000;
            match PluginProcess::start_with(path, quiet(), (far, far), TIMEOUT) {
                Err(HostError::Incompatible { plugin_range, .. }) if plugin_range.1 < far => Ok(()),
                Err(e) => Err(format!("expected a version mismatch, got: {e}")),
                Ok(_) => Err("the plugin accepted a protocol version it can't speak".into()),
            }
        })(),
    );

    check(
        "describe is answered",
        (|| {
            let mut p = start(path).map_err(|e| e.to_string())?;
            p.describe().map_err(|e| e.to_string())?;
            p.close().map_err(|e| e.to_string())
        })(),
    );

    check(
        "an unknown request gets an error reply and the plugin keeps serving",
        (|| {
            let mut p = start(path).map_err(|e| e.to_string())?;
            p.write_raw(&frame_json(r#"{"type":"frobnicate"}"#))
                .map_err(|e| e.to_string())?;
            match p
                .recv(Some(TIMEOUT), "an error reply")
                .map_err(|e| e.to_string())?
            {
                Incoming::Json(Response::Error { .. }) => {}
                other => return Err(format!("expected an error reply, got {other:?}")),
            }
            p.describe()
                .map_err(|e| format!("plugin stopped serving after an unknown request: {e}"))?;
            p.close().map_err(|e| e.to_string())
        })(),
    );

    check(
        "a request meant for another plugin kind gets an error reply",
        (|| {
            let mut p = start(path).map_err(|e| e.to_string())?;
            let req = match p.info().kind {
                crate::Kind::Source => Request::Finish {},
                _ => Request::Check {
                    sql: "select 1".into(),
                },
            };
            p.send(&req).map_err(|e| e.to_string())?;
            match p
                .recv(Some(TIMEOUT), "an error reply")
                .map_err(|e| e.to_string())?
            {
                Incoming::Json(Response::Error { .. }) => {}
                other => return Err(format!("expected an error reply, got {other:?}")),
            }
            p.close().map_err(|e| e.to_string())
        })(),
    );

    check(
        "a malformed frame is reported or ends the plugin, never hangs",
        (|| {
            let mut p = start(path).map_err(|e| e.to_string())?;
            let _ = p.write_raw(&[0, 0, 0, 2, b'X', b'!']);
            match p.recv(Some(TIMEOUT), "a reply to a malformed frame") {
                Ok(Incoming::Json(Response::Error { .. })) | Err(HostError::Crashed { .. }) => Ok(()),
                Err(HostError::Timeout { .. }) => Err("the plugin hung on a malformed frame".into()),
                other => Err(format!("unexpected reply to a malformed frame: {other:?}")),
            }
        })(),
    );

    check(
        "close ends the process with exit code 0",
        (|| {
            let mut p = start(path).map_err(|e| e.to_string())?;
            p.send(&Request::Close {}).map_err(|e| e.to_string())?;
            let _ = p.recv(Some(TIMEOUT), "the close reply");
            match p.wait_exit(TIMEOUT) {
                Some(s) if s.success() => Ok(()),
                Some(s) => Err(format!("exited with {s}")),
                None => Err("still running after close".into()),
            }
        })(),
    );

    check(
        "end of input ends the process",
        (|| {
            let mut p = start(path).map_err(|e| e.to_string())?;
            match p.wait_exit(TIMEOUT) {
                Some(_) => Ok(()),
                None => Err("still running after stdin closed".into()),
            }
        })(),
    );

    out
}

/// Panic with a readable report unless every check passes. For use in plugin tests.
pub fn assert_conforms(path: &Path) {
    let checks = run(path);
    let failed: Vec<String> = checks
        .iter()
        .filter(|c| !c.passed)
        .map(|c| format!("  ✗ {}: {}", c.name, c.detail))
        .collect();
    assert!(
        failed.is_empty(),
        "{} fails protocol conformance:\n{}",
        path.display(),
        failed.join("\n")
    );
}

fn frame_json(body: &str) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(&((body.len() + 1) as u32).to_be_bytes());
    v.push(crate::frame::JSON_TAG);
    v.extend_from_slice(body.as_bytes());
    v
}
