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
//! Both revisions are served side by side in one process; the only state is
//! the version an `initialize` answered. `server/discover` and `ping` are the
//! methods so far; the tools come with the registry.

use log::debug;
use serde_json::{Map, Value, json};

use crate::discover::{self, ServerConfig};
use crate::jsonrpc::{Notification, Request, RpcError};
use crate::legacy;
use crate::transport::{Handler, Responder};

/// The duscope MCP server.
#[derive(Debug)]
pub struct Server {
    config: ServerConfig,
    /// The version an `initialize` answered, for the requests that carry
    /// none.
    session: Option<&'static str>,
}

impl Server {
    /// A server describing itself as `config` says.
    pub fn new(config: ServerConfig) -> Server {
        Server {
            config,
            session: None,
        }
    }

    fn answer(&mut self, request: &Request) -> Result<Value, RpcError> {
        let params = request.params.as_ref();
        if request.method == "initialize" {
            let result = legacy::initialize(&self.config, params)?;
            self.session = Some(discover::LEGACY);
            debug!("initialized at {}", discover::LEGACY);
            return Ok(result);
        }
        match discover::request_version(params) {
            Some(Ok(version)) => self.modern(version, request),
            Some(Err(error)) => Err(error),
            None => self.in_session(request),
        }
    }

    /// A request carrying its version: answered on its own.
    fn modern(&self, version: &str, request: &Request) -> Result<Value, RpcError> {
        debug!(
            "{} at {version}, from its _meta",
            request.method.escape_debug()
        );
        match request.method.as_str() {
            "server/discover" => Ok(discover::discover_result(&self.config)),
            "ping" => Ok(discover::complete(&self.config, Map::new())),
            method => Err(RpcError::method_not_found(method)),
        }
    }

    /// A request carrying no version: one of an `initialize` session's.
    fn in_session(&self, request: &Request) -> Result<Value, RpcError> {
        match (request.method.as_str(), self.session) {
            ("ping", _) => Ok(json!({})),
            (method, Some(_)) => Err(RpcError::method_not_found(method)),
            (_, None) => Err(RpcError::invalid_params(
                "_meta[\"io.modelcontextprotocol/protocolVersion\"] is missing: send it, with \
                 the client's capabilities, on every request (2026-07-28), or call initialize \
                 first (2025-11-25)",
            )),
        }
    }
}

impl Handler for Server {
    fn request(&mut self, request: Request, responder: Responder) {
        let outcome = self.answer(&request);
        // A failed write stops the transport, which reports it.
        if let Err(e) = responder.respond(outcome) {
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
