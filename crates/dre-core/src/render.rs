//! Runtime Jinja rendering: one environment per connection a Binding uses (its queries' SQL) plus
//! one for output paths and template values. Provides `run.*`, `var()`, `env_var()`,
//! `run_query()`, `columns()`, `ref()`, `source()`, `target.name`, `connection.*`,
//! `destination.*`, `profile()`, the calendar functions in [`crate::dates`] and every macro in
//! `macros/`.
//!
//! [`Mode::Parse`] renders without a database, as dbt's parse does, to find every `source()` a
//! query calls: `run_query()` returns no rows, `columns()` none, `connection.*` nothing, and
//! `raise_error()` doesn't fire. [`Limited`] renders the values that choose a connection or a
//! source (every `profile:`, a source's `database`/`schema`/`identifier`), where only `var()`,
//! `env_var()`, `run.*` and `target.name` exist.

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

/// How a [`Renderer`] treats the database.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// Rendering SQL to run (or compile): `run_query()` and `columns()` reach the database.
    #[default]
    Run,
    /// The parse pass: no database, to find `source()` calls.
    Parse,
}

/// Ambient `run.*` context.
#[derive(Debug, Clone)]
pub struct RunContext {
    pub report: String,
    pub set: Option<String>,
    /// The run's target (environment): `run.target` and `target.name`.
    pub target: String,
    /// The schedule's name under `dre run --schedule`, else `None`.
    pub schedule: Option<String>,
    pub date: NaiveDate,
    /// When the run started, or the instant it was scheduled for: `run.now`.
    pub now: chrono::DateTime<Utc>,
    /// `DRE_RUN_AT`: `run.scheduled_at`.
    pub scheduled_at: Option<chrono::DateTime<Utc>>,
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

/// Executes `run_query()` SQL on the Binding's sessions: on `profile`'s connection, else on the
/// renderer's own.
pub trait QueryRunner: Send + Sync {
    /// Run `sql`, failing if it returns more than `max_rows` rows.
    fn run_query(&self, sql: &str, max_rows: u64, profile: Option<&str>) -> Result<QueryRows, String>;
    /// The columns `sql` returns, without fetching rows.
    fn columns(&self, sql: &str, profile: Option<&str>) -> Result<Vec<Column>, String>;
    /// Load a lookup into a temp table on the renderer's own session: `Some((relation, plugin
    /// warning))`, or `None` when the source can't load rows.
    fn load(&self, _name: &str, _table: &Table) -> Result<Option<(String, Option<String>)>, String> {
        Ok(None)
    }
}

/// One profile target as templates see it: `connection.*`, `destination.*` and
/// `profile('name').*`.
#[derive(Debug, Clone, Default)]
pub struct Connection {
    /// The profile's name: `.name` and `.profile`.
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

/// Where `connection`, `destination` and `profile()` get their values. `role` is `connection`
/// or `destination`; `None` finds the profile in whichever section has it.
pub trait Connections: Send + Sync {
    fn profile(&self, name: &str, role: Option<&str>) -> Result<Connection, String>;
    /// The identifier quote character a source plugin type reports (`None`: it doesn't say).
    fn identifier_quote(&self, kind: &str) -> Result<Option<String>, String>;
}

/// Which parts of a source's name to quote.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct Quoting {
    pub database: bool,
    pub schema: bool,
    pub identifier: bool,
}

impl Quoting {
    pub fn any(&self) -> bool {
        self.database || self.schema || self.identifier
    }
}

/// A declared source table with its fields rendered for one Binding: what `source()` returns.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ResolvedSource {
    pub source: String,
    pub table: String,
    /// The connection the source names, rendered; `None` runs it wherever its query runs.
    pub profile: Option<String>,
    pub database: Option<String>,
    pub schema: String,
    pub identifier: String,
    pub quoting: Quoting,
}

impl ResolvedSource {
    /// `database.schema.identifier`, or `schema.identifier` without a database, with the parts
    /// `quoting` asks for quoted by `quote` (doubled inside a name).
    pub fn relation(&self, quote: Option<&str>) -> String {
        let part = |s: &str, q: bool| match (q, quote) {
            (true, Some(c)) => format!("{c}{}{c}", s.replace(c, &format!("{c}{c}"))),
            _ => s.to_string(),
        };
        let mut parts = Vec::new();
        if let Some(d) = &self.database {
            parts.push(part(d, self.quoting.database));
        }
        parts.push(part(&self.schema, self.quoting.schema));
        parts.push(part(&self.identifier, self.quoting.identifier));
        parts.join(".")
    }

    /// `source.table`, as selectors and the manifest name it.
    pub fn key(&self) -> String {
        format!("{}.{}", self.source, self.table)
    }
}

/// Resolves `source('name', 'table')` for one Binding.
pub type SourceResolver = Arc<dyn Fn(&str, &str) -> Result<ResolvedSource, String> + Send + Sync>;

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

/// What `columns()` found, by relation and connection.
type ColumnsCache = BTreeMap<(String, Option<String>), Value>;

pub struct Renderer {
    env: Environment<'static>,
    /// The parse pass's first `raise_error()` message in the current render.
    raised: Arc<Mutex<Option<String>>>,
    /// What `columns()` found, by relation, for the file being rendered.
    columns_cache: Arc<Mutex<ColumnsCache>>,
    /// `(source, table)` of every `source()` call since the last [`Renderer::take_sources`].
    source_calls: Arc<Mutex<Vec<(String, String)>>>,
    /// The destination being rendered: `destination.*`.
    destination: Arc<Mutex<Option<Connection>>>,
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
    /// Profiles for `connection`, `destination` and `profile()`; `None` offline.
    pub connections: Option<Arc<dyn Connections>>,
    pub mode: Mode,
    /// The connection `connection.*` reads and `run_query()` uses by default: the query's in
    /// query SQL, the Binding's inherited one elsewhere.
    pub connection: Option<String>,
    /// That connection's plugin type, for `dispatch()` and source quoting; empty when unknown.
    pub source_type: String,
    /// `source()`; `None`: the project declares no sources.
    pub sources: Option<SourceResolver>,
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
        let parse = cfg.mode == Mode::Parse;
        // The parse pass has no data: `run_query(...)[0].x` must render as nothing, not fail.
        env.set_undefined_behavior(if parse {
            UndefinedBehavior::Chainable
        } else {
            UndefinedBehavior::Strict
        });
        env.set_keep_trailing_newline(true);
        crate::mutable::register(&mut env);

        add_vars(&mut env, cfg.vars, cfg.cli_vars);
        // The parse pass has no data, so a `raise_error()` there may only mean `run_query()` came
        // back empty: it doesn't stop the render, but if rendering fails anyway, its message is the
        // one reported.
        let raised: Arc<Mutex<Option<String>>> = Arc::default();
        let raised_c = raised.clone();
        env.add_function("raise_error", move |message: String| -> Result<Value, Error> {
            if parse {
                raised_c.lock().unwrap().get_or_insert(message);
                return Ok(Value::from(""));
            }
            Err(Error::new(ErrorKind::InvalidOperation, message))
        });
        let source_type = cfg.source_type.clone();
        crate::dates::register(&mut env, cfg.context.calendar, cfg.context.date);
        env.add_global("target", Value::from_object(Target(cfg.context.target.clone())));
        env.add_global("run", Value::from_object(Run(cfg.context)));
        let destination: Arc<Mutex<Option<Connection>>> = Arc::default();
        if parse {
            env.add_global("connection", Value::from_object(Lenient));
            env.add_global("destination", Value::from_object(Lenient));
            env.add_function(
                "profile",
                |_name: String, kwargs: Kwargs| -> Result<Value, Error> {
                    let role: Option<String> = kwargs.get("role")?;
                    kwargs.assert_all_used()?;
                    check_role(role.as_deref())?;
                    Ok(Value::from_object(Lenient))
                },
            );
        } else {
            env.add_global(
                "connection",
                Value::from_object(LazyConnection {
                    connections: cfg.connections.clone(),
                    name: cfg.connection.clone(),
                    resolved: std::sync::OnceLock::new(),
                }),
            );
            env.add_global(
                "destination",
                Value::from_object(DestinationSlot(destination.clone())),
            );
            let c = cfg.connections.clone();
            env.add_function(
                "profile",
                move |name: String, kwargs: Kwargs| -> Result<Value, Error> {
                    let role: Option<String> = kwargs.get("role")?;
                    kwargs.assert_all_used()?;
                    check_role(role.as_deref())?;
                    let Some(c) = &c else {
                        return Err(Error::new(
                            ErrorKind::InvalidOperation,
                            format!("`profile('{name}')`: no profiles.yml is loaded here"),
                        ));
                    };
                    c.profile(&name, role.as_deref())
                        .map(|t| Value::from_object(ConnectionValue(t)))
                        .map_err(|e| {
                            Error::new(ErrorKind::InvalidOperation, format!("`profile('{name}')`: {e}"))
                        })
                },
            );
        }
        // `source()`: every call is recorded; in a real render the relation is quoted with the
        // connection's quote character where the source asks for it.
        let source_calls: Arc<Mutex<Vec<(String, String)>>> = Arc::default();
        let relations: Arc<Mutex<Vec<(String, ResolvedSource)>>> = Arc::default();
        {
            let calls = source_calls.clone();
            let relations = relations.clone();
            let resolver = cfg.sources.clone();
            let connections = cfg.connections.clone();
            let kind = cfg.source_type.clone();
            let quote: Arc<std::sync::OnceLock<Result<Option<String>, String>>> = Arc::default();
            env.add_function(
                "source",
                move |source: String, table: String| -> Result<Value, Error> {
                    let fail = |e: String| {
                        Error::new(
                            ErrorKind::InvalidOperation,
                            format!("`source('{source}', '{table}')`: {e}"),
                        )
                    };
                    let Some(resolve) = &resolver else {
                        return Err(fail("the project declares no `sources:`".into()));
                    };
                    let r = resolve(&source, &table).map_err(fail)?;
                    calls.lock().unwrap().push((source.clone(), table.clone()));
                    let q = if parse || !r.quoting.any() {
                        None
                    } else {
                        let got = quote.get_or_init(|| match &connections {
                            Some(c) if !kind.is_empty() => c.identifier_quote(&kind),
                            _ => Err("no connection is known here to quote with".into()),
                        });
                        match got {
                            Ok(Some(q)) => Some(q.clone()),
                            Ok(None) => {
                                return Err(fail(format!(
                                    "`quoting` needs the identifier quote character, but the `{kind}` plugin doesn't report one; update it (`dre plugin update {kind}`)"
                                )));
                            }
                            Err(e) => return Err(fail(format!("can't quote: {e}"))),
                        }
                    };
                    let text = r.relation(q.as_deref());
                    relations.lock().unwrap().push((text.clone(), r));
                    Ok(Value::from(text))
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
        let rels = relations.clone();
        env.add_function(
            "run_query",
            move |sql: String, kwargs: Kwargs| -> Result<Value, Error> {
                let max_rows: Option<u64> = kwargs.get("max_rows")?;
                let profile: Option<String> = kwargs.get("profile")?;
                kwargs.assert_all_used()?;
                if parse {
                    let mut empty = QueryResult::new(QueryRows::default());
                    empty.lenient = true;
                    return Ok(Value::from_object(empty));
                }
                let profile = choose_profile("run_query()", &sql, profile, &rels)?;
                let Some(runner) = &runner else {
                    return Err(Error::new(
                        ErrorKind::InvalidOperation,
                        "`run_query()` has no connection here",
                    ));
                };
                let rows = runner
                    .run_query(&sql, max_rows.unwrap_or(default_max), profile.as_deref())
                    .map_err(|e| Error::new(ErrorKind::InvalidOperation, e))?;
                Ok(Value::from_object(QueryResult::new(rows)))
            },
        );
        // Per rendered file: a later query may have recreated the relation.
        let columns_cache: Arc<Mutex<ColumnsCache>> = Arc::default();
        let cache = columns_cache.clone();
        let rels = relations.clone();
        env.add_function(
            "columns",
            move |rel: String, kwargs: Kwargs| -> Result<Value, Error> {
                let profile: Option<String> = kwargs.get("profile")?;
                kwargs.assert_all_used()?;
                if parse {
                    return Ok(Value::from(Vec::<Value>::new()));
                }
                let what = format!("columns('{}')", crate::secrets::mask(&rel));
                let profile = choose_profile(&what, &rel, profile, &rels)?;
                let key = (rel.clone(), profile.clone());
                if let Some(v) = cache.lock().unwrap().get(&key) {
                    return Ok(v.clone());
                }
                let Some(runner) = &runner_c else {
                    return Err(Error::new(
                        ErrorKind::InvalidOperation,
                        format!("`{what}` has no connection here"),
                    ));
                };
                let sql = format!("select * from {rel} as _dre_cols where 1=0");
                let cols = runner
                    .columns(&sql, profile.as_deref())
                    .map_err(|e| Error::new(ErrorKind::InvalidOperation, format!("`{what}` failed: {e}")))?;
                let v = Value::from(
                    cols.into_iter()
                        .map(|c| {
                            Value::from_iter([
                                ("name", Value::from(c.name)),
                                ("type", Value::from(c.data_type)),
                            ])
                        })
                        .collect::<Vec<_>>(),
                );
                cache.lock().unwrap().insert(key, v.clone());
                Ok(v)
            },
        );

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
            parse,
        );
        let mut r = Renderer {
            env,
            raised,
            columns_cache,
            source_calls,
            destination,
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

    /// `(source, table)` of every `source()` called since the last call, in order, each once.
    pub fn take_sources(&self) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = Vec::new();
        for c in std::mem::take(&mut *self.source_calls.lock().unwrap()) {
            if !out.contains(&c) {
                out.push(c);
            }
        }
        out
    }

    /// Set (or clear) the destination `destination.*` reads.
    pub fn set_destination(&self, d: Option<Connection>) {
        *self.destination.lock().unwrap() = d;
    }

    /// Render `src`, reporting errors against `file`.
    pub fn render(&self, file: &Path, src: &str) -> Result<String, RenderError> {
        self.columns_cache.lock().unwrap().clear();
        *self.raised.lock().unwrap() = None;
        let full = format!("{}{src}", self.import);
        let name = file.to_string_lossy().to_string();
        let tmpl = self
            .env
            .template_from_named_str(&name, &full)
            .map_err(|e| self.error(file, &e))?;
        tmpl.render(()).map_err(|e| {
            let mut err = self.error(file, &e);
            if let Some(m) = self.raised.lock().unwrap().take() {
                err.message = m;
            }
            err
        })
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

/// `var()` (`--var` first, then the Binding's vars, then the default) and `env_var()`.
fn add_vars(env: &mut Environment<'static>, vars: JsonMap<String, Json>, cli: BTreeMap<String, String>) {
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
}

/// Renders the values that choose a connection or a source: every `profile:` value and a
/// source's `profile`, `database`, `schema` and `identifier`. Only `var()`, `env_var()`, `run.*`,
/// `target.name` and the calendar functions exist, because these are rendered before any
/// connection is chosen: the parse pass, `dre ls` and the manifest need no database.
pub struct Limited {
    env: Environment<'static>,
}

/// What a [`Limited`] value can't use, and why.
const LIMITED_FUNCTIONS: &[&str] = &[
    "run_query",
    "columns",
    "source",
    "ref",
    "lookup",
    "dispatch",
    "profile",
];
const LIMITED_GLOBALS: &[&str] = &["connection", "destination"];

impl Limited {
    pub fn new(
        context: RunContext,
        vars: JsonMap<String, Json>,
        cli_vars: BTreeMap<String, String>,
    ) -> Limited {
        let mut env = Environment::new();
        env.set_undefined_behavior(UndefinedBehavior::Strict);
        crate::mutable::register(&mut env);
        add_vars(&mut env, vars, cli_vars);
        env.add_function("raise_error", |message: String| -> Result<Value, Error> {
            Err(Error::new(ErrorKind::InvalidOperation, message))
        });
        crate::dates::register(&mut env, context.calendar, context.date);
        env.add_global("target", Value::from_object(Target(context.target.clone())));
        env.add_global("run", Value::from_object(Run(context)));
        for f in LIMITED_FUNCTIONS {
            let name = *f;
            env.add_function(name, move |_args: minijinja::value::Rest<Value>| -> Result<Value, Error> {
                Err(Error::new(
                    ErrorKind::InvalidOperation,
                    format!("`{name}()` can't be used here: this value chooses a connection or a source, so it's rendered before any connection is open"),
                ))
            });
        }
        for g in LIMITED_GLOBALS {
            env.add_global(*g, Value::from_object(Forbidden(g)));
        }
        Limited { env }
    }

    /// Render `src`, the value of `key` (named in errors, e.g. "`profile`").
    pub fn render(&self, key: &str, src: &str) -> Result<String, String> {
        if !crate::preflight::is_templated(src) {
            return Ok(src.to_string());
        }
        self.env
            .render_str(src, ())
            .map(|v| v.trim().to_string())
            .map_err(|e| {
                let mut deepest: &Error = &e;
                while let Some(s) = std::error::Error::source(deepest).and_then(|s| s.downcast_ref::<Error>())
                {
                    deepest = s;
                }
                let msg = deepest
                    .detail()
                    .map(str::to_string)
                    .unwrap_or_else(|| deepest.kind().to_string());
                format!("{key} `{src}`: {msg}")
            })
    }
}

/// `connection` or `destination` in a [`Limited`] value.
#[derive(Debug)]
struct Forbidden(&'static str);

impl Object for Forbidden {
    fn get_value(self: &Arc<Self>, _key: &Value) -> Option<Value> {
        Some(Value::from(Error::new(
            ErrorKind::InvalidOperation,
            format!(
                "`{}.*` can't be used here: this value chooses a connection or a source, so it's rendered before any connection is open",
                self.0
            ),
        )))
    }

    fn render(self: &Arc<Self>, _f: &mut fmt::Formatter<'_>) -> fmt::Result {
        Err(fmt::Error)
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
    parse: bool,
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
            if parse {
                // The parse pass doesn't know the connection type: a macro only some types
                // implement renders as nothing.
                return Ok(Value::from_object(Dispatched {
                    template: String::new(),
                    name: String::new(),
                }));
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
        if self.template.is_empty() {
            return Ok(Value::from(""));
        }
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
            "profile" => removed("run.profile", "`connection.name` (the query's connection)"),
            "source_type" => removed("run.source_type", "`connection.type` (the query's connection)"),
            "schedule" => c.schedule.clone().map(Value::from).unwrap_or(Value::from(())),
            "date" => Date::value(c.date, c.calendar),
            "now" => DateTime::now(c.now, c.calendar),
            "scheduled_at" => c
                .scheduled_at
                .map(|t| DateTime::now(t, c.calendar))
                .unwrap_or(Value::from(())),
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

/// An error value naming a removed template name and what to write instead.
fn removed(name: &str, instead: &str) -> Value {
    Value::from(Error::new(
        ErrorKind::InvalidOperation,
        format!("`{name}` was removed in DRE 0.2: use {instead}"),
    ))
}

/// `profile(..., role=)` takes `connection` or `destination`.
fn check_role(role: Option<&str>) -> Result<(), Error> {
    match role {
        None | Some("connection") | Some("destination") => Ok(()),
        Some("source") => Err(Error::new(
            ErrorKind::InvalidOperation,
            "`role='source'` was renamed in DRE 0.2: use `role='connection'`",
        )),
        Some(r) => Err(Error::new(
            ErrorKind::InvalidOperation,
            format!("`role='{r}'`: role must be 'connection' or 'destination'"),
        )),
    }
}

/// The connection `run_query()`/`columns()` uses: `profile=` when given (it must agree with
/// every source `sql` reads), else the connection of the sources `sql` reads, else `None` (the
/// renderer's own).
fn choose_profile(
    what: &str,
    sql: &str,
    explicit: Option<String>,
    relations: &Mutex<Vec<(String, ResolvedSource)>>,
) -> Result<Option<String>, Error> {
    let mut used: Vec<(String, String)> = Vec::new();
    for (text, r) in relations.lock().unwrap().iter() {
        if let Some(p) = &r.profile
            && mentions(sql, text)
            && !used.iter().any(|(k, _)| *k == r.key())
        {
            used.push((r.key(), p.clone()));
        }
    }
    let fail = |m: String| Error::new(ErrorKind::InvalidOperation, format!("`{what}`: {m}"));
    match explicit {
        Some(p) => {
            if let Some((k, q)) = used.iter().find(|(_, q)| *q != p) {
                return Err(fail(format!(
                    "`profile='{p}'` disagrees with source `{k}`, which is on connection `{q}`"
                )));
            }
            Ok(Some(p))
        }
        None => match used.as_slice() {
            [] => Ok(None),
            [(_, p), rest @ ..] => {
                if let Some((k, q)) = rest.iter().find(|(_, q)| q != p) {
                    return Err(fail(format!(
                        "reads source `{}` on connection `{p}` and source `{k}` on `{q}`; one statement runs on one connection",
                        used[0].0
                    )));
                }
                Ok(Some(p.clone()))
            }
        },
    }
}

/// Whether `text` (a rendered relation) appears in `sql` as a whole name.
fn mentions(sql: &str, text: &str) -> bool {
    let ident = |c: char| c.is_alphanumeric() || c == '_';
    sql.match_indices(text).any(|(i, _)| {
        let before = sql[..i].chars().next_back();
        let after = sql[i + text.len()..].chars().next();
        !before.is_some_and(|c| ident(c) || c == '.') && !after.is_some_and(|c| ident(c) || c == '.')
    })
}

/// `target`: the run's environment. Only `target.name` remains; every other field moved to
/// `connection.*` in DRE 0.2.
#[derive(Debug)]
struct Target(String);

impl Object for Target {
    fn get_value(self: &Arc<Self>, key: &Value) -> Option<Value> {
        let k = key.as_str()?;
        Some(match k {
            "name" => Value::from(self.0.clone()),
            "type" => removed("target.type", "`connection.type` (the query's connection)"),
            "profile" => removed("target.profile", "`connection.name` (the query's connection)"),
            _ => removed(
                &format!("target.{k}"),
                &format!(
                    "`connection.{k}` (the query's connection) or `profile('<name>').{k}`; `target` is now only the environment, `target.name`"
                ),
            ),
        })
    }

    fn render(self: &Arc<Self>, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The parse pass's stand-in for `connection`, `destination` and `profile()`: every field is
/// undefined, which renders as nothing.
#[derive(Debug)]
struct Lenient;

impl Object for Lenient {
    fn get_value(self: &Arc<Self>, _key: &Value) -> Option<Value> {
        None
    }

    fn render(self: &Arc<Self>, _f: &mut fmt::Formatter<'_>) -> fmt::Result {
        Ok(())
    }
}

/// `profile('name')`: a profile target's fields, refusing secret ones.
#[derive(Debug)]
struct ConnectionValue(Connection);

impl Object for ConnectionValue {
    fn get_value(self: &Arc<Self>, key: &Value) -> Option<Value> {
        connection_field(&self.0, key)
    }

    fn render(self: &Arc<Self>, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0.profile)
    }
}

/// `connection`: the query's connection (or the Binding's inherited one outside query SQL),
/// looked up the first time a template reads it.
struct LazyConnection {
    connections: Option<Arc<dyn Connections>>,
    name: Option<String>,
    resolved: std::sync::OnceLock<Result<Connection, String>>,
}

impl fmt::Debug for LazyConnection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("connection")
    }
}

impl LazyConnection {
    fn get(&self) -> &Result<Connection, String> {
        self.resolved.get_or_init(|| {
            let name = self.name.as_deref().ok_or(
                "no connection here: this isn't a query's SQL and the Binding inherits no `profile`",
            )?;
            match &self.connections {
                Some(c) => c.profile(name, Some("connection")),
                None => Err("no profiles.yml is loaded here".into()),
            }
        })
    }
}

impl Object for LazyConnection {
    fn get_value(self: &Arc<Self>, key: &Value) -> Option<Value> {
        match self.get() {
            Ok(c) => connection_field(c, key),
            Err(e) => Some(Value::from(Error::new(
                ErrorKind::InvalidOperation,
                format!("`connection`: {e}"),
            ))),
        }
    }

    fn render(self: &Arc<Self>, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.name {
            Some(n) => f.write_str(n),
            None => Err(fmt::Error),
        }
    }
}

/// `destination`: the destination whose `path` and options are being rendered.
#[derive(Debug)]
struct DestinationSlot(Arc<Mutex<Option<Connection>>>);

impl Object for DestinationSlot {
    fn get_value(self: &Arc<Self>, key: &Value) -> Option<Value> {
        match &*self.0.lock().unwrap() {
            Some(c) => connection_field(c, key),
            None => Some(Value::from(Error::new(
                ErrorKind::InvalidOperation,
                "`destination` only exists while rendering a destination's `path` and options",
            ))),
        }
    }

    fn render(self: &Arc<Self>, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &*self.0.lock().unwrap() {
            Some(c) => f.write_str(&c.profile),
            None => Err(fmt::Error),
        }
    }
}

/// One field of a profile target for templates; a secret or missing one is an error value.
fn connection_field(c: &Connection, key: &Value) -> Option<Value> {
    {
        let k = key.as_str()?;
        Some(match k {
            "name" | "profile" => Value::from(c.profile.clone()),
            "target" => Value::from(c.target.clone()),
            "type" => Value::from(c.kind.clone()),
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
    /// The parse pass's empty result: any `column()` is an empty list.
    lenient: bool,
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
            lenient: false,
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
        if self.lenient {
            return Ok(Value::from(Vec::<Value>::new()));
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
                schedule: None,
                date: NaiveDate::from_ymd_opt(2026, 1, 25).unwrap(),
                now: Utc::now(),
                scheduled_at: None,
                calendar: Calendar::default(),
            },
            vars: vars.as_object().cloned().unwrap_or_default(),
            cli_vars: cli.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            runner: None,
            connections: None,
            mode: Mode::Run,
            connection: None,
            source_type: "duckdb".into(),
            sources: None,
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

    fn ok(src: &str) -> String {
        let (_d, r) = renderer(serde_json::json!({"fixed": [3, 1, 2], "m": {"a": 1}}), &[], &[]);
        render(&r, src).unwrap_or_else(|e| panic!("{src}: {e}"))
    }

    fn err(src: &str) -> String {
        let (_d, r) = renderer(serde_json::json!({"fixed": [3, 1, 2], "m": {"a": 1}}), &[], &[]);
        render(&r, src).unwrap_err().to_string()
    }

    #[test]
    fn python_string_and_dict_methods_work() {
        assert_eq!(ok("{{ 'a,b,c'.split(',') | join('-') }}"), "a-b-c");
        assert_eq!(
            ok("{{ 'y' if 'abc'.startswith('a') }}{{ 'y' if 'abc'.endswith('x') }}"),
            "y"
        );
        assert_eq!(
            ok("{{ ' x '.strip() }}|{{ 'abc'.upper() }}|{{ 'a-b'.replace('-', '_') }}"),
            "x|ABC|a_b"
        );
        assert_eq!(
            ok("{{ var('m').keys() | list }} {{ var('m').items() | list }}"),
            r#"["a"] [["a", 1]]"#
        );
        assert_eq!(
            ok("{{ var('m').get('a') }} {{ var('m').get('z', 'none') }}"),
            "1 none"
        );
    }

    #[test]
    fn list_appends_in_a_loop_and_joins() {
        assert_eq!(
            ok(
                "{% set sq = list() %}{% for i in range(1, 5) %}{% set _ = sq.append(i * i) %}{% endfor %}{{ sq | join('+') }}"
            ),
            "1+4+9+16"
        );
    }

    #[test]
    fn list_methods_follow_python() {
        assert_eq!(
            ok("{% set l = list([3, 1, 2]) %}{% set _ = l.sort() %}{{ l }}"),
            "[1, 2, 3]"
        );
        assert_eq!(
            ok("{% set l = list([3, 1, 2]) %}{% set _ = l.sort(reverse=true) %}{{ l }}"),
            "[3, 2, 1]"
        );
        assert_eq!(
            ok("{% set l = list([1, 2]) %}{% set _ = l.extend([3, 4]) %}{{ l }} {{ l | length }}"),
            "[1, 2, 3, 4] 4"
        );
        assert_eq!(
            ok("{% set l = list([1, 2, 3]) %}{{ l.pop() }} {{ l.pop(0) }} {{ l }}"),
            "3 1 [2]"
        );
        assert_eq!(
            ok("{% set l = list([1, 2]) %}{% set _ = l.insert(0, 9) %}{% set _ = l.insert(99, 7) %}{{ l }}"),
            "[9, 1, 2, 7]"
        );
        assert_eq!(
            ok(
                "{% set l = list([1, 2, 1]) %}{% set _ = l.remove(1) %}{{ l }} {{ l.index(1) }} {{ l.count(1) }}"
            ),
            "[2, 1] 1 1"
        );
        assert_eq!(
            ok("{% set l = list([1, 2]) %}{% set _ = l.reverse() %}{{ l }}"),
            "[2, 1]"
        );
        assert_eq!(
            ok("{% set l = list([1, 2]) %}{% set _ = l.clear() %}{{ l }} {{ 'empty' if not l }}"),
            "[] empty"
        );
        assert_eq!(
            ok("{% set l = list([1, 2, 3]) %}{{ l[0] }} {{ l[-1] }} {{ l[1:] }}"),
            "1 3 [2, 3]"
        );
    }

    #[test]
    fn lists_share_by_reference_and_copy_apart() {
        assert_eq!(
            ok("{% set a = list() %}{% set b = a %}{% set _ = b.append(1) %}{{ a }}"),
            "[1]"
        );
        assert_eq!(
            ok("{% set a = list([1]) %}{% set b = a.copy() %}{% set _ = b.append(2) %}{{ a }} {{ b }}"),
            "[1] [1, 2]"
        );
    }

    #[test]
    fn a_list_changed_inside_its_own_loop_does_not_skip_or_repeat() {
        assert_eq!(
            ok(
                "{% set l = list([1, 2, 3]) %}{% for x in l %}{% set _ = l.append(x * 10) %}{{ x }} {% endfor %}{{ l }}"
            ),
            "1 2 3 [1, 2, 3, 10, 20, 30]"
        );
        assert_eq!(
            ok("{% set l = list([1, 2]) %}{% set _ = l.extend(l) %}{{ l }}"),
            "[1, 2, 1, 2]"
        );
    }

    #[test]
    fn a_list_starts_from_a_var_without_changing_it() {
        assert_eq!(
            ok("{% set l = list(var('fixed')) %}{% set _ = l.append(4) %}{{ l }} {{ var('fixed') }}"),
            "[3, 1, 2, 4] [3, 1, 2]"
        );
    }

    #[test]
    fn dict_methods_follow_python() {
        assert_eq!(
            ok("{% set d = dict(a=1) %}{% set _ = d.update(b=2) %}{% set _ = d.update({'c': 3}) %}{{ d }}"),
            r#"{"a": 1, "b": 2, "c": 3}"#
        );
        // Insertion order, as in Python. (Keyword arguments and `{}` literals reach us already sorted
        // until `preserve_order` is on, and the `| items` filter sorts; so add keys one by one.)
        let make = "{% set d = dict() %}{% set _ = d.setdefault('b', 1) %}{% set _ = d.setdefault('a', 2) %}";
        assert_eq!(
            ok(&(make.to_string() + "{% for k, v in d.items() %}{{ k }}={{ v }} {% endfor %}")),
            "b=1 a=2 "
        );
        assert_eq!(
            ok(&(make.to_string() + "{% for k in d %}{{ k }} {% endfor %}{{ d.values() | list }}")),
            "b a [1, 2]"
        );
        assert_eq!(
            ok("{% set d = dict(a=1) %}{{ d.setdefault('a', 9) }} {{ d.setdefault('b', 2) }} {{ d }}"),
            r#"1 2 {"a": 1, "b": 2}"#
        );
        assert_eq!(
            ok("{% set d = dict(a=1) %}{{ d.pop('a') }} {{ d.pop('a', 'gone') }} {{ d }}"),
            "1 gone {}"
        );
        assert_eq!(
            ok(
                "{% set d = dict(a=1) %}{{ d.get('a') }} {{ d['a'] }} {{ d.a }} {{ 'in' if 'a' in d }} {{ d.keys() | list }}"
            ),
            r#"1 1 1 in ["a"]"#
        );
        assert_eq!(
            ok("{% set d = dict(a=1) %}{% set e = d.copy() %}{% set _ = e.update(a=2) %}{{ d.a }} {{ e.a }}"),
            "1 2"
        );
        assert_eq!(
            ok("{% set d = dict(a=1) %}{% set _ = d.update(a=5, b=6) %}{{ d | length }} {{ d.a }}"),
            "2 5"
        );
        assert_eq!(
            ok("{% set d = dict(a=1) %}{% set _ = d.update(d) %}{{ d }}"),
            r#"{"a": 1}"#
        );
    }

    #[test]
    fn mutable_values_print_and_serialise_like_plain_ones() {
        assert_eq!(
            ok("{{ list([1, 'a']) }} {{ dict(k='v') }}"),
            r#"[1, "a"] {"k": "v"}"#
        );
        assert_eq!(
            ok("{{ list(dict(k='v')) }} {{ list(list([2, 1])) | sort }}"),
            r#"["k"] [1, 2]"#
        );
        assert_eq!(ok("{% set l = list() %}{{ 'yes' if l else 'no' }}"), "no");
    }

    #[test]
    fn mutating_an_immutable_value_says_what_to_write() {
        let e = err("{% set l = [] %}{% set _ = l.append(1) %}");
        assert!(e.contains("append()") && e.contains("list()"), "{e}");
        let e = err("{% set _ = var('fixed').append(1) %}");
        assert!(e.contains("list()"), "{e}");
        let e = err("{% set d = {} %}{% set _ = d.update(a=1) %}");
        assert!(e.contains("dict()"), "{e}");
    }

    #[test]
    fn bad_arguments_to_mutable_methods_are_clear_errors() {
        assert!(err("{% set l = list() %}{% set _ = l.append() %}").contains("append()"));
        assert!(err("{% set l = list() %}{% set _ = l.pop() %}").contains("pop()"));
        assert!(err("{% set l = list([1]) %}{% set _ = l.remove(2) %}").contains("not in the list"));
        assert!(err("{% set d = dict() %}{% set _ = d.pop('x') %}").contains("no key"));
        assert!(err("{% set d = dict('nope') %}").contains("mapping"));
    }

    #[test]
    fn run_context_and_date_formats() {
        let (_d, r) = renderer(Json::Null, &[], &[]);
        let out = render(
            &r,
            "{{ run.report }}/{{ run.set }}/{{ run.target }}/{{ target.name }}/{{ target }} {{ run.date }} {{ run.date.yyyymmdd }} \
             {{ run.date.ddmmyyyy }} {{ run.date.yyyy }}-{{ run.date.mm }}-{{ run.date.dd }} {{ run.date_format('%Y-W%V') }}",
        )
        .unwrap();
        assert_eq!(
            out,
            "monthly/client_a/prod/prod/prod 2026-01-25 20260125 25012026 2026-01-25 2026-W04"
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
        fn run_query(&self, sql: &str, max_rows: u64, _profile: Option<&str>) -> Result<QueryRows, String> {
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
        fn columns(&self, _: &str, _: Option<&str>) -> Result<Vec<Column>, String> {
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
                schedule: None,
                date: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
                now: Utc::now(),
                scheduled_at: None,
                calendar: Calendar::default(),
            },
            vars: JsonMap::new(),
            cli_vars: BTreeMap::new(),
            runner: Some(Arc::new(Fake)),
            connections: None,
            mode: Mode::Run,
            connection: None,
            source_type: "duckdb".into(),
            sources: None,
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

    #[test]
    fn removed_names_say_what_to_write_instead() {
        let e = err("{{ run.profile }}");
        assert!(
            e.contains("`run.profile` was removed") && e.contains("connection.name"),
            "{e}"
        );
        let e = err("{{ target.schema }}");
        assert!(
            e.contains("`target.schema` was removed") && e.contains("connection.schema"),
            "{e}"
        );
    }

    #[test]
    fn a_source_relation_quotes_the_parts_asked_for() {
        let r = ResolvedSource {
            source: "sales".into(),
            table: "orders".into(),
            profile: None,
            database: None,
            schema: "Sales".into(),
            identifier: "Or\"ders".into(),
            quoting: Quoting {
                database: true,
                schema: false,
                identifier: true,
            },
        };
        assert_eq!(r.relation(Some("\"")), "Sales.\"Or\"\"ders\"");
        assert_eq!(r.relation(None), "Sales.Or\"ders");
        let r = ResolvedSource {
            database: Some("main".into()),
            ..r
        };
        assert_eq!(r.relation(Some("`")), "`main`.Sales.`Or\"ders`");
    }

    #[test]
    fn a_relation_is_mentioned_only_as_a_whole_name() {
        assert!(mentions("select * from sales.orders where 1", "sales.orders"));
        assert!(!mentions("select * from sales.orders_old", "sales.orders"));
        assert!(!mentions("select * from x.sales.orders", "sales.orders"));
    }
}
