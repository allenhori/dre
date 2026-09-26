//! The DRE plugin protocol, version 0.
//!
//! A plugin is an executable named `dre-<kind>-<name>` that talks to DRE core over stdin/stdout
//! in length-prefixed frames. Each frame is a JSON control message or an Arrow IPC stream; stderr
//! is the plugin's log channel. See `docs/protocol.md` in the repository for the public contract.
//!
//! - [`frame`]: the wire format.
//! - [`msg`]: every control message.
//! - [`host`]: the core side: spawning a plugin, the handshake, requests.
//! - [`plugin`]: the SDK plugin authors use to serve requests.
//! - [`conformance`]: checks any plugin binary against the protocol.

pub mod conformance;
pub mod frame;
pub mod host;
pub mod msg;
pub mod plugin;
pub mod util;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
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

    /// The declaration block listing plugins of this kind (`sources:` ...).
    pub fn block(self) -> &'static str {
        match self {
            Kind::Source => "sources",
            Kind::Format => "formats",
            Kind::Destination => "destinations",
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

/// The executable file name for a plugin on this platform.
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
    let valid = !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
    valid.then(|| (kind, name.to_string()))
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
    }
}
