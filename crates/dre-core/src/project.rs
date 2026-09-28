//! Loading a DRE project directory into a resolved, validated project.
//!
//! Every YAML file in the project is parsed and classified by its shape, not its name. Report
//! config merges by report name from anywhere, then resolves with one precedence rule:
//! Binding > report YAML > folder config (deepest wins) > project defaults > built-in defaults.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use serde::Serialize;
use serde_json::{Map as JsonMap, Value as Json};
use serde_yaml_ng::{Mapping, Value};

use crate::dates::{WeekNumbering, WeekStart};
use crate::diag::Diagnostics;
use crate::lookups::{self, DEFAULT_INLINE_MAX_ROWS, LOOKUPS_DIR, Lookup};
use crate::packages::{self, DispatchOrder, Package};

/// Names DRE puts in every Jinja context; a package can't take one.
const RESERVED_NAMES: &[&str] = &[
    "run",
    "var",
    "env_var",
    "run_query",
    "columns",
    "ref",
    "lookup",
    "dispatch",
    "target",
    "profile",
    "date",
    "datetime",
    "date_range",
    "month_of",
    "quarter_of",
    "year_of",
    "week_of",
    "period",
    "raise_error",
];
use crate::profiles::{LOCAL_TYPE, Profiles, Role};
use crate::yaml::YamlFile;
use crate::{constraints, options, preflight, schedule, selector, sqlsplit};

pub const PROJECT_FILE: &str = "dre_project.yml";
pub const REPORTS_DIR: &str = "reports";
pub const MACROS_DIR: &str = "macros";
pub const TARGET_DIR: &str = "target";
pub const LOGS_DIR: &str = "logs";
pub use crate::plugins::DEPS_DIR;
pub const DEFAULT_RUN_QUERY_MAX_ROWS: u64 = 10_000;

const REPORT_KEYS: &[&str] = &[
    "name",
    "tags",
    "queries",
    "output",
    "profile",
    "sets",
    "default_set",
    "schedule",
    "vars",
    "timezone",
];
/// Declares the project's plugin packages, in any project YAML file.
const PLUGINS_KEY: &str = "plugins";
const PLUGIN_KEYS: &[&str] = &[PLUGINS_KEY];
/// Where plugins were declared before packages; now an error pointing at `plugins:`.
const OLD_PLUGIN_KEYS: &[&str] = &["sources", "destinations", "formats"];
const PROJECT_KEYS: &[&str] = &[
    "name",
    "default_profile",
    "default_output",
    "format_options",
    "default_set",
    "vars",
    "run_query_max_rows",
    "lookup_inline_max_rows",
    "dispatch",
    "mask_secrets",
    "timezone",
    "week_start",
    "week_numbering",
    "reports",
];
const FOLDER_CONFIG_KEYS: &[&str] = &["+tags", "+output", "+profile", "+schedule", "+vars", "+timezone"];
const SET_ENTRY_KEYS: &[&str] = &[
    "name",
    "profile",
    "vars",
    "exclude",
    "queries",
    "tab_names",
    "output",
    "schedule",
];
const QUERY_ENTRY_KEYS: &[&str] = &["query", "tab", "tab_name", "anchor", "header"];
const OUTPUT_SHARED_KEYS: &[&str] = &["format", "destination", "template", "extension"];

// ---------------------------------------------------------------------------------------------
// The resolved project: the stable contract every later consumer uses.
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct Project {
    pub name: String,
    #[serde(skip)]
    pub root: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_profile: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_set: Option<String>,
    pub vars: JsonMap<String, Json>,
    pub run_query_max_rows: u64,
    /// `format_options:`: per format, defaults under every output of that format.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub format_options: BTreeMap<String, JsonMap<String, Json>>,
    pub reports: Vec<Report>,
    pub sets: BTreeMap<String, SetDef>,
    /// The declared plugin packages.
    pub plugins: Vec<PluginRequirement>,
    /// Every plugin the project uses, checked against the declared packages once they're
    /// installed ([`crate::plugins::check_uses`]).
    #[serde(skip)]
    pub plugin_uses: Vec<PluginUse>,
    /// Some package's declarations conflict, so it's missing from `plugins`.
    #[serde(skip)]
    pub plugins_incomplete: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub schedules: Vec<ScheduleEntry>,
    /// Macro files under `macros/`, relative to the root.
    pub macros: Vec<PathBuf>,
    /// Macro packages, each called through its name (`{{ dre_utils.x() }}`).
    pub packages: Vec<Package>,
    /// Mask `DRE_SECRET_*` values in logs and records (default true).
    pub mask_secrets: bool,
    /// The project's default `timezone:` (IANA name); runs default to UTC without one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
    /// `week_start:` and `week_numbering:`, for the calendar functions.
    #[serde(skip)]
    pub week_start: WeekStart,
    #[serde(skip)]
    pub week_numbering: WeekNumbering,
    /// `dispatch:` search orders, by macro namespace.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub dispatch: DispatchOrder,
    /// Every uniquely named `.sql` file under `reports/`, by basename: what `ref()` resolves.
    #[serde(skip)]
    pub sql: BTreeMap<String, PathBuf>,
    /// Lookups under `lookups/`, which `ref()` also resolves.
    #[serde(skip)]
    pub lookups: BTreeMap<String, Lookup>,
    /// A lookup with more rows than this is loaded into a temp table rather than inlined.
    pub lookup_inline_max_rows: u64,
    /// Every folder under `reports/`, as path segments.
    #[serde(skip)]
    pub folders: Vec<Vec<String>>,
    #[serde(skip)]
    pub profiles: Profiles,
}

impl Project {
    pub fn report(&self, name: &str) -> Option<&Report> {
        self.reports.iter().find(|r| r.name == name)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub name: String,
    pub managed: bool,
    /// The defining YAML (managed) or `.sql` (unmanaged), relative to the root.
    pub file: PathBuf,
    /// Folder under `reports/` holding `file`, as path segments.
    pub folder: Vec<String>,
    pub tags: Vec<String>,
    pub queries: Vec<QueryEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_set: Option<String>,
    /// The report's `timezone:`, else its folder config's `+timezone`, else the project's.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
    /// Whether the report declares `sets:` (a Binding per Set) or runs as a single Binding.
    pub has_sets: bool,
    pub bindings: Vec<Binding>,
    /// Report-level resolution with no Set applied: the starting point for an ad hoc `--set`.
    #[serde(skip)]
    pub base: Binding,
}

impl Report {
    pub fn binding(&self, set: &str) -> Option<&Binding> {
        self.bindings.iter().find(|b| b.set.as_deref() == Some(set))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct QueryEntry {
    pub query: String,
    /// The `.sql` file, relative to the root.
    pub path: PathBuf,
    /// Whether this query's result becomes a tab (a sheet, or a file for single-table formats).
    /// One .sql file makes at most one tab: its last statement's result. `tab: false` runs the
    /// file only for its effects (temp views, `SET`s) and discards any result.
    #[serde(skip_serializing_if = "is_true")]
    pub tab: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tab_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anchor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub header: Option<bool>,
}

fn is_true(b: &bool) -> bool {
    *b
}

/// The message for a `tab_name` list: one .sql file makes one tab.
fn one_tab_per_file(name: &str) -> String {
    format!(
        "`tab_name` of `{name}` is a list, but one .sql file makes one tab; put each tab's query in its own .sql file and list each in `queries:`"
    )
}

/// A Report paired with a Set (or the report's single default Binding): everything a run needs.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Binding {
    /// The Set name; `None` for a report without `sets:`.
    pub set: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// Fully merged vars: project < folders < report < Set registry < inline Binding.
    pub vars: JsonMap<String, Json>,
    pub queries: Vec<QueryEntry>,
    pub output: Output,
    /// Names of every `schedules.yml` entry that runs this Binding.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub schedules: Vec<String>,
}

impl Binding {
    /// Directory name for this Binding under `target/`.
    pub fn dir_name(&self) -> &str {
        self.set.as_deref().unwrap_or("default")
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Output {
    pub format: String,
    /// Format options: every key except `format`, `destination` and `template`.
    pub options: JsonMap<String, Json>,
    /// Where the output is delivered, in order. Empty: it stays in `target/`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub destinations: Vec<Destination>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub template: Option<Template>,
    /// `extension:` replaces the format's file extension in the default file name
    /// (`<report>.<extension>`); `Some("")` means no extension. `None`: the format's own.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extension: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Destination {
    pub profile: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Plugin options: every key other than `profile` and `path`, passed to the plugin after
    /// rendering.
    #[serde(skip_serializing_if = "JsonMap::is_empty")]
    pub options: JsonMap<String, Json>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Template {
    pub file: String,
    pub bindings: Vec<TemplateBinding>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TemplateBinding {
    pub sheet: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result_index: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anchor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub header: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub columns: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cell: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SetDef {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    pub vars: JsonMap<String, Json>,
}

/// A plugin's kind; the same type the protocol uses.
pub use dre_protocol::Kind as PluginKind;
pub use dre_protocol::PluginId;

/// A declared plugin package.
#[derive(Debug, Clone, Serialize)]
pub struct PluginRequirement {
    /// The package's name.
    pub name: String,
    /// The intersection of every declared constraint.
    pub version: String,
    /// Files declaring it, relative to the root.
    pub declared_in: Vec<PathBuf>,
    /// Where it's installed from.
    #[serde(skip_serializing_if = "PluginSource::is_default")]
    pub source: PluginSource,
}

/// A plugin the project uses: a profile's `type:` or an output's `format:`.
#[derive(Debug, Clone, Serialize)]
pub struct PluginUse {
    pub plugin: PluginId,
    pub file: Option<PathBuf>,
    pub line: Option<usize>,
    /// What uses it, for messages: "`type: s3` used by destination profile `reports`".
    pub what: String,
}

/// Where a plugin comes from: `dependencies.yml`'s `registry:`, `github:` or `local:`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginSource {
    /// The default registry (`DRE_REGISTRY_URL`, else DRE's own).
    #[default]
    Default,
    /// Another registry index: a URL or a file path.
    Registry(String),
    /// The GitHub Releases of `owner/repo`.
    Github(String),
    /// An executable on disk, relative to the project root. Used in place, never installed.
    Local(String),
}

impl PluginSource {
    pub fn is_default(&self) -> bool {
        *self == PluginSource::Default
    }

    /// How `dre.lock` records it; `None` for the default registry.
    pub fn lock_key(&self) -> Option<String> {
        match self {
            PluginSource::Default => None,
            PluginSource::Registry(u) => Some(format!("registry:{u}")),
            PluginSource::Github(r) => Some(format!("github:{r}")),
            PluginSource::Local(p) => Some(format!("local:{p}")),
        }
    }
}

impl PluginRequirement {
    pub fn req(&self) -> semver::VersionReq {
        semver::VersionReq::parse(&self.version).unwrap_or(semver::VersionReq::STAR)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ScheduleEntry {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub select: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub report: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub set: Option<String>,
    pub schedule: JsonMap<String, Json>,
    /// Layered into `var()` when run with `--schedule <name>`, above the Binding's own vars.
    #[serde(skip_serializing_if = "JsonMap::is_empty")]
    pub vars: JsonMap<String, Json>,
    /// The run's timezone under `--schedule <name>`, above the report's.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
    /// Line in its schedules.yml, for messages.
    #[serde(skip)]
    pub location: (PathBuf, Option<usize>),
}

// ---------------------------------------------------------------------------------------------
// Loading
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct LoadOptions {
    /// `--profiles-dir`; falls back to `DRE_PROFILES_DIR`, the project directory, then `~/.dre`.
    pub profiles_dir: Option<PathBuf>,
    /// `--target`, checked against referenced source profiles.
    pub target: Option<String>,
    /// `--var name=value`, the top of every `var()` chain.
    pub vars: BTreeMap<String, String>,
}

/// Parse and validate the project at `root`. Returns the resolved project when it could be
/// built at all, plus every diagnostic found along the way.
pub fn load(root: &Path, opts: &LoadOptions) -> (Option<Project>, Diagnostics) {
    let mut l = Loader {
        root: root.to_path_buf(),
        opts: opts.clone(),
        diags: Diagnostics::default(),
        format_options: Mapping::new(),
    };
    let project = l.run();
    (project, l.diags)
}

struct Loader {
    root: PathBuf,
    opts: LoadOptions,
    diags: Diagnostics,
    /// `format_options:` from the project file: per format, defaults under every output of it.
    format_options: Mapping,
}

/// One report YAML fragment, before merging.
struct Fragment {
    file: Rc<YamlFile>,
    name: String,
    explicit_name: bool,
    folder: Vec<String>,
    map: Mapping,
}

/// Folder-level config from `reports:` in `dre_project.yml`.
#[derive(Default, Clone)]
struct FolderCfg {
    tags: Vec<String>,
    output: Option<Mapping>,
    profile: Option<(String, Option<usize>)>,
    vars: Option<Mapping>,
    timezone: Option<String>,
}

struct Discovered {
    yaml: Vec<PathBuf>,
    /// `.sql` under `reports/`, relative paths.
    sql: Vec<PathBuf>,
    macros: Vec<PathBuf>,
    /// Files under `lookups/`, relative paths.
    lookups: Vec<PathBuf>,
    folders: Vec<Vec<String>>,
}

/// Where a report-level value came from, for error messages.
struct Located<T> {
    value: T,
    file: Rc<YamlFile>,
}

impl Loader {
    fn rel(&self, p: &Path) -> PathBuf {
        crate::slash(p.strip_prefix(&self.root).unwrap_or(p))
    }

    fn run(&mut self) -> Option<Project> {
        let pfile = self.root.join(PROJECT_FILE);
        if !pfile.is_file() {
            self.diags.error(
                "project-file-missing",
                Some(PathBuf::from(PROJECT_FILE)),
                None,
                format!(
                    "no {PROJECT_FILE} found in {}",
                    crate::slash(&self.root).display()
                ),
            );
            return None;
        }
        let pyaml = Rc::new(YamlFile::load(
            &pfile,
            PathBuf::from(PROJECT_FILE),
            &mut self.diags,
        )?);
        let mut project = self.parse_project_file(&pyaml)?;

        let found = self.discover();
        project.folders = found.folders.clone();
        project.macros = found.macros.clone();
        let declared = packages::declared(&self.root, &mut self.diags);
        project.packages = packages::resolve(&self.root, &declared, &mut self.diags);
        self.check_macro_namespaces(&project);

        let (profiles_dir, found_by) =
            crate::profiles::locate(self.opts.profiles_dir.as_deref(), Some(&self.root));
        project.profiles = Profiles::load(&profiles_dir, found_by, &mut self.diags);

        // Folder config needs the folder list to warn about folders that don't exist.
        let folder_cfg = self.parse_folder_config(&pyaml, &project.folders);
        let mut used = Usage::default();
        if let Some(p) = &project.default_profile {
            used.source(
                p,
                Some(pyaml.display.clone()),
                pyaml.line_of("default_profile", None),
            );
        }
        for cfg in folder_cfg.values() {
            if let Some((p, line)) = &cfg.profile {
                used.source(p, Some(pyaml.display.clone()), *line);
            }
        }

        // Classify every YAML file by shape.
        let mut fragments = Vec::new();
        let mut plugin_decls: Vec<(Rc<YamlFile>, Mapping)> = Vec::new();
        let mut set_files = Vec::new();
        let mut schedule_files = Vec::new();
        plugin_decls.push((pyaml.clone(), pick(&pyaml.value, PLUGIN_KEYS)));
        for path in &found.yaml {
            let display = self.rel(path);
            let Some(yf) = YamlFile::load(path, display.clone(), &mut self.diags) else {
                continue;
            };
            let yf = Rc::new(yf);
            self.classify(
                yf,
                &mut fragments,
                &mut plugin_decls,
                &mut set_files,
                &mut schedule_files,
            );
        }

        project.sets = self.parse_sets(&set_files);
        let sql_index = self.index_sql(&found.sql);
        project.sql = sql_index
            .iter()
            .filter(|(_, paths)| paths.len() == 1)
            .map(|(name, paths)| (name.clone(), paths[0].clone()))
            .collect();
        // Queries resolve by bare basename; whatever no report YAML references is unmanaged.
        let mut referenced: BTreeSet<String> = fragments
            .iter()
            .filter_map(|f| f.map.get("queries").and_then(Value::as_sequence))
            .flatten()
            .filter_map(entry_name)
            .collect();
        project.lookups = lookups::discover(&self.root, &found.lookups, &mut self.diags);
        for (name, l) in &project.lookups {
            if let Some(sql) = sql_index.get(name) {
                self.diags.error(
                    "duplicate-ref-name",
                    Some(l.file.clone()),
                    None,
                    format!(
                        "lookup `{name}` has the same name as {}; `ref()` names must be unique",
                        sql[0].display()
                    ),
                );
            }
            if let Err(e) = lookups::read(&self.root, l) {
                self.diags.error("invalid-lookup", Some(l.file.clone()), None, e);
            }
        }
        // A file used through `ref()` is shared SQL, not an unmanaged report.
        referenced.extend(self.check_refs(&found.sql, &found.macros, &sql_index, &project.lookups));
        let mut reports = self.merge_reports(fragments);

        let mut built = Vec::new();
        for r in reports.iter_mut() {
            let queries = self.resolve_queries(r, &sql_index, &mut referenced);
            built.push((queries, r));
        }
        let mut resolved: Vec<Report> = Vec::new();
        for (queries, raw) in built {
            if let Some(rep) = self.resolve_managed(raw, queries, &project, &folder_cfg, &mut used) {
                resolved.push(rep);
            }
        }
        let managed_names: BTreeMap<String, PathBuf> = resolved
            .iter()
            .map(|r| (r.name.clone(), r.file.clone()))
            .collect();
        for (name, path) in &sql_index {
            if referenced.contains(name) || path.len() != 1 {
                continue;
            }
            let path = &path[0];
            if let Some(other) = managed_names.get(name) {
                self.diags.error(
                    "duplicate-report-name",
                    Some(path.clone()),
                    None,
                    format!(
                        "unmanaged report `{name}` has the same name as the report declared in {}; report names must be unique project-wide",
                        other.display()
                    ),
                );
                continue;
            }
            resolved.push(self.resolve_unmanaged(name, path, &project, &folder_cfg, &mut used));
        }
        resolved.sort_by(|a, b| a.name.cmp(&b.name));
        project.reports = resolved;

        self.check_profiles(&project, &used);
        self.check_plugins(&plugin_decls, &mut project, &used);
        project.schedules = self.parse_schedules(&schedule_files, &project);
        self.apply_schedules(&mut project);
        self.preflight(&project, &used);
        self.check_template_files(&project);

        Some(project)
    }

    // -- dre_project.yml ----------------------------------------------------------------------

    fn parse_project_file(&mut self, yf: &Rc<YamlFile>) -> Option<Project> {
        let file = Some(yf.display.clone());
        let Some(m) = yf.value.as_mapping() else {
            self.diags.error(
                "invalid-project",
                file,
                None,
                format!("{PROJECT_FILE} must be a map"),
            );
            return None;
        };
        for k in m.keys().filter_map(Value::as_str) {
            if OLD_PLUGIN_KEYS.contains(&k) {
                self.old_plugin_key(yf, k);
            } else if !PROJECT_KEYS.contains(&k) && !PLUGIN_KEYS.contains(&k) {
                self.diags.error(
                    "unknown-key",
                    file.clone(),
                    yf.line_of(k, None),
                    format!("unknown key `{k}`"),
                );
            }
        }
        let name = match m.get("name") {
            Some(Value::String(s)) if !s.trim().is_empty() => Some(s.clone()),
            Some(_) => {
                self.diags.error(
                    "invalid-field",
                    file.clone(),
                    yf.line_of("name", None),
                    "`name` must be a non-empty string",
                );
                None
            }
            None => {
                self.diags.error(
                    "missing-field",
                    file.clone(),
                    None,
                    "missing required field `name`",
                );
                None
            }
        };
        let default_profile = self.opt_string(yf, m, "default_profile");
        let default_set = self.opt_string(yf, m, "default_set");
        let vars = self.opt_vars(yf, m.get("vars"), "vars");
        let run_query_max_rows = match m.get("run_query_max_rows") {
            None => DEFAULT_RUN_QUERY_MAX_ROWS,
            Some(v) => match v.as_u64() {
                Some(n) if n > 0 => n,
                _ => {
                    self.diags.error(
                        "invalid-field",
                        file.clone(),
                        yf.line_of("run_query_max_rows", None),
                        "`run_query_max_rows` must be a positive whole number",
                    );
                    DEFAULT_RUN_QUERY_MAX_ROWS
                }
            },
        };
        let dispatch = self.parse_dispatch(yf, m.get("dispatch"));
        match m.get("format_options") {
            None | Some(Value::Null) => {}
            Some(Value::Mapping(f)) if f.values().all(|v| v.is_mapping()) => self.format_options = f.clone(),
            Some(_) => self.diags.error(
                "invalid-field",
                file.clone(),
                yf.line_of("format_options", None),
                "`format_options` must map format names to their options, e.g. `delimited: {delimiter: \"|\"}`",
            ),
        }
        let mask_secrets = match m.get("mask_secrets") {
            None => true,
            Some(Value::Bool(b)) => *b,
            Some(_) => {
                self.diags.error(
                    "invalid-field",
                    file.clone(),
                    yf.line_of("mask_secrets", None),
                    "`mask_secrets` must be true or false",
                );
                true
            }
        };
        let timezone = match m.get("timezone") {
            None => None,
            Some(v) => self.timezone_value(v, &yf.display, yf.line_of("timezone", None), "`timezone`"),
        };
        let week_start = match m.get("week_start") {
            None => WeekStart::Monday,
            Some(v) => v.as_str().and_then(WeekStart::parse).unwrap_or_else(|| {
                self.diags.error(
                    "invalid-field",
                    file.clone(),
                    yf.line_of("week_start", None),
                    "`week_start` must be `monday` or `sunday`",
                );
                WeekStart::Monday
            }),
        };
        let week_numbering = match m.get("week_numbering") {
            None => WeekNumbering::Iso,
            Some(v) => v.as_str().and_then(WeekNumbering::parse).unwrap_or_else(|| {
                self.diags.error(
                    "invalid-field",
                    file.clone(),
                    yf.line_of("week_numbering", None),
                    "`week_numbering` must be `iso` or `us`",
                );
                WeekNumbering::Iso
            }),
        };
        let lookup_inline_max_rows = match m.get("lookup_inline_max_rows") {
            None => DEFAULT_INLINE_MAX_ROWS,
            Some(v) => match v.as_u64() {
                Some(n) => n,
                _ => {
                    self.diags.error(
                        "invalid-field",
                        file.clone(),
                        yf.line_of("lookup_inline_max_rows", None),
                        "`lookup_inline_max_rows` must be a whole number",
                    );
                    DEFAULT_INLINE_MAX_ROWS
                }
            },
        };
        Some(Project {
            name: name?,
            root: self.root.clone(),
            default_profile,
            default_set,
            vars,
            run_query_max_rows,
            reports: Vec::new(),
            sets: BTreeMap::new(),
            plugins: Vec::new(),
            plugin_uses: Vec::new(),
            plugins_incomplete: false,
            schedules: Vec::new(),
            macros: Vec::new(),
            packages: Vec::new(),
            mask_secrets,
            timezone,
            week_start,
            week_numbering,
            dispatch,
            sql: BTreeMap::new(),
            lookups: BTreeMap::new(),
            lookup_inline_max_rows,
            format_options: self
                .format_options
                .iter()
                .filter_map(|(k, v)| Some((k.as_str()?.to_string(), yaml_map_to_json(v.as_mapping()?))))
                .collect(),
            folders: Vec::new(),
            profiles: Profiles::default(),
        })
    }

    /// A `timezone:` value: an IANA name, else an error at `line`.
    fn timezone_value(&mut self, v: &Value, file: &Path, line: Option<usize>, what: &str) -> Option<String> {
        let msg = match v.as_str() {
            Some(s) => match crate::dates::parse_tz(s) {
                Ok(_) => return Some(s.to_string()),
                Err(e) => format!("{what}: {e}"),
            },
            None => format!("{what} must be a string, an IANA timezone name such as `Australia/Sydney`"),
        };
        self.diags
            .error("invalid-timezone", Some(file.to_path_buf()), line, msg);
        None
    }

    fn opt_string(&mut self, yf: &YamlFile, m: &Mapping, key: &str) -> Option<String> {
        match m.get(key)? {
            Value::String(s) => Some(s.clone()),
            _ => {
                self.diags.error(
                    "invalid-field",
                    Some(yf.display.clone()),
                    yf.line_of(key, None),
                    format!("`{key}` must be a string"),
                );
                None
            }
        }
    }

    /// `dispatch: [{macro_namespace: dre_utils, search_order: [my_project, dre_utils]}]`.
    fn parse_dispatch(&mut self, yf: &YamlFile, v: Option<&Value>) -> DispatchOrder {
        let mut out = DispatchOrder::new();
        let Some(v) = v else { return out };
        let line = yf.line_of("dispatch", None);
        let Some(list) = v.as_sequence() else {
            self.diags.error(
                "invalid-field",
                Some(yf.display.clone()),
                line,
                "`dispatch` must be a list of `{macro_namespace, search_order}`",
            );
            return out;
        };
        for e in list {
            let ns = e.get("macro_namespace").and_then(Value::as_str);
            let order: Option<Vec<String>> = e
                .get("search_order")
                .and_then(Value::as_sequence)
                .map(|s| s.iter().filter_map(|x| x.as_str().map(str::to_string)).collect());
            match (ns, order) {
                (Some(ns), Some(order)) if !order.is_empty() => {
                    out.insert(ns.to_string(), order);
                }
                _ => self.diags.error(
                    "invalid-field",
                    Some(yf.display.clone()),
                    line,
                    "each `dispatch` entry needs `macro_namespace` and a non-empty `search_order` list",
                ),
            }
        }
        out
    }

    /// Package names are Jinja variables: they can't clash with each other's macros, the
    /// project's own macros, or DRE's functions.
    fn check_macro_namespaces(&mut self, project: &Project) {
        let own: BTreeSet<String> = project
            .macros
            .iter()
            .filter_map(|m| std::fs::read_to_string(self.root.join(m)).ok())
            .flat_map(|src| preflight::macro_defs(&src).into_iter().map(|d| d.name))
            .collect();
        for p in &project.packages {
            let clash = if RESERVED_NAMES.contains(&p.name.as_str()) {
                Some("a DRE function or variable".to_string())
            } else if own.contains(&p.name) {
                Some("a macro in macros/".to_string())
            } else if p.name == project.name {
                Some("this project".to_string())
            } else {
                None
            };
            if let Some(c) = clash {
                self.diags.error(
                    "package-name-clash",
                    None,
                    None,
                    format!(
                        "package `{}` has the same name as {c}; macros couldn't be called through it",
                        p.name
                    ),
                );
            }
        }
        for (ns, order) in &project.dispatch {
            for n in order {
                if n != &project.name && !project.packages.iter().any(|p| &p.name == n) {
                    self.diags.error(
                        "invalid-field",
                        Some(PathBuf::from(PROJECT_FILE)),
                        None,
                        format!("`dispatch` for `{ns}` searches `{n}`, which is neither this project nor an installed package"),
                    );
                }
            }
        }
    }

    fn opt_vars(&mut self, yf: &YamlFile, v: Option<&Value>, key: &str) -> JsonMap<String, Json> {
        match v {
            None | Some(Value::Null) => JsonMap::new(),
            Some(Value::Mapping(m)) => yaml_map_to_json(m),
            Some(_) => {
                self.diags.error(
                    "invalid-field",
                    Some(yf.display.clone()),
                    yf.line_of(key, None),
                    format!("`{key}` must be a map"),
                );
                JsonMap::new()
            }
        }
    }

    fn parse_folder_config(
        &mut self,
        yf: &Rc<YamlFile>,
        folders: &[Vec<String>],
    ) -> BTreeMap<Vec<String>, FolderCfg> {
        let mut out = BTreeMap::new();
        let Some(tree) = yf.value.get("reports") else {
            return out;
        };
        let mut stack = vec![(Vec::<String>::new(), tree.clone(), yf.line_of("reports", None))];
        while let Some((path, node, line)) = stack.pop() {
            let Some(m) = node.as_mapping() else {
                self.diags.error(
                    "invalid-folder-config",
                    Some(yf.display.clone()),
                    line,
                    format!("folder config for `{}` must be a map", dotted(&path)),
                );
                continue;
            };
            let mut cfg = FolderCfg::default();
            let mut any = false;
            for (k, v) in m {
                let Some(k) = k.as_str() else { continue };
                let kline = yf.line_of(k, line);
                if let Some(key) = k.strip_prefix('+') {
                    any = true;
                    if !FOLDER_CONFIG_KEYS.contains(&k) {
                        self.diags.error(
                            "unknown-key",
                            Some(yf.display.clone()),
                            kline,
                            format!(
                                "unknown folder config `{k}`; folder config keys are {}",
                                FOLDER_CONFIG_KEYS.join(", ")
                            ),
                        );
                        continue;
                    }
                    let bad = |s: &mut Self, what: &str| {
                        s.diags.error(
                            "invalid-field",
                            Some(yf.display.clone()),
                            kline,
                            format!("`{k}` must be {what}"),
                        )
                    };
                    match key {
                        "tags" => match string_list(v) {
                            Some(t) => cfg.tags = t,
                            None => bad(self, "a list of strings"),
                        },
                        "output" => match v.as_mapping() {
                            Some(o) => cfg.output = Some(o.clone()),
                            None => bad(self, "a map"),
                        },
                        "profile" => match v.as_str() {
                            Some(p) => cfg.profile = Some((p.to_string(), kline)),
                            None => bad(self, "a string"),
                        },
                        "schedule" => {
                            let ctx = format!("folder `{}`: `+schedule`", dotted(&path));
                            self.moved_to_schedules(&yf.display, kline, &ctx);
                        }
                        "timezone" => {
                            cfg.timezone = self.timezone_value(v, &yf.display, kline, "`+timezone`");
                        }
                        _ => match v.as_mapping() {
                            Some(s) => cfg.vars = Some(s.clone()),
                            None => bad(self, "a map"),
                        },
                    }
                } else {
                    let mut p = path.clone();
                    p.push(k.to_string());
                    stack.push((p, v.clone(), kline));
                }
            }
            if any && !path.is_empty() {
                if !folders.contains(&path) {
                    self.diags.warning(
                        "unknown-folder",
                        Some(yf.display.clone()),
                        line,
                        format!(
                            "folder config for `{}` matches no folder under reports/",
                            dotted(&path)
                        ),
                    );
                }
                out.insert(path, cfg);
            } else if any {
                out.insert(path, cfg);
            }
        }
        out
    }

    // -- discovery ----------------------------------------------------------------------------

    fn discover(&mut self) -> Discovered {
        let mut d = Discovered {
            yaml: Vec::new(),
            sql: Vec::new(),
            macros: Vec::new(),
            lookups: Vec::new(),
            folders: Vec::new(),
        };
        let walker = walkdir::WalkDir::new(&self.root)
            .sort_by_file_name()
            .into_iter()
            .filter_entry(|e| {
                let name = e.file_name().to_string_lossy();
                // Generated or installed, never part of the project's own sources.
                e.depth() == 0
                    || !(name.starts_with('.')
                        || (e.depth() == 1 && [TARGET_DIR, DEPS_DIR, LOGS_DIR].contains(&name.as_ref())))
            });
        for e in walker.filter_map(Result::ok) {
            let rel = self.rel(e.path());
            let in_reports = rel.starts_with(REPORTS_DIR);
            if e.file_type().is_dir() {
                if in_reports && rel != Path::new(REPORTS_DIR) {
                    d.folders.push(folder_segments(&rel));
                }
                continue;
            }
            let ext = e.path().extension().and_then(|x| x.to_str()).unwrap_or("");
            if rel.starts_with(LOOKUPS_DIR) {
                d.lookups.push(rel);
                continue;
            }
            match ext {
                // A profiles.yml at the root holds connections, not project config.
                "yml" | "yaml"
                    if rel != Path::new(PROJECT_FILE) && rel != Path::new(crate::profiles::PROFILES_FILE) =>
                {
                    d.yaml.push(e.path().to_path_buf())
                }
                "sql" if rel.starts_with(MACROS_DIR) => d.macros.push(rel),
                "sql" if in_reports => d.sql.push(rel),
                _ => {}
            }
        }
        d
    }

    fn classify(
        &mut self,
        yf: Rc<YamlFile>,
        fragments: &mut Vec<Fragment>,
        plugins: &mut Vec<(Rc<YamlFile>, Mapping)>,
        sets: &mut Vec<Rc<YamlFile>>,
        schedules: &mut Vec<Rc<YamlFile>>,
    ) {
        let in_reports = yf.display.starts_with(REPORTS_DIR);
        match &yf.value {
            Value::Null => {}
            Value::Sequence(items)
                if !items.is_empty()
                    && items
                        .iter()
                        .all(|i| ["name", "select", "report"].iter().any(|k| i.get(k).is_some())) =>
            {
                schedules.push(yf.clone());
            }
            Value::Mapping(m) => {
                let pl = pick(&yf.value, PLUGIN_KEYS);
                if !pl.is_empty() {
                    plugins.push((yf.clone(), pl));
                }
                // `packages:` belongs to the root dependency files, read by `packages::declared`.
                let dependency_file = packages::DEPENDENCY_FILES
                    .iter()
                    .any(|f| yf.display == Path::new(f));
                if !dependency_file && m.contains_key("packages") {
                    self.diags.error(
                        "misplaced-packages",
                        Some(yf.display.clone()),
                        yf.line_of("packages", None),
                        "`packages:` goes in dependencies.yml or packages.yml at the project root",
                    );
                }
                if dependency_file {
                    for k in m.keys().filter_map(Value::as_str) {
                        if OLD_PLUGIN_KEYS.contains(&k) {
                            self.old_plugin_key(&yf, k);
                        }
                    }
                }
                let rest: Mapping = m
                    .iter()
                    .filter(|(k, _)| !is_one_of(k, PLUGIN_KEYS) && k.as_str() != Some("packages"))
                    .filter(|(k, _)| !(dependency_file && is_one_of(k, OLD_PLUGIN_KEYS)))
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                if rest.is_empty() {
                    return;
                }
                let looks_like_report = rest.contains_key("queries") || rest.contains_key("name");
                if in_reports || looks_like_report {
                    self.report_fragment(yf.clone(), rest, fragments);
                } else if is_set_registry(&rest) {
                    sets.push(yf.clone());
                } else {
                    self.diags.warning(
                        "unrecognized-yaml",
                        Some(yf.display.clone()),
                        None,
                        "not recognised as report, Set, plugin or schedule config; ignored",
                    );
                }
            }
            _ => self.diags.warning(
                "unrecognized-yaml",
                Some(yf.display.clone()),
                None,
                "not recognised as report, Set, plugin or schedule config; ignored",
            ),
        }
    }

    fn report_fragment(&mut self, yf: Rc<YamlFile>, map: Mapping, out: &mut Vec<Fragment>) {
        for k in map.keys().filter_map(Value::as_str) {
            if !REPORT_KEYS.contains(&k) {
                self.diags.error(
                    "unknown-key",
                    Some(yf.display.clone()),
                    yf.line_of(k, None),
                    format!("unknown report key `{k}`"),
                );
            }
        }
        let folder = folder_segments(yf.display.parent().unwrap_or(Path::new("")));
        let (name, explicit_name) = match map.get("name") {
            Some(Value::String(s)) if !s.is_empty() => (s.clone(), true),
            Some(_) => {
                self.diags.error(
                    "invalid-field",
                    Some(yf.display.clone()),
                    yf.line_of("name", None),
                    "`name` must be a non-empty string",
                );
                return;
            }
            None => match folder.last() {
                Some(f) => (f.clone(), false),
                None => {
                    self.diags.error(
                        "missing-field",
                        Some(yf.display.clone()),
                        None,
                        "a report outside a report folder needs an explicit `name:`",
                    );
                    return;
                }
            },
        };
        out.push(Fragment {
            file: yf,
            name,
            explicit_name,
            folder,
            map,
        });
    }

    // -- sets.yml -----------------------------------------------------------------------------

    fn parse_sets(&mut self, files: &[Rc<YamlFile>]) -> BTreeMap<String, SetDef> {
        let mut out = BTreeMap::new();
        let mut seen: BTreeMap<String, PathBuf> = BTreeMap::new();
        for yf in files {
            let Some(m) = yf.value.as_mapping() else { continue };
            for (k, v) in m {
                let Some(name) = k.as_str() else { continue };
                let line = yf.line_of(name, None);
                if let Some(prev) = seen.get(name) {
                    self.diags.error(
                        "duplicate-set",
                        Some(yf.display.clone()),
                        line,
                        format!("Set `{name}` is also declared in {}", prev.display()),
                    );
                    continue;
                }
                seen.insert(name.to_string(), yf.display.clone());
                let profile = v.get("profile").and_then(Value::as_str).map(str::to_string);
                if v.get("profile").is_some() && profile.is_none() {
                    self.diags.error(
                        "invalid-field",
                        Some(yf.display.clone()),
                        line,
                        format!("Set `{name}`: `profile` must be a string"),
                    );
                }
                let vars = match v.get("vars") {
                    Some(Value::Mapping(m)) => yaml_map_to_json(m),
                    None | Some(Value::Null) => JsonMap::new(),
                    Some(_) => {
                        self.diags.error(
                            "invalid-field",
                            Some(yf.display.clone()),
                            line,
                            format!("Set `{name}`: `vars` must be a map"),
                        );
                        JsonMap::new()
                    }
                };
                out.insert(name.to_string(), SetDef { profile, vars });
            }
        }
        out
    }

    // -- .sql index ---------------------------------------------------------------------------

    /// Literal `ref('name')` calls in SQL and macro files: each must name a project `.sql` file.
    /// Returns the names referenced.
    fn check_refs(
        &mut self,
        sql: &[PathBuf],
        macros: &[PathBuf],
        index: &BTreeMap<String, Vec<PathBuf>>,
        lookups: &BTreeMap<String, Lookup>,
    ) -> BTreeSet<String> {
        let mut names = BTreeSet::new();
        // name → the names its file refs, for cycle detection.
        let mut graph: BTreeMap<String, Vec<(String, usize)>> = BTreeMap::new();
        for file in sql.iter().chain(macros) {
            let Ok(src) = std::fs::read_to_string(self.root.join(file)) else {
                continue;
            };
            let is_sql = !file.starts_with(MACROS_DIR);
            for (name, line) in preflight::refs(&src) {
                if index.contains_key(&name) {
                    if is_sql {
                        let stem = file.file_stem().unwrap().to_string_lossy().to_string();
                        graph.entry(stem).or_default().push((name.clone(), line));
                    }
                    names.insert(name);
                } else if !lookups.contains_key(&name) {
                    self.diags.error(
                        "unknown-ref",
                        Some(file.clone()),
                        Some(line),
                        format!("`ref('{name}')`: there's no `{name}.sql` under reports/ and no lookup `{name}` under lookups/"),
                    );
                }
            }
        }
        // Report each cycle once, at the ref that closes it.
        let mut reported: BTreeSet<Vec<String>> = BTreeSet::new();
        for start in graph.keys() {
            let mut path = vec![start.clone()];
            let mut stack = vec![graph[start].iter()];
            while let Some(it) = stack.last_mut() {
                let Some((next, line)) = it.next() else {
                    stack.pop();
                    path.pop();
                    continue;
                };
                if let Some(i) = path.iter().position(|n| n == next) {
                    let mut cycle = path[i..].to_vec();
                    let mut key = cycle.clone();
                    key.sort();
                    if reported.insert(key) {
                        cycle.push(next.clone());
                        let file = index[path.last().unwrap()][0].clone();
                        self.diags.error(
                            "ref-cycle",
                            Some(file),
                            Some(*line),
                            format!("`ref()` cycle: {}", cycle.join(" → ")),
                        );
                    }
                    continue;
                }
                if let Some(edges) = graph.get(next) {
                    path.push(next.clone());
                    stack.push(edges.iter());
                }
            }
        }
        names
    }

    fn index_sql(&mut self, sql: &[PathBuf]) -> BTreeMap<String, Vec<PathBuf>> {
        let mut index: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
        for p in sql {
            let stem = p.file_stem().unwrap().to_string_lossy().to_string();
            index.entry(stem).or_default().push(p.clone());
        }
        for (name, paths) in &index {
            if paths.len() > 1 {
                self.diags.error(
                    "duplicate-sql-name",
                    Some(paths[1].clone()),
                    None,
                    format!(
                        "`{name}.sql` exists more than once: {}; .sql basenames must be unique project-wide",
                        paths
                            .iter()
                            .map(|p| p.display().to_string())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                );
            }
        }
        index
    }

    // -- merging report fragments ---------------------------------------------------------------

    fn merge_reports(&mut self, fragments: Vec<Fragment>) -> Vec<RawReport> {
        let mut by_name: BTreeMap<String, Vec<Fragment>> = BTreeMap::new();
        for f in fragments {
            by_name.entry(f.name.clone()).or_default().push(f);
        }
        let mut out = Vec::new();
        for (name, frags) in by_name {
            let definers: Vec<&Fragment> = frags.iter().filter(|f| f.map.contains_key("queries")).collect();
            if definers.len() > 1 {
                self.diags.error(
                    "duplicate-report-name",
                    Some(definers[1].file.display.clone()),
                    definers[1].file.line_of("queries", None),
                    format!(
                        "report `{name}` is declared in both {} and {}; report names must be unique project-wide",
                        definers[0].file.display.display(),
                        definers[1].file.display.display()
                    ),
                );
                continue;
            }
            let Some(def) = definers.first() else {
                for f in &frags {
                    let msg = if f.explicit_name {
                        format!(
                            "report `{name}` has no `queries`; a config fragment must name a report that declares `queries:`"
                        )
                    } else {
                        format!("report `{name}` has no `queries`")
                    };
                    self.diags
                        .error("missing-queries", Some(f.file.display.clone()), None, msg);
                }
                continue;
            };
            let mut keys: BTreeMap<String, Located<Value>> = BTreeMap::new();
            let mut ok = true;
            for f in &frags {
                for (k, v) in &f.map {
                    let Some(k) = k.as_str() else { continue };
                    if k == "name" || !REPORT_KEYS.contains(&k) {
                        continue;
                    }
                    if let Some(prev) = keys.get(k) {
                        self.diags.error(
                            "conflicting-declaration",
                            Some(f.file.display.clone()),
                            f.file.line_of(k, None),
                            format!(
                                "`{k}` for report `{name}` is declared in both {} and {}",
                                prev.file.display.display(),
                                f.file.display.display()
                            ),
                        );
                        ok = false;
                        continue;
                    }
                    keys.insert(
                        k.to_string(),
                        Located {
                            value: v.clone(),
                            file: f.file.clone(),
                        },
                    );
                }
            }
            let _ = ok;
            out.push(RawReport {
                name,
                file: def.file.clone(),
                folder: def.folder.clone(),
                keys,
            });
        }
        out
    }

    fn resolve_queries(
        &mut self,
        r: &RawReport,
        index: &BTreeMap<String, Vec<PathBuf>>,
        referenced: &mut BTreeSet<String>,
    ) -> Vec<QueryEntry> {
        let Some(q) = r.keys.get("queries") else {
            return Vec::new();
        };
        let yf = q.file.clone();
        let Some(items) = q.value.as_sequence() else {
            self.diags.error(
                "invalid-field",
                Some(yf.display.clone()),
                yf.line_of("queries", None),
                format!("report `{}`: `queries` must be a list", r.name),
            );
            return Vec::new();
        };
        if items.is_empty() {
            self.diags.error(
                "missing-queries",
                Some(yf.display.clone()),
                yf.line_of("queries", None),
                format!("report `{}` has an empty `queries` list", r.name),
            );
        }
        let mut out = Vec::new();
        for item in items {
            if let Some(e) = self.query_entry(&r.name, item, &yf, index) {
                referenced.insert(e.query.clone());
                out.push(e);
            } else if let Some(n) = entry_name(item) {
                referenced.insert(n);
            }
        }
        out
    }

    fn query_entry(
        &mut self,
        report: &str,
        item: &Value,
        yf: &YamlFile,
        index: &BTreeMap<String, Vec<PathBuf>>,
    ) -> Option<QueryEntry> {
        let file = Some(yf.display.clone());
        let (name, m) = match item {
            Value::String(s) => (s.clone(), None),
            Value::Mapping(m) => match m.get("query").and_then(Value::as_str) {
                Some(s) => (s.to_string(), Some(m)),
                None => {
                    self.diags.error(
                        "invalid-field",
                        file,
                        yf.line_of("queries", None),
                        format!("report `{report}`: a `queries` map entry needs a `query:` name"),
                    );
                    return None;
                }
            },
            _ => {
                self.diags.error(
                    "invalid-field",
                    file,
                    yf.line_of("queries", None),
                    format!("report `{report}`: `queries` entries must be names or maps"),
                );
                return None;
            }
        };
        let line = yf.line_containing(&name);
        if name.ends_with(".sql") || name.contains('/') || name.contains('\\') {
            self.diags.error(
                "invalid-query-name",
                file,
                line,
                format!(
                    "report `{report}`: query `{name}` must be a bare .sql basename, with no path and no extension"
                ),
            );
            return None;
        }
        let mut e = QueryEntry {
            query: name.clone(),
            path: PathBuf::new(),
            tab: true,
            tab_name: None,
            anchor: None,
            header: None,
        };
        if let Some(m) = m {
            for k in m.keys().filter_map(Value::as_str) {
                if !QUERY_ENTRY_KEYS.contains(&k) {
                    self.diags.error(
                        "unknown-key",
                        file.clone(),
                        line,
                        format!("report `{report}`: unknown key `{k}` on query `{name}`"),
                    );
                }
            }
            e.tab_name = match m.get("tab_name") {
                None => None,
                Some(Value::String(s)) => Some(s.clone()),
                Some(Value::Sequence(_)) => {
                    self.diags.error(
                        "invalid-field",
                        file.clone(),
                        line,
                        format!("report `{report}`: {}", one_tab_per_file(&name)),
                    );
                    None
                }
                Some(_) => {
                    self.diags.error(
                        "invalid-field",
                        file.clone(),
                        line,
                        format!("report `{report}`: `tab_name` of `{name}` must be a string"),
                    );
                    None
                }
            };
            match m.get("tab").map(Value::as_bool) {
                None => {}
                Some(Some(b)) => e.tab = b,
                Some(None) => self.diags.error(
                    "invalid-field",
                    file.clone(),
                    line,
                    format!("report `{report}`: `tab` of `{name}` must be true or false"),
                ),
            }
            if !e.tab && e.tab_name.is_some() {
                self.diags.error(
                    "invalid-field",
                    file.clone(),
                    line,
                    format!("report `{report}`: `{name}` has `tab: false`, so its `tab_name` would never be used; remove one of them"),
                );
            }
            if let Some(a) = m.get("anchor") {
                match a.as_str() {
                    Some(s) if options::is_cell(s) => e.anchor = Some(s.to_string()),
                    _ => self.diags.error(
                        "invalid-cell",
                        file.clone(),
                        line,
                        format!("report `{report}`: `anchor` of `{name}` must be a cell reference like `A1`"),
                    ),
                }
            }
            if let Some(h) = m.get("header") {
                match h.as_bool() {
                    Some(b) => e.header = Some(b),
                    None => self.diags.error(
                        "invalid-field",
                        file.clone(),
                        line,
                        format!("report `{report}`: `header` of `{name}` must be true or false"),
                    ),
                }
            }
        }
        match index.get(&name) {
            Some(paths) => {
                e.path = paths[0].clone();
                Some(e)
            }
            None => {
                self.diags.error(
                    "unknown-query",
                    file,
                    line,
                    format!("report `{report}`: query `{name}` doesn't match any .sql file under reports/"),
                );
                None
            }
        }
    }

    // -- resolving a managed report -------------------------------------------------------------

    fn resolve_managed(
        &mut self,
        r: &RawReport,
        queries: Vec<QueryEntry>,
        project: &Project,
        folders: &BTreeMap<Vec<String>, FolderCfg>,
        used: &mut Usage,
    ) -> Option<Report> {
        let name = r.name.clone();
        let layers = folder_layers(folders, &r.folder);
        let key = |k: &str| r.keys.get(k);
        let located = |k: &str| {
            r.keys
                .get(k)
                .map(|l| (l.file.display.clone(), l.file.line_of(k, None)))
        };

        let mut tags: Vec<String> = layers.iter().flat_map(|l| l.tags.clone()).collect();
        if let Some(t) = key("tags") {
            match string_list(&t.value) {
                Some(l) => tags.extend(l),
                None => {
                    let (f, l) = located("tags").unwrap();
                    self.diags.error(
                        "invalid-field",
                        Some(f),
                        l,
                        format!("report `{name}`: `tags` must be a list of strings"),
                    );
                }
            }
        }
        dedup(&mut tags);

        if key("profile").is_some() && key("sets").is_some() {
            let (f, l) = located("sets").unwrap();
            self.diags.error(
                "profile-and-sets",
                Some(f),
                l,
                format!("report `{name}` declares both `profile:` and `sets:`; use one or the other"),
            );
        }

        let report_profile = match key("profile") {
            Some(p) => match p.value.as_str() {
                Some(s) => {
                    let (f, l) = located("profile").unwrap();
                    used.source(s, Some(f), l);
                    Some(s.to_string())
                }
                None => {
                    let (f, l) = located("profile").unwrap();
                    self.diags.error(
                        "invalid-field",
                        Some(f),
                        l,
                        format!("report `{name}`: `profile` must be a string"),
                    );
                    None
                }
            },
            None => None,
        };
        let folder_profile = layers
            .iter()
            .rev()
            .find_map(|l| l.profile.as_ref().map(|p| p.0.clone()));
        let report_timezone = match key("timezone") {
            Some(t) => {
                let (f, l) = located("timezone").unwrap();
                self.timezone_value(&t.value, &f, l, &format!("report `{name}`: `timezone`"))
            }
            None => None,
        };
        let timezone = report_timezone
            .or_else(|| layers.iter().rev().find_map(|l| l.timezone.clone()))
            .or_else(|| project.timezone.clone());
        let base_profile = report_profile
            .or(folder_profile)
            .or(project.default_profile.clone());

        let mut vars = project.vars.clone();
        for l in &layers {
            if let Some(v) = &l.vars {
                vars.extend(yaml_map_to_json(v));
            }
        }
        if let Some(v) = key("vars") {
            match v.value.as_mapping() {
                Some(m) => vars.extend(yaml_map_to_json(m)),
                None => {
                    let (f, l) = located("vars").unwrap();
                    self.diags.error(
                        "invalid-field",
                        Some(f),
                        l,
                        format!("report `{name}`: `vars` must be a map"),
                    );
                }
            }
        }

        let mut output = builtin_output();
        let mut merge_problems = Vec::new();
        if let Some(o) = project_default_output(project) {
            merge_problems.extend(merge_output(&mut output, &o));
        }
        for l in &layers {
            if let Some(o) = &l.output {
                merge_problems.extend(merge_output(&mut output, o));
            }
        }
        for p in merge_problems {
            self.diags.error(
                "invalid-field",
                Some(r.file.display.clone()),
                None,
                format!("report `{name}`: folder config: {p}"),
            );
        }
        if let Some(o) = key("output") {
            match o.value.as_mapping() {
                Some(m) => {
                    if let Some(p) = merge_output(&mut output, m) {
                        let (f, l) = located("output").unwrap();
                        self.diags
                            .error("invalid-field", Some(f), l, format!("report `{name}`: {p}"));
                    }
                }
                None => {
                    let (f, l) = located("output").unwrap();
                    self.diags.error(
                        "invalid-field",
                        Some(f),
                        l,
                        format!("report `{name}`: `output` must be a map"),
                    );
                }
            }
        }

        if key("schedule").is_some() {
            let (f, l) = located("schedule").unwrap();
            self.moved_to_schedules(&f, l, &format!("report `{name}`: `schedule`"));
        }

        let default_set = match key("default_set") {
            Some(d) => d.value.as_str().map(str::to_string),
            None => None,
        };

        let base = BindingBase {
            profile: base_profile,
            vars,
            output,
        };
        let has_sets = key("sets").is_some();
        let report_base = self.silent_binding(&name, &base, &queries, &r.file.display);
        let mut bindings = Vec::new();
        if let Some(s) = key("sets") {
            bindings = self.resolve_sets(&name, s, &queries, &base, project, used);
            let declared: Vec<&str> = bindings.iter().filter_map(|b| b.set.as_deref()).collect();
            if let Some(d) = &default_set
                && !declared.contains(&d.as_str())
            {
                let (f, l) = located("default_set").unwrap();
                self.diags.error(
                    "unknown-default-set",
                    Some(f),
                    l,
                    format!("report `{name}`: `default_set` `{d}` isn't one of the report's `sets:`"),
                );
            }
        } else {
            if default_set.is_some() {
                let (f, l) = located("default_set").unwrap();
                self.diags.error(
                    "unknown-default-set",
                    Some(f),
                    l,
                    format!("report `{name}`: `default_set` needs a `sets:` list"),
                );
            }
            let b = self.finish_binding(
                &name,
                None,
                &base,
                None,
                queries.clone(),
                r.file.display.clone(),
                used,
            );
            bindings.push(b);
        }

        Some(Report {
            name,
            managed: true,
            file: r.file.display.clone(),
            folder: r.folder.clone(),
            tags,
            queries,
            default_set,
            timezone,
            has_sets,
            bindings,
            base: report_base,
        })
    }

    fn resolve_sets(
        &mut self,
        report: &str,
        sets: &Located<Value>,
        queries: &[QueryEntry],
        base: &BindingBase,
        project: &Project,
        used: &mut Usage,
    ) -> Vec<Binding> {
        let yf = sets.file.clone();
        let file = Some(yf.display.clone());
        let Some(items) = sets.value.as_sequence() else {
            self.diags.error(
                "invalid-field",
                file,
                yf.line_of("sets", None),
                format!("report `{report}`: `sets` must be a list"),
            );
            return Vec::new();
        };
        let sets_line = yf.line_of("sets", None);
        let mut seen = BTreeSet::new();
        let mut out = Vec::new();
        for item in items {
            let (name, inline): (String, Option<&Mapping>) = match item {
                Value::String(s) => (s.clone(), None),
                Value::Mapping(m) => match m.get("name").and_then(Value::as_str) {
                    Some(n) => (n.to_string(), Some(m)),
                    None => {
                        self.diags.error(
                            "invalid-field",
                            file.clone(),
                            sets_line,
                            format!("report `{report}`: a `sets:` map entry needs a `name:`"),
                        );
                        continue;
                    }
                },
                _ => {
                    self.diags.error(
                        "invalid-field",
                        file.clone(),
                        sets_line,
                        format!("report `{report}`: `sets:` entries must be names or maps"),
                    );
                    continue;
                }
            };
            let line = yf.line_of(&name, sets_line).or_else(|| yf.line_containing(&name));
            if !seen.insert(name.clone()) {
                self.diags.error(
                    "duplicate-set",
                    file.clone(),
                    line,
                    format!("report `{report}` lists Set `{name}` more than once"),
                );
                continue;
            }
            let registry = project.sets.get(&name);
            if inline.is_none() && registry.is_none() {
                self.diags.error(
                    "unknown-set",
                    file.clone(),
                    line,
                    format!("report `{report}`: Set `{name}` isn't declared in sets.yml"),
                );
                continue;
            }
            let mut b = BindingBase {
                profile: base.profile.clone(),
                vars: base.vars.clone(),
                output: base.output.clone(),
            };
            if let Some(reg) = registry {
                if let Some(p) = &reg.profile {
                    b.profile = Some(p.clone());
                    used.source(p, None, None);
                }
                b.vars.extend(reg.vars.clone());
            }
            let mut qs = queries.to_vec();
            let mut tab_names: Option<Mapping> = None;
            if let Some(m) = inline {
                let ctx = format!("report `{report}`, Set `{name}`");
                for k in m.keys().filter_map(Value::as_str) {
                    if !SET_ENTRY_KEYS.contains(&k) {
                        self.diags.error(
                            "unknown-key",
                            file.clone(),
                            line,
                            format!("{ctx}: unknown key `{k}`"),
                        );
                    }
                }
                if let Some(p) = m.get("profile") {
                    match p.as_str() {
                        Some(p) => {
                            b.profile = Some(p.to_string());
                            used.source(p, file.clone(), yf.line_of("profile", line));
                        }
                        None => self.diags.error(
                            "invalid-field",
                            file.clone(),
                            line,
                            format!("{ctx}: `profile` must be a string"),
                        ),
                    }
                }
                match m.get("vars") {
                    Some(Value::Mapping(v)) => b.vars.extend(yaml_map_to_json(v)),
                    None => {}
                    Some(_) => self.diags.error(
                        "invalid-field",
                        file.clone(),
                        line,
                        format!("{ctx}: `vars` must be a map"),
                    ),
                }
                if let Some(o) = m.get("output") {
                    match o.as_mapping() {
                        Some(o) => {
                            if let Some(p) = merge_output(&mut b.output, o) {
                                self.diags
                                    .error("invalid-field", file.clone(), line, format!("{ctx}: {p}"));
                            }
                        }
                        None => self.diags.error(
                            "invalid-field",
                            file.clone(),
                            line,
                            format!("{ctx}: `output` must be a map"),
                        ),
                    }
                }
                if m.contains_key("schedule") {
                    self.moved_to_schedules(&yf.display, line, &format!("{ctx}: `schedule`"));
                }
                let listed: Vec<&str> = queries.iter().map(|q| q.query.as_str()).collect();
                match (m.get("exclude"), m.get("queries")) {
                    (Some(_), Some(_)) => self.diags.error(
                        "exclude-and-queries",
                        file.clone(),
                        line,
                        format!("{ctx}: use either `exclude:` or `queries:`, not both"),
                    ),
                    (Some(ex), None) => match string_list(ex) {
                        Some(ex) => {
                            for q in &ex {
                                if !listed.contains(&q.as_str()) {
                                    self.diags.error(
                                        "unknown-query",
                                        file.clone(),
                                        yf.line_of("exclude", line),
                                        format!("{ctx}: `exclude` names `{q}`, which isn't in the report's `queries:`"),
                                    );
                                }
                            }
                            qs.retain(|q| !ex.contains(&q.query));
                        }
                        None => self.diags.error(
                            "invalid-field",
                            file.clone(),
                            line,
                            format!("{ctx}: `exclude` must be a list of query names"),
                        ),
                    },
                    (None, Some(ov)) => match ov.as_sequence() {
                        Some(items) => {
                            let mut sub = Vec::new();
                            for it in items {
                                let Some(qn) = entry_name(it) else { continue };
                                match queries.iter().find(|q| q.query == qn) {
                                    Some(q) => {
                                        let mut q = q.clone();
                                        match it.get("tab_name") {
                                            None => {}
                                            Some(Value::String(s)) => q.tab_name = Some(s.clone()),
                                            Some(_) => self.diags.error(
                                                "invalid-field",
                                                file.clone(),
                                                line,
                                                format!("{ctx}: {}", one_tab_per_file(&qn)),
                                            ),
                                        }
                                        if let Some(b) = it.get("tab").and_then(Value::as_bool) {
                                            q.tab = b;
                                        }
                                        sub.push(q);
                                    }
                                    None => self.diags.error(
                                        "unknown-query",
                                        file.clone(),
                                        yf.line_of("queries", line),
                                        format!("{ctx}: `queries` names `{qn}`, which isn't in the report's `queries:`"),
                                    ),
                                }
                            }
                            qs = sub;
                        }
                        None => self.diags.error(
                            "invalid-field",
                            file.clone(),
                            line,
                            format!("{ctx}: `queries` must be a list"),
                        ),
                    },
                    (None, None) => {}
                }
                tab_names = match m.get("tab_names") {
                    None => None,
                    Some(Value::Mapping(t)) => Some(t.clone()),
                    Some(_) => {
                        self.diags.error(
                            "invalid-field",
                            file.clone(),
                            line,
                            format!("{ctx}: `tab_names` must be a map of query name to tab name"),
                        );
                        None
                    }
                };
            }
            let bind = self.finish_binding(
                report,
                Some(name),
                &b,
                tab_names.as_ref(),
                qs,
                yf.display.clone(),
                used,
            );
            out.push(bind);
        }
        out
    }

    /// Resolve a Binding without recording diagnostics or usage (they're reported elsewhere).
    fn silent_binding(
        &mut self,
        report: &str,
        base: &BindingBase,
        queries: &[QueryEntry],
        file: &Path,
    ) -> Binding {
        let saved = std::mem::take(&mut self.diags);
        let mut scratch = Usage::default();
        let b = self.finish_binding(
            report,
            None,
            base,
            None,
            queries.to_vec(),
            file.to_path_buf(),
            &mut scratch,
        );
        self.diags = saved;
        b
    }

    #[allow(clippy::too_many_arguments)]
    fn finish_binding(
        &mut self,
        report: &str,
        set: Option<String>,
        base: &BindingBase,
        tab_names: Option<&Mapping>,
        mut queries: Vec<QueryEntry>,
        file: PathBuf,
        used: &mut Usage,
    ) -> Binding {
        let ctx = match &set {
            Some(s) => format!("report `{report}`, Set `{s}`"),
            None => format!("report `{report}`"),
        };
        if let Some(t) = tab_names {
            for (k, v) in t {
                let Some(q) = k.as_str() else { continue };
                match queries.iter_mut().find(|e| e.query == q) {
                    Some(e) => match v {
                        Value::String(s) => e.tab_name = Some(s.clone()),
                        _ => self.diags.error(
                            "invalid-field",
                            Some(file.clone()),
                            None,
                            format!("{ctx}: {}", one_tab_per_file(q)),
                        ),
                    },
                    None => self.diags.error(
                        "unknown-query",
                        Some(file.clone()),
                        None,
                        format!("{ctx}: `tab_names` names `{q}`, which isn't one of this Binding's queries"),
                    ),
                }
            }
        }
        let output = self.typed_output(&base.output, &ctx, &file, &queries, used);
        if let Some(p) = &base.profile {
            used.source(p, None, None);
        }
        if base.profile.is_none() {
            self.diags.error(
                "no-source-profile",
                Some(file.clone()),
                None,
                format!(
                    "{ctx} has no source profile: declare `profile:` (report, Set or folder `+profile`) or `default_profile` in {PROJECT_FILE}"
                ),
            );
        }
        Binding {
            set,
            profile: base.profile.clone(),
            vars: base.vars.clone(),
            queries: std::mem::take(&mut queries),
            output,
            schedules: Vec::new(),
        }
    }

    /// Convert a merged output map into a typed `Output`, validating options and references.
    fn typed_output(
        &mut self,
        m: &Mapping,
        ctx: &str,
        file: &Path,
        queries: &[QueryEntry],
        used: &mut Usage,
    ) -> Output {
        let file_path = file.to_path_buf();
        let file = Some(file_path.clone());
        let format = m
            .get("format")
            .and_then(Value::as_str)
            .unwrap_or("csv")
            .to_string();
        used.format(&format, ctx, &file_path);
        let mut opts = JsonMap::new();
        for (k, v) in m {
            let Some(k) = k.as_str() else { continue };
            if !OUTPUT_SHARED_KEYS.contains(&k) {
                opts.insert(k.to_string(), yaml_to_json(v));
            }
        }
        // The project's defaults for this format sit under whatever the layers set.
        if let Some(Value::Mapping(d)) = self.format_options.get(format.as_str()) {
            for (k, v) in d {
                if let Some(k) = k.as_str()
                    && !OUTPUT_SHARED_KEYS.contains(&k)
                {
                    opts.entry(k.to_string()).or_insert_with(|| yaml_to_json(v));
                }
            }
        }
        let destinations = match m.get("destination") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Mapping(d)) => self.typed_destination(d, ctx, &file, used).into_iter().collect(),
            Some(Value::Sequence(list)) if list.is_empty() => {
                self.diags.error(
                    "invalid-field",
                    file.clone(),
                    None,
                    format!(
                        "{ctx}: `output.destination` is an empty list; name at least one destination, or remove it to keep the output in target/"
                    ),
                );
                Vec::new()
            }
            Some(Value::Sequence(list)) => {
                let mut out = Vec::new();
                for (i, d) in list.iter().enumerate() {
                    match d.as_mapping() {
                        Some(d) => out.extend(self.typed_destination(d, ctx, &file, used)),
                        None => self.diags.error(
                            "invalid-field",
                            file.clone(),
                            None,
                            format!("{ctx}: `output.destination` entry {} must be a map", i + 1),
                        ),
                    }
                }
                out
            }
            Some(_) => {
                self.diags.error(
                    "invalid-field",
                    file.clone(),
                    None,
                    format!("{ctx}: `output.destination` must be a map or a list of maps"),
                );
                Vec::new()
            }
        };
        let template = match m.get("template") {
            None | Some(Value::Null) => None,
            Some(t) => {
                if format != "xlsx" {
                    self.diags.error(
                        "invalid-output-option",
                        file.clone(),
                        None,
                        format!("{ctx}: `output.template` only applies to the xlsx format"),
                    );
                }
                self.typed_template(t, ctx, &file, queries)
            }
        };
        let extension = match m.get("extension") {
            None => None,
            Some(Value::Null) | Some(Value::Bool(false)) => Some(String::new()),
            Some(Value::String(e)) => {
                let e = e.strip_prefix('.').unwrap_or(e);
                if e.contains(['/', '\\']) || e.chars().any(char::is_whitespace) {
                    self.diags.error(
                        "invalid-output-option",
                        file.clone(),
                        None,
                        format!(
                            "{ctx}: `extension` must be a file extension like `aba` (no path, no spaces)"
                        ),
                    );
                }
                Some(e.to_string())
            }
            Some(_) => {
                self.diags.error(
                    "invalid-output-option",
                    file.clone(),
                    None,
                    format!("{ctx}: `extension` must be a string (`aba`), or `\"\"`/`false` for none"),
                );
                None
            }
        };
        if extension.is_some() && format == "xlsx" {
            self.diags.error(
                "invalid-output-option",
                file.clone(),
                None,
                format!("{ctx}: `extension` doesn't apply to xlsx (Excel only opens .xlsx workbooks)"),
            );
        }
        Output {
            format,
            options: opts,
            destinations,
            template,
            extension,
        }
    }

    /// One `output.destination` entry: `profile`, optional `path`, and plugin options.
    fn typed_destination(
        &mut self,
        d: &Mapping,
        ctx: &str,
        file: &Option<PathBuf>,
        used: &mut Usage,
    ) -> Option<Destination> {
        let Some(p) = d.get("profile").and_then(Value::as_str) else {
            self.diags.error(
                "invalid-field",
                file.clone(),
                None,
                format!("{ctx}: `output.destination` needs a `profile:` naming a profiles.yml entry"),
            );
            return None;
        };
        let path = match d.get("path") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) => Some(s.clone()),
            Some(_) => {
                self.diags.error(
                    "invalid-field",
                    file.clone(),
                    None,
                    format!("{ctx}: destination `{p}`: `path` must be a string"),
                );
                None
            }
        };
        used.destination(p, file.clone(), None);
        let options = d
            .iter()
            .filter(|(k, _)| !is_one_of(k, &["profile", "path"]))
            .filter_map(|(k, v)| Some((k.as_str()?.to_string(), yaml_to_json(v))))
            .collect();
        Some(Destination {
            profile: p.to_string(),
            path,
            options,
        })
    }

    fn typed_template(
        &mut self,
        t: &Value,
        ctx: &str,
        file: &Option<PathBuf>,
        queries: &[QueryEntry],
    ) -> Option<Template> {
        let err = |s: &mut Self, msg: String| {
            s.diags
                .error("invalid-template", file.clone(), None, format!("{ctx}: {msg}"))
        };
        let Some(tf) = t.get("file").and_then(Value::as_str) else {
            err(self, "`output.template` needs a `file:`".into());
            return None;
        };
        let mut bindings = Vec::new();
        let items = match t.get("bindings") {
            None => Vec::new(),
            Some(Value::Sequence(s)) => s.clone(),
            Some(_) => {
                err(self, "`output.template.bindings` must be a list".into());
                Vec::new()
            }
        };
        let names: Vec<&str> = queries.iter().map(|q| q.query.as_str()).collect();
        for (i, b) in items.iter().enumerate() {
            let n = i + 1;
            let Some(m) = b.as_mapping() else {
                err(self, format!("template binding {n} must be a map"));
                continue;
            };
            let s = |k: &str| m.get(k).and_then(Value::as_str).map(str::to_string);
            for k in m.keys().filter_map(Value::as_str) {
                if ![
                    "sheet",
                    "query",
                    "result_index",
                    "anchor",
                    "header",
                    "columns",
                    "cell",
                    "value",
                    "column",
                ]
                .contains(&k)
                {
                    err(self, format!("template binding {n} has unknown key `{k}`"));
                }
            }
            let Some(sheet) = s("sheet") else {
                err(self, format!("template binding {n} needs a `sheet:`"));
                continue;
            };
            let tb = TemplateBinding {
                sheet,
                query: s("query"),
                result_index: m.get("result_index").and_then(Value::as_u64).map(|x| x as usize),
                anchor: s("anchor"),
                header: m.get("header").and_then(Value::as_bool),
                columns: m.get("columns").and_then(string_list),
                cell: s("cell"),
                value: s("value"),
                column: s("column"),
            };
            if let Some(q) = &tb.query
                && !names.contains(&q.as_str())
            {
                err(
                    self,
                    format!(
                        "template binding {n} uses query `{q}`, which isn't one of this Binding's queries"
                    ),
                );
            }
            if let Some(ri) = m.get("result_index")
                && ri.as_u64().is_none_or(|x| x == 0)
            {
                err(
                    self,
                    format!("template binding {n}: `result_index` must be 1 or more"),
                );
            }
            match &tb.cell {
                Some(c) => {
                    if !options::is_cell(c) {
                        err(
                            self,
                            format!("template binding {n}: `cell` `{c}` isn't a valid cell reference"),
                        );
                    }
                    let by_value = tb.value.is_some();
                    let by_query = tb.query.is_some() && tb.column.is_some();
                    if by_value == by_query || (tb.query.is_some() != tb.column.is_some()) {
                        err(
                            self,
                            format!(
                                "template binding {n}: a single-cell binding needs exactly one of `value` or `query` + `column`"
                            ),
                        );
                    }
                    if tb.anchor.is_some() || tb.columns.is_some() {
                        err(
                            self,
                            format!(
                                "template binding {n}: `anchor`/`columns` apply to table blocks, not single cells"
                            ),
                        );
                    }
                }
                None => {
                    if tb.query.is_none() {
                        err(
                            self,
                            format!(
                                "template binding {n}: a table block needs a `query` (or use `cell` for a single cell)"
                            ),
                        );
                    }
                    if let Some(a) = &tb.anchor
                        && !options::is_cell(a)
                    {
                        err(
                            self,
                            format!("template binding {n}: `anchor` `{a}` isn't a valid cell reference"),
                        );
                    }
                    if tb.value.is_some() || tb.column.is_some() {
                        err(
                            self,
                            format!(
                                "template binding {n}: `value`/`column` apply to single-cell bindings (add `cell`)"
                            ),
                        );
                    }
                }
            }
            bindings.push(tb);
        }
        Some(Template {
            file: tf.to_string(),
            bindings,
        })
    }

    // -- unmanaged reports --------------------------------------------------------------------

    fn resolve_unmanaged(
        &mut self,
        name: &str,
        path: &Path,
        project: &Project,
        folders: &BTreeMap<Vec<String>, FolderCfg>,
        used: &mut Usage,
    ) -> Report {
        let folder = folder_segments(path.parent().unwrap_or(Path::new("")));
        let layers = folder_layers(folders, &folder);
        let mut tags: Vec<String> = layers.iter().flat_map(|l| l.tags.clone()).collect();
        dedup(&mut tags);
        let profile = layers
            .iter()
            .rev()
            .find_map(|l| l.profile.as_ref().map(|p| p.0.clone()))
            .or(project.default_profile.clone());
        let mut vars = project.vars.clone();
        for l in &layers {
            if let Some(v) = &l.vars {
                vars.extend(yaml_map_to_json(v));
            }
        }
        let mut output = builtin_output();
        let mut merge_problems = Vec::new();
        if let Some(o) = project_default_output(project) {
            merge_problems.extend(merge_output(&mut output, &o));
        }
        for l in &layers {
            if let Some(o) = &l.output {
                merge_problems.extend(merge_output(&mut output, o));
            }
        }
        for p in merge_problems {
            self.diags.error(
                "invalid-field",
                Some(path.to_path_buf()),
                None,
                format!("folder config: {p}"),
            );
        }
        self.diags.warning(
            "unmanaged-report",
            Some(path.to_path_buf()),
            None,
            format!(
                "`{name}` is an unmanaged report (no YAML lists it in `queries:`); unmanaged reports are for quick tests — add a YAML to make it a managed report"
            ),
        );
        self.check_unmanaged_sql(name, path);
        let query = QueryEntry {
            query: name.to_string(),
            path: path.to_path_buf(),
            tab: true,
            tab_name: None,
            anchor: None,
            header: None,
        };
        let base = BindingBase {
            profile: profile.clone(),
            vars,
            output,
        };
        if let Some(p) = &profile {
            used.source(p, None, None);
        }
        let b = self.finish_binding(
            name,
            None,
            &base,
            None,
            vec![query.clone()],
            path.to_path_buf(),
            used,
        );
        Report {
            name: name.to_string(),
            managed: false,
            file: path.to_path_buf(),
            folder,
            tags,
            queries: vec![query],
            default_set: None,
            timezone: layers
                .iter()
                .rev()
                .find_map(|l| l.timezone.clone())
                .or_else(|| project.timezone.clone()),
            has_sets: false,
            base: b.clone(),
            bindings: vec![b],
        }
    }

    /// Best-effort check on the raw text; the run path re-checks the rendered SQL.
    fn check_unmanaged_sql(&mut self, name: &str, path: &Path) {
        let Ok(text) = std::fs::read_to_string(self.root.join(path)) else {
            return;
        };
        let head = sqlsplit::strip_leading_comments(&text);
        if head.starts_with("{{") || head.starts_with("{%") || head.starts_with("{#") {
            return;
        }
        for st in sqlsplit::split(&text) {
            if !sqlsplit::classify(&st.text).is_read_only_safe() {
                let body = sqlsplit::strip_leading_comments(&st.text);
                let line = st.line + st.text[..st.text.len() - body.len()].matches('\n').count();
                self.diags.error(
                    "unmanaged-side-effect",
                    Some(path.to_path_buf()),
                    Some(line),
                    format!(
                        "unmanaged report `{name}` may only run SELECT/WITH or CREATE [OR REPLACE] TEMP|TEMPORARY TABLE|VIEW, but found `{}`; rewrite the statement, or give the report a YAML to declare it",
                        dre_protocol::util::summarize(body, 60)
                    ),
                );
            }
        }
    }

    // -- schedules ----------------------------------------------------------------------------

    /// Schedules live only in schedules.yml (named, with vars); anywhere else is an error.
    fn moved_to_schedules(&mut self, file: &Path, line: Option<usize>, ctx: &str) {
        self.diags.error(
            "schedule-moved",
            Some(file.to_path_buf()),
            line,
            format!(
                "{ctx} is no longer supported; declare schedules in schedules.yml as named entries (`name`, `report:`/`select:`, `cron`/`every`/`rrule`, optional `vars`)"
            ),
        );
    }

    fn parse_schedules(&mut self, files: &[Rc<YamlFile>], project: &Project) -> Vec<ScheduleEntry> {
        let mut out = Vec::new();
        let mut seen: BTreeMap<String, String> = BTreeMap::new();
        for yf in files {
            let Some(items) = yf.value.as_sequence() else {
                continue;
            };
            for (i, item) in items.iter().enumerate() {
                let m = item.as_mapping().unwrap();
                let line = nth_item_line(&yf.text, i);
                let file = Some(yf.display.clone());
                let s = |k: &str| m.get(k).and_then(Value::as_str).map(str::to_string);
                let (select, report, set) = (s("select"), s("report"), s("set"));
                let mut ok = true;
                let name = match s("name") {
                    Some(n) if is_identifier(&n) => {
                        if let Some(prev) = seen.get(&n) {
                            self.diags.error(
                                "duplicate-schedule-name",
                                file.clone(),
                                line,
                                format!("schedule `{n}` is already declared at {prev}; schedule names must be unique"),
                            );
                            ok = false;
                        }
                        seen.insert(
                            n.clone(),
                            format!("{}:{}", yf.display.display(), line.unwrap_or(0)),
                        );
                        n
                    }
                    Some(n) => {
                        self.diags.error(
                            "invalid-schedule",
                            file.clone(),
                            line,
                            format!("schedule name `{n}` must be letters, digits and `_`, not starting with a digit"),
                        );
                        ok = false;
                        n
                    }
                    None => {
                        self.diags.error(
                            "invalid-schedule",
                            file.clone(),
                            line,
                            "every schedule needs a `name`",
                        );
                        ok = false;
                        String::new()
                    }
                };
                let vars = match m.get("vars") {
                    None => JsonMap::new(),
                    Some(Value::Mapping(v)) => yaml_map_to_json(v),
                    Some(_) => {
                        self.diags.error(
                            "invalid-schedule",
                            file.clone(),
                            line,
                            format!("schedule `{name}`: `vars` must be a map"),
                        );
                        ok = false;
                        JsonMap::new()
                    }
                };
                let mut sched = Mapping::new();
                for (k, v) in m {
                    let Some(k) = k.as_str() else { continue };
                    if ["name", "select", "report", "set", "vars", "timezone"].contains(&k) {
                        continue;
                    }
                    if !schedule::SCHEDULE_KEYS.contains(&k) {
                        self.diags.error(
                            "invalid-schedule",
                            file.clone(),
                            line,
                            format!("unknown schedule key `{k}`"),
                        );
                        continue;
                    }
                    sched.insert(Value::String(k.to_string()), v.clone());
                }
                for e in schedule::validate_block(&sched) {
                    self.diags.error("invalid-schedule", file.clone(), line, e);
                    ok = false;
                }
                if select.is_some() && report.is_some() {
                    self.diags.error(
                        "invalid-schedule",
                        file.clone(),
                        line,
                        "use either `select:` or `report:`, not both",
                    );
                    ok = false;
                }
                if select.is_some() && set.is_some() {
                    self.diags.error(
                        "invalid-schedule",
                        file.clone(),
                        line,
                        "`set:` only applies with `report:`",
                    );
                    ok = false;
                }
                if let Some(sel) = &select {
                    match selector::resolve(project, sel) {
                        Ok(r) if r.is_empty() => {
                            self.diags.error(
                                "selector-matches-nothing",
                                file.clone(),
                                line,
                                format!("selector `{sel}` matches no report"),
                            );
                            ok = false;
                        }
                        Ok(_) => {}
                        Err(e) => {
                            self.diags.error(e.code(), file.clone(), line, e.to_string());
                            ok = false;
                        }
                    }
                }
                if let Some(rn) = &report {
                    match project.report(rn) {
                        None => {
                            self.diags.error(
                                "selector-matches-nothing",
                                file.clone(),
                                line,
                                format!("report `{rn}` doesn't exist"),
                            );
                            ok = false;
                        }
                        Some(r) => {
                            if let Some(sn) = &set
                                && r.binding(sn).is_none()
                            {
                                self.diags.error(
                                    "selector-matches-nothing",
                                    file.clone(),
                                    line,
                                    format!("report `{rn}` has no Set `{sn}`"),
                                );
                                ok = false;
                            }
                        }
                    }
                }
                if select.is_none() && report.is_none() {
                    self.diags.error(
                        "invalid-schedule",
                        file.clone(),
                        line,
                        format!("schedule `{name}` needs `report:` (optionally with `set:`) or `select:`"),
                    );
                    ok = false;
                }
                let timezone = match m.get("timezone") {
                    None => None,
                    Some(v) => {
                        let t = self.timezone_value(
                            v,
                            &yf.display,
                            line,
                            &format!("schedule `{name}`: `timezone`"),
                        );
                        ok &= t.is_some();
                        t
                    }
                };
                if ok {
                    out.push(ScheduleEntry {
                        name,
                        select,
                        report,
                        set,
                        schedule: yaml_map_to_json(&sched),
                        vars,
                        timezone,
                        location: (yf.display.clone(), line),
                    });
                }
            }
        }
        out
    }

    /// Warn when two schedules of one Binding would deliver to the same path: each schedule's
    /// paths are rendered with its vars and one fixed date. Paths that can't render offline (a
    /// `run_query()`, a missing `env_var()`) are skipped.
    fn check_schedule_paths(&mut self, project: &Project) {
        let date = chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        for report in &project.reports {
            for b in report.bindings.iter().filter(|b| b.schedules.len() > 1) {
                let paths: Vec<&String> = b
                    .output
                    .destinations
                    .iter()
                    .filter_map(|d| d.path.as_ref())
                    .collect();
                if paths.is_empty() {
                    continue;
                }
                let mut rendered: Vec<(&String, Vec<String>)> = Vec::new();
                for name in &b.schedules {
                    let Some(e) = project.schedules.iter().find(|e| &e.name == name) else {
                        continue;
                    };
                    let mut vars = b.vars.clone();
                    vars.extend(e.vars.clone());
                    let Ok(r) = crate::render::Renderer::new(crate::render::RendererConfig {
                        root: &project.root,
                        macros: &project.macros,
                        context: crate::render::RunContext {
                            report: report.name.clone(),
                            set: b.set.clone(),
                            target: String::new(),
                            profile: b.profile.clone().unwrap_or_default(),
                            source_type: String::new(),
                            schedule: Some(name.clone()),
                            date,
                            now: chrono::Utc::now(),
                            calendar: crate::dates::Calendar::default(),
                        },
                        vars,
                        cli_vars: self.opts.vars.clone(),
                        runner: None,
                        connections: None,
                        run_query_max_rows: project.run_query_max_rows,
                        sql: project.sql.clone(),
                        lookups: project.lookups.clone(),
                        lookup_inline_max_rows: project.lookup_inline_max_rows,
                        packages: project.packages.clone(),
                        project_name: project.name.clone(),
                        dispatch: project.dispatch.clone(),
                    }) else {
                        continue;
                    };
                    let out: Result<Vec<String>, _> =
                        paths.iter().map(|p| r.render(&report.file, p)).collect();
                    if let Ok(out) = out {
                        rendered.push((name, out));
                    }
                }
                for (i, (a, pa)) in rendered.iter().enumerate() {
                    for (bn, pb) in &rendered[i + 1..] {
                        if let Some(same) = pa.iter().find(|p| pb.contains(p)) {
                            self.diags.warning(
                                "schedule-path-clash",
                                Some(report.file.clone()),
                                None,
                                format!(
                                    "schedules `{a}` and `{bn}` both run report `{}`{} and deliver to `{same}`; the second overwrites the first — put a schedule var or `run.schedule` in the path",
                                    report.name,
                                    b.set.as_ref().map(|s| format!(", Set `{s}`,")).unwrap_or_default()
                                ),
                            );
                        }
                    }
                }
            }
        }
    }

    /// Attach every schedules.yml entry to the Bindings it targets: `report:` + `set:` one
    /// Binding, `report:` alone all of that report's Bindings, `select:` all Bindings of every
    /// report it matches. Several schedules per Binding is the point, not a conflict.
    fn apply_schedules(&mut self, project: &mut Project) {
        let entries = project.schedules.clone();
        for e in &entries {
            let reports: Vec<String> = match (&e.select, &e.report) {
                (Some(sel), _) => selector::resolve(project, sel)
                    .map(|r| r.into_iter().map(|r| r.name.clone()).collect())
                    .unwrap_or_default(),
                (_, Some(r)) => vec![r.clone()],
                _ => Vec::new(),
            };
            for report in project.reports.iter_mut().filter(|r| reports.contains(&r.name)) {
                for b in &mut report.bindings {
                    if e.set.is_none() || e.set == b.set {
                        b.schedules.push(e.name.clone());
                    }
                }
            }
        }
        self.check_schedule_paths(project);
    }

    // -- profiles and plugins -----------------------------------------------------------------

    fn check_profiles(&mut self, project: &Project, used: &Usage) {
        let profiles = &project.profiles;
        if used.sources.is_empty() && used.destinations.is_empty() {
            return;
        }
        if !profiles.exists() {
            self.diags.error(
                "profiles-missing",
                None,
                None,
                if profiles.found_by == "~/.dre" {
                    format!(
                        "the project references profiles, but no profiles.yml was found in the project directory or at {}",
                        profiles.path.display()
                    )
                } else {
                    format!(
                        "the project references profiles, but no profiles.yml was found at {} (from {})",
                        profiles.path.display(),
                        profiles.found_by
                    )
                },
            );
            return;
        }
        for (role, refs) in [
            (Role::Source, &used.sources),
            (Role::Destination, &used.destinations),
        ] {
            for (name, (file, line)) in refs {
                if role == Role::Destination && name == LOCAL_TYPE {
                    continue;
                }
                if !profiles.declares(role, name) {
                    self.diags.error(
                        "unknown-profile",
                        file.clone(),
                        *line,
                        format!(
                            "{} profile `{name}` isn't defined under `{}:` in {}",
                            role.as_str(),
                            role.section(),
                            profiles.path.display()
                        ),
                    );
                }
            }
        }
        if let Some(t) = &self.opts.target {
            let sources: Vec<&String> = used
                .sources
                .keys()
                .filter(|p| profiles.get(Role::Source, p).is_some())
                .collect();
            let defining = sources
                .iter()
                .filter(|p| profiles.get(Role::Source, p).unwrap().targets.contains_key(t))
                .count();
            if !sources.is_empty() && defining == 0 {
                self.diags.error(
                    "unknown-target",
                    None,
                    None,
                    format!(
                        "target `{t}` isn't defined by any referenced source profile ({})",
                        sources
                            .iter()
                            .map(|s| format!("`{s}`"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                );
            } else {
                for p in sources
                    .iter()
                    .filter(|p| !profiles.get(Role::Source, p).unwrap().targets.contains_key(t))
                {
                    self.diags.warning(
                        "missing-target",
                        None,
                        None,
                        format!("source profile `{p}` has no `{t}` target; its reports can't run with --target {t}"),
                    );
                }
            }
        }
    }

    /// A `plugins:` entry in its map form: `{name: foo, github: acme/dre-foo, version: "^1"}`.
    fn plugin_entry(
        &mut self,
        m: &Mapping,
        yf: &YamlFile,
        line: Option<usize>,
    ) -> Option<(String, Option<Value>, PluginSource)> {
        let file = Some(yf.display.clone());
        let name = m
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let line = yf.line_of(&name, line);
        let err = |s: &mut Self, msg: String| {
            s.diags.error(
                "invalid-plugin-declaration",
                file.clone(),
                line,
                format!("plugin package `{name}`: {msg}"),
            );
        };
        if name.is_empty() {
            err(self, "`name` must be a non-empty string".into());
            return None;
        }
        for k in m.keys().filter_map(Value::as_str) {
            if !["name", "version", "github", "local", "registry"].contains(&k) {
                err(
                    self,
                    format!(
                        "unknown key `{k}`; use `name`, `version` and one of `github`, `local`, `registry`"
                    ),
                );
                return None;
            }
        }
        let given: Vec<(&str, &str)> = ["github", "local", "registry"]
            .into_iter()
            .filter_map(|k| m.get(k).map(|v| (k, v.as_str().unwrap_or(""))))
            .collect();
        let source = match given.as_slice() {
            [] => PluginSource::Default,
            [(k, "")] => {
                err(self, format!("`{k}` must be a non-empty string"));
                return None;
            }
            [("github", r)] => {
                let ok = r.split('/').count() == 2 && r.split('/').all(|p| !p.is_empty());
                if !ok {
                    err(self, format!("`github: {r}` must be `owner/repo`"));
                    return None;
                }
                PluginSource::Github(r.to_string())
            }
            [("local", p)] => {
                if m.get("version").is_some() {
                    err(
                        self,
                        "a `local` package has no `version`: it's used as it is".into(),
                    );
                    return None;
                }
                PluginSource::Local(p.to_string())
            }
            [(_, u)] => PluginSource::Registry(u.to_string()),
            _ => {
                err(self, "give only one of `github`, `local` and `registry`".into());
                return None;
            }
        };
        Some((name, m.get("version").cloned(), source))
    }

    /// The `plugins:` declarations, merged across files; and every plugin the project uses, for
    /// [`crate::plugins::check_uses`] to check against what the declared packages provide.
    fn check_plugins(&mut self, decls: &[(Rc<YamlFile>, Mapping)], project: &mut Project, used: &Usage) {
        struct Decl {
            req: semver::VersionReq,
            raw: String,
            file: PathBuf,
            source: PluginSource,
        }
        let mut by_package: BTreeMap<String, Vec<Decl>> = BTreeMap::new();
        for (yf, m) in decls {
            for (block, v) in m {
                if block.as_str() != Some(PLUGINS_KEY) {
                    continue;
                }
                let line = yf.line_of(PLUGINS_KEY, None);
                let file = Some(yf.display.clone());
                let entries: Vec<(String, Option<Value>, PluginSource)> = match v {
                    Value::Sequence(items) => items
                        .iter()
                        .filter_map(|i| match i {
                            Value::String(s) => Some((s.clone(), None, PluginSource::Default)),
                            Value::Mapping(m) if m.get("name").is_some() => self.plugin_entry(m, yf, line),
                            Value::Mapping(m) if m.len() == 1 => {
                                let (k, v) = m.iter().next().unwrap();
                                k.as_str()
                                    .map(|k| (k.to_string(), Some(v.clone()), PluginSource::Default))
                            }
                            _ => {
                                self.diags.error(
                                    "invalid-plugin-declaration",
                                    file.clone(),
                                    line,
                                    "each `plugins` entry is a package name, `name: \"<version>\"`, or a map with `name:` and one of `github:`, `local:`, `registry:`",
                                );
                                None
                            }
                        })
                        .collect(),
                    Value::Mapping(m) => m
                        .iter()
                        .filter_map(|(k, v)| {
                            k.as_str().map(|k| (k.to_string(), Some(v.clone()), PluginSource::Default))
                        })
                        .collect(),
                    Value::Null => Vec::new(),
                    _ => {
                        self.diags.error(
                            "invalid-plugin-declaration",
                            file.clone(),
                            line,
                            "`plugins` must be a list like `- duckdb: \">=1.0\"`",
                        );
                        continue;
                    }
                };
                for (name, c, source) in entries {
                    if !dre_protocol::valid_name(&name) {
                        self.diags.error(
                            "invalid-plugin-declaration",
                            file.clone(),
                            yf.line_of(&name, line),
                            format!(
                                "plugin package `{name}`: a package name is lowercase letters, digits and `_`"
                            ),
                        );
                        continue;
                    }
                    let raw = match &c {
                        None | Some(Value::Null) => "*".to_string(),
                        Some(v) => crate::yaml::scalar_str(v).unwrap_or_default(),
                    };
                    match semver::VersionReq::parse(&raw) {
                        Ok(req) => by_package.entry(name).or_default().push(Decl {
                            req,
                            raw,
                            file: yf.display.clone(),
                            source,
                        }),
                        Err(e) => self.diags.error(
                            "invalid-version-constraint",
                            file.clone(),
                            yf.line_of(&name, line),
                            format!("plugin package `{name}`: invalid version constraint `{raw}`: {e}"),
                        ),
                    }
                }
            }
        }
        let mut out = Vec::new();
        for (name, ds) in &by_package {
            // Every declaration of one package must agree on where it comes from.
            let source = ds
                .iter()
                .map(|d| &d.source)
                .find(|s| !s.is_default())
                .cloned()
                .unwrap_or_default();
            if let Some(other) = ds.iter().find(|d| !d.source.is_default() && d.source != source) {
                self.diags.error(
                    "conflicting-plugin-sources",
                    Some(other.file.clone()),
                    None,
                    format!(
                        "plugin package `{name}` is declared with two sources: {} and {}",
                        source.lock_key().unwrap_or_default(),
                        other.source.lock_key().unwrap_or_default()
                    ),
                );
                project.plugins_incomplete = true;
                continue;
            }
            let reqs: Vec<&semver::VersionReq> = ds.iter().map(|d| &d.req).collect();
            if !constraints::compatible(&reqs) {
                for (i, a) in ds.iter().enumerate() {
                    for b in &ds[i + 1..] {
                        if !constraints::compatible(&[&a.req, &b.req]) {
                            self.diags.error(
                                "conflicting-plugin-constraints",
                                Some(b.file.clone()),
                                None,
                                format!(
                                    "plugin package `{name}` is declared `{}` in {} and `{}` in {}; no version satisfies both",
                                    a.raw,
                                    a.file.display(),
                                    b.raw,
                                    b.file.display()
                                ),
                            );
                        }
                    }
                }
                project.plugins_incomplete = true;
                continue;
            }
            let combined = constraints::combine(&reqs);
            let mut files: Vec<PathBuf> = ds.iter().map(|d| d.file.clone()).collect();
            files.dedup();
            out.push(PluginRequirement {
                name: name.clone(),
                version: combined.to_string(),
                declared_in: files,
                source,
            });
        }
        project.plugins = out;

        let mut uses = Vec::new();
        let profiles = &project.profiles;
        for (role, kind, refs) in [
            (Role::Source, PluginKind::Source, &used.sources),
            (Role::Destination, PluginKind::Destination, &used.destinations),
        ] {
            for name in refs.keys() {
                let Some(p) = profiles.get(role, name) else {
                    continue;
                };
                let role_name = role.as_str();
                for out_ in p.targets.values() {
                    if kind == PluginKind::Destination && out_.kind == LOCAL_TYPE {
                        continue;
                    }
                    let id = PluginId::new(kind, out_.kind.clone());
                    if uses.iter().any(|u: &PluginUse| u.plugin == id) {
                        continue;
                    }
                    uses.push(PluginUse {
                        plugin: id,
                        file: profiles.file.as_ref().map(|f| f.display.clone()),
                        line: profiles.line_of(role, name),
                        what: format!("`type: {}` used by {role_name} profile `{name}`", out_.kind),
                    });
                }
            }
        }
        for (fmt, (ctx, file)) in &used.formats {
            let note = if fmt == "csv" {
                " (csv is the built-in default output)"
            } else {
                ""
            };
            uses.push(PluginUse {
                plugin: PluginId::new(PluginKind::Format, fmt.clone()),
                file: Some(file.clone()),
                line: None,
                what: format!("format `{fmt}` is used by {ctx}{note}"),
            });
        }
        project.plugin_uses = uses;
    }

    /// `sources:`, `formats:` or `destinations:` where plugins were once declared.
    fn old_plugin_key(&mut self, yf: &YamlFile, key: &str) {
        self.diags.error(
            "moved-plugin-declaration",
            Some(yf.display.clone()),
            yf.line_of(key, None),
            format!(
                "`{key}:` no longer declares plugins: list plugin packages under `plugins:` instead (e.g. `plugins: [duckdb, object_store]`)"
            ),
        );
    }

    // -- Jinja pre-flight ---------------------------------------------------------------------

    fn preflight(&mut self, project: &Project, _used: &Usage) {
        let cli_owned: Vec<String> = self.opts.vars.keys().cloned().collect();
        let cli: BTreeSet<&str> = cli_owned.iter().map(String::as_str).collect();
        let mut sources: BTreeMap<PathBuf, String> = BTreeMap::new();
        let mut read = |root: &Path, p: &Path| -> Option<String> {
            if let Some(s) = sources.get(p) {
                return Some(s.clone());
            }
            let s = std::fs::read_to_string(root.join(p)).ok()?;
            sources.insert(p.to_path_buf(), s.clone());
            Some(s)
        };
        let mut checked: BTreeSet<PathBuf> = BTreeSet::new();
        let root = self.root.clone();
        // Macros: syntax, env_var, run.*; their definitions, for var() checks per Binding.
        let mut defs: BTreeMap<String, (PathBuf, preflight::MacroDef)> = BTreeMap::new();
        for m in &project.macros {
            if let Some(src) = read(&root, m) {
                self.check_template_text(m, &src, 0, None, &cli);
                checked.insert(m.clone());
                for d in preflight::macro_defs(&src) {
                    defs.insert(d.name.clone(), (m.clone(), d));
                }
            }
        }
        for r in &project.reports {
            for b in &r.bindings {
                let ctx = match &b.set {
                    Some(s) => format!("report `{}`, Set `{s}`", r.name),
                    None => format!("report `{}`", r.name),
                };
                let mut called: Vec<String> = Vec::new();
                let mut refs: Vec<String> = Vec::new();
                for q in &b.queries {
                    let Some(src) = read(&root, &q.path) else { continue };
                    let first = checked.insert(q.path.clone());
                    self.check_vars(&q.path, &src, 0, &b.vars, &cli, &ctx);
                    if first {
                        self.check_template_text(&q.path, &src, 0, None, &cli);
                    }
                    called.extend(preflight::called_names(&src));
                    refs.extend(preflight::refs(&src).into_iter().map(|(n, _)| n));
                }
                // SQL this Binding pulls in through ref(), checked in the Binding's context.
                let mut seen_refs = BTreeSet::new();
                while let Some(name) = refs.pop() {
                    let Some(path) = project.sql.get(&name) else {
                        continue;
                    };
                    if !seen_refs.insert(name) {
                        continue;
                    }
                    let Some(src) = read(&root, path) else { continue };
                    self.check_vars(path, &src, 0, &b.vars, &cli, &ctx);
                    if checked.insert(path.clone()) {
                        self.check_template_text(path, &src, 0, None, &cli);
                    }
                    called.extend(preflight::called_names(&src));
                    refs.extend(preflight::refs(&src).into_iter().map(|(n, _)| n));
                }
                // Macros this Binding calls, directly or through other macros.
                let mut seen = BTreeSet::new();
                while let Some(name) = called.pop() {
                    let Some((file, def)) = defs.get(&name) else {
                        continue;
                    };
                    if !seen.insert(name) {
                        continue;
                    }
                    self.check_vars(file, &def.body, def.line_offset, &b.vars, &cli, &ctx);
                    called.extend(preflight::called_names(&def.body));
                }
                // Templated output values render with the same context.
                let mut values: Vec<String> = Vec::new();
                for d in &b.output.destinations {
                    values.extend(d.path.clone());
                    values.extend(d.options.values().flat_map(json_strings));
                }
                if let Some(t) = &b.output.template {
                    values.extend(t.bindings.iter().filter_map(|tb| tb.value.clone()));
                }
                for v in values.iter().filter(|v| preflight::is_templated(v)) {
                    let yf = std::fs::read_to_string(root.join(&r.file)).unwrap_or_default();
                    let line = yf.lines().position(|l| l.contains(v.as_str())).map(|i| i + 1);
                    self.check_template_text(&r.file, v, line.map_or(0, |l| l - 1), line, &cli);
                    self.check_vars(&r.file, v, line.map_or(0, |l| l - 1), &b.vars, &cli, &ctx);
                }
            }
        }
    }

    /// Syntax, `env_var()` and `run.*` checks for one template source.
    fn check_template_text(
        &mut self,
        file: &Path,
        src: &str,
        line_offset: usize,
        fixed_line: Option<usize>,
        _cli: &BTreeSet<&str>,
    ) {
        let f = Some(file.to_path_buf());
        if let Err((line, msg)) = preflight::check_syntax(&file.to_string_lossy(), src) {
            let line = fixed_line.or(line.map(|l| l + line_offset));
            self.diags.error(
                "jinja-syntax",
                f.clone(),
                line,
                format!("Jinja syntax error: {msg}"),
            );
            return;
        }
        for c in preflight::calls(src)
            .into_iter()
            .filter(|c| c.func == "env_var" && !c.has_default)
        {
            if std::env::var_os(&c.name).is_none() {
                self.diags.error(
                    "unset-env-var",
                    f.clone(),
                    Some(fixed_line.unwrap_or(c.line + line_offset)),
                    format!(
                        "`env_var('{}')`: environment variable `{}` is not set and no default is given",
                        c.name, c.name
                    ),
                );
            }
        }
        for (r, line) in preflight::unknown_run_refs(src) {
            self.diags.error(
                "unknown-run-attribute",
                f.clone(),
                Some(fixed_line.unwrap_or(line + line_offset)),
                format!(
                    "`{r}` isn't part of the run context; known: run.report, run.set, run.target, run.profile, run.source_type, run.schedule, run.date (a date: .prev_month, .month_start, .yyyymmdd, ...), run.now, run.timezone, run.date_format(...)"
                ),
            );
        }
    }

    fn check_vars(
        &mut self,
        file: &Path,
        src: &str,
        line_offset: usize,
        vars: &JsonMap<String, Json>,
        cli: &BTreeSet<&str>,
        ctx: &str,
    ) {
        for c in preflight::calls(src)
            .into_iter()
            .filter(|c| c.func == "var" && !c.has_default)
        {
            if !vars.contains_key(&c.name) && !cli.contains(c.name.as_str()) {
                self.diags.error(
                    "unresolved-var",
                    Some(file.to_path_buf()),
                    Some(c.line + line_offset),
                    format!(
                        "`var('{}')` has no value for {ctx} (checked --var, Set, report, folder and project vars) and no default",
                        c.name
                    ),
                );
            }
        }
    }

    fn check_template_files(&mut self, project: &Project) {
        let mut seen = BTreeSet::new();
        for r in &project.reports {
            for b in &r.bindings {
                let Some(t) = &b.output.template else { continue };
                if !seen.insert((r.name.clone(), t.file.clone(), format!("{:?}", t.bindings))) {
                    continue;
                }
                let candidates = [self.root.join(&t.file), self.root.join("templates").join(&t.file)];
                let Some(path) = candidates.iter().find(|p| p.is_file()) else {
                    self.diags.error(
                        "missing-template",
                        Some(r.file.clone()),
                        None,
                        format!("report `{}`: template file `{}` doesn't exist", r.name, t.file),
                    );
                    continue;
                };
                match template_sheets(path) {
                    Ok(sheets) => {
                        for tb in &t.bindings {
                            if !sheets.contains(&tb.sheet) {
                                self.diags.error(
                                    "invalid-template",
                                    Some(r.file.clone()),
                                    None,
                                    format!(
                                        "report `{}`: template `{}` has no sheet `{}` (sheets: {})",
                                        r.name,
                                        t.file,
                                        tb.sheet,
                                        sheets.join(", ")
                                    ),
                                );
                            }
                        }
                    }
                    Err(e) => self.diags.error(
                        "invalid-template",
                        Some(r.file.clone()),
                        None,
                        format!("report `{}`: can't read template `{}`: {e}", r.name, t.file),
                    ),
                }
            }
        }
    }
}

/// Sheet names in an xlsx template, read without modifying it.
pub fn template_sheets(path: &Path) -> Result<Vec<String>, String> {
    use calamine::Reader;
    let wb: calamine::Xlsx<_> =
        calamine::open_workbook(path).map_err(|e: calamine::XlsxError| e.to_string())?;
    Ok(wb.sheet_names().to_vec())
}

struct RawReport {
    name: String,
    file: Rc<YamlFile>,
    folder: Vec<String>,
    keys: BTreeMap<String, Located<Value>>,
}

struct BindingBase {
    profile: Option<String>,
    vars: JsonMap<String, Json>,
    output: Mapping,
}

/// Everything the project references, for profile and plugin checks.
#[derive(Default)]
struct Usage {
    sources: BTreeMap<String, (Option<PathBuf>, Option<usize>)>,
    destinations: BTreeMap<String, (Option<PathBuf>, Option<usize>)>,
    /// format → first user, for messages.
    formats: BTreeMap<String, (String, PathBuf)>,
}

impl Usage {
    fn source(&mut self, p: &str, file: Option<PathBuf>, line: Option<usize>) {
        let e = self.sources.entry(p.to_string()).or_insert((None, None));
        if e.0.is_none() {
            *e = (file, line);
        }
    }
    fn destination(&mut self, p: &str, file: Option<PathBuf>, line: Option<usize>) {
        let e = self.destinations.entry(p.to_string()).or_insert((None, None));
        if e.0.is_none() {
            *e = (file, line);
        }
    }
    fn format(&mut self, f: &str, ctx: &str, file: &Path) {
        self.formats
            .entry(f.to_string())
            .or_insert_with(|| (ctx.to_string(), file.to_path_buf()));
    }
}

// ---------------------------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------------------------

fn builtin_output() -> Mapping {
    let mut m = Mapping::new();
    m.insert(Value::String("format".into()), Value::String("csv".into()));
    m
}

fn project_default_output(p: &Project) -> Option<Mapping> {
    let yf = YamlFile::parse(
        std::fs::read_to_string(p.root.join(PROJECT_FILE)).ok()?,
        PathBuf::from(PROJECT_FILE),
        &mut Diagnostics::default(),
    )?;
    yf.value
        .get("default_output")
        .and_then(Value::as_mapping)
        .cloned()
}

/// Merge an output layer over `base`. Changing `format` drops the lower layers' format options,
/// since they belong to a different format.
///
/// `destination` (a map or a list of maps):
/// - a list replaces whatever was inherited;
/// - a map naming a different `profile` replaces it too, so one plugin's options never leak into
///   another's;
/// - a map without `profile` (or with the same one) merges key by key into the inherited single
///   destination, so a Binding can override just `path`. With several inherited destinations
///   that's ambiguous: the layer's destination is ignored and the returned message says why.
pub fn merge_output(base: &mut Mapping, over: &Mapping) -> Option<String> {
    let fmt_key = Value::String("format".into());
    if let Some(f) = over.get(&fmt_key)
        && base.get(&fmt_key) != Some(f)
    {
        base.retain(|k, _| is_one_of(k, &["destination", "template"]));
    }
    let mut problem = None;
    for (k, v) in over {
        if k.as_str() == Some("destination")
            && let Value::Mapping(o) = v
        {
            let profile = |m: &Mapping| m.get("profile").cloned();
            match base.get_mut(k) {
                Some(Value::Sequence(list)) if o.get("profile").is_none() && list.len() > 1 => {
                    problem = Some(format!(
                        "this overrides `destination` with no `profile:`, but it inherits {} destinations, so it's unclear which one to change; override the full list instead",
                        list.len()
                    ));
                    continue;
                }
                Some(Value::Sequence(list))
                    if list.len() == 1
                        && list[0]
                            .as_mapping()
                            .is_some_and(|b| o.get("profile").is_none() || profile(b) == profile(o)) =>
                {
                    let mut b = list[0].as_mapping().cloned().unwrap_or_default();
                    b.extend(o.clone());
                    base.insert(k.clone(), Value::Mapping(b));
                    continue;
                }
                Some(Value::Mapping(b)) if o.get("profile").is_none() || profile(b) == profile(o) => {
                    b.extend(o.clone());
                    continue;
                }
                _ => {}
            }
        }
        base.insert(k.clone(), v.clone());
    }
    problem
}

fn folder_layers<'a>(folders: &'a BTreeMap<Vec<String>, FolderCfg>, folder: &[String]) -> Vec<&'a FolderCfg> {
    (0..=folder.len())
        .filter_map(|n| folders.get(&folder[..n].to_vec()))
        .collect()
}

fn folder_segments(rel: &Path) -> Vec<String> {
    rel.components()
        .skip(1)
        .map(|c| c.as_os_str().to_string_lossy().to_string())
        .collect()
}

pub fn dotted(path: &[String]) -> String {
    path.join(".")
}

fn pick(v: &Value, keys: &[&str]) -> Mapping {
    v.as_mapping()
        .map(|m| {
            m.iter()
                .filter(|(k, _)| is_one_of(k, keys))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect()
        })
        .unwrap_or_default()
}

/// Every string inside a JSON value (the templated values of a destination option).
fn json_strings(v: &Json) -> Vec<String> {
    match v {
        Json::String(s) => vec![s.clone()],
        Json::Array(a) => a.iter().flat_map(json_strings).collect(),
        Json::Object(o) => o.values().flat_map(json_strings).collect(),
        _ => Vec::new(),
    }
}

fn is_one_of(k: &Value, keys: &[&str]) -> bool {
    k.as_str().is_some_and(|k| keys.contains(&k))
}

/// Every entry is a Set: a map of `profile`/`vars`, which may be empty (`plain: {}`, the
/// report's defaults) or left blank.
fn is_set_registry(m: &Mapping) -> bool {
    m.values().any(Value::is_mapping)
        && m.values().all(|v| {
            v.is_null()
                || v.as_mapping()
                    .is_some_and(|e| e.keys().all(|k| is_one_of(k, &["profile", "vars"])))
        })
}

fn string_list(v: &Value) -> Option<Vec<String>> {
    v.as_sequence()?
        .iter()
        .map(|i| i.as_str().map(str::to_string))
        .collect()
}

fn entry_name(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Mapping(m) => m.get("query").and_then(Value::as_str).map(str::to_string),
        _ => None,
    }
}

fn dedup(v: &mut Vec<String>) {
    let mut seen = BTreeSet::new();
    v.retain(|t| seen.insert(t.clone()));
}

fn nth_item_line(text: &str, n: usize) -> Option<usize> {
    text.lines()
        .enumerate()
        .filter(|(_, l)| l.starts_with("- ") || l.trim() == "-")
        .nth(n)
        .map(|(i, _)| i + 1)
}

pub fn yaml_to_json(v: &Value) -> Json {
    serde_json::to_value(v).unwrap_or(Json::Null)
}

pub fn yaml_map_to_json(m: &Mapping) -> JsonMap<String, Json> {
    match yaml_to_json(&Value::Mapping(m.clone())) {
        Json::Object(o) => o,
        _ => JsonMap::new(),
    }
}

/// Letters, digits and `_`, not starting with a digit.
fn is_identifier(s: &str) -> bool {
    !s.is_empty()
        && !s.starts_with(|c: char| c.is_ascii_digit())
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}
