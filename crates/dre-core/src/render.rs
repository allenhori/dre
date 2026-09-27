//! Runtime Jinja rendering: one environment per Binding, shared by SQL, output paths and
//! template values. Provides `run.*`, `var()`, `env_var()`, `run_query()`, `columns()`,
//! `ref()`, `target`, `profile()`, the calendar functions in [`crate::dates`] and every macro in
//! `macros/`.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use chrono::{NaiveDate, Utc};

use crate::dates::{Calendar, Date, DateTime};

use crate::lookups::{self, Cell, Load, Lookup, Table};
use crate::packages::{DispatchOrder, Package};
use minijinja::value::{Enumerator, Kwargs, Object, ObjectRepr, Value};
use minijinja::{Environment, Error, ErrorKind, UndefinedBehavior};
use serde_json::{Map as JsonMap, Value as Json};

const MACROS: &str = "__dre_macros__";

/// Ambient `run.*` context.
#[derive(Debug, Clone)]
pub struct RunContext {
    pub report: String,
    pub set: Option<String>,
    pub target: String,
    pub profile: String,
    /// The source plugin type (`duckdb`, `databricks`, ...), for SQL that differs per database.
    pub source_type: String,
    /// The schedule's name under `dre run --schedule`, else `None`.
    pub schedule: Option<String>,
    pub date: NaiveDate,
    /// When the run started: `run.now`.
    pub now: chrono::DateTime<Utc>,
    /// The run's timezone and week settings.
    pub calendar: Calendar,
}

/// Rows returned to templates by `run_query()`.
#[derive(Debug, Clone, Default)]
pub struct QueryRows {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Value>>,
}

/// One column of a relation, as `columns()` returns it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Column {
    pub name: String,
    /// The Arrow type name (`Int64`, `Utf8`, `Date32`, ...).
    pub data_type: String,
}

/// Executes `run_query()` SQL on the Binding's own session.
pub trait QueryRunner: Send + Sync {
    /// Run `sql`, failing if it returns more than `max_rows` rows.
    fn run_query(&self, sql: &str, max_rows: u64) -> Result<QueryRows, String>;
    /// The columns `sql` returns, without fetching rows.
    fn columns(&self, sql: &str) -> Result<Vec<Column>, String>;
    /// Load a lookup into a temp table on the session: `Some((relation, plugin warning))`, or
    /// `None` when the source can't load rows.
    fn load(&self, _name: &str, _table: &Table) -> Result<Option<(String, Option<String>)>, String> {
        Ok(None)
    }
}

/// One profile target as templates see it: `target.*` and `profile('name').*`.
#[derive(Debug, Clone, Default)]
pub struct Connection {
    /// The profile's name.
    pub profile: String,
    /// The target's name (`dev`, `prod`).
    pub target: String,
    /// The plugin type.
    pub kind: String,
    /// Every field, with `env_var()` already rendered.
    pub fields: JsonMap<String, Json>,
    /// Fields that hold secrets: reading one is an error.
    pub secrets: Vec<String>,
}

/// Where `target` and `profile()` get their values. `role` is `source` or `destination`.
pub trait Connections: Send + Sync {
    /// The Binding's source connection.
    fn source(&self) -> Result<Connection, String>;
    fn profile(&self, name: &str, role: Option<&str>) -> Result<Connection, String>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderError {
    pub file: PathBuf,
    pub line: Option<usize>,
    pub message: String,
}

impl fmt::Display for RenderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.line {
            Some(l) => write!(f, "{}:{l}: {}", self.file.display(), self.message),
            None => write!(f, "{}: {}", self.file.display(), self.message),
        }
    }
}

pub struct Renderer {
    env: Environment<'static>,
    /// What `columns()` found, by relation, for the file being rendered.
    columns_cache: Arc<Mutex<BTreeMap<String, Value>>>,
    /// Warnings raised while rendering (e.g. a large lookup inlined), drained by the caller.
    warnings: Arc<Mutex<Vec<String>>>,
    import: String,
    /// Per combined macro template: `(first line, file, line count)` of each macro file in it.
    macro_files: BTreeMap<String, Vec<(usize, PathBuf, usize)>>,
}

pub struct RendererConfig<'a> {
    pub root: &'a Path,
    pub macros: &'a [PathBuf],
    pub context: RunContext,
    /// The Binding's merged vars.
    pub vars: JsonMap<String, Json>,
    /// `--var` overrides, highest precedence.
    pub cli_vars: BTreeMap<String, String>,
    pub runner: Option<Arc<dyn QueryRunner>>,
    /// Profiles for `target` and `profile()`; `None` where no profile resolves (offline checks).
    pub connections: Option<Arc<dyn Connections>>,
    pub run_query_max_rows: u64,
    /// Every `.sql` file `ref()` can name, by basename, relative to `root`.
    pub sql: BTreeMap<String, PathBuf>,
    /// Lookups `ref()` and `lookup()` can name.
    pub lookups: BTreeMap<String, Lookup>,
    pub lookup_inline_max_rows: u64,
    /// Macro packages, imported under their names.
    pub packages: Vec<Package>,
    /// The project's name: the root namespace in `dispatch()` search orders.
    pub project_name: String,
    pub dispatch: DispatchOrder,
}

impl Renderer {
    pub fn new(cfg: RendererConfig<'_>) -> Result<Renderer, RenderError> {
        let mut env = Environment::new();
        env.set_undefined_behavior(UndefinedBehavior::Strict);
        env.set_keep_trailing_newline(true);

        let vars = cfg.vars;
        let cli = cfg.cli_vars;
        env.add_function(
            "var",
            move |name: String, default: Option<Value>| -> Result<Value, Error> {
                if let Some(v) = cli.get(&name) {
                    return Ok(Value::from(v.clone()));
                }
                if let Some(v) = vars.get(&name) {
                    return Ok(Value::from_serialize(v));
                }
                default.ok_or_else(|| {
                    Error::new(
                        ErrorKind::UndefinedError,
                        format!("`var('{name}')` has no value and no default"),
                    )
                })
            },
        );
        env.add_function("env_var", |name: String, default: Option<Value>| -> Result<Value, Error> {
            match std::env::var(&name) {
                Ok(v) => Ok(Value::from(v)),
                Err(_) => default.ok_or_else(|| {
                    Error::new(
                        ErrorKind::UndefinedError,
                        format!("`env_var('{name}')`: environment variable `{name}` is not set and no default is given"),
                    )
                }),
            }
        });
        env.add_function("raise_error", |message: String| -> Result<Value, Error> {
            Err(Error::new(ErrorKind::InvalidOperation, message))
        });
        let source_type = cfg.context.source_type.clone();
        crate::dates::register(&mut env, cfg.context.calendar, cfg.context.date);
        env.add_global("run", Value::from_object(Run(cfg.context)));
        if let Some(c) = cfg.connections {
            // Resolved on first use: a report that never reads `target` never asks for it.
            env.add_global(
                "target",
                Value::from_object(LazyTarget {
                    connections: c.clone(),
                    resolved: std::sync::OnceLock::new(),
                }),
            );
            env.add_function(
                "profile",
                move |name: String, kwargs: Kwargs| -> Result<Value, Error> {
                    let role: Option<String> = kwargs.get("role")?;
                    kwargs.assert_all_used()?;
                    if let Some(r) = &role
                        && r != "source"
                        && r != "destination"
                    {
                        return Err(Error::new(
                            ErrorKind::InvalidOperation,
                            format!(
                                "`profile('{name}', role='{r}')`: role must be 'source' or 'destination'"
                            ),
                        ));
                    }
                    c.profile(&name, role.as_deref())
                        .map(|t| Value::from_object(ConnectionValue(t)))
                        .map_err(|e| {
                            Error::new(ErrorKind::InvalidOperation, format!("`profile('{name}')`: {e}"))
                        })
                },
            );
        }
        let runner = cfg.runner;
        let warnings: Arc<Mutex<Vec<String>>> = Arc::default();
        let lookups = Arc::new(Lookups {
            root: cfg.root.to_path_buf(),
            defs: cfg.lookups,
            inline_max: cfg.lookup_inline_max_rows,
            runner: runner.clone(),
            tables: Mutex::default(),
            loaded: Mutex::default(),
            warnings: warnings.clone(),
        });
        let l = lookups.clone();
        env.add_function("lookup", move |name: String| -> Result<Value, Error> {
            let t = l.table(&name)?;
            let rows = QueryRows {
                columns: t.columns.clone(),
                rows: t
                    .rows
                    .iter()
                    .map(|r| r.iter().map(cell_value).collect())
                    .collect(),
            };
            Ok(Value::from_object(QueryResult::new(rows)))
        });
        let default_max = cfg.run_query_max_rows;
        let runner_c = runner.clone();
        env.add_function(
            "run_query",
            move |sql: String, kwargs: Kwargs| -> Result<Value, Error> {
                let max_rows: Option<u64> = kwargs.get("max_rows")?;
                kwargs.assert_all_used()?;
                let Some(runner) = &runner else {
                    return Err(Error::new(
                        ErrorKind::InvalidOperation,
                        "`run_query()` has no connection here",
                    ));
                };
                let rows = runner
                    .run_query(&sql, max_rows.unwrap_or(default_max))
                    .map_err(|e| Error::new(ErrorKind::InvalidOperation, e))?;
                Ok(Value::from_object(QueryResult::new(rows)))
            },
        );
        // Per rendered file: a later query may have recreated the relation.
        let columns_cache: Arc<Mutex<BTreeMap<String, Value>>> = Arc::default();
        let cache = columns_cache.clone();
        env.add_function("columns", move |rel: String| -> Result<Value, Error> {
            if let Some(v) = cache.lock().unwrap().get(&rel) {
                return Ok(v.clone());
            }
            let Some(runner) = &runner_c else {
                return Err(Error::new(
                    ErrorKind::InvalidOperation,
                    format!("`columns('{rel}')` has no connection here"),
                ));
            };
            let sql = format!("select * from {rel} as _dre_cols where 1=0");
            let cols = runner.columns(&sql).map_err(|e| {
                Error::new(
                    ErrorKind::InvalidOperation,
                    format!("`columns('{rel}')` failed: {e}"),
                )
            })?;
            let v = Value::from(
                cols.into_iter()
                    .map(|c| {
                        Value::from_iter([("name", Value::from(c.name)), ("type", Value::from(c.data_type))])
                    })
                    .collect::<Vec<_>>(),
            );
            cache.lock().unwrap().insert(rel, v.clone());
            Ok(v)
        });

        // Every macro file is combined into one template, imported (on the first line, so line
        // numbers don't move) into everything rendered. Each package gets its own template,
        // imported under the package's name.
        let root = cfg.root.to_path_buf();
        let display = |p: &Path| {
            p.strip_prefix(&root)
                .map(Path::to_path_buf)
                .unwrap_or_else(|_| p.to_path_buf())
        };
        let own: Vec<(PathBuf, PathBuf)> = cfg.macros.iter().map(|m| (cfg.root.join(m), m.clone())).collect();
        let (combined, names, spans) = combine(&own)?;
        let mut import = if names.is_empty() {
            String::new()
        } else {
            format!("{{% from \"{MACROS}\" import {} %}}", names.join(", "))
        };
        let mut templates = vec![(MACROS.to_string(), combined, spans, !names.is_empty())];
        for p in &cfg.packages {
            let files: Vec<(PathBuf, PathBuf)> = p.macros.iter().map(|m| (m.clone(), display(m))).collect();
            let (src, _, spans) = combine(&files)?;
            let t = package_template(&p.name);
            import.push_str(&format!("{{% import \"{t}\" as {} %}}", p.name));
            templates.push((t, src, spans, true));
        }
        add_ref(&mut env, cfg.root, cfg.sql, lookups, &import);
        add_dispatch(
            &mut env,
            &cfg.project_name,
            !names.is_empty(),
            &cfg.packages,
            cfg.dispatch,
            source_type,
        );
        let mut r = Renderer {
            env,
            columns_cache,
            warnings,
            import,
            macro_files: BTreeMap::new(),
        };
        for (name, src, spans, add) in templates {
            r.macro_files.insert(name.clone(), spans);
            if add {
                r.env
                    .add_template_owned(name.clone(), src)
                    .map_err(|e| r.error(Path::new(&name), &e))?;
            }
        }
        Ok(r)
    }

    /// Warnings raised since the last call.
    pub fn take_warnings(&self) -> Vec<String> {
        std::mem::take(&mut *self.warnings.lock().unwrap())
    }

    /// Render `src`, reporting errors against `file`.
    pub fn render(&self, file: &Path, src: &str) -> Result<String, RenderError> {
        self.columns_cache.lock().unwrap().clear();
        let full = format!("{}{src}", self.import);
        let name = file.to_string_lossy().to_string();
        let tmpl = self
            .env
            .template_from_named_str(&name, &full)
            .map_err(|e| self.error(file, &e))?;
        tmpl.render(()).map_err(|e| self.error(file, &e))
    }

    fn error(&self, file: &Path, e: &Error) -> RenderError {
        // Report the innermost location: a macro file if the error happened inside a macro.
        let mut deepest: &Error = e;
        while let Some(src) = std::error::Error::source(deepest).and_then(|s| s.downcast_ref::<Error>()) {
            deepest = src;
        }
        let (name, line) = match (deepest.name(), deepest.line()) {
            (Some(n), l) => (n.to_string(), l),
            _ => (e.name().unwrap_or_default().to_string(), e.line()),
        };
        let (file, line) = if let Some(spans) = self.macro_files.get(&name) {
            match line.and_then(|l| {
                spans
                    .iter()
                    .find(|(s, _, n)| l >= *s && l < s + n)
                    .map(|(s, f, _)| (f, l - s + 1))
            }) {
                Some((f, l)) => (f.clone(), Some(l)),
                None => (file.to_path_buf(), line),
            }
        } else {
            (file.to_path_buf(), line)
        };
        let message = deepest
            .detail()
            .map(str::to_string)
            .unwrap_or_else(|| deepest.kind().to_string());
        RenderError { file, line, message }
    }
}

fn package_template(name: &str) -> String {
    format!("__dre_package_{name}__")
}

/// Concatenate macro files `(path to read, path to report)` into one template source, with
/// every macro name defined and where each file starts.
#[allow(clippy::type_complexity)]
fn combine(
    files: &[(PathBuf, PathBuf)],
) -> Result<(String, Vec<String>, Vec<(usize, PathBuf, usize)>), RenderError> {
    let re = regex::Regex::new(r"\{%-?\s*macro\s+([A-Za-z_]\w*)").unwrap();
    let mut combined = String::new();
    let mut names = Vec::new();
    let mut spans = Vec::new();
    for (read, shown) in files {
        let src = std::fs::read_to_string(read).map_err(|e| RenderError {
            file: shown.clone(),
            line: None,
            message: format!("can't read macro file: {e}"),
        })?;
        let start = combined.matches('\n').count() + 1;
        names.extend(re.captures_iter(&src).map(|c| c[1].to_string()));
        combined.push_str(&src);
        if !combined.ends_with('\n') {
            combined.push('\n');
        }
        spans.push((start, shown.clone(), src.matches('\n').count() + 1));
    }
    Ok((combined, names, spans))
}

/// `dispatch('name', 'namespace')` returns the macro to call for the active source: in each
/// namespace of the search order, `<source type>__name`, then `default__name`. The search order
/// is `dispatch:` in dre_project.yml, else the root project then the namespace, so a project can
/// override a package's macro by defining, say, `databricks__name` in its own macros/.
fn add_dispatch(
    env: &mut Environment<'static>,
    project: &str,
    project_has_macros: bool,
    packages: &[Package],
    order: DispatchOrder,
    source_type: String,
) {
    let project = project.to_string();
    let templates: BTreeMap<String, String> = packages
        .iter()
        .map(|p| (p.name.clone(), package_template(&p.name)))
        .collect();
    env.add_function(
        "dispatch",
        move |state: &minijinja::State<'_, '_>,
              name: String,
              namespace: Option<String>|
              -> Result<Value, Error> {
            let ns = namespace.unwrap_or_else(|| project.clone());
            let search = order.get(&ns).cloned().unwrap_or_else(|| {
                if ns == project {
                    vec![ns.clone()]
                } else {
                    vec![project.clone(), ns.clone()]
                }
            });
            let candidates = [format!("{source_type}__{name}"), format!("default__{name}")];
            for n in &search {
                let template = if n == &project {
                    if !project_has_macros {
                        continue;
                    }
                    MACROS.to_string()
                } else {
                    match templates.get(n) {
                        Some(t) => t.clone(),
                        None => continue,
                    }
                };
                let tmpl = state.env().get_template(&template)?;
                let captured = tmpl.render_captured(())?;
                let st = captured.state();
                for c in &candidates {
                    if st.lookup(c).is_some_and(|v| !v.is_undefined()) {
                        return Ok(Value::from_object(Dispatched {
                            template,
                            name: c.clone(),
                        }));
                    }
                }
            }
            Err(Error::new(
                ErrorKind::InvalidOperation,
                format!(
                    "`dispatch('{name}', '{ns}')`: no `{}` or `{}` in {}",
                    candidates[0],
                    candidates[1],
                    search.join(", ")
                ),
            ))
        },
    );
}

/// A macro chosen by `dispatch()`, called by name in its own template.
#[derive(Debug)]
struct Dispatched {
    template: String,
    name: String,
}

impl Object for Dispatched {
    fn call(self: &Arc<Self>, state: &minijinja::State<'_, '_>, args: &[Value]) -> Result<Value, Error> {
        let tmpl = state.env().get_template(&self.template)?;
        let out = tmpl.render_captured(())?.state().call_macro(&self.name, args)?;
        Ok(Value::from(out))
    }
}

/// `ref('name')`: another `.sql` file, rendered in the same context (vars, `run.*`, macros, the
/// Binding's connection) and returned in parentheses, ready to use as a subquery or CTE body.
/// DRE builds no tables, so a ref inlines SQL rather than pointing at a materialised model.
fn add_ref(
    env: &mut Environment<'static>,
    root: &Path,
    sql: BTreeMap<String, PathBuf>,
    lookups: Arc<Lookups>,
    import: &str,
) {
    let root = root.to_path_buf();
    let import = import.to_string();
    // The chain of refs being rendered, to report cycles instead of recursing forever.
    let stack: Arc<Mutex<Vec<String>>> = Arc::default();
    env.add_function(
        "ref",
        move |state: &minijinja::State<'_, '_>, name: String| -> Result<Value, Error> {
            if lookups.defs.contains_key(&name) {
                return lookups.relation(&name).map(Value::from);
            }
            let Some(rel) = sql.get(&name) else {
                return Err(Error::new(
                    ErrorKind::InvalidOperation,
                    format!("`ref('{name}')`: no `{name}.sql` and no lookup `{name}` in the project"),
                ));
            };
            {
                let mut chain = stack.lock().unwrap();
                if chain.contains(&name) {
                    chain.push(name.clone());
                    let cycle = chain.join(" → ");
                    chain.clear();
                    return Err(Error::new(
                        ErrorKind::InvalidOperation,
                        format!("`ref()` cycle: {cycle}"),
                    ));
                }
                chain.push(name.clone());
            }
            let result = (|| {
                let src = std::fs::read_to_string(root.join(rel)).map_err(|e| {
                    Error::new(
                        ErrorKind::InvalidOperation,
                        format!("can't read {}: {e}", rel.display()),
                    )
                })?;
                let file = rel.to_string_lossy().to_string();
                let rendered = state
                    .env()
                    .template_from_named_str(&file, &format!("{import}{src}"))?
                    .render(())?;
                let statements = crate::sqlsplit::split(&rendered);
                match statements.as_slice() {
                    [one] => Ok(Value::from(format!("(\n{}\n)", one.text))),
                    _ => Err(Error::new(
                        ErrorKind::InvalidOperation,
                        format!(
                            "`ref('{name}')` needs {} to hold exactly one statement, but it has {}",
                            rel.display(),
                            statements.len()
                        ),
                    )),
                }
            })();
            let mut chain = stack.lock().unwrap();
            if chain.last() == Some(&name) {
                chain.pop();
            }
            result
        },
    );
}

/// Lookups for one Binding: read once, inlined or loaded once.
struct Lookups {
    root: PathBuf,
    defs: BTreeMap<String, Lookup>,
    inline_max: u64,
    runner: Option<Arc<dyn QueryRunner>>,
    tables: Mutex<BTreeMap<String, Arc<Table>>>,
    /// What `ref()` returns for each lookup already used: inline SQL or a temp table's name.
    loaded: Mutex<BTreeMap<String, String>>,
    warnings: Arc<Mutex<Vec<String>>>,
}

impl Lookups {
    fn table(&self, name: &str) -> Result<Arc<Table>, Error> {
        let def = self.defs.get(name).ok_or_else(|| {
            Error::new(
                ErrorKind::InvalidOperation,
                format!("no lookup `{name}` under lookups/"),
            )
        })?;
        if let Some(t) = self.tables.lock().unwrap().get(name) {
            return Ok(t.clone());
        }
        let t = Arc::new(lookups::read(&self.root, def).map_err(|e| {
            Error::new(
                ErrorKind::InvalidOperation,
                format!("{}: {e}", def.file.display()),
            )
        })?);
        self.tables.lock().unwrap().insert(name.to_string(), t.clone());
        Ok(t)
    }

    fn relation(&self, name: &str) -> Result<String, Error> {
        if let Some(r) = self.loaded.lock().unwrap().get(name) {
            return Ok(r.clone());
        }
        let t = self.table(name)?;
        let rows = t.rows.len() as u64;
        let load = match self.defs[name].load {
            Load::Inline => false,
            Load::TempTable => true,
            Load::Auto => rows > self.inline_max,
        };
        let loaded = match (&self.runner, load) {
            (Some(runner), true) => runner.load(name, &t).map_err(|e| {
                Error::new(
                    ErrorKind::InvalidOperation,
                    format!("loading lookup `{name}`: {e}"),
                )
            })?,
            _ => None,
        };
        let relation = match loaded {
            Some((relation, warning)) => {
                if let Some(w) = warning {
                    self.warn(format!("lookup `{name}`: {w}"));
                }
                relation
            }
            None => {
                if load && self.runner.is_some() {
                    self.warn(format!(
                        "lookup `{name}` has {rows} rows; this source can't load it into a temp table, so it's inlined in the SQL. Data this size probably belongs in a table in the database"
                    ));
                }
                t.inline_sql()
            }
        };
        self.loaded
            .lock()
            .unwrap()
            .insert(name.to_string(), relation.clone());
        Ok(relation)
    }

    fn warn(&self, w: String) {
        self.warnings.lock().unwrap().push(w);
    }
}

fn cell_value(c: &Cell) -> Value {
    match c {
        Cell::Null => Value::from(()),
        Cell::Text(s) => Value::from(s.clone()),
        Cell::Int(n) => Value::from(*n),
        Cell::Num(n) => Value::from(*n),
        Cell::Bool(b) => Value::from(*b),
        Cell::Date(d) => Value::from(d.to_string()),
    }
}

#[derive(Debug)]
struct Run(RunContext);

impl Object for Run {
    fn get_value(self: &Arc<Self>, key: &Value) -> Option<Value> {
        let c = &self.0;
        Some(match key.as_str()? {
            "report" => Value::from(c.report.clone()),
            "set" => c.set.clone().map(Value::from).unwrap_or(Value::from(())),
            "target" => Value::from(c.target.clone()),
            "profile" => Value::from(c.profile.clone()),
            "source_type" => Value::from(c.source_type.clone()),
            "schedule" => c.schedule.clone().map(Value::from).unwrap_or(Value::from(())),
            "date" => Date::value(c.date, c.calendar),
            "now" => DateTime::now(c.now, c.calendar),
            "timezone" => Value::from(c.calendar.tz.name()),
            _ => return None,
        })
    }

    fn call_method(
        self: &Arc<Self>,
        _: &minijinja::State<'_, '_>,
        method: &str,
        args: &[Value],
    ) -> Result<Value, Error> {
        match method {
            "date_format" => {
                let fmt = args.first().and_then(|a| a.as_str()).ok_or_else(|| {
                    Error::new(
                        ErrorKind::InvalidOperation,
                        "`run.date_format()` takes a strftime format string",
                    )
                })?;
                let mut out = String::new();
                use std::fmt::Write;
                write!(out, "{}", self.0.date.format(fmt)).map_err(|_| {
                    Error::new(
                        ErrorKind::InvalidOperation,
                        format!("invalid date format `{fmt}`"),
                    )
                })?;
                Ok(Value::from(out))
            }
            _ => Err(Error::from(ErrorKind::UnknownMethod)),
        }
    }
}

/// `target` and `profile('name')`: a profile target's fields, refusing secret ones.
#[derive(Debug)]
struct ConnectionValue(Connection);

impl Object for ConnectionValue {
    fn get_value(self: &Arc<Self>, key: &Value) -> Option<Value> {
        connection_field(&self.0, key)
    }

    fn render(self: &Arc<Self>, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0.target)
    }
}

/// `target`: the Binding's source connection, looked up the first time a template reads it.
struct LazyTarget {
    connections: Arc<dyn Connections>,
    resolved: std::sync::OnceLock<Result<Connection, String>>,
}

impl fmt::Debug for LazyTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("target")
    }
}

impl LazyTarget {
    fn get(&self) -> &Result<Connection, String> {
        self.resolved.get_or_init(|| self.connections.source())
    }
}

impl Object for LazyTarget {
    fn get_value(self: &Arc<Self>, key: &Value) -> Option<Value> {
        match self.get() {
            Ok(c) => connection_field(c, key),
            Err(e) => Some(Value::from(Error::new(
                ErrorKind::InvalidOperation,
                format!("`target`: {e}"),
            ))),
        }
    }

    fn render(self: &Arc<Self>, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.get() {
            Ok(c) => write!(f, "{}", c.target),
            Err(_) => Err(fmt::Error),
        }
    }
}

/// One field of a profile target for templates; a secret or missing one is an error value.
fn connection_field(c: &Connection, key: &Value) -> Option<Value> {
    {
        let k = key.as_str()?;
        Some(match k {
            "name" => Value::from(c.target.clone()),
            "type" => Value::from(c.kind.clone()),
            "profile" => Value::from(c.profile.clone()),
            _ if c.secrets.iter().any(|s| s == k) => Value::from(Error::new(
                ErrorKind::InvalidOperation,
                format!(
                    "`{k}` of profile `{}` holds a secret, so templates can't read it (it would end up in compiled SQL and logs)",
                    c.profile
                ),
            )),
            _ => match c.fields.get(k) {
                Some(v) => Value::from_serialize(v),
                None => Value::from(Error::new(
                    ErrorKind::UndefinedError,
                    format!(
                        "profile `{}` (target `{}`) has no field `{k}`; it has: {}",
                        c.profile,
                        c.target,
                        c.fields
                            .keys()
                            .filter(|f| !c.secrets.contains(f))
                            .cloned()
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                )),
            },
        })
    }
}

#[derive(Debug)]
struct QueryResult {
    columns: Vec<String>,
    rows: Vec<Value>,
}

impl QueryResult {
    fn new(q: QueryRows) -> QueryResult {
        let cols: Arc<Vec<String>> = Arc::new(q.columns.clone());
        let rows = q
            .rows
            .into_iter()
            .map(|values| {
                Value::from_object(Row {
                    columns: cols.clone(),
                    values,
                })
            })
            .collect();
        QueryResult {
            columns: q.columns,
            rows,
        }
    }
}

impl Object for QueryResult {
    fn repr(self: &Arc<Self>) -> ObjectRepr {
        ObjectRepr::Seq
    }

    fn get_value(self: &Arc<Self>, key: &Value) -> Option<Value> {
        match key.as_str() {
            Some("rows") => Some(Value::from(self.rows.clone())),
            Some("columns") => Some(Value::from(self.columns.clone())),
            _ => self.rows.get(key.as_usize()?).cloned(),
        }
    }

    fn enumerate(self: &Arc<Self>) -> Enumerator {
        Enumerator::Seq(self.rows.len())
    }

    /// `result.column('name')` (or an index): that column's values, one per row.
    fn call_method(
        self: &Arc<Self>,
        _: &minijinja::State<'_, '_>,
        method: &str,
        args: &[Value],
    ) -> Result<Value, Error> {
        if method != "column" {
            return Err(Error::from(ErrorKind::UnknownMethod));
        }
        let [key] = args else {
            return Err(Error::new(
                ErrorKind::InvalidOperation,
                "`column()` takes one column name or index",
            ));
        };
        let i = match key.as_str() {
            Some(n) => self.columns.iter().position(|c| c == n).ok_or_else(|| {
                Error::new(
                    ErrorKind::InvalidOperation,
                    format!("no column `{n}` (columns: {})", self.columns.join(", ")),
                )
            })?,
            None => key
                .as_usize()
                .filter(|i| *i < self.columns.len())
                .ok_or_else(|| Error::new(ErrorKind::InvalidOperation, format!("no column {key}")))?,
        };
        Ok(Value::from(
            self.rows
                .iter()
                .map(|r| r.get_item_by_index(i).unwrap_or_default())
                .collect::<Vec<_>>(),
        ))
    }
}

/// One `run_query()` row: accessible by column name (`row.region`, `row['region']`) or index.
#[derive(Debug)]
struct Row {
    columns: Arc<Vec<String>>,
    values: Vec<Value>,
}

impl Object for Row {
    fn repr(self: &Arc<Self>) -> ObjectRepr {
        ObjectRepr::Seq
    }

    fn get_value(self: &Arc<Self>, key: &Value) -> Option<Value> {
        if let Some(name) = key.as_str() {
            let i = self.columns.iter().position(|c| c == name)?;
            return self.values.get(i).cloned();
        }
        self.values.get(key.as_usize()?).cloned()
    }

    fn enumerate(self: &Arc<Self>) -> Enumerator {
        Enumerator::Seq(self.values.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn renderer(vars: Json, cli: &[(&str, &str)], macros: &[(&str, &str)]) -> (tempfile::TempDir, Renderer) {
        let dir = tempfile::tempdir().unwrap();
        let mut files = Vec::new();
        for (name, src) in macros {
            let p = PathBuf::from("macros").join(name);
            std::fs::create_dir_all(dir.path().join("macros")).unwrap();
            std::fs::write(dir.path().join(&p), src).unwrap();
            files.push(p);
        }
        let r = Renderer::new(RendererConfig {
            root: dir.path(),
            macros: &files,
            context: RunContext {
                report: "monthly".into(),
                set: Some("client_a".into()),
                target: "prod".into(),
                profile: "warehouse".into(),
                source_type: "duckdb".into(),
                schedule: None,
                date: NaiveDate::from_ymd_opt(2026, 1, 25).unwrap(),
                now: Utc::now(),
                calendar: Calendar::default(),
            },
            vars: vars.as_object().cloned().unwrap_or_default(),
            cli_vars: cli.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            runner: None,
            connections: None,
            run_query_max_rows: 10_000,
            sql: BTreeMap::new(),
            lookups: BTreeMap::new(),
            lookup_inline_max_rows: 200,
            packages: Vec::new(),
            project_name: "acme".into(),
            dispatch: DispatchOrder::new(),
        })
        .unwrap();
        (dir, r)
    }

    fn render(r: &Renderer, src: &str) -> Result<String, RenderError> {
        r.render(Path::new("reports/q.sql"), src)
    }

    #[test]
    fn run_context_and_date_formats() {
        let (_d, r) = renderer(Json::Null, &[], &[]);
        let out = render(
            &r,
            "{{ run.report }}/{{ run.set }}/{{ run.target }}/{{ run.profile }} {{ run.date }} {{ run.date.yyyymmdd }} \
             {{ run.date.ddmmyyyy }} {{ run.date.yyyy }}-{{ run.date.mm }}-{{ run.date.dd }} {{ run.date_format('%Y-W%V') }}",
        )
        .unwrap();
        assert_eq!(
            out,
            "monthly/client_a/prod/warehouse 2026-01-25 20260125 25012026 2026-01-25 2026-W04"
        );
    }

    #[test]
    fn var_precedence_is_cli_then_binding_then_default() {
        let (_d, r) = renderer(
            serde_json::json!({"a": "binding", "b": "binding", "n": 5}),
            &[("a", "cli")],
            &[],
        );
        assert_eq!(
            render(
                &r,
                "{{ var('a') }} {{ var('b') }} {{ var('c', 'dflt') }} {{ var('n') + 1 }}"
            )
            .unwrap(),
            "cli binding dflt 6"
        );
        let e = render(&r, "select\n{{ var('missing') }}").unwrap_err();
        assert_eq!(
            (e.line, e.message.contains("`var('missing')` has no value")),
            (Some(2), true),
            "{e}"
        );
    }

    #[test]
    fn env_var_reads_the_environment_or_errors() {
        let (_d, r) = renderer(Json::Null, &[], &[]);
        assert_eq!(
            render(&r, "{{ env_var('DRE_TEST_UNSET_VAR', 'x') }}").unwrap(),
            "x"
        );
        let home = std::env::var("PATH").unwrap();
        assert_eq!(render(&r, "{{ env_var('PATH') }}").unwrap(), home);
        assert!(
            render(&r, "{{ env_var('DRE_TEST_UNSET_VAR') }}")
                .unwrap_err()
                .message
                .contains("is not set")
        );
    }

    #[test]
    fn macros_from_every_file_are_callable_and_keep_line_numbers() {
        let (_d, r) = renderer(
            Json::Null,
            &[],
            &[
                ("a.sql", "{% macro double(x) %}{{ x * 2 }}{% endmacro %}\n"),
                (
                    "b.sql",
                    "{% macro broken() %}\n{{ var('nope') }}\n{% endmacro %}\n{% macro quad(x) %}{{ x * 4 }}{% endmacro %}\n",
                ),
            ],
        );
        assert_eq!(
            render(&r, "select {{ double(2) }}, {{ quad(1) }}\n").unwrap(),
            "select 4, 4\n"
        );
        let e = render(&r, "line one\nselect {{ broken() }}").unwrap_err();
        assert_eq!(e.file, PathBuf::from("macros/b.sql"), "{e}");
        assert_eq!(e.line, Some(2), "{e}");
        let e = render(&r, "one\ntwo\n{{ nope }}").unwrap_err();
        assert_eq!(
            (e.file.clone(), e.line),
            (PathBuf::from("reports/q.sql"), Some(3)),
            "{e}"
        );
    }

    struct Fake;
    impl QueryRunner for Fake {
        fn run_query(&self, sql: &str, max_rows: u64) -> Result<QueryRows, String> {
            if max_rows < 2 {
                return Err(format!("`{sql}` returned more than {max_rows} rows"));
            }
            Ok(QueryRows {
                columns: vec!["region".into(), "n".into()],
                rows: vec![
                    vec![Value::from("apac"), Value::from(1)],
                    vec![Value::from("emea"), Value::from(2)],
                ],
            })
        }
        fn columns(&self, _: &str) -> Result<Vec<Column>, String> {
            Ok(Vec::new())
        }
    }

    #[test]
    fn run_query_rows_are_accessible_by_name_and_index() {
        let dir = tempfile::tempdir().unwrap();
        let r = Renderer::new(RendererConfig {
            root: dir.path(),
            macros: &[],
            context: RunContext {
                report: "r".into(),
                set: None,
                target: "t".into(),
                profile: "p".into(),
                source_type: "duckdb".into(),
                schedule: None,
                date: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
                now: Utc::now(),
                calendar: Calendar::default(),
            },
            vars: JsonMap::new(),
            cli_vars: BTreeMap::new(),
            runner: Some(Arc::new(Fake)),
            connections: None,
            run_query_max_rows: 10_000,
            sql: BTreeMap::new(),
            lookups: BTreeMap::new(),
            lookup_inline_max_rows: 200,
            packages: Vec::new(),
            project_name: "acme".into(),
            dispatch: DispatchOrder::new(),
        })
        .unwrap();
        let src = "{% set res = run_query('select') %}{{ res.columns | join(',') }}|\
                   {% for row in res.rows %}{{ row.region }}={{ row[1] }}{{ ',' if not loop.last }}{% endfor %}|{{ res | length }}|{{ res[1]['region'] }}";
        assert_eq!(render(&r, src).unwrap(), "region,n|apac=1,emea=2|2|emea");
        let e = render(&r, "{{ run_query('select', max_rows=1) }}").unwrap_err();
        assert!(e.message.contains("more than 1 rows"), "{e}");
        assert!(render(&r, "{% if run.set is none %}none{% endif %}").unwrap() == "none");
    }
}
