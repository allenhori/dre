//! The destination side of the protocol: options and multi-file delivery, against the fixture
//! destination plugin.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use dre_protocol::host::{HostError, LogSink, PluginProcess};
use dre_protocol::msg::DeliveryFile;
use dre_protocol::{CAP_MULTI_FILE, MAX_VERSION, MIN_VERSION, conformance};
use serde_json::{Map, Value, json};

fn fixture() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_dre-destination-fixture"))
}

fn quiet() -> LogSink {
    Arc::new(|_, _| {})
}

fn start(single_file: bool) -> PluginProcess {
    let env: &[(&str, &str)] = if single_file {
        &[("DRE_FIXTURE_SINGLE_FILE", "1")]
    } else {
        &[]
    };
    let mut p = PluginProcess::spawn_env(fixture(), quiet(), env).unwrap();
    p.handshake((MIN_VERSION, MAX_VERSION), Duration::from_secs(20))
        .unwrap();
    p
}

struct Files {
    dir: tempfile::TempDir,
}

impl Files {
    fn new() -> Files {
        Files {
            dir: tempfile::tempdir().unwrap(),
        }
    }
    fn file(&self, name: &str, content: &str) -> DeliveryFile {
        let path = self.dir.path().join(name);
        std::fs::write(&path, content).unwrap();
        DeliveryFile {
            local_path: path.to_string_lossy().to_string(),
            remote_path: Some(format!("out/{name}")),
        }
    }
    fn connection(&self) -> Map<String, Value> {
        let rec: PathBuf = self.dir.path().join("rec");
        json!({"dir": rec}).as_object().unwrap().clone()
    }
    fn deliveries(&self) -> Vec<Value> {
        std::fs::read_to_string(self.dir.path().join("rec/deliveries.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }
}

#[test]
fn the_fixture_destination_passes_the_conformance_suite_in_both_modes() {
    conformance::assert_conforms(fixture());
    let checks = conformance::run_with_env(fixture(), &[("DRE_FIXTURE_SINGLE_FILE", "1")]);
    let failed: Vec<_> = checks.iter().filter(|c| !c.passed).collect();
    assert!(failed.is_empty(), "{failed:?}");
}

#[test]
fn options_reach_the_plugin() {
    let f = Files::new();
    let mut p = start(false);
    let opts = json!({"to": ["finance@example.com"], "subject": "Monthly"});
    let loc = p
        .deliver_files(
            &[f.file("a.csv", "n\n1\n")],
            f.connection(),
            opts.as_object().unwrap().clone(),
        )
        .unwrap();
    assert_eq!(loc, "fixture:out/a.csv");
    let d = f.deliveries();
    assert_eq!(d.len(), 1);
    assert_eq!(d[0]["options"], opts);
    assert_eq!(d[0]["files"][0]["content"], "n\n1\n");
}

#[test]
fn a_plain_deliver_sends_empty_options() {
    let f = Files::new();
    let mut p = start(false);
    let file = f.file("a.csv", "x");
    p.deliver(&file.local_path, file.remote_path.as_deref(), f.connection())
        .unwrap();
    assert_eq!(f.deliveries()[0]["options"], json!({}));
}

#[test]
fn a_multi_file_plugin_gets_every_file_in_one_delivery() {
    let f = Files::new();
    let mut p = start(false);
    assert!(p.has(CAP_MULTI_FILE));
    let loc = p
        .deliver_files(
            &[f.file("a.csv", "1"), f.file("b.csv", "2")],
            f.connection(),
            Map::new(),
        )
        .unwrap();
    assert_eq!(loc, "fixture:out/a.csv,out/b.csv");
    let d = f.deliveries();
    assert_eq!(d.len(), 1);
    assert_eq!(d[0]["files"].as_array().unwrap().len(), 2);
    assert_eq!(d[0]["files"][1]["content"], "2");
}

#[test]
fn several_files_are_refused_for_a_plugin_without_multi_file() {
    let f = Files::new();
    let mut p = start(true);
    assert!(!p.has(CAP_MULTI_FILE));
    let err = p
        .deliver_files(
            &[f.file("a.csv", "1"), f.file("b.csv", "2")],
            f.connection(),
            Map::new(),
        )
        .unwrap_err();
    assert!(matches!(err, HostError::Plugin { .. }), "{err}");
    assert!(err.to_string().contains("multi_file"), "{err}");
    assert!(f.deliveries().is_empty());
    // One at a time still works.
    p.deliver_files(&[f.file("a.csv", "1")], f.connection(), Map::new())
        .unwrap();
    assert_eq!(f.deliveries().len(), 1);
}

#[test]
fn a_failed_delivery_is_reported_and_the_plugin_keeps_serving() {
    let f = Files::new();
    let mut p = start(false);
    let mut bad = f.connection();
    bad.insert("fail".into(), json!(true));
    let err = p
        .deliver_files(&[f.file("a.csv", "1")], bad, Map::new())
        .unwrap_err();
    assert!(err.to_string().contains("told to fail"), "{err}");
    p.deliver_files(&[f.file("a.csv", "1")], f.connection(), Map::new())
        .unwrap();
}
