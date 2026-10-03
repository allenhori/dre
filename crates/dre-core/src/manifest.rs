//! The project manifest, `<target>/manifest.json`: everything a project declares, as the engine
//! resolves it, for orchestrators, CI and other tools. It's a deliberate projection of the loaded
//! [`Project`], not a dump: built offline (no connection, no profiles, no plugins), the same bytes
//! for the same project on every OS, and free of secrets and machine-specific values.
//!
//! The format is a public, versioned contract (`docs/manifest.md`, `docs/manifest.schema.json`):
//! adding optional fields keeps [`SCHEMA`]; removing, renaming or re-typing a field, or changing
//! what one means, bumps it.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::{Map as JsonMap, Value as Json, json};
use sha2::{Digest, Sha256};

use crate::diag::{Diagnostics, Severity};
use crate::lookups::LOOKUPS_DIR;
use crate::project::{Binding, MACROS_DIR, PluginSource, Project, QueryEntry, REPORTS_DIR, Report};

/// The manifest format's version.
pub const SCHEMA: u64 = 1;
/// The manifest's file name in the target folder.
pub const FILE: &str = "manifest.json";

/// Per-report problems, by report name: they mark the report invalid in the manifest.
pub type ReportErrors = BTreeMap<String, Vec<String>>;

/// Where the manifest goes for `project`.
pub fn path(project: &Project) -> PathBuf {
    project.target_dir.join(FILE)
}

/// The whole project's manifest.
pub fn build(project: &Project, errors: &ReportErrors) -> Json {
    let schedules: JsonMap<String, Json> = project
        .schedules
        .iter()
        .map(|e| (e.name.clone(), schedule(project, &e.name)))
        .collect();
    document(
        project,
        project
            .reports
            .iter()
            .map(|r| (r, r.bindings.iter().collect()))
            .collect(),
        schedules,
        errors,
    )
}

/// A manifest-shaped document holding only `reports` (each with the given Bindings) and
/// `schedules`: what `dre ls --output json` prints.
pub fn subset(
    project: &Project,
    reports: Vec<(&Report, Vec<&Binding>)>,
    schedule_names: &[String],
    errors: &ReportErrors,
) -> Json {
    let schedules = schedule_names
        .iter()
        .map(|n| (n.clone(), schedule(project, n)))
        .collect();
    document(project, reports, schedules, errors)
}

fn document(
    project: &Project,
    reports: Vec<(&Report, Vec<&Binding>)>,
    schedules: JsonMap<String, Json>,
    errors: &ReportErrors,
) -> Json {
    let reports: JsonMap<String, Json> = reports
        .into_iter()
        .map(|(r, bs)| (r.name.clone(), report(project, r, &bs, errors.get(&r.name))))
        .collect();
    let plugins: Vec<Json> = project
        .plugins
        .iter()
        .map(|p| {
            let (kind, location) = match &p.source {
                PluginSource::Default => ("registry", None),
                PluginSource::Registry(u) => ("registry", Some(u.clone())),
                PluginSource::Github(r) => ("github", Some(r.clone())),
                PluginSource::Local(l) => ("local", Some(l.clone())),
            };
            let mut source = JsonMap::new();
            source.insert("type".into(), json!(kind));
            if let Some(l) = location {
                source.insert("location".into(), json!(l));
            }
            json!({"package": p.name, "version": p.version, "source": source})
        })
        .collect();
    let mut proj = JsonMap::new();
    proj.insert("name".into(), json!(project.name));
    insert_some(&mut proj, "default_profile", project.default_profile.as_ref());
    insert_some(&mut proj, "timezone", project.timezone.as_ref());
    proj.insert("checksum".into(), json!(project_checksum(project)));
    let doc = json!({
        "schema": SCHEMA,
        "version": crate::version(),
        "project": proj,
        "reports": reports,
        "schedules": schedules,
        "plugins": plugins,
    });
    sorted(doc)
}

fn report(project: &Project, r: &Report, bindings: &[&Binding], errors: Option<&Vec<String>>) -> Json {
    let mut m = JsonMap::new();
    m.insert("name".into(), json!(r.name));
    m.insert("managed".into(), json!(r.managed));
    m.insert("file".into(), json!(slash(&r.file)));
    m.insert("folder".into(), json!(r.folder));
    m.insert("tags".into(), json!(r.tags));
    insert_some(&mut m, "timezone", r.timezone.as_ref());
    insert_some(&mut m, "default_set", r.default_set.as_ref());
    m.insert(
        "queries".into(),
        json!(r.queries.iter().map(query).collect::<Vec<_>>()),
    );
    m.insert("checksum".into(), json!(report_checksum(project, r)));
    let errors = errors.cloned().unwrap_or_default();
    m.insert("valid".into(), json!(errors.is_empty()));
    if !errors.is_empty() {
        m.insert("errors".into(), json!(errors));
    }
    m.insert(
        "bindings".into(),
        json!(bindings.iter().map(|b| binding(b)).collect::<Vec<_>>()),
    );
    Json::Object(m)
}

fn query(q: &QueryEntry) -> Json {
    let mut m = JsonMap::new();
    m.insert("query".into(), json!(q.query));
    m.insert("file".into(), json!(slash(&q.path)));
    m.insert("tab".into(), json!(q.tab));
    insert_some(&mut m, "tab_name", q.tab_name.as_ref());
    insert_some(&mut m, "anchor", q.anchor.as_ref());
    insert_some(&mut m, "header", q.header.as_ref());
    if !q.columns.is_empty() {
        m.insert(
            "columns".into(),
            serde_json::to_value(&q.columns).unwrap_or(Json::Null),
        );
    }
    Json::Object(m)
}

fn binding(b: &Binding) -> Json {
    let mut output = JsonMap::new();
    output.insert("format".into(), json!(b.output.format));
    output.insert("options".into(), Json::Object(b.output.options.clone()));
    insert_some(&mut output, "extension", b.output.extension.as_ref());
    if let Some(t) = &b.output.template {
        output.insert("template".into(), json!(t.file));
    }
    let destinations: Vec<Json> = b
        .output
        .destinations
        .iter()
        .map(|d| {
            let mut m = JsonMap::new();
            m.insert("profile".into(), json!(d.profile));
            insert_some(&mut m, "path", d.path.as_ref());
            Json::Object(m)
        })
        .collect();
    let mut m = JsonMap::new();
    m.insert("set".into(), json!(b.set));
    m.insert("profile".into(), json!(b.profile));
    m.insert("vars".into(), Json::Object(b.vars.clone()));
    m.insert(
        "queries".into(),
        json!(b.queries.iter().map(query).collect::<Vec<_>>()),
    );
    m.insert("output".into(), Json::Object(output));
    m.insert("destinations".into(), json!(destinations));
    m.insert("schedules".into(), json!(b.schedules));
    Json::Object(m)
}

fn schedule(project: &Project, name: &str) -> Json {
    let Some(e) = project.schedules.iter().find(|e| e.name == name) else {
        return Json::Null;
    };
    let bindings: Vec<Json> = project
        .reports
        .iter()
        .flat_map(|r| {
            r.bindings
                .iter()
                .filter(|b| b.schedules.iter().any(|s| s == name))
                .map(|b| json!({"report": r.name, "set": b.set}))
        })
        .collect();
    let mut m = JsonMap::new();
    m.insert("name".into(), json!(e.name));
    insert_some(&mut m, "report", e.report.as_ref());
    insert_some(&mut m, "select", e.select.as_ref());
    insert_some(&mut m, "set", e.set.as_ref());
    m.insert("schedule".into(), Json::Object(e.schedule.clone()));
    m.insert("vars".into(), Json::Object(e.vars.clone()));
    insert_some(&mut m, "timezone", e.timezone.as_ref());
    m.insert("bindings".into(), json!(bindings));
    Json::Object(m)
}

fn insert_some<T: serde::Serialize>(m: &mut JsonMap<String, Json>, key: &str, v: Option<&T>) {
    if let Some(v) = v {
        m.insert(key.into(), serde_json::to_value(v).unwrap_or(Json::Null));
    }
}

fn slash(p: &Path) -> String {
    crate::slash(p).to_string_lossy().into_owned()
}

/// `v` as compact JSON with every object's keys sorted: the same bytes for the same value.
pub fn canonical(v: &Json) -> String {
    serde_json::to_string(&sorted(v.clone())).unwrap()
}

/// `v` with every object's keys sorted, so the bytes don't depend on insertion order.
fn sorted(v: Json) -> Json {
    match v {
        Json::Object(m) => {
            let mut entries: Vec<(String, Json)> = m.into_iter().collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            Json::Object(entries.into_iter().map(|(k, v)| (k, sorted(v))).collect())
        }
        Json::Array(a) => Json::Array(a.into_iter().map(sorted).collect()),
        v => v,
    }
}

// -- checksums --------------------------------------------------------------------------------

/// SHA-256 (hex) over `files` (relative to the root), in sorted order, each as its path and
/// contents with a fixed separator; a missing file hashes as its path alone.
fn checksum(root: &Path, files: &BTreeSet<String>) -> String {
    let mut h = Sha256::new();
    for f in files {
        h.update(f.as_bytes());
        h.update([0u8]);
        match std::fs::read(root.join(f)) {
            Ok(bytes) => {
                h.update([1u8]);
                h.update((bytes.len() as u64).to_le_bytes());
                h.update(&bytes);
            }
            Err(_) => h.update([0u8]),
        }
    }
    hex(&h.finalize())
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A report's own files: its defining file, every query file of any Binding, and templates.
fn report_files(project: &Project, r: &Report) -> BTreeSet<String> {
    let mut files = BTreeSet::new();
    files.insert(slash(&r.file));
    for q in r
        .queries
        .iter()
        .chain(r.bindings.iter().flat_map(|b| b.queries.iter()))
    {
        files.insert(slash(&q.path));
    }
    for t in r.bindings.iter().filter_map(|b| b.output.template.as_ref()) {
        files.insert(template_file(&project.root, &t.file));
    }
    files
}

/// Where a template file is read from (the run's rule): the root, else `templates/`.
fn template_file(root: &Path, file: &str) -> String {
    let direct = Path::new(file);
    let under = Path::new("templates").join(file);
    if !root.join(direct).is_file() && root.join(&under).is_file() {
        slash(&under)
    } else {
        slash(direct)
    }
}

fn report_checksum(project: &Project, r: &Report) -> String {
    checksum(&project.root, &report_files(project, r))
}

/// The shared inputs: `dre_project.yml`, folder config, `schedules.yml`, `dependencies.yml` and
/// other YAML that isn't a report's own, `macros/`, `lookups/`, and every `.sql` under
/// `reports/` that isn't a declared query (the usual `ref()` targets).
fn project_checksum(project: &Project) -> String {
    // An unmanaged report's `.sql` stays in: it's a `ref()` target like any other.
    let owned: BTreeSet<String> = project
        .reports
        .iter()
        .flat_map(|r| {
            let mut files = report_files(project, r);
            if !r.managed {
                files.remove(&slash(&r.file));
            }
            files
        })
        .collect();
    let files: BTreeSet<String> = project
        .sources
        .iter()
        .map(|p| slash(p))
        .filter(|p| !owned.contains(p))
        .filter(|p| {
            let p = Path::new(p);
            let yaml = matches!(p.extension().and_then(|e| e.to_str()), Some("yml" | "yaml"));
            yaml || p.starts_with(MACROS_DIR)
                || p.starts_with(LOOKUPS_DIR)
                || (p.starts_with(REPORTS_DIR) && p.extension().is_some_and(|e| e == "sql"))
        })
        .collect();
    checksum(&project.root, &files)
}

// -- errors -----------------------------------------------------------------------------------

/// The error diagnostics that belong to a report: those in one of its own files.
pub fn report_errors(project: &Project, diags: &Diagnostics) -> ReportErrors {
    let mut out = ReportErrors::new();
    for r in &project.reports {
        let files = report_files(project, r);
        let msgs: Vec<String> = diags
            .sorted()
            .into_iter()
            .filter(|d| d.severity == Severity::Error)
            .filter(|d| d.file.as_ref().is_some_and(|f| files.contains(&slash(f))))
            .map(|d| d.to_string())
            .collect();
        if !msgs.is_empty() {
            out.insert(r.name.clone(), msgs);
        }
    }
    out
}

// -- writing ----------------------------------------------------------------------------------

/// The manifest's text: pretty JSON, secrets masked, newline-terminated.
pub fn render(doc: &Json) -> String {
    crate::secrets::to_json_pretty(doc).unwrap() + "\n"
}

/// Write `project`'s manifest atomically (a temp file in the target folder, then a rename).
/// Returns the SHA-256 of the bytes written.
pub fn write(project: &Project, errors: &ReportErrors) -> Result<String, String> {
    let text = render(&build(project, errors));
    let dir = &project.target_dir;
    crate::target::ensure(dir).map_err(|e| format!("can't create {}: {e}", dir.display()))?;
    let dst = dir.join(FILE);
    let tmp = dir.join(format!(".{FILE}.{}.tmp", std::process::id()));
    let write = || -> std::io::Result<()> {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, &dst)
    };
    if let Err(e) = write() {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("can't write {}: {e}", dst.display()));
    }
    Ok(hex(&Sha256::digest(text.as_bytes())))
}

/// Remove a stale manifest from `target_dir` (the project didn't load).
pub fn remove(target_dir: &Path) {
    let _ = std::fs::remove_file(target_dir.join(FILE));
}
