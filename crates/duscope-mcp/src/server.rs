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
//!
//! A `tools/call` runs on the [`worker`](crate::worker), one at a time and
//! in the order received, while the reader answers `ping`,
//! `server/discover` and `tools/list` at once and reads
//! `notifications/cancelled`; the answers to the other requests wait for
//! the calls received before them.

use std::fmt;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use log::{debug, info};
use serde::Serialize;
use serde_json::value::RawValue;
use serde_json::{Map, Value, json};

use crate::discover::{self, ServerConfig};
use crate::jsonrpc::{Id, Notification, Request, RpcError};
use crate::legacy;
use crate::roots::Roots;
use crate::tools::{ListedTool, Registry, Runner};
use crate::transport::{Handler, Responder};
use crate::worker::{Timing, Worker};

/// How long a client may keep a `tools/list` result, in milliseconds: an
/// hour. The tools do not change while the process runs.
pub const TOOLS_TTL_MS: u64 = 3_600_000;

/// The requests the reader answers at once, whatever a tool call is doing.
const AT_ONCE: [&str; 3] = ["ping", "server/discover", "tools/list"];

/// The duscope MCP server.
#[derive(Debug)]
pub struct Server {
    config: ServerConfig,
    /// The version an `initialize` answered, for the requests that carry
    /// none: set once its answer is sent or queued, never by one refused as
    /// busy, which the client is told did not run.
    session: Option<&'static str>,
    tools: Option<Arc<Tools>>,
    timing: Timing,
    /// Started with the first tool call.
    worker: Option<Worker>,
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
pub(crate) enum Revision {
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

/// How a request is answered.
enum Answer {
    /// With this, once the requests before it allow.
    Now(Result<Box<RawValue>, RpcError>),
    /// On the worker: a tool call, in the revision it speaks.
    Call(Revision),
}

impl Server {
    /// A server describing itself as `config` says, with no tools.
    pub fn new(config: ServerConfig) -> Server {
        Server {
            config,
            session: None,
            tools: None,
            timing: Timing::default(),
            worker: None,
        }
    }

    /// The same server offering `tools`, and saying so in its capabilities.
    #[must_use]
    pub fn with_tools(mut self, tools: Tools) -> Server {
        self.config = self
            .config
            .with_capability("tools", json!({ "listChanged": false }));
        self.tools = Some(Arc::new(tools));
        self
    }

    /// The same server, reporting progress and timing tool calls as
    /// `timing` says.
    #[must_use]
    pub fn with_timing(mut self, timing: Timing) -> Server {
        self.timing = timing;
        self
    }

    fn answer(&mut self, request: &Request) -> Answer {
        let params = request.params.as_ref();
        if request.method == "initialize" {
            // The session starts once the answer is admitted (`request`).
            let result = legacy::initialize(&self.config, params)
                .and_then(|result| written(serde_json::value::to_raw_value(&result)));
            return Answer::Now(result);
        }
        let (revision, version) = match discover::request_version(params) {
            Some(Ok(version)) => (Revision::PerRequest, version),
            Some(Err(error)) => return Answer::Now(Err(error)),
            None => match self.session {
                Some(version) => (Revision::Session, version),
                None if request.method == "ping" => (Revision::Session, discover::LEGACY),
                None => {
                    return Answer::Now(Err(RpcError::invalid_params(
                        "_meta[\"io.modelcontextprotocol/protocolVersion\"] is missing: send \
                         it, with the client's capabilities, on every request (2026-07-28), or \
                         call initialize first (2025-11-25)",
                    )));
                }
            },
        };
        debug!("{} at {version}", request.method.escape_debug());
        if request.method == "tools/call" && self.tools.is_some() {
            return Answer::Call(revision);
        }
        Answer::Now(self.call(revision, &request.method))
    }

    /// The worker, started on first use.
    fn worker(&mut self) -> io::Result<&Worker> {
        match (&mut self.worker, &self.tools) {
            (Some(worker), _) => Ok(worker),
            (worker @ None, Some(tools)) => Ok(worker.insert(Worker::start(
                Arc::clone(tools),
                self.config.clone(),
                self.timing,
            )?)),
            (None, None) => Err(io::Error::other("the server has no tools to run")),
        }
    }

    /// `notifications/cancelled`: the call it names, queued or running, is
    /// cancelled and never answered.
    fn cancelled(&self, params: Option<&Map<String, Value>>) {
        let Some(id) = params
            .and_then(|params| params.get("requestId"))
            .and_then(Id::from_value)
        else {
            debug!("ignored a cancellation that names no request");
            return;
        };
        let reason = params
            .and_then(|params| params.get("reason"))
            .and_then(Value::as_str)
            .unwrap_or("no reason given");
        match &self.worker {
            Some(worker) if worker.cancel(&id) => {
                info!("cancelled request {id}: {}", reason.escape_debug());
            }
            _ => debug!(
                "ignored the cancellation of request {id}: no call of that id is waiting, or \
                 running and able to stop"
            ),
        }
    }

    /// Every method but `initialize` and a `tools/call` the server runs: its
    /// result, as the revision writes it. 2026-07-28 has no `ping`: a request
    /// naming that version gets "method not found", as rmcp's server answers
    /// it (DEV-139).
    fn call(&self, revision: Revision, method: &str) -> Result<Box<RawValue>, RpcError> {
        match (method, revision, &self.tools) {
            ("ping", Revision::Session, _) => self.finish(revision, &Map::new()),
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
            (method, _, _) => Err(RpcError::method_not_found(method)),
        }
    }

    fn finish<T: Serialize>(
        &self,
        revision: Revision,
        body: &T,
    ) -> Result<Box<RawValue>, RpcError> {
        finish(&self.config, revision, body)
    }
}

/// `body` as the result `revision` sends: 2026-07-28's marked complete and
/// naming the server `config` describes, 2025-11-25's as it is.
pub(crate) fn finish<T: Serialize>(
    config: &ServerConfig,
    revision: Revision,
    body: &T,
) -> Result<Box<RawValue>, RpcError> {
    written(match revision {
        Revision::PerRequest => discover::complete(config, body),
        Revision::Session => serde_json::value::to_raw_value(body),
    })
}

/// A result written as JSON, or the internal error that it could not be.
fn written(result: serde_json::Result<Box<RawValue>>) -> Result<Box<RawValue>, RpcError> {
    result.map_err(|e| RpcError::internal(format!("cannot write the result: {e}")))
}

/// Sends a response. A failed write stops the transport, which reports it.
fn send(responder: Responder, outcome: Result<Box<RawValue>, RpcError>) {
    let id = responder.id().clone();
    if let Err(e) = responder.respond_raw(outcome) {
        debug!("could not answer request {id}: {e}");
    }
}

impl Handler for Server {
    fn request(&mut self, request: Request, responder: Responder) {
        match self.answer(&request) {
            Answer::Call(revision) => match self.worker() {
                Ok(worker) => worker.call(request, revision, responder),
                Err(e) => send(
                    responder,
                    Err(RpcError::internal(format!(
                        "cannot start the thread that runs the tools: {e}"
                    ))),
                ),
            },
            Answer::Now(outcome) => {
                let initialized = request.method == "initialize" && outcome.is_ok();
                let admitted = match &self.worker {
                    Some(worker) if !AT_ONCE.contains(&request.method.as_str()) => {
                        worker.answer(responder, outcome)
                    }
                    _ => {
                        send(responder, outcome);
                        true
                    }
                };
                // An `initialize` refused as busy never ran: no session.
                if initialized && admitted {
                    self.session = Some(discover::LEGACY);
                    debug!("initialized at {}", discover::LEGACY);
                }
            }
        }
    }

    fn notification(&mut self, notification: Notification) {
        match notification.method.as_str() {
            "notifications/initialized" => {
                debug!("the client finished the initialize handshake");
            }
            "notifications/cancelled" => self.cancelled(notification.params.as_ref()),
            method => debug!("ignored notification {}", method.escape_debug()),
        }
    }

    fn end(&mut self, ended: &io::Result<()>) {
        if let Some(worker) = self.worker.take() {
            match ended {
                Ok(()) => worker.finish(),
                Err(_) => worker.abort(),
            }
        }
    }
}
