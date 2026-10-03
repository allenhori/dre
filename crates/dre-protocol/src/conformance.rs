//! The protocol conformance suite. Every first-party plugin runs it in its tests; third-party
//! plugin authors can run it too. It only checks protocol behaviour common to every plugin kind;
//! each plugin's own tests cover what it does with real data. A package executable
//! (`dre-plugin-<package>`) is checked once per plugin it provides.

// Each check is an immediately-invoked closure so `?` works inside it.
#![allow(clippy::redundant_closure_call)]

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use crate::host::{HostError, Incoming, LogSink, PluginProcess};
use serde_json::{Map, json};

use crate::msg::{DeliveryFile, Request, Response};
use crate::{MAX_VERSION, MIN_VERSION, PluginId, parse_executable_name, parse_package_executable_name};

const TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug)]
pub struct Check {
    /// The plugin checked (`<kind>/<name>`), or the package for checks of the package itself.
    pub plugin: String,
    pub name: &'static str,
    pub passed: bool,
    pub detail: String,
}

fn quiet() -> LogSink {
    Arc::new(|_, _| {})
}

fn start_versions(
    path: &Path,
    plugin: Option<&PluginId>,
    env: &[(&str, &str)],
    versions: (u32, u32),
) -> Result<PluginProcess, HostError> {
    let mut p = PluginProcess::spawn_env(path, quiet(), env)?;
    if plugin.is_some() {
        p.ask_for(plugin.cloned());
    }
    p.handshake(versions, TIMEOUT)?;
    Ok(p)
}

/// Run every check against the plugin executable at `path`.
pub fn run(path: &Path) -> Vec<Check> {
    run_with_env(path, &[])
}

/// Like [`run`], with extra environment variables for every plugin process (for plugins whose
/// capabilities depend on their environment).
pub fn run_with_env(path: &Path, env: &[(&str, &str)]) -> Vec<Check> {
    let file = path.file_name().unwrap().to_string_lossy().to_string();
    if let Some((kind, name)) = parse_executable_name(&file) {
        return run_plugin(path, &PluginId::new(kind, name), false, env);
    }
    let fail = |detail: String| {
        vec![Check {
            plugin: file.clone(),
            name: "the executable is named dre-plugin-<package> or dre-<kind>-<name>",
            passed: false,
            detail,
        }]
    };
    let Some(package) = parse_package_executable_name(&file) else {
        return fail(format!("`{file}` is neither"));
    };
    let provides = match start_versions(path, None, env, (MIN_VERSION, MAX_VERSION)) {
        Ok(p) => {
            let provides = p.info().provides.clone();
            let _ = p.close();
            provides
        }
        Err(e) => return fail(format!("the handshake failed: {e}")),
    };
    let mut out = Vec::new();
    let unknown = PluginId::new(crate::Kind::Format, "dre_conformance_unknown");
    let refused = match start_versions(path, Some(&unknown), env, (MIN_VERSION, MAX_VERSION)) {
        Err(HostError::Plugin { .. }) => Ok(()),
        Err(e) => Err(format!("expected an error reply, got: {e}")),
        Ok(_) => Err(format!("the package served {unknown}, which it doesn't provide")),
    };
    out.push(Check {
        plugin: package,
        name: "asking for a plugin the package doesn't provide gets an error reply",
        passed: refused.is_ok(),
        detail: refused.err().unwrap_or_default(),
    });
    for id in &provides {
        out.extend(run_plugin(path, id, true, env));
    }
    out
}

/// Every check, for one plugin. `ask`: name it in the handshake (a package's plugins).
fn run_plugin(path: &Path, id: &PluginId, ask: bool, env: &[(&str, &str)]) -> Vec<Check> {
    let asked = ask.then_some(id);
    let start = |path: &Path| start_versions(path, asked, env, (MIN_VERSION, MAX_VERSION));
    let mut out = Vec::new();
    let mut check = |name: &'static str, r: Result<(), String>| {
        out.push(Check {
            plugin: id.to_string(),
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
            if info.kind != id.kind || info.name != id.name {
                return Err(format!(
                    "expected {id}, handshake says {}/{}",
                    info.kind, info.name
                ));
            }
            if !info.provides.contains(id) {
                return Err(format!("`provides` doesn't list {id}: {:?}", info.provides));
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
            match start_versions(path, asked, env, (far, far)) {
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

    if id.kind == crate::Kind::Source {
        check(
            "describe gives the identifier quote character",
            (|| {
                let mut p = start(path).map_err(|e| e.to_string())?;
                let d = p.description().map_err(|e| e.to_string())?;
                match d.identifier_quote.as_deref() {
                    Some(q) if q.chars().count() == 1 => {}
                    Some(q) => return Err(format!("`identifier_quote` must be one character, got {q:?}")),
                    None => return Err("a source's describe reply has no `identifier_quote`".into()),
                }
                p.close().map_err(|e| e.to_string())
            })(),
        );
    }

    check(
        "validate is advertised, answered, and refuses an option the plugin doesn't declare",
        (|| {
            let mut p = start(path).map_err(|e| e.to_string())?;
            if !p.has(crate::CAP_VALIDATE) {
                return Err("the plugin doesn't advertise `validate`".into());
            }
            let (_, fields) = p.describe_all().map_err(|e| e.to_string())?;
            p.validate(Map::new()).map_err(|e| e.to_string())?;
            let key = "dre_conformance_unknown_option";
            if fields.iter().any(|f| f.name == key) {
                return Err(format!("the plugin declares `{key}`"));
            }
            let errors = p
                .validate(json!({key: true}).as_object().unwrap().clone())
                .map_err(|e| e.to_string())?;
            if !errors.iter().any(|e| e.contains(key)) {
                return Err(format!("an unknown option was accepted: {errors:?}"));
            }
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

    // Destinations: `deliver` with options (and, with `multi_file`, several files) is parsed and
    // answered. The file doesn't exist, so a delivered or an error reply are both fine.
    let destination = start(path).ok().map(|p| {
        let info = p.info().clone();
        let _ = p.close();
        info
    });
    if let Some(info) = destination.filter(|i| i.kind == crate::Kind::Destination) {
        let missing = |n: &str| DeliveryFile {
            local_path: format!("/nonexistent/dre-conformance/{n}"),
            remote_path: Some(format!("dre-conformance/{n}")),
        };
        let one = missing("a.csv");
        let mut forms = vec![(
            "deliver with options gets a reply and the plugin keeps serving",
            Request::Deliver {
                local_path: Some(one.local_path),
                remote_path: one.remote_path,
                files: Vec::new(),
                connection: Map::new(),
                options: json!({"conformance": true}).as_object().unwrap().clone(),
            },
        )];
        if info.capabilities.iter().any(|c| c == crate::CAP_MULTI_FILE) {
            forms.push((
                "a multi-file deliver gets a reply and the plugin keeps serving",
                Request::Deliver {
                    local_path: None,
                    remote_path: None,
                    files: vec![missing("a.csv"), missing("b.csv")],
                    connection: Map::new(),
                    options: Map::new(),
                },
            ));
        }
        for (name, req) in forms {
            check(
                name,
                (|| {
                    let mut p = start(path).map_err(|e| e.to_string())?;
                    p.send(&req).map_err(|e| e.to_string())?;
                    match p
                        .recv(Some(TIMEOUT), "a deliver reply")
                        .map_err(|e| e.to_string())?
                    {
                        Incoming::Json(Response::Delivered { .. } | Response::Error { .. }) => {}
                        other => return Err(format!("expected delivered or error, got {other:?}")),
                    }
                    p.describe()
                        .map_err(|e| format!("plugin stopped serving after a deliver: {e}"))?;
                    p.close().map_err(|e| e.to_string())
                })(),
            );
        }
    }

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
        .map(|c| format!("  ✗ {}: {}: {}", c.plugin, c.name, c.detail))
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
