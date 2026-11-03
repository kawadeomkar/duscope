//! `server/discover` and the per-request protocol metadata of MCP 2026-07-28:
//! the versions duscope serves, what a request's `_meta` must carry, and the
//! result that names the server, its capabilities and its instructions.
//!
//! A 2026-07-28 request carries its version in
//! `params._meta["io.modelcontextprotocol/protocolVersion"]`, beside the
//! client's capabilities, and is answered on its own: nothing a request says
//! carries over to the next. A version the server does
//! not serve this way gets the revision's `UnsupportedProtocolVersionError`,
//! which lists the versions it does.

use serde::Serialize;
use serde_json::value::RawValue;
use serde_json::{Map, Value, json};

use crate::jsonrpc::RpcError;

/// The revision whose requests carry their version in `_meta`.
pub const MODERN: &str = "2026-07-28";

/// The revision with the `initialize` handshake, which older clients speak.
pub const LEGACY: &str = "2025-11-25";

/// The versions served, newest first: what discovery and every version
/// error list.
pub const SUPPORTED_VERSIONS: [&str; 2] = [MODERN, LEGACY];

/// The versions a request may name in its `_meta`: those without a
/// handshake. 2025-11-25 is served over `initialize` alone.
const PER_REQUEST_VERSIONS: [&str; 1] = [MODERN];

/// MCP 2026-07-28's `UnsupportedProtocolVersionError`: a request named a
/// version the server does not serve. A client that receives it retries with
/// one of the versions in its data; any other error answering
/// `server/discover` would tell a client that probes for the server's era
/// that the server is legacy.
pub const UNSUPPORTED_PROTOCOL_VERSION: i64 = -32022;

/// The longest `instructions`, in characters as a JavaScript string counts
/// them (UTF-16 code units): as much as Claude Code's tool search loads at
/// startup.
pub const MAX_INSTRUCTIONS_CHARS: usize = 4096;

/// How long a client may keep a discovery result, in milliseconds: an hour.
/// Nothing in it changes while the process runs.
pub const DISCOVER_TTL_MS: u64 = 3_600_000;

/// `instructions` until the agent briefing (P3-AGT-01) supplies them.
pub const PLACEHOLDER_INSTRUCTIONS: &str = "duscope answers questions about disk usage from saved \
     scans: the largest directories and files, files by type, stale files, caches, and what a \
     path holds. Sizes are bytes.";

const PROTOCOL_VERSION_KEY: &str = "io.modelcontextprotocol/protocolVersion";
const CLIENT_CAPABILITIES_KEY: &str = "io.modelcontextprotocol/clientCapabilities";
const SERVER_INFO_KEY: &str = "io.modelcontextprotocol/serverInfo";

/// Who the server is and what it offers: the same in every answer.
#[derive(Clone, Debug, PartialEq)]
pub struct ServerConfig {
    name: String,
    version: String,
    instructions: String,
    capabilities: Map<String, Value>,
}

impl ServerConfig {
    /// duscope at this crate's version, with `instructions` cut to
    /// [`MAX_INSTRUCTIONS_CHARS`] and no capabilities yet (the tool registry
    /// adds its own).
    pub fn new(instructions: &str) -> ServerConfig {
        ServerConfig {
            name: "duscope".to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            instructions: cut(instructions),
            capabilities: Map::new(),
        }
    }

    /// The same server offering `capabilities`, the object both handshakes
    /// return under `capabilities`.
    #[must_use]
    pub fn with_capabilities(mut self, capabilities: Map<String, Value>) -> ServerConfig {
        self.capabilities = capabilities;
        self
    }

    /// The same server offering one more capability, `name`, as `value`.
    #[must_use]
    pub fn with_capability(mut self, name: &str, value: Value) -> ServerConfig {
        self.capabilities.insert(name.to_owned(), value);
        self
    }

    /// The instructions, as cut.
    pub fn instructions(&self) -> &str {
        &self.instructions
    }

    /// The server's name and version, as both revisions spell an
    /// `Implementation`.
    pub(crate) fn server_info(&self) -> Value {
        json!({ "name": self.name, "version": self.version })
    }

    pub(crate) fn capabilities(&self) -> Value {
        Value::Object(self.capabilities.clone())
    }
}

/// `text` up to [`MAX_INSTRUCTIONS_CHARS`] UTF-16 code units, never splitting
/// a character.
fn cut(text: &str) -> String {
    let mut units = 0;
    text.chars()
        .take_while(|c| {
            units += c.len_utf16();
            units <= MAX_INSTRUCTIONS_CHARS
        })
        .collect()
}

/// The version a request names in its `_meta`: `None` when it names none,
/// as a request of the `initialize` revision does; else the version, once
/// the request carries the fields 2026-07-28 requires (`-32602` without
/// them) and names a version served without a handshake (`-32022`
/// otherwise, 2025-11-25 included).
pub(crate) fn request_version(
    params: Option<&Map<String, Value>>,
) -> Option<Result<&'static str, RpcError>> {
    let meta = params?.get("_meta")?.as_object()?;
    let requested = meta.get(PROTOCOL_VERSION_KEY)?;
    let Some(requested) = requested.as_str() else {
        return Some(Err(RpcError::invalid_params(format!(
            "_meta[\"{PROTOCOL_VERSION_KEY}\"] must be a string"
        ))));
    };
    if !meta
        .get(CLIENT_CAPABILITIES_KEY)
        .is_some_and(Value::is_object)
    {
        return Some(Err(RpcError::invalid_params(format!(
            "_meta[\"{CLIENT_CAPABILITIES_KEY}\"] must be an object: every 2026-07-28 request \
             carries the client's capabilities"
        ))));
    }
    Some(
        PER_REQUEST_VERSIONS
            .into_iter()
            .find(|&served| served == requested)
            .ok_or_else(|| version_error(UNSUPPORTED_PROTOCOL_VERSION, requested)),
    )
}

/// The error refusing `requested`, with `code`: both revisions word it the
/// same and list the versions served in its data.
pub(crate) fn version_error(code: i64, requested: &str) -> RpcError {
    RpcError::new(code, "Unsupported protocol version").with_data(json!({
        "supported": SUPPORTED_VERSIONS,
        "requested": requested,
    }))
}

/// A 2026-07-28 result: `body`'s fields, then the mark that it is complete
/// and the server's name in its `_meta`, as every result of the revision
/// should carry.
pub(crate) fn complete<T: Serialize>(
    config: &ServerConfig,
    body: &T,
) -> serde_json::Result<Box<RawValue>> {
    #[derive(Serialize)]
    struct Complete<'a, T> {
        #[serde(flatten)]
        body: &'a T,
        #[serde(rename = "resultType")]
        result_type: &'static str,
        #[serde(rename = "_meta")]
        meta: Value,
    }
    serde_json::value::to_raw_value(&Complete {
        body,
        result_type: "complete",
        meta: json!({ SERVER_INFO_KEY: config.server_info() }),
    })
}

/// The fields of `server/discover`'s answer: the versions served, the
/// capabilities, the instructions, and how long the answer holds.
/// `cacheScope` is `public`: every client gets the same answer.
pub(crate) fn discover_fields(config: &ServerConfig) -> Map<String, Value> {
    let mut fields = Map::new();
    fields.insert("supportedVersions".to_owned(), json!(SUPPORTED_VERSIONS));
    fields.insert("capabilities".to_owned(), config.capabilities());
    fields.insert("instructions".to_owned(), json!(config.instructions));
    fields.insert("ttlMs".to_owned(), json!(DISCOVER_TTL_MS));
    fields.insert("cacheScope".to_owned(), json!("public"));
    fields
}
