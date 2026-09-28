//! The plugin SDK: implement one of [`Source`], [`Format`] or [`Destination`] and call the
//! matching `serve_*` function from `main`. The SDK owns stdin/stdout, the handshake, framing,
//! error replies and panics; plugins log with `eprintln!`.
//!
//! Formats and destinations declare their options (`options()`) and add any rule a declaration
//! can't express (`validate()`). The SDK checks a config block against both on `validate` and
//! before every `write` and `deliver`, so plugin code only sees options that passed.

use std::io::{BufReader, BufWriter, Read, Stdout};
use std::path::{Path, PathBuf};

use arrow::array::RecordBatch;
use arrow::datatypes::SchemaRef;
use serde_json::{Map, Value};

use crate::frame::{self, Frame, FrameError};
use crate::msg::{ConnectionField, Request, Response, ResultSetMeta};
use crate::options::{self, OptionField};
use crate::{CAP_VALIDATE, Kind, MAX_VERSION, MIN_VERSION, PluginId};

pub type Error = Box<dyn std::error::Error + Send + Sync>;
pub type Result<T> = std::result::Result<T, Error>;

/// Identity reported in the handshake.
#[derive(Debug, Clone)]
pub struct About {
    pub name: &'static str,
    pub version: &'static str,
    pub capabilities: &'static [&'static str],
}

impl About {
    /// `version` "0.0.0" (the crate has no version yet) is reported as "unreleased".
    pub fn new(name: &'static str, version: &'static str) -> About {
        let version = if version == "0.0.0" { "unreleased" } else { version };
        About {
            name,
            version,
            capabilities: &[],
        }
    }

    pub fn capabilities(mut self, caps: &'static [&'static str]) -> About {
        self.capabilities = caps;
        self
    }
}

/// Where a source pushes the outcome of `execute`: either `no_result`, or `begin` followed by
/// any number of `batch` calls.
pub trait ResultSink {
    fn no_result(&mut self, rows_affected: Option<u64>) -> Result<()>;
    fn begin(&mut self, schema: SchemaRef) -> Result<()>;
    /// Returns `false` once the row limit is reached: stop producing batches.
    fn batch(&mut self, batch: RecordBatch) -> Result<bool>;
}

pub trait Source {
    fn connection_fields(&self) -> Vec<ConnectionField> {
        Vec::new()
    }
    fn open(&mut self, connection: &Map<String, Value>, read_only: bool) -> Result<()>;
    /// Run one statement. `row_limit` is a hint; the SDK enforces it either way.
    fn execute(&mut self, sql: &str, row_limit: Option<u64>, out: &mut dyn ResultSink) -> Result<()>;
    /// Verify without executing. Only called when the plugin advertises `check`.
    fn check(&mut self, _sql: &str) -> Result<()> {
        Err("this source can't check statements".into())
    }
    /// Load `data` into a temporary table on the session, named after `name`. Only called when
    /// the plugin advertises `load`.
    fn load(&mut self, _name: &str, _data: &mut ResultSet<'_>) -> Result<Loaded> {
        Err("this source can't load rows".into())
    }
    /// End the session cleanly (called on `close` and at end of input, before exiting).
    fn close(&mut self) {}
}

/// The outcome of `Source::load`.
pub struct Loaded {
    /// How SQL refers to the loaded rows, e.g. the temp table's name.
    pub relation: String,
    pub rows: u64,
    /// Shown to the user, e.g. when the database has no bulk path.
    pub warning: Option<String>,
}

/// One incoming result set, streamed from core.
pub struct ResultSet<'a> {
    pub meta: ResultSetMeta,
    pub schema: SchemaRef,
    first: Option<Vec<RecordBatch>>,
    input: &'a mut Input,
    done: bool,
}

impl ResultSet<'_> {
    /// The next batch, or `None` at the end of this result set.
    pub fn next_batch(&mut self) -> Result<Option<RecordBatch>> {
        loop {
            if let Some(first) = self.first.as_mut() {
                if !first.is_empty() {
                    return Ok(Some(first.remove(0)));
                }
                self.first = None;
            }
            if self.done {
                return Ok(None);
            }
            match self.input.read()? {
                Frame::Arrow(ipc) => {
                    let (_, batches) = frame::decode_batches(&ipc)?;
                    self.first = Some(batches);
                }
                Frame::Json(v) => match serde_json::from_value::<Request>(v)? {
                    Request::ResultSetEnd {} => {
                        self.done = true;
                        return Ok(None);
                    }
                    other => return Err(format!("expected result data, got {other:?}").into()),
                },
            }
        }
    }

    fn drain(&mut self) -> Result<()> {
        while self.next_batch()?.is_some() {}
        Ok(())
    }
}

/// A format `write` request.
pub struct WriteRequest {
    pub path: String,
    pub format: String,
    pub options: Map<String, Value>,
    pub result_sets: Vec<ResultSetMeta>,
    pub template: Option<Value>,
}

/// Hands a format plugin its result sets one at a time, in order.
pub struct ResultSets<'a> {
    metas: std::vec::IntoIter<ResultSetMeta>,
    input: &'a mut Input,
    warnings: Vec<String>,
}

impl ResultSets<'_> {
    /// Tell the person something about the output, shown as a warning by core.
    pub fn warn(&mut self, message: impl Into<String>) {
        self.warnings.push(message.into());
    }

    /// The next result set, or `None` after the last. Each must be read (or is drained) before
    /// the next is requested.
    pub fn next_set(&mut self) -> Result<Option<ResultSet<'_>>> {
        let Some(meta) = self.metas.next() else {
            return Ok(None);
        };
        let ipc = match self.input.read()? {
            Frame::Arrow(ipc) => ipc,
            Frame::Json(v) => {
                return Err(format!(
                    "expected an Arrow frame starting result set `{}`, got {v}",
                    meta.name
                )
                .into());
            }
        };
        let (schema, batches) = frame::decode_batches(&ipc)?;
        Ok(Some(ResultSet {
            meta,
            schema,
            first: Some(batches),
            input: &mut *self.input,
            done: false,
        }))
    }
}

pub trait Format {
    /// The options this format takes: every key of a report's `output:` block except `format`,
    /// `destination`, `template` and `extension`, which core owns.
    fn options(&self) -> Vec<OptionField> {
        Vec::new()
    }
    /// Problems the declared [`Format::options`] can't catch, each a sentence naming the key.
    /// Only called on options whose declared types already passed.
    fn validate(&self, _options: &Map<String, Value>) -> Vec<String> {
        Vec::new()
    }
    /// Write every result set to `req.path` (and siblings, if the format needs several files);
    /// return the files written.
    fn write(&mut self, req: &WriteRequest, sets: &mut ResultSets<'_>) -> Result<Vec<String>>;
}

/// A destination `deliver` request.
pub struct Delivery {
    /// One file, or every file of an output when the plugin advertises `multi_file`.
    pub files: Vec<DeliveryFile>,
    pub connection: Map<String, Value>,
    /// The destination entry's plugin options, rendered by core (empty when none).
    pub options: Map<String, Value>,
}

/// One file to deliver: the local copy in `target/run/` and its rendered remote path, if any.
pub struct DeliveryFile {
    pub local: PathBuf,
    pub remote: Option<String>,
}

pub trait Destination {
    fn connection_fields(&self) -> Vec<ConnectionField> {
        Vec::new()
    }
    /// The options a report's destination entry takes: every key but `profile` and `path`.
    /// String values may still hold Jinja when checked (see [`options::is_template`]); core
    /// renders it before `deliver`.
    fn options(&self) -> Vec<OptionField> {
        Vec::new()
    }
    /// Problems the declared [`Destination::options`] can't catch, each a sentence naming the key.
    /// Only called on options whose declared types already passed.
    fn validate(&self, _options: &Map<String, Value>) -> Vec<String> {
        Vec::new()
    }
    /// Deliver `local` to `remote` (rendered by core); return where it landed. Enough for a
    /// destination that takes no options; others implement [`Destination::deliver_files`].
    fn deliver(
        &mut self,
        _local: &Path,
        _remote: Option<&str>,
        _connection: &Map<String, Value>,
    ) -> Result<String> {
        Err("this destination doesn't implement `deliver`".into())
    }
    /// The whole request, options included (already checked). The default hands a single file
    /// to [`Destination::deliver`]; several files arrive only with `multi_file` advertised.
    fn deliver_files(&mut self, d: &Delivery) -> Result<String> {
        match d.files.as_slice() {
            [f] => self.deliver(&f.local, f.remote.as_deref(), &d.connection),
            _ => Err("this destination takes one file per delivery".into()),
        }
    }
}

pub struct Input {
    r: BufReader<Box<dyn Read>>,
}

impl Input {
    fn read(&mut self) -> Result<Frame> {
        frame::read_frame(&mut self.r).map_err(|e| -> Error {
            match e {
                FrameError::Eof => "core closed the connection".into(),
                e => e.to_string().into(),
            }
        })
    }
}

struct Output {
    w: BufWriter<Stdout>,
}

impl Output {
    fn send(&mut self, r: &Response) {
        if frame::write_json(&mut self.w, r).is_err() {
            // Core has gone; nothing left to talk to.
            std::process::exit(1);
        }
    }

    fn batch(&mut self, b: &RecordBatch) -> Result<()> {
        let ipc = frame::encode_batch(b)?;
        frame::write_arrow(&mut self.w, &ipc).map_err(|_| -> Error { "core closed the connection".into() })
    }
}

enum Handler<'a> {
    Source(&'a mut dyn Source),
    Format(&'a mut dyn Format),
    Destination(&'a mut dyn Destination),
}

impl Handler<'_> {
    fn kind(&self) -> Kind {
        match self {
            Handler::Source(_) => Kind::Source,
            Handler::Format(_) => Kind::Format,
            Handler::Destination(_) => Kind::Destination,
        }
    }

    fn option_fields(&self) -> Vec<OptionField> {
        match self {
            Handler::Source(_) => Vec::new(),
            Handler::Format(f) => f.options(),
            Handler::Destination(d) => d.options(),
        }
    }

    /// Every problem with a config block: the declared checks, then the plugin's own once the
    /// declared ones pass.
    fn check(&self, name: &str, o: &Map<String, Value>) -> Vec<String> {
        let errs = options::check(self.kind(), name, &self.option_fields(), o);
        if !errs.is_empty() {
            return errs;
        }
        match self {
            Handler::Source(_) => Vec::new(),
            Handler::Format(f) => f.validate(o),
            Handler::Destination(d) => d.validate(o),
        }
    }

    /// The config block's problems as one error, for `write` and `deliver`.
    fn checked(&self, name: &str, o: &Map<String, Value>) -> Result<()> {
        let errs = self.check(name, o);
        if errs.is_empty() {
            Ok(())
        } else {
            Err(errs.join("; ").into())
        }
    }
}

pub fn serve_source(about: About, s: impl Source + 'static) -> ! {
    serve_package(vec![Plugin::Source(about, Box::new(s))])
}

pub fn serve_format(about: About, f: impl Format + 'static) -> ! {
    serve_package(vec![Plugin::Format(about, Box::new(f))])
}

pub fn serve_destination(about: About, d: impl Destination + 'static) -> ! {
    serve_package(vec![Plugin::Destination(about, Box::new(d))])
}

/// One of the plugins a package's executable serves (see [`serve_package`]).
pub enum Plugin {
    Source(About, Box<dyn Source>),
    Format(About, Box<dyn Format>),
    Destination(About, Box<dyn Destination>),
}

impl Plugin {
    fn id(&self) -> PluginId {
        let (kind, about) = match self {
            Plugin::Source(a, _) => (Kind::Source, a),
            Plugin::Format(a, _) => (Kind::Format, a),
            Plugin::Destination(a, _) => (Kind::Destination, a),
        };
        PluginId::new(kind, about.name)
    }

    fn handler(&mut self) -> (&About, Handler<'_>) {
        match self {
            Plugin::Source(a, s) => (a, Handler::Source(s.as_mut())),
            Plugin::Format(a, f) => (a, Handler::Format(f.as_mut())),
            Plugin::Destination(a, d) => (a, Handler::Destination(d.as_mut())),
        }
    }
}

/// Serve every plugin of a package from one executable. Core's `hello` names the plugin it
/// wants; without one, the first is served.
pub fn serve_package(mut plugins: Vec<Plugin>) -> ! {
    assert!(!plugins.is_empty(), "a package serves at least one plugin");
    let mut input = Input {
        r: BufReader::new(Box::new(std::io::stdin())),
    };
    let mut out = Output {
        w: BufWriter::new(std::io::stdout()),
    };
    let provides: Vec<PluginId> = plugins.iter().map(Plugin::id).collect();
    let chosen = loop {
        let req = match next_request(&mut input, &mut out) {
            Some(r) => r,
            None => continue,
        };
        let Request::Hello {
            min_version,
            max_version,
            plugin,
            ..
        } = req
        else {
            out.send(&Response::Error {
                message: "the first request must be `hello`".into(),
            });
            continue;
        };
        let Some(chosen) = negotiate((min_version, max_version), (MIN_VERSION, MAX_VERSION)) else {
            out.send(&Response::VersionMismatch {
                min_version: MIN_VERSION,
                max_version: MAX_VERSION,
            });
            std::process::exit(1);
        };
        let i = match &plugin {
            None => 0,
            Some(want) => match provides.iter().position(|p| p == want) {
                Some(i) => i,
                None => {
                    let list: Vec<String> = provides.iter().map(|p| p.to_string()).collect();
                    out.send(&Response::Error {
                        message: format!("this executable provides {}, not {want}", list.join(", ")),
                    });
                    std::process::exit(1);
                }
            },
        };
        let (about, h) = plugins[i].handler();
        out.send(&Response::Hello {
            protocol_version: chosen,
            kind: h.kind(),
            name: about.name.to_string(),
            version: about.version.to_string(),
            capabilities: about
                .capabilities
                .iter()
                .chain(std::iter::once(&CAP_VALIDATE))
                .map(|c| c.to_string())
                .collect(),
            provides: if provides.len() > 1 {
                provides.clone()
            } else {
                Vec::new()
            },
        });
        break i;
    };
    let (about, h) = plugins[chosen].handler();
    serve(about.name, h, input, out)
}

/// The next request, or `None` after answering one that can't be read. Exits at end of input.
fn next_request(input: &mut Input, out: &mut Output) -> Option<Request> {
    let frame = match frame::read_frame(&mut input.r) {
        Ok(f) => f,
        Err(FrameError::Eof) => std::process::exit(0),
        Err(e) => {
            eprintln!("{e}");
            out.send(&Response::Error {
                message: e.to_string(),
            });
            std::process::exit(2);
        }
    };
    match frame {
        Frame::Json(v) => match serde_json::from_value::<Request>(v.clone()) {
            Ok(r) => Some(r),
            Err(_) => {
                let t = v.get("type").and_then(Value::as_str).unwrap_or("?").to_string();
                out.send(&Response::Error {
                    message: format!("unsupported request `{t}`"),
                });
                None
            }
        },
        Frame::Arrow(_) => {
            out.send(&Response::Error {
                message: "unexpected Arrow frame".into(),
            });
            None
        }
    }
}

/// Serve requests after the handshake.
fn serve(name: &str, mut h: Handler<'_>, mut input: Input, mut out: Output) -> ! {
    loop {
        let frame = match frame::read_frame(&mut input.r) {
            Ok(f) => f,
            Err(FrameError::Eof) => {
                if let Handler::Source(src) = &mut h {
                    src.close();
                }
                std::process::exit(0)
            }
            Err(e) => {
                eprintln!("{e}");
                out.send(&Response::Error {
                    message: e.to_string(),
                });
                std::process::exit(2);
            }
        };
        let req = match frame {
            Frame::Json(v) => match serde_json::from_value::<Request>(v.clone()) {
                Ok(r) => r,
                Err(_) => {
                    let t = v.get("type").and_then(Value::as_str).unwrap_or("?").to_string();
                    out.send(&Response::Error {
                        message: format!("unsupported request `{t}`"),
                    });
                    continue;
                }
            },
            Frame::Arrow(_) => {
                out.send(&Response::Error {
                    message: "unexpected Arrow frame".into(),
                });
                continue;
            }
        };
        if let Request::Hello { .. } = req {
            out.send(&Response::Error {
                message: "`hello` was already answered".into(),
            });
            continue;
        }
        if let Request::Close {} = req {
            if let Handler::Source(src) = &mut h {
                src.close();
            }
            out.send(&Response::Ok {});
            std::process::exit(0);
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            handle(&mut h, name, req, &mut input, &mut out)
        }));
        match result {
            Ok(Ok(())) => {}
            Ok(Err(e)) => out.send(&Response::Error {
                message: e.to_string(),
            }),
            Err(p) => {
                let msg = p
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string()))
                    .unwrap_or_else(|| "unknown panic".into());
                out.send(&Response::Error {
                    message: format!("plugin panicked: {msg}"),
                });
                std::process::exit(101);
            }
        }
    }
}

fn handle(h: &mut Handler<'_>, name: &str, req: Request, input: &mut Input, out: &mut Output) -> Result<()> {
    if let Request::Describe {} = req {
        let connection_fields = match h {
            Handler::Source(s) => s.connection_fields(),
            Handler::Destination(d) => d.connection_fields(),
            Handler::Format(_) => Vec::new(),
        };
        out.send(&Response::Describe {
            connection_fields,
            option_fields: h.option_fields(),
        });
        return Ok(());
    }
    if let Request::Validate { options } = req {
        out.send(&Response::Validated {
            errors: h.check(name, &options),
        });
        return Ok(());
    }
    let checked = match &req {
        Request::Write { options, .. } | Request::Deliver { options, .. } => h.checked(name, options),
        _ => Ok(()),
    };
    match (h, req) {
        (
            Handler::Source(s),
            Request::Open {
                connection,
                read_only,
            },
        ) => {
            s.open(&connection, read_only)?;
            out.send(&Response::Ok {});
        }
        (Handler::Source(s), Request::Load { name }) => {
            let ipc = match input.read()? {
                Frame::Arrow(ipc) => ipc,
                Frame::Json(v) => return Err(format!("expected the rows to load, got {v}").into()),
            };
            let (schema, batches) = frame::decode_batches(&ipc)?;
            let mut data = ResultSet {
                meta: ResultSetMeta {
                    name: name.clone(),
                    query: name.clone(),
                    result_index: 1,
                    anchor: None,
                    header: None,
                },
                schema,
                first: Some(batches),
                input,
                done: false,
            };
            let r = s.load(&name, &mut data);
            data.drain()?;
            let loaded = r?;
            out.send(&Response::Loaded {
                relation: loaded.relation,
                rows: loaded.rows,
                warning: loaded.warning,
            });
        }
        (Handler::Source(s), Request::Check { sql }) => {
            s.check(&sql)?;
            out.send(&Response::Ok {});
        }
        (Handler::Source(s), Request::Execute { sql, row_limit }) => {
            let mut sink = FrameSink {
                out,
                row_limit,
                rows: 0,
                state: SinkState::Idle,
                schema: None,
                sent_any: false,
            };
            let r = s.execute(&sql, row_limit, &mut sink);
            match (r, sink.state) {
                (Err(e), _) => return Err(e),
                (Ok(()), SinkState::Idle) => {
                    return Err("the source produced neither a result nor `no_result`".into());
                }
                (Ok(()), SinkState::NoResult) => {}
                (Ok(()), SinkState::Result) => {
                    if !sink.sent_any {
                        let schema = sink.schema.clone().unwrap();
                        sink.out.batch(&RecordBatch::new_empty(schema))?;
                    }
                    let rows = sink.rows;
                    sink.out.send(&Response::ResultEnd { rows });
                }
            }
        }
        (
            Handler::Format(f),
            Request::Write {
                path,
                format,
                options,
                result_sets,
                template,
            },
        ) => {
            let req = WriteRequest {
                path,
                format,
                options,
                result_sets: result_sets.clone(),
                template,
            };
            let mut sets = ResultSets {
                metas: result_sets.into_iter(),
                input,
                warnings: Vec::new(),
            };
            let written = checked.and_then(|()| f.write(&req, &mut sets));
            // A failed write is reported at once, so core can stop streaming; this is the
            // request's one reply.
            if let Err(e) = &written {
                out.send(&Response::Error {
                    message: e.to_string(),
                });
            }
            // Consume whatever the format didn't read (after an error, possibly the rest of a
            // result set), so the stream stays in sync.
            loop {
                match sets.input.read()? {
                    Frame::Json(v) if v.get("type").and_then(Value::as_str) == Some("finish") => break,
                    Frame::Json(v) if v.get("type").and_then(Value::as_str) == Some("result_set_end") => {}
                    Frame::Arrow(_) => {}
                    other => return Err(format!("expected `finish`, got {other:?}").into()),
                }
            }
            if let Ok(files) = written {
                out.send(&Response::Written {
                    files,
                    warnings: sets.warnings,
                });
            }
        }
        (
            Handler::Destination(d),
            Request::Deliver {
                local_path,
                remote_path,
                files,
                connection,
                options,
            },
        ) => {
            checked?;
            let files = match (local_path, files.is_empty()) {
                (Some(local), true) => vec![DeliveryFile {
                    local: PathBuf::from(local),
                    remote: remote_path,
                }],
                (None, false) => files
                    .into_iter()
                    .map(|f| DeliveryFile {
                        local: PathBuf::from(f.local_path),
                        remote: f.remote_path,
                    })
                    .collect(),
                _ => return Err("`deliver` needs exactly one of `local_path` or `files`".into()),
            };
            let location = d.deliver_files(&Delivery {
                files,
                connection,
                options,
            })?;
            out.send(&Response::Delivered { location });
        }
        (h, req) => {
            let t = serde_json::to_value(&req)
                .ok()
                .and_then(|v| v.get("type").cloned())
                .unwrap_or_default();
            return Err(format!("a {} plugin doesn't handle {t} requests", h.kind()).into());
        }
    }
    Ok(())
}

#[derive(PartialEq)]
enum SinkState {
    Idle,
    NoResult,
    Result,
}

struct FrameSink<'a> {
    out: &'a mut Output,
    row_limit: Option<u64>,
    rows: u64,
    state: SinkState,
    schema: Option<SchemaRef>,
    sent_any: bool,
}

impl ResultSink for FrameSink<'_> {
    fn no_result(&mut self, rows_affected: Option<u64>) -> Result<()> {
        if self.state != SinkState::Idle {
            return Err("`no_result` after the result started".into());
        }
        self.state = SinkState::NoResult;
        self.out.send(&Response::NoResult { rows_affected });
        Ok(())
    }

    fn begin(&mut self, schema: SchemaRef) -> Result<()> {
        if self.state != SinkState::Idle {
            return Err("a statement can only produce one result".into());
        }
        self.state = SinkState::Result;
        self.out.send(&Response::Result {
            columns: schema.fields().iter().map(|f| f.name().clone()).collect(),
        });
        self.schema = Some(schema);
        Ok(())
    }

    fn batch(&mut self, b: RecordBatch) -> Result<bool> {
        if self.state != SinkState::Result {
            return Err("`batch` before `begin`".into());
        }
        if self.row_limit.is_some_and(|l| self.rows >= l) {
            return Ok(false);
        }
        let b = match self.row_limit {
            Some(l) if self.rows + b.num_rows() as u64 > l => b.slice(0, (l - self.rows) as usize),
            _ => b,
        };
        if b.num_rows() > 0 || !self.sent_any {
            self.rows += b.num_rows() as u64;
            self.out.batch(&b)?;
            self.sent_any = true;
        }
        Ok(self.row_limit.is_none_or(|l| self.rows < l))
    }
}

/// The highest version in both ranges.
fn negotiate(core: (u32, u32), ours: (u32, u32)) -> Option<u32> {
    let (lo, hi) = (core.0.max(ours.0), core.1.min(ours.1));
    (lo <= hi).then_some(hi)
}

/// Read a string field from a connection map.
pub fn conn_str<'a>(c: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    c.get(key).and_then(Value::as_str).filter(|s| !s.is_empty())
}

/// Read a required string field from a connection map.
pub fn conn_required<'a>(c: &'a Map<String, Value>, key: &str) -> Result<&'a str> {
    conn_str(c, key).ok_or_else(|| format!("the profile output needs a `{key}` field").into())
}

/// Read a flag that may be written as a boolean or a string.
pub fn conn_bool(c: &Map<String, Value>, key: &str) -> Option<bool> {
    match c.get(key)? {
        Value::Bool(b) => Some(*b),
        Value::String(s) => Some(matches!(s.as_str(), "true" | "yes" | "1")),
        _ => None,
    }
}
