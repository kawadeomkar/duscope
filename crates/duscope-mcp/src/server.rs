//! The server: the [`Handler`] the transport hands each message to, which
//! answers it in the revision it speaks.
//!
//! - `initialize` is the 2025-11-25 handshake.
//! - A request whose `_meta` names a protocol version is a 2026-07-28 request,
//!   answered on its own ([`discover`]).
//! - Any other request belongs to an `initialize` session: once one has been
//!   answered, it is served at the version answered; before, it is malformed
//!   for 2026-07-28 (`-32602`), but for `ping`, which 2025-11-25 allows ahead
//!   of the handshake.
//!
//! Every method is looked up in one table whichever revision asks; the
//! revision only shapes the result (2026-07-28 marks it complete and names
//! the server, and gives `tools/list` how long it holds). Both revisions are
//! served side by side in one process; the only state is the version an
//! `initialize` answered. The methods are `server/discover`, `ping`, and,
//! once the server has its [`Tools`], `tools/list` and `tools/call`.

use std::fmt;
use std::path::PathBuf;

use log::debug;
use serde::Serialize;
use serde_json::value::RawValue;
use serde_json::{Map, Value, json};

use crate::discover::{self, ServerConfig};
use crate::jsonrpc::{Notification, Request, RpcError};
use crate::legacy;
use crate::roots::Roots;
use crate::tools::{Context, ListedTool, Registry, Runner};
use crate::transport::{Handler, Responder};

/// How long a client may keep a `tools/list` result, in milliseconds: an
/// hour. The tools do not change while the process runs.
pub const TOOLS_TTL_MS: u64 = 3_600_000;

/// The duscope MCP server.
#[derive(Debug)]
pub struct Server {
    config: ServerConfig,
    /// The version an `initialize` answered, for the requests that carry
    /// none.
    session: Option<&'static str>,
    tools: Option<Tools>,
}

/// The tools a server offers, and what they run with.
pub struct Tools {
    /// The tools.
    pub registry: Registry,
    /// The directories a client may reach.
    pub roots: Roots,
    /// Where the saved scans are.
    pub index_dir: PathBuf,
    /// What runs a tool's verb.
    pub runner: Box<dyn Runner>,
}

impl fmt::Debug for Tools {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Tools")
            .field("registry", &self.registry)
            .field("roots", &self.roots)
            .field("index_dir", &self.index_dir)
            .finish_non_exhaustive()
    }
}

/// The revision a request speaks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Revision {
    /// It names its version in `_meta` (2026-07-28).
    PerRequest,
    /// It belongs to an `initialize` session (2025-11-25).
    Session,
}

/// `tools/list`'s fields. 2026-07-28 asks a list to say how long it holds
/// and who may share it; every client gets the same tools.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ToolList<'a> {
    tools: Vec<ListedTool<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ttl_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cache_scope: Option<&'static str>,
}

impl Server {
    /// A server describing itself as `config` says, with no tools.
    pub fn new(config: ServerConfig) -> Server {
        Server {
            config,
            session: None,
            tools: None,
        }
    }

    /// The same server offering `tools`, and saying so in its capabilities.
    #[must_use]
    pub fn with_tools(mut self, tools: Tools) -> Server {
        self.config = self
            .config
            .with_capability("tools", json!({ "listChanged": false }));
        self.tools = Some(tools);
        self
    }

    fn answer(&mut self, request: &Request) -> Result<Box<RawValue>, RpcError> {
        let params = request.params.as_ref();
        if request.method == "initialize" {
            let result = legacy::initialize(&self.config, params)?;
            self.session = Some(discover::LEGACY);
            debug!("initialized at {}", discover::LEGACY);
            return written(serde_json::value::to_raw_value(&result));
        }
        let (revision, version) = match discover::request_version(params) {
            Some(Ok(version)) => (Revision::PerRequest, version),
            Some(Err(error)) => return Err(error),
            None => match self.session {
                Some(version) => (Revision::Session, version),
                None if request.method == "ping" => (Revision::Session, discover::LEGACY),
                None => {
                    return Err(RpcError::invalid_params(
                        "_meta[\"io.modelcontextprotocol/protocolVersion\"] is missing: send \
                         it, with the client's capabilities, on every request (2026-07-28), or \
                         call initialize first (2025-11-25)",
                    ));
                }
            },
        };
        debug!("{} at {version}", request.method.escape_debug());
        self.call(revision, &request.method, params)
    }

    /// Every method: its result, as the revision writes it.
    fn call(
        &self,
        revision: Revision,
        method: &str,
        params: Option<&Map<String, Value>>,
    ) -> Result<Box<RawValue>, RpcError> {
        match (method, revision, &self.tools) {
            ("ping", _, _) => self.finish(revision, &Map::new()),
            ("server/discover", Revision::PerRequest, _) => {
                self.finish(revision, &discover::discover_fields(&self.config))
            }
            ("tools/list", _, Some(tools)) => {
                let cacheable = revision == Revision::PerRequest;
                self.finish(
                    revision,
                    &ToolList {
                        tools: tools.registry.list(),
                        ttl_ms: cacheable.then_some(TOOLS_TTL_MS),
                        cache_scope: cacheable.then_some("public"),
                    },
                )
            }
            ("tools/call", _, Some(tools)) => {
                let context = Context {
                    roots: &tools.roots,
                    index_dir: &tools.index_dir,
                };
                let result = tools
                    .registry
                    .call(params, &context, tools.runner.as_ref())?;
                debug!(
                    "tools/call answered{}",
                    if result.is_error() {
                        " with an error"
                    } else {
                        ""
                    }
                );
                self.finish(revision, &result)
            }
            (method, _, _) => Err(RpcError::method_not_found(method)),
        }
    }

    /// `body` as the result `revision` sends: 2026-07-28's marked complete
    /// and naming the server, 2025-11-25's as it is.
    fn finish<T: Serialize>(
        &self,
        revision: Revision,
        body: &T,
    ) -> Result<Box<RawValue>, RpcError> {
        written(match revision {
            Revision::PerRequest => discover::complete(&self.config, body),
            Revision::Session => serde_json::value::to_raw_value(body),
        })
    }
}

/// A result written as JSON, or the internal error that it could not be.
fn written(result: serde_json::Result<Box<RawValue>>) -> Result<Box<RawValue>, RpcError> {
    result.map_err(|e| RpcError::internal(format!("cannot write the result: {e}")))
}

impl Handler for Server {
    fn request(&mut self, request: Request, responder: Responder) {
        let outcome = self.answer(&request);
        // A failed write stops the transport, which reports it.
        if let Err(e) = responder.respond_raw(outcome) {
            debug!("could not answer request {}: {e}", request.id);
        }
    }

    fn notification(&mut self, notification: Notification) {
        match notification.method.as_str() {
            "notifications/initialized" => {
                debug!("the client finished the initialize handshake");
            }
            method => debug!("ignored notification {}", method.escape_debug()),
        }
    }
}
