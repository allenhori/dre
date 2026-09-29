//! The DRE plugin protocol, version 0.
//!
//! Plugins ship in packages: one executable, `dre-plugin-<package>`, serving every plugin the
//! package provides (a single plugin may also be named `dre-<kind>-<name>`). It talks to DRE core
//! over stdin/stdout in length-prefixed frames. Each frame is a JSON control message or an Arrow IPC stream; stderr
//! is the plugin's log channel. See `docs/protocol.md` in the repository for the public contract.
//!
//! - [`frame`]: the wire format.
//! - [`msg`]: every control message.
//! - [`host`]: the core side: spawning a plugin, the handshake, requests.
//! - [`plugin`]: the SDK plugin authors use to serve requests.
//! - [`options`]: the options a plugin declares, and how they're checked.
//! - [`conformance`]: checks any plugin binary against the protocol.
//! - [`sessions`]: OAuth sessions plugins keep in `~/.dre/oauth_sessions.json`.

pub mod conformance;
pub mod frame;
pub mod host;
pub mod msg;
pub mod options;
pub mod plugin;
pub mod sessions;
pub mod util;

/// This crate's version (its own, not DRE's); the fixture plugins report it.
pub const CRATE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Protocol versions this build speaks.
pub const MIN_VERSION: u32 = 0;
pub const MAX_VERSION: u32 = 0;

/// Capability: can hold one session (and its temp objects) across requests.
pub const CAP_SESSIONS: &str = "sessions";
/// Capability: can open a connection read-only.
pub const CAP_READ_ONLY: &str = "read_only";
/// Capability: can verify a statement without executing it.
pub const CAP_CHECK: &str = "check";
/// Capability (destinations): takes every file of one output in a single `deliver`.
pub const CAP_MULTI_FILE: &str = "multi_file";
/// Source: loads rows into a temporary table on the session (`load`), for large lookups.
pub const CAP_LOAD: &str = "load";
/// Answers `validate` (checks a config block of options). The Rust SDK always advertises it.
pub const CAP_VALIDATE: &str = "validate";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Source,
    Format,
    Destination,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Source => "source",
            Kind::Format => "format",
            Kind::Destination => "destination",
        }
    }

    /// Parse a kind, singular or plural (`source`, `sources`).
    pub fn parse(s: &str) -> Option<Kind> {
        match s {
            "source" | "sources" => Some(Kind::Source),
            "format" | "formats" => Some(Kind::Format),
            "destination" | "destinations" => Some(Kind::Destination),
            _ => None,
        }
    }
}

impl std::fmt::Display for Kind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One plugin: its kind and name. Written `<kind>/<name>` (`destination/s3`), in messages as
/// well as in `dre.lock`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PluginId {
    pub kind: Kind,
    pub name: String,
}

impl PluginId {
    pub fn new(kind: Kind, name: impl Into<String>) -> PluginId {
        PluginId {
            kind,
            name: name.into(),
        }
    }
}

impl std::fmt::Display for PluginId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.kind, self.name)
    }
}

impl std::str::FromStr for PluginId {
    type Err = String;

    fn from_str(s: &str) -> Result<PluginId, String> {
        let bad = || format!("`{s}` isn't `<kind>/<name>` (e.g. `destination/s3`)");
        let (k, n) = s.split_once('/').ok_or_else(bad)?;
        let kind = Kind::parse(k).ok_or_else(bad)?;
        valid_name(n).then(|| PluginId::new(kind, n)).ok_or_else(bad)
    }
}

impl serde::Serialize for PluginId {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> serde::Deserialize<'de> for PluginId {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        String::deserialize(d)?.parse().map_err(serde::de::Error::custom)
    }
}

/// Whether `name` is a valid plugin or package name: `[a-z0-9_]+`.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// The executable file name for a package on this platform: `dre-plugin-<package>`.
pub fn package_executable_name(package: &str) -> String {
    format!("dre-plugin-{package}{}", std::env::consts::EXE_SUFFIX)
}

/// Parse `dre-plugin-<package>[.exe]` into the package name.
pub fn parse_package_executable_name(file: &str) -> Option<String> {
    let stem = file.strip_suffix(std::env::consts::EXE_SUFFIX).unwrap_or(file);
    if !std::env::consts::EXE_SUFFIX.is_empty() && stem == file {
        return None;
    }
    let name = stem.strip_prefix("dre-plugin-")?;
    valid_name(name).then(|| name.to_string())
}

/// The executable file name for a single plugin on this platform.
pub fn executable_name(kind: Kind, name: &str) -> String {
    format!("dre-{}-{name}{}", kind.as_str(), std::env::consts::EXE_SUFFIX)
}

/// Parse `dre-<kind>-<name>[.exe]` into its kind and name. Names are `[a-z0-9_]+`.
pub fn parse_executable_name(file: &str) -> Option<(Kind, String)> {
    let stem = file.strip_suffix(std::env::consts::EXE_SUFFIX).unwrap_or(file);
    if !std::env::consts::EXE_SUFFIX.is_empty() && stem == file {
        return None;
    }
    let rest = stem.strip_prefix("dre-")?;
    let (kind, name) = rest.split_once('-')?;
    let kind = Kind::parse(kind)?;
    valid_name(name).then(|| (kind, name.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plugin_executable_names() {
        let exe = |s: &str| format!("{s}{}", std::env::consts::EXE_SUFFIX);
        assert_eq!(
            parse_executable_name(&exe("dre-source-duckdb")),
            Some((Kind::Source, "duckdb".into()))
        );
        assert_eq!(
            parse_executable_name(&exe("dre-destination-azure_blob")),
            Some((Kind::Destination, "azure_blob".into()))
        );
        assert_eq!(parse_executable_name(&exe("dre-format-csv.d")), None);
        assert_eq!(parse_executable_name(&exe("dre-widget-x")), None);
        assert_eq!(parse_executable_name(&exe("dre")), None);
        assert_eq!(parse_executable_name(&exe("dre-plugin-csv")), None);
    }

    #[test]
    fn parses_package_executable_names() {
        let exe = |s: &str| format!("{s}{}", std::env::consts::EXE_SUFFIX);
        assert_eq!(
            parse_package_executable_name(&exe("dre-plugin-object_store")).as_deref(),
            Some("object_store")
        );
        assert_eq!(parse_package_executable_name(&exe("dre-source-duckdb")), None);
        assert_eq!(parse_package_executable_name(&exe("dre-plugin-")), None);
    }

    #[test]
    fn plugin_ids_read_and_write_as_kind_slash_name() {
        let id: PluginId = "destination/s3".parse().unwrap();
        assert_eq!(id, PluginId::new(Kind::Destination, "s3"));
        assert_eq!(id.to_string(), "destination/s3");
        assert_eq!(serde_json::to_string(&id).unwrap(), "\"destination/s3\"");
        assert!("s3".parse::<PluginId>().is_err());
        assert!("widget/s3".parse::<PluginId>().is_err());
    }
}
