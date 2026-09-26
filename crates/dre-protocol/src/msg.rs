//! Control messages. Every JSON frame is an object with a `type` field.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::Kind;

/// Core → plugin.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    /// Always first. The plugin picks a version in both ranges or replies `version_mismatch`.
    Hello {
        min_version: u32,
        max_version: u32,
        core_version: String,
    },
    /// Describe the connection fields a profile output for this plugin takes (used by `dre init`).
    Describe {},
    /// Source: open a session. All later `execute`/`check` requests run on it.
    Open {
        connection: Map<String, Value>,
        read_only: bool,
    },
    /// Source: run one statement. Replies `result` (then Arrow frames, then `result_end`) or
    /// `no_result`.
    Execute {
        sql: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        row_limit: Option<u64>,
    },
    /// Source: verify one statement without executing it. Replies `ok` or `error`.
    Check {
        sql: String,
    },
    /// Format: write result sets to `path`. Followed, per result set, by Arrow frames (at least one,
    /// carrying the schema) and a `result_set_end`, then `finish`. Replies `written`.
    Write {
        path: String,
        format: String,
        options: Map<String, Value>,
        result_sets: Vec<ResultSetMeta>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        template: Option<Value>,
    },
    ResultSetEnd {},
    Finish {},
    /// Destination: deliver a local file, or with `files` (only to a plugin advertising
    /// `multi_file`) every file of one output at once. Exactly one of `local_path`/`files` is set.
    /// `options` are the destination entry's plugin options, rendered by core. Replies `delivered`.
    Deliver {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        local_path: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        remote_path: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        files: Vec<DeliveryFile>,
        connection: Map<String, Value>,
        #[serde(default)]
        options: Map<String, Value>,
    },
    /// Source: load rows into a temporary table on the session, named after `name`. Followed by
    /// Arrow frames (at least one, carrying the schema) and a `result_set_end`. Replies `loaded`.
    /// Only sent to plugins advertising `load`.
    Load {
        name: String,
    },
    /// End the conversation; the plugin exits 0.
    Close {},
}

/// Plugin → core.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    Hello {
        protocol_version: u32,
        kind: Kind,
        name: String,
        version: String,
        #[serde(default)]
        capabilities: Vec<String>,
    },
    VersionMismatch {
        min_version: u32,
        max_version: u32,
    },
    Describe {
        connection_fields: Vec<ConnectionField>,
    },
    Ok {},
    /// A result set follows as Arrow frames, ended by `result_end`.
    Result {
        columns: Vec<String>,
    },
    ResultEnd {
        rows: u64,
    },
    NoResult {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rows_affected: Option<u64>,
    },
    Written {
        files: Vec<String>,
    },
    Delivered {
        location: String,
    },
    /// `relation` is what SQL uses to read the loaded rows. `warning`, when set, is shown to the
    /// user (e.g. the database has no bulk path, so a load this size is slow).
    Loaded {
        relation: String,
        rows: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        warning: Option<String>,
    },
    Error {
        message: String,
    },
}

/// One file of a multi-file `deliver`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeliveryFile {
    pub local_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_path: Option<String>,
}

/// Per result set, what a format plugin needs to lay it out.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResultSetMeta {
    /// Sheet name (xlsx) or file suffix (single-table formats).
    pub name: String,
    /// The query (`.sql` basename) that produced it.
    pub query: String,
    /// 1-based index among that query's result sets.
    pub result_index: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConnectionField {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub required: bool,
    /// Offered as an `env_var()` reference by default in `dre init`.
    #[serde(default)]
    pub secret: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,
    /// A source type whose profile has the same field: `dre init` offers the value entered for
    /// that source as this field's default (e.g. one Databricks host for source and destination).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub same_as_source: Option<String>,
}

impl ConnectionField {
    pub fn new(name: &str, description: &str) -> Self {
        ConnectionField {
            name: name.into(),
            description: description.into(),
            required: false,
            secret: false,
            default: None,
            same_as_source: None,
        }
    }
    pub fn required(mut self) -> Self {
        self.required = true;
        self
    }
    pub fn secret(mut self) -> Self {
        self.secret = true;
        self
    }
    pub fn default(mut self, v: impl Into<Value>) -> Self {
        self.default = Some(v.into());
        self
    }
    pub fn same_as_source(mut self, source_type: &str) -> Self {
        self.same_as_source = Some(source_type.into());
        self
    }
}
