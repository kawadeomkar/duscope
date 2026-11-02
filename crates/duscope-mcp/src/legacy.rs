//! The 2025-11-25 `initialize` handshake, for clients that predate per-request
//! metadata: the client names a version in `initialize`, the server answers
//! with the version it will speak, its capabilities, its name and its
//! instructions, and the client confirms with `notifications/initialized`.
//! From there the client's requests carry no version; the server holds the
//! one it answered for as long as the process runs.

use serde_json::{Map, Value, json};

use crate::discover::{LEGACY, SUPPORTED_VERSIONS, ServerConfig};
use crate::jsonrpc::{INVALID_PARAMS, RpcError};

/// The answer to `initialize`: served at [`LEGACY`] when the client names it,
/// and when it names a newer version the server serves (2026-07-28 has no
/// handshake, so 2025-11-25 is the newest version it can speak over one, as
/// the 2025-11-25 negotiation rule asks). Any other version, older or
/// unknown, is refused with `-32602`, its data listing the versions served
/// (the revision's initialization error): duscope's results lean on what
/// 2025-11-25 added, such as structured tool output, which an older client
/// would not read.
pub(crate) fn initialize(
    config: &ServerConfig,
    params: Option<&Map<String, Value>>,
) -> Result<Value, RpcError> {
    let requested = params
        .and_then(|params| params.get("protocolVersion"))
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::invalid_params("protocolVersion must be a string"))?;
    if requested != LEGACY && !newer_and_served(requested) {
        return Err(unsupported_version(requested));
    }
    Ok(json!({
        "protocolVersion": LEGACY,
        "capabilities": config.capabilities(),
        "serverInfo": config.server_info(),
        "instructions": config.instructions(),
    }))
}

/// Whether `version` is served and newer than [`LEGACY`].
fn newer_and_served(version: &str) -> bool {
    SUPPORTED_VERSIONS.contains(&version) && version > LEGACY
}

/// The initialization error for `requested`: `-32602`, listing the versions
/// served.
fn unsupported_version(requested: &str) -> RpcError {
    RpcError::new(INVALID_PARAMS, "Unsupported protocol version").with_data(json!({
        "supported": SUPPORTED_VERSIONS,
        "requested": requested,
    }))
}
