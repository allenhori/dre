//! Runtime Jinja rendering: one environment per Binding, shared by SQL, output paths and
//! template values. Provides `run.*`, `var()`, `env_var()`, `run_query()` and every macro in
//! `macros/`.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::NaiveDate;
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
    pub date: NaiveDate,
}

/// Rows returned to templates by `run_query()`.
#[derive(Debug, Clone, Default)]
pub struct QueryRows {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Value>>,
}

/// Executes `run_query()` SQL on the Binding's own session.
pub trait QueryRunner: Send + Sync {
    /// Run `sql`, failing if it returns more than `max_rows` rows.
    fn run_query(&self, sql: &str, max_rows: u64) -> Result<QueryRows, String>;
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
    import: String,
    /// `(first line in the combined macro template, file, line count)` per macro file.
    macro_files: Vec<(usize, PathBuf, usize)>,
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
    pub run_query_max_rows: u64,
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
        env.add_global("run", Value::from_object(Run(cfg.context)));
        let runner = cfg.runner;
        let default_max = cfg.run_query_max_rows;
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

        // Every macro file is combined into one template, imported (on the first line, so line
        // numbers don't move) into everything rendered.
        let mut combined = String::new();
        let mut names = Vec::new();
        let mut macro_files = Vec::new();
        let re = regex::Regex::new(r"\{%-?\s*macro\s+([A-Za-z_]\w*)").unwrap();
        for m in cfg.macros {
            let src = std::fs::read_to_string(cfg.root.join(m)).map_err(|e| RenderError {
                file: m.clone(),
                line: None,
                message: format!("can't read macro file: {e}"),
            })?;
            let start = combined.matches('\n').count() + 1;
            names.extend(re.captures_iter(&src).map(|c| c[1].to_string()));
            combined.push_str(&src);
            if !combined.ends_with('\n') {
                combined.push('\n');
            }
            macro_files.push((start, m.clone(), src.matches('\n').count() + 1));
        }
        let import = if names.is_empty() {
            String::new()
        } else {
            format!("{{% from \"{MACROS}\" import {} %}}", names.join(", "))
        };
        let mut r = Renderer {
            env,
            import,
            macro_files,
        };
        if !names.is_empty() {
            r.env
                .add_template_owned(MACROS, combined)
                .map_err(|e| r.error(Path::new(MACROS), &e))?;
        }
        Ok(r)
    }

    /// Render `src`, reporting errors against `file`.
    pub fn render(&self, file: &Path, src: &str) -> Result<String, RenderError> {
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
        let (file, line) = if name == MACROS {
            match line.and_then(|l| {
                self.macro_files
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
            "date" => Value::from_object(RunDate(c.date)),
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

#[derive(Debug)]
struct RunDate(NaiveDate);

impl Object for RunDate {
    fn get_value(self: &Arc<Self>, key: &Value) -> Option<Value> {
        let f = match key.as_str()? {
            "yyyymmdd" => "%Y%m%d",
            "ddmmyyyy" => "%d%m%Y",
            "yyyy" => "%Y",
            "mm" => "%m",
            "dd" => "%d",
            "iso" => "%Y-%m-%d",
            _ => return None,
        };
        Some(Value::from(self.0.format(f).to_string()))
    }

    fn render(self: &Arc<Self>, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0.format("%Y-%m-%d"))
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
                date: NaiveDate::from_ymd_opt(2026, 1, 25).unwrap(),
            },
            vars: vars.as_object().cloned().unwrap_or_default(),
            cli_vars: cli.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            runner: None,
            run_query_max_rows: 10_000,
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
                date: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            },
            vars: JsonMap::new(),
            cli_vars: BTreeMap::new(),
            runner: Some(Arc::new(Fake)),
            run_query_max_rows: 10_000,
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
