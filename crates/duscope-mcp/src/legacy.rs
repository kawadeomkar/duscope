//! The 2025-11-25 `initialize` handshake, for clients that predate per-request
//! metadata: the client names a version in `initialize`, the server answers
//! with the version it will speak, its capabilities, its name and its
//! instructions, and the client confirms with `notifications/initialized`.
//! From there the client's requests carry no version; the server holds the
//! one it answered for as long as the process runs.

use serde_json::{Map, Value, json};

use crate::discover::{self, LEGACY, MODERN, ServerConfig};
use crate::jsonrpc::{INVALID_PARAMS, RpcError};

/// The answer to `initialize`, always at [`LEGACY`]:
///
/// - naming 2025-11-25, the client is served at it;
/// - naming 2026-07-28, which the server serves but which has no handshake,
///   it is answered with 2025-11-25, as the 2025-11-25 negotiation rule has a
///   server answer with another version it supports;
/// - naming any other version, older or unknown, it is refused with
///   `-32602` listing the versions served (the revision's initialization
///   error), where that rule would answer 2025-11-25: duscope's results
///   lean on what 2025-11-25 added, such as structured tool output, which an
///   older client would not read.
pub(crate) fn initialize(
    config: &ServerConfig,
    params: Option<&Map<String, Value>>,
) -> Result<Value, RpcError> {
    let requested = params
        .and_then(|params| params.get("protocolVersion"))
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::invalid_params("protocolVersion must be a string"))?;
    if requested != LEGACY && requested != MODERN {
        return Err(discover::version_error(INVALID_PARAMS, requested));
    }
    Ok(json!({
        "protocolVersion": LEGACY,
        "capabilities": config.capabilities(),
        "serverInfo": config.server_info(),
        "instructions": config.instructions(),
    }))
}
