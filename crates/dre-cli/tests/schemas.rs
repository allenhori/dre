//! The JSON Schemas for DRE's YAML files (docs/schemas/) against the parser: every key one knows,
//! the other knows, every key is described, and every project the parser accepts validates.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use dre_core::diag::Diagnostics;
use dre_core::project::{self, LoadOptions};
use dre_core::yaml::YamlFile;
use serde_json::Value;

const FILES: [&str; 8] = [
    "project",
    "report",
    "sets",
    "schedules",
    "timings",
    "profiles",
    "dependencies",
    "lookup",
];

fn schemas_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/schemas")
}

fn raw(name: &str) -> Value {
    let path = schemas_dir().join(format!("{name}.schema.json"));
    serde_json::from_str(
        &std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display())),
    )
    .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn base(name: &str) -> String {
    format!("https://schemas.test/{name}.schema.json")
}

/// The schema with an `$id`, and every other schema registered, so cross-file `$ref`s resolve.
fn validator(name: &str) -> jsonschema::Validator {
    let mut opts = jsonschema::options();
    for other in FILES {
        let mut doc = raw(other);
        doc["$id"] = Value::String(base(other));
        opts = opts.with_resource(base(other), jsonschema::Resource::from_contents(doc).unwrap());
    }
    let mut doc = raw(name);
    doc["$id"] = Value::String(base(name));
    opts.build(&doc)
        .unwrap_or_else(|e| panic!("{name}.schema.json is not a valid schema: {e}"))
}

fn set(keys: &[&str]) -> BTreeSet<String> {
    keys.iter().map(|k| k.to_string()).collect()
}

fn props(v: &Value) -> BTreeSet<String> {
    v["properties"]
        .as_object()
        .unwrap_or_else(|| panic!("no properties in {v}"))
        .keys()
        .cloned()
        .collect()
}

fn minus(a: &[&str], b: &[&str]) -> BTreeSet<String> {
    a.iter()
        .filter(|k| !b.contains(k))
        .map(|k| k.to_string())
        .collect()
}

fn plus(mut a: BTreeSet<String>, b: &[&str]) -> BTreeSet<String> {
    a.extend(b.iter().map(|k| k.to_string()));
    a
}

fn same(what: &str, schema: BTreeSet<String>, parser: BTreeSet<String>) {
    assert_eq!(
        schema,
        parser,
        "{what}: the schema (left) and the parser (right) disagree. Keys only in the schema: {:?}; only in the parser: {:?}",
        schema.difference(&parser).collect::<Vec<_>>(),
        parser.difference(&schema).collect::<Vec<_>>(),
    );
}

/// The schemas are versioned with DRE's minor version (`x-dre-schema-version`), published under
/// `/schemas/v<minor>/`. A patch release never breaks a project, so it never changes what the
/// schemas accept; a new minor may, and this test fails until the schemas are reviewed and
/// stamped with the new version.
#[test]
fn the_schemas_carry_dres_minor_version() {
    let v: Vec<&str> = env!("CARGO_PKG_VERSION").split(['.', '-']).collect();
    let minor = format!("{}.{}", v[0], v[1]);
    for f in FILES {
        assert_eq!(
            raw(f)["x-dre-schema-version"],
            Value::String(minor.clone()),
            "{f}.schema.json: review the schemas for DRE {minor}, then set `x-dre-schema-version` to \"{minor}\" in every file"
        );
    }
}

#[test]
fn every_schema_is_a_valid_schema() {
    for f in FILES {
        validator(f);
    }
}

#[test]
fn the_schemas_have_the_keys_the_parser_has() {
    use dre_core::project::*;
    let project = raw("project");
    let report = raw("report");
    // `schedule:` is still a key the parser recognises, but only to say it moved to schedules.yml.
    same(
        "dre_project.yml",
        props(&project),
        plus(minus(PROJECT_KEYS, &[]), PLUGIN_KEYS),
    );
    same(
        "report",
        props(&report),
        plus(minus(REPORT_KEYS, &["schedule"]), PLUGIN_KEYS),
    );
    same(
        "query entry",
        props(&report["$defs"]["queryEntry"]),
        minus(QUERY_ENTRY_KEYS, &[]),
    );
    same(
        "Set entry in a report",
        props(&report["$defs"]["setEntry"]),
        minus(SET_ENTRY_KEYS, &["schedule"]),
    );
    same(
        "output",
        props(&report["$defs"]["output"]),
        minus(OUTPUT_SHARED_KEYS, &[]),
    );
    same(
        "destination",
        props(&report["$defs"]["destination"]),
        set(&["profile", "path"]),
    );
    same(
        "template",
        props(&report["$defs"]["template"]),
        set(&["file", "bindings"]),
    );
    same(
        "template binding",
        props(&report["$defs"]["template"]["properties"]["bindings"]["items"]),
        minus(TEMPLATE_BINDING_KEYS, &[]),
    );
    same(
        "folder config",
        props(&project["$defs"]["folder"]),
        minus(FOLDER_CONFIG_KEYS, &["+schedule"]),
    );
    same(
        "Set in sets.yml",
        props(&raw("sets")["$defs"]["set"]),
        set(&["profile", "vars"]),
    );
    same(
        "schedule",
        props(&raw("schedules")["$defs"]["schedule"]),
        plus(
            minus(dre_core::schedule::SCHEDULE_KEYS, &[]),
            dre_core::project::SCHEDULE_ENTRY_KEYS,
        ),
    );
    same(
        "timing",
        props(&raw("timings")["$defs"]["timing"]),
        minus(dre_core::schedule::TIMING_KEYS, &[]),
    );
    same(
        "profiles.yml",
        props(&raw("profiles")),
        set(&["sources", "destinations"]),
    );
    same(
        "profile",
        props(&raw("profiles")["$defs"]["profile"]),
        set(&["target", "targets"]),
    );
    same(
        "dependencies",
        props(&raw("dependencies")),
        set(&["plugins", "packages"]),
    );
    same(
        "lookup config",
        props(&raw("lookup")),
        minus(dre_core::lookups::CONFIG_KEYS, &[]),
    );
    let plugin_map = &report["properties"]["plugins"]["oneOf"][0]["items"]["oneOf"][2];
    same("plugin entry", props(plugin_map), minus(PLUGIN_ENTRY_KEYS, &[]));
}

#[test]
fn schedule_keys_are_still_rejected_where_the_schema_leaves_them_out() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root.join("reports/r")).unwrap();
    std::fs::write(root.join("dre_project.yml"), "name: p\n").unwrap();
    std::fs::write(root.join("reports/r/q.sql"), "select 1\n").unwrap();
    std::fs::write(
        root.join("reports/r/r.yml"),
        "queries: [q]\nschedule: {cron: '0 6 * * *'}\n",
    )
    .unwrap();
    let (_, diags) = project::load(root, &LoadOptions::default());
    assert!(diags.has_errors(), "`schedule:` in a report must stay an error");
}

fn resolve<'a>(from: &'a str, r: &str, all: &'a [(String, Value)]) -> Option<&'a Value> {
    let (file, ptr) = r.split_once('#')?;
    let doc = if file.is_empty() {
        &all.iter().find(|(n, _)| n == from)?.1
    } else {
        &all.iter().find(|(n, _)| format!("{n}.schema.json") == file)?.1
    };
    doc.pointer(ptr)
}

fn walk(from: &str, v: &Value, path: &str, all: &[(String, Value)], missing: &mut Vec<String>) {
    match v {
        Value::Object(m) => {
            if let Some(Value::Object(props)) = m.get("properties") {
                for (k, sub) in props {
                    let described = sub.get("description").is_some()
                        || sub
                            .get("$ref")
                            .and_then(Value::as_str)
                            .and_then(|r| resolve(from, r, all))
                            .is_some_and(|t| t.get("description").is_some());
                    if !described {
                        missing.push(format!("{from}: {path}/{k}"));
                    }
                }
            }
            for (k, sub) in m {
                walk(from, sub, &format!("{path}/{k}"), all, missing);
            }
        }
        Value::Array(a) => {
            for (i, sub) in a.iter().enumerate() {
                walk(from, sub, &format!("{path}/{i}"), all, missing);
            }
        }
        _ => {}
    }
}

#[test]
fn every_key_has_a_description() {
    let all: Vec<(String, Value)> = FILES.iter().map(|f| (f.to_string(), raw(f))).collect();
    let mut missing = Vec::new();
    for (name, doc) in &all {
        assert!(
            doc["title"].is_string() && doc["description"].is_string(),
            "{name}: title and description"
        );
        walk(name, doc, "", &all, &mut missing);
    }
    assert!(missing.is_empty(), "keys without a description: {missing:#?}");
}

// -- every project the parser accepts validates ------------------------------------------------

fn fixture_projects(dir: &Path, out: &mut Vec<PathBuf>) {
    if dir.join("dre_project.yml").is_file() {
        out.push(dir.to_path_buf());
    }
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        if e.path().is_dir() {
            fixture_projects(&e.path(), out);
        }
    }
}

fn yaml_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().to_string();
        if p.is_dir() {
            if !["dre_deps", "target", "logs", ".git"].contains(&name.as_str()) {
                yaml_files(&p, out);
            }
        } else if name.ends_with(".yml") || name.ends_with(".yaml") {
            out.push(p);
        }
    }
}

fn kind_of(root: &Path, path: &Path, value: &Value) -> Option<&'static str> {
    let rel = path.strip_prefix(root).unwrap();
    let rel_s = rel.to_string_lossy().replace('\\', "/");
    let name = rel.file_name().unwrap().to_string_lossy().to_string();
    if name == "profiles.yml" && !rel_s.starts_with("reports/") {
        return Some("profiles");
    }
    if rel_s == "dre_project.yml" {
        return Some("project");
    }
    if name == "timings.yml" {
        return Some("timings");
    }
    if rel_s == "dependencies.yml" || rel_s == "packages.yml" {
        return Some("dependencies");
    }
    if rel_s.starts_with("lookups/") {
        return if value.is_array() { None } else { Some("lookup") };
    }
    match value {
        Value::Array(_) => Some("schedules"),
        Value::Object(m) if m.is_empty() => None,
        Value::Object(m) => {
            let reportish = m.contains_key("queries") || m.contains_key("name");
            if rel_s.starts_with("reports/") || reportish {
                Some("report")
            } else if m.keys().all(|k| k == "plugins") {
                Some("dependencies")
            } else if name == "sets.yml" || m.values().all(|v| v.is_object()) {
                Some("sets")
            } else {
                None
            }
        }
        _ => None,
    }
}

#[test]
fn every_project_the_parser_accepts_validates_against_the_schemas() {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut roots = Vec::new();
    fixture_projects(&fixtures, &mut roots);
    // More projects to check, e.g. a big example project: DRE_SCHEMA_EXTRA_PROJECTS=/path/a:/path/b
    if let Ok(extra) = std::env::var("DRE_SCHEMA_EXTRA_PROJECTS") {
        roots.extend(extra.split(':').filter(|p| !p.is_empty()).map(PathBuf::from));
    }
    let validators: Vec<(&str, jsonschema::Validator)> = FILES.iter().map(|f| (*f, validator(f))).collect();
    let by_kind = |k: &str| &validators.iter().find(|(n, _)| *n == k).unwrap().1;
    let (mut projects, mut files) = (0, 0);
    let mut failures = Vec::new();
    for root in roots {
        let profiles_dir = root.parent().map(|p| p.join("profiles")).filter(|p| p.is_dir());
        let opts = LoadOptions {
            profiles_dir: profiles_dir.clone(),
            ..Default::default()
        };
        let (_, diags) = project::load(&root, &opts);
        if diags.has_errors() {
            continue; // a project the parser rejects needn't validate
        }
        projects += 1;
        let mut paths = Vec::new();
        yaml_files(&root, &mut paths);
        let mut targets: Vec<(PathBuf, Option<&str>)> = paths.into_iter().map(|p| (p, None)).collect();
        if let Some(pd) = &profiles_dir {
            targets.push((pd.join("profiles.yml"), Some("profiles")));
        }
        for (path, forced) in targets {
            if !path.is_file() {
                continue;
            }
            let mut d = Diagnostics::default();
            let Some(yf) = YamlFile::load(&path, path.clone(), &mut d) else {
                continue;
            };
            let json = project::yaml_to_json(&yf.value);
            let Some(kind) = forced.or_else(|| kind_of(&root, &path, &json)) else {
                continue;
            };
            files += 1;
            for e in by_kind(kind).iter_errors(&json) {
                failures.push(format!("{} ({kind}): {e} at {}", path.display(), e.instance_path));
            }
        }
    }
    assert!(
        projects >= 10 && files >= 40,
        "too few fixtures checked: {projects} projects, {files} files"
    );
    assert!(
        failures.is_empty(),
        "{} problems:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn the_schemas_reject_what_is_wrong() {
    let cases: [(&str, &str); 10] = [
        ("project", r#"{"name": "p", "colour": "blue"}"#),
        ("project", r#"{"run_query_max_rows": 0, "name": "p"}"#),
        ("project", r#"{"week_start": "friday", "name": "p"}"#),
        ("report", r#"{"queries": [{"tab": false}]}"#),
        ("report", r#"{"output": {"destination": []}}"#),
        (
            "schedules",
            r#"[{"name": "a b", "report": "r", "cron": "* * * * *"}]"#,
        ),
        (
            "profiles",
            r#"{"sources": {"w": {"target": "dev", "targets": {"dev": {"path": "x"}}}}}"#,
        ),
        (
            "timings",
            r#"{"month_start": {"cron": "0 6 1 * *", "colour": "blue"}}"#,
        ),
        ("timings", r#"{"month start": {"cron": "0 6 1 * *"}}"#),
        (
            "dependencies",
            r#"{"packages": [{"git": "https://example.com/p.git"}]}"#,
        ),
    ];
    for (kind, doc) in cases {
        let doc: Value = serde_json::from_str(doc).unwrap();
        assert!(!validator(kind).is_valid(&doc), "{kind} schema accepted {doc}");
    }
}

#[test]
fn what_dre_new_writes_validates_and_points_at_the_schemas() {
    let dir = tempfile::tempdir().unwrap();
    let project_dir = dir.path().join("my_reports");
    assert_cmd::Command::cargo_bin("dre")
        .unwrap()
        .args(["new", project_dir.to_str().unwrap()])
        .assert()
        .success();
    let minor = {
        let v: Vec<&str> = env!("CARGO_PKG_VERSION").split(['.', '-']).collect();
        format!("{}.{}", v[0], v[1])
    };
    for (rel, kind) in [
        ("dre_project.yml", "project"),
        ("dependencies.yml", "dependencies"),
        ("reports/examples/hello/hello.yml", "report"),
        ("timings.yml", "timings"),
    ] {
        let text = std::fs::read_to_string(project_dir.join(rel)).unwrap();
        let first = text.lines().next().unwrap();
        assert_eq!(
            first,
            format!("# yaml-language-server: $schema=https://getdre.com/schemas/v{minor}/{kind}.schema.json"),
            "{rel}"
        );
        let mut doc: Value = project::yaml_to_json(&serde_yaml_ng::from_str(&text).unwrap());
        if doc.is_null() {
            // All comments: check the commented-out example instead.
            let example: String = text
                .lines()
                .skip_while(|l| !l.starts_with("# month_start:"))
                .take_while(|l| !l.starts_with("# Then"))
                .map(|l| format!("{}\n", l.strip_prefix("# ").unwrap_or("")))
                .collect();
            doc = project::yaml_to_json(&serde_yaml_ng::from_str(&example).unwrap());
            assert!(doc.is_object(), "{rel}: {example}");
        }
        let errors: Vec<String> = validator(kind).iter_errors(&doc).map(|e| e.to_string()).collect();
        assert!(errors.is_empty(), "{rel}: {errors:?}");
    }
}
