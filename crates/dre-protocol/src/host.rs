//! Core's side of the protocol: spawn a plugin, negotiate a version, send requests.
//!
//! Frames are read on a background thread so core can time out a silent plugin during the
//! handshake and never blocks forever on a plugin that has died. stderr is drained on another
//! thread, forwarded to a log sink and kept (last lines) for error messages.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use arrow::array::RecordBatch;
use arrow::datatypes::SchemaRef;
use serde_json::{Map, Value};

use crate::frame::{self, Frame, FrameError};
use crate::msg::{ConnectionField, Request, Response, ResultSetMeta};
use crate::{Kind, MAX_VERSION, MIN_VERSION};

/// Receives each stderr line a plugin writes.
pub type LogSink = Arc<dyn Fn(&str, &str) + Send + Sync>;

/// A log sink that prefixes each plugin line with the plugin's file name, on core's stderr.
pub fn stderr_log() -> LogSink {
    Arc::new(|plugin, line| eprintln!("[{plugin}] {line}"))
}

pub const DEFAULT_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);
const STDERR_TAIL: usize = 20;

#[derive(Debug, Clone, PartialEq)]
pub struct PluginInfo {
    pub protocol_version: u32,
    pub kind: Kind,
    pub name: String,
    pub version: String,
    pub capabilities: Vec<String>,
}

#[derive(Debug)]
pub enum HostError {
    Spawn {
        plugin: String,
        error: std::io::Error,
    },
    Crashed {
        plugin: String,
        status: Option<ExitStatus>,
        stderr: Vec<String>,
    },
    Malformed {
        plugin: String,
        message: String,
    },
    Incompatible {
        plugin: String,
        core: (u32, u32),
        plugin_range: (u32, u32),
    },
    Timeout {
        plugin: String,
        waiting_for: &'static str,
    },
    Unexpected {
        plugin: String,
        expected: &'static str,
        got: String,
    },
    /// The plugin reported an error for a request.
    Plugin {
        plugin: String,
        message: String,
    },
    Arrow {
        plugin: String,
        message: String,
    },
}

impl std::fmt::Display for HostError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HostError::Spawn { plugin, error } => write!(f, "can't start plugin `{plugin}`: {error}"),
            HostError::Crashed {
                plugin,
                status,
                stderr,
            } => {
                match status {
                    Some(s) => write!(f, "plugin `{plugin}` exited unexpectedly ({s})")?,
                    None => write!(f, "plugin `{plugin}` closed its output unexpectedly")?,
                }
                if !stderr.is_empty() {
                    write!(f, "; its last log lines:\n  {}", stderr.join("\n  "))?;
                }
                Ok(())
            }
            HostError::Malformed { plugin, message } => {
                write!(f, "plugin `{plugin}` broke the protocol: {message}")
            }
            HostError::Incompatible {
                plugin,
                core,
                plugin_range,
            } => write!(
                f,
                "plugin `{plugin}` speaks protocol versions {}..={}, but this DRE core speaks {}..={}; update {}",
                plugin_range.0,
                plugin_range.1,
                core.0,
                core.1,
                if plugin_range.1 < core.0 {
                    "the plugin"
                } else {
                    "DRE"
                }
            ),
            HostError::Timeout { plugin, waiting_for } => {
                write!(
                    f,
                    "plugin `{plugin}` didn't answer in time (waiting for {waiting_for})"
                )
            }
            HostError::Unexpected {
                plugin,
                expected,
                got,
            } => {
                write!(f, "plugin `{plugin}` sent {got} where {expected} was expected")
            }
            HostError::Plugin { message, .. } => f.write_str(message),
            HostError::Arrow { plugin, message } => {
                write!(f, "plugin `{plugin}` sent invalid Arrow data: {message}")
            }
        }
    }
}

impl std::error::Error for HostError {}

pub type Result<T> = std::result::Result<T, HostError>;

/// A message from the plugin.
#[derive(Debug)]
pub enum Incoming {
    Json(Response),
    Arrow(Vec<u8>),
}

/// The outcome of a source `execute`.
#[derive(Debug)]
pub enum Execution {
    NoResult { rows_affected: Option<u64> },
    Result { schema: SchemaRef, rows: u64 },
}

pub struct PluginProcess {
    label: String,
    path: PathBuf,
    child: Child,
    stdin: Option<BufWriter<ChildStdin>>,
    rx: Receiver<std::result::Result<Frame, FrameError>>,
    stderr: Arc<Mutex<VecDeque<String>>>,
    info: Option<PluginInfo>,
}

impl PluginProcess {
    /// Spawn the plugin at `path` and complete the handshake.
    pub fn start(path: &Path, log: LogSink) -> Result<PluginProcess> {
        let timeout = std::env::var("DRE_PLUGIN_HANDSHAKE_TIMEOUT_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .map(Duration::from_millis)
            .unwrap_or(DEFAULT_HANDSHAKE_TIMEOUT);
        Self::start_with(path, log, (MIN_VERSION, MAX_VERSION), timeout)
    }

    /// Spawn and handshake offering an explicit version range (the conformance suite uses this).
    pub fn start_with(
        path: &Path,
        log: LogSink,
        versions: (u32, u32),
        timeout: Duration,
    ) -> Result<PluginProcess> {
        let mut p = Self::spawn(path, log)?;
        p.handshake(versions, timeout)?;
        Ok(p)
    }

    /// Spawn without a handshake (for tests that probe raw protocol behaviour).
    pub fn spawn(path: &Path, log: LogSink) -> Result<PluginProcess> {
        Self::spawn_env(path, log, &[])
    }

    /// Spawn with extra environment variables, without a handshake.
    pub fn spawn_env(path: &Path, log: LogSink, env: &[(&str, &str)]) -> Result<PluginProcess> {
        let label = path
            .file_name()
            .map(|f| f.to_string_lossy().to_string())
            .unwrap_or_default();
        let mut child = Command::new(path)
            .envs(env.iter().copied())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| HostError::Spawn {
                plugin: label.clone(),
                error,
            })?;
        let stdout = child.stdout.take().unwrap();
        let stderr_pipe = child.stderr.take().unwrap();
        let stdin = child.stdin.take().map(BufWriter::new);

        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let mut r = BufReader::new(stdout);
            loop {
                let f = frame::read_frame(&mut r);
                let stop = f.is_err();
                if tx.send(f).is_err() || stop {
                    break;
                }
            }
        });
        let stderr = Arc::new(Mutex::new(VecDeque::new()));
        let tail = stderr.clone();
        let who = label.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr_pipe).lines() {
                let Ok(line) = line else { break };
                log(&who, &line);
                let mut t = tail.lock().unwrap();
                if t.len() == STDERR_TAIL {
                    t.pop_front();
                }
                t.push_back(line);
            }
        });
        Ok(PluginProcess {
            label,
            path: path.to_path_buf(),
            child,
            stdin,
            rx,
            stderr,
            info: None,
        })
    }

    /// Complete the handshake on a process from `spawn`/`spawn_env`.
    pub fn handshake(&mut self, (min, max): (u32, u32), timeout: Duration) -> Result<()> {
        self.send(&Request::Hello {
            min_version: min,
            max_version: max,
            core_version: core_version(),
        })?;
        match self.recv(Some(timeout), "the hello reply")? {
            Incoming::Json(Response::Hello {
                protocol_version,
                kind,
                name,
                version,
                capabilities,
            }) => {
                if protocol_version < min || protocol_version > max {
                    return Err(HostError::Incompatible {
                        plugin: self.label.clone(),
                        core: (min, max),
                        plugin_range: (protocol_version, protocol_version),
                    });
                }
                self.info = Some(PluginInfo {
                    protocol_version,
                    kind,
                    name,
                    version,
                    capabilities,
                });
                Ok(())
            }
            Incoming::Json(Response::VersionMismatch {
                min_version,
                max_version,
            }) => Err(HostError::Incompatible {
                plugin: self.label.clone(),
                core: (min, max),
                plugin_range: (min_version, max_version),
            }),
            Incoming::Json(Response::Error { message }) => Err(HostError::Plugin {
                plugin: self.label.clone(),
                message,
            }),
            other => Err(self.unexpected("a hello reply", &other)),
        }
    }

    pub fn label(&self) -> &str {
        &self.label
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn info(&self) -> &PluginInfo {
        self.info.as_ref().expect("handshake completed")
    }

    pub fn has(&self, capability: &str) -> bool {
        self.info
            .as_ref()
            .is_some_and(|i| i.capabilities.iter().any(|c| c == capability))
    }

    fn unexpected(&self, expected: &'static str, got: &Incoming) -> HostError {
        let got = match got {
            Incoming::Json(r) => format!("{:?}", r)
                .split([' ', '{'])
                .next()
                .unwrap_or("?")
                .to_string(),
            Incoming::Arrow(_) => "Arrow data".into(),
        };
        HostError::Unexpected {
            plugin: self.label.clone(),
            expected,
            got: format!("`{got}`"),
        }
    }

    fn crashed(&mut self) -> HostError {
        // Give the process a moment to finish exiting so its status and last stderr are known.
        let mut status = None;
        for _ in 0..50 {
            if let Ok(Some(s)) = self.child.try_wait() {
                status = Some(s);
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        std::thread::sleep(Duration::from_millis(50));
        let stderr = self.stderr.lock().unwrap().iter().cloned().collect();
        HostError::Crashed {
            plugin: self.label.clone(),
            status,
            stderr,
        }
    }

    pub fn send(&mut self, req: &Request) -> Result<()> {
        let ok = match self.stdin.as_mut() {
            Some(w) => frame::write_json(w, req).is_ok(),
            None => false,
        };
        if ok { Ok(()) } else { Err(self.write_failed()) }
    }

    pub fn send_batch(&mut self, batch: &RecordBatch) -> Result<()> {
        let ipc = frame::encode_batch(batch).map_err(|e| HostError::Arrow {
            plugin: self.label.clone(),
            message: e.to_string(),
        })?;
        let ok = match self.stdin.as_mut() {
            Some(w) => frame::write_arrow(w, &ipc).is_ok(),
            None => false,
        };
        if ok { Ok(()) } else { Err(self.write_failed()) }
    }

    /// Writing failed: the plugin has gone away. Prefer its own error message if it sent one.
    fn write_failed(&mut self) -> HostError {
        match self.rx.recv_timeout(Duration::from_secs(2)) {
            Ok(Ok(Frame::Json(v))) => match serde_json::from_value::<Response>(v) {
                Ok(Response::Error { message }) => HostError::Plugin {
                    plugin: self.label.clone(),
                    message,
                },
                _ => self.crashed(),
            },
            _ => self.crashed(),
        }
    }

    /// Receive the next message. `timeout` of `None` waits as long as the plugin is alive.
    pub fn recv(&mut self, timeout: Option<Duration>, waiting_for: &'static str) -> Result<Incoming> {
        let got = match timeout {
            Some(t) => match self.rx.recv_timeout(t) {
                Ok(f) => Some(f),
                Err(RecvTimeoutError::Timeout) => {
                    return Err(HostError::Timeout {
                        plugin: self.label.clone(),
                        waiting_for,
                    });
                }
                Err(RecvTimeoutError::Disconnected) => None,
            },
            None => self.rx.recv().ok(),
        };
        match got {
            Some(Ok(Frame::Json(v))) => match serde_json::from_value::<Response>(v.clone()) {
                Ok(r) => Ok(Incoming::Json(r)),
                Err(e) => Err(HostError::Malformed {
                    plugin: self.label.clone(),
                    message: format!("unknown message {v}: {e}"),
                }),
            },
            Some(Ok(Frame::Arrow(b))) => Ok(Incoming::Arrow(b)),
            Some(Err(FrameError::Malformed(m))) => Err(HostError::Malformed {
                plugin: self.label.clone(),
                message: m,
            }),
            Some(Err(_)) | None => Err(self.crashed()),
        }
    }

    /// Receive a JSON response, turning `error` into `HostError::Plugin`.
    pub fn recv_json(&mut self, waiting_for: &'static str) -> Result<Response> {
        match self.recv(None, waiting_for)? {
            Incoming::Json(Response::Error { message }) => Err(HostError::Plugin {
                plugin: self.label.clone(),
                message,
            }),
            Incoming::Json(r) => Ok(r),
            other => Err(self.unexpected(waiting_for, &other)),
        }
    }

    /// Send a request and expect `ok`.
    fn call_ok(&mut self, req: &Request, what: &'static str) -> Result<()> {
        self.send(req)?;
        match self.recv_json(what)? {
            Response::Ok {} => Ok(()),
            other => Err(self.unexpected(what, &Incoming::Json(other))),
        }
    }

    pub fn describe(&mut self) -> Result<Vec<ConnectionField>> {
        self.send(&Request::Describe {})?;
        match self.recv_json("a describe reply")? {
            Response::Describe { connection_fields } => Ok(connection_fields),
            other => Err(self.unexpected("a describe reply", &Incoming::Json(other))),
        }
    }

    pub fn open(&mut self, connection: Map<String, Value>, read_only: bool) -> Result<()> {
        self.call_ok(
            &Request::Open {
                connection,
                read_only,
            },
            "an open reply",
        )
    }

    pub fn check(&mut self, sql: &str) -> Result<()> {
        self.call_ok(&Request::Check { sql: sql.to_string() }, "a check reply")
    }

    /// Run one statement, handing every batch to `on_batch` as it arrives.
    pub fn execute(
        &mut self,
        sql: &str,
        row_limit: Option<u64>,
        mut on_batch: impl FnMut(&SchemaRef, RecordBatch) -> std::result::Result<(), String>,
    ) -> Result<Execution> {
        self.send(&Request::Execute {
            sql: sql.to_string(),
            row_limit,
        })?;
        match self.recv_json("an execute reply")? {
            Response::NoResult { rows_affected } => return Ok(Execution::NoResult { rows_affected }),
            Response::Result { .. } => {}
            other => return Err(self.unexpected("an execute reply", &Incoming::Json(other))),
        }
        let mut schema: Option<SchemaRef> = None;
        let mut rows = 0u64;
        let mut sink_error = None;
        loop {
            match self.recv(None, "result data")? {
                Incoming::Arrow(ipc) => {
                    let (s, batches) = frame::decode_batches(&ipc).map_err(|e| HostError::Arrow {
                        plugin: self.label.clone(),
                        message: e.to_string(),
                    })?;
                    let schema = schema.get_or_insert(s);
                    for b in batches {
                        rows += b.num_rows() as u64;
                        if sink_error.is_none()
                            && let Err(e) = on_batch(schema, b)
                        {
                            // Keep draining so the session stays usable; report after.
                            sink_error = Some(e);
                        }
                    }
                }
                Incoming::Json(Response::ResultEnd { .. }) => break,
                Incoming::Json(Response::Error { message }) => {
                    return Err(HostError::Plugin {
                        plugin: self.label.clone(),
                        message,
                    });
                }
                other => return Err(self.unexpected("result data", &other)),
            }
        }
        if let Some(e) = sink_error {
            return Err(HostError::Plugin {
                plugin: self.label.clone(),
                message: e,
            });
        }
        let schema = schema.ok_or_else(|| HostError::Malformed {
            plugin: self.label.clone(),
            message: "a result ended without any Arrow frame carrying its schema".into(),
        })?;
        Ok(Execution::Result { schema, rows })
    }

    /// Start a format `write`; follow with `write_result_set` per result set, then `write_finish`.
    pub fn write_begin(
        &mut self,
        path: &str,
        format: &str,
        options: Map<String, Value>,
        result_sets: Vec<ResultSetMeta>,
        template: Option<Value>,
    ) -> Result<()> {
        self.send(&Request::Write {
            path: path.to_string(),
            format: format.to_string(),
            options,
            result_sets,
            template,
        })
    }

    /// Stream one result set: at least one batch (carrying the schema), then the end marker.
    pub fn write_result_set(
        &mut self,
        schema: &SchemaRef,
        batches: impl IntoIterator<Item = RecordBatch>,
    ) -> Result<()> {
        let mut any = false;
        for b in batches {
            any = true;
            self.send_batch(&b)?;
        }
        if !any {
            self.send_batch(&RecordBatch::new_empty(schema.clone()))?;
        }
        self.send(&Request::ResultSetEnd {})
    }

    pub fn write_finish(&mut self) -> Result<Vec<String>> {
        self.send(&Request::Finish {})?;
        match self.recv_json("a written reply")? {
            Response::Written { files } => Ok(files),
            other => Err(self.unexpected("a written reply", &Incoming::Json(other))),
        }
    }

    pub fn deliver(
        &mut self,
        local_path: &str,
        remote_path: Option<&str>,
        connection: Map<String, Value>,
    ) -> Result<String> {
        self.send(&Request::Deliver {
            local_path: local_path.to_string(),
            remote_path: remote_path.map(str::to_string),
            connection,
        })?;
        match self.recv_json("a delivered reply")? {
            Response::Delivered { location } => Ok(location),
            other => Err(self.unexpected("a delivered reply", &Incoming::Json(other))),
        }
    }

    /// Ask the plugin to exit, and wait for it.
    pub fn close(mut self) -> Result<()> {
        let _ = self.send(&Request::Close {});
        let _ = self.recv(Some(Duration::from_secs(5)), "the close reply");
        self.stdin.take();
        for _ in 0..250 {
            if let Ok(Some(_)) = self.child.try_wait() {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = self.child.kill();
        Ok(())
    }

    /// Raw access for protocol tests: write arbitrary bytes to the plugin.
    pub fn write_raw(&mut self, bytes: &[u8]) -> Result<()> {
        let ok = match self.stdin.as_mut() {
            Some(w) => w.write_all(bytes).and_then(|_| w.flush()).is_ok(),
            None => false,
        };
        if ok { Ok(()) } else { Err(self.write_failed()) }
    }

    /// Wait up to `timeout` for the process to exit.
    pub fn wait_exit(&mut self, timeout: Duration) -> Option<ExitStatus> {
        self.stdin.take();
        let deadline = std::time::Instant::now() + timeout;
        while std::time::Instant::now() < deadline {
            if let Ok(Some(s)) = self.child.try_wait() {
                return Some(s);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        None
    }
}

impl Drop for PluginProcess {
    fn drop(&mut self) {
        if let Ok(None) = self.child.try_wait() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn core_version() -> String {
    match env!("CARGO_PKG_VERSION") {
        "0.0.0" => "unreleased".into(),
        v => v.into(),
    }
}
