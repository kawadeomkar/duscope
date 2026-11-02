//! JSON-RPC 2.0 messages as MCP carries them on stdio, in both protocol
//! revisions duscope serves (2026-07-28 and 2025-11-25): one request,
//! notification or response per line, never a batch.
//!
//! [`parse`] turns one line into an [`Incoming`] message, or into the
//! [`Rejection`] the transport answers it with. Outgoing messages are written
//! as compact JSON, whose strings escape every control character, so no
//! message ever holds a newline.

use std::fmt;

use serde::Serialize;
use serde_json::{Map, Number, Value};

/// The JSON-RPC version every message names.
pub const VERSION: &str = "2.0";

/// Not JSON, or not UTF-8.
pub const PARSE_ERROR: i64 = -32700;
/// JSON, but not one request or notification.
pub const INVALID_REQUEST: i64 = -32600;
/// A request for a method the server does not have.
pub const METHOD_NOT_FOUND: i64 = -32601;
/// A method's parameters it cannot accept.
pub const INVALID_PARAMS: i64 = -32602;
/// A failure inside the server.
pub const INTERNAL_ERROR: i64 = -32603;

/// A request's id: a string or an integer. MCP narrows JSON-RPC here: an id
/// is never null, and a fraction is not an integer.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(untagged)]
pub enum Id {
    /// An integer, kept as sent (`u64::MAX` included).
    Number(Number),
    /// A string.
    String(String),
}

impl Id {
    fn from_value(value: &Value) -> Option<Id> {
        match value {
            Value::String(id) => Some(Id::String(id.clone())),
            Value::Number(id) if id.is_i64() || id.is_u64() => Some(Id::Number(id.clone())),
            _ => None,
        }
    }
}

impl fmt::Display for Id {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Id::Number(id) => write!(f, "{id}"),
            Id::String(id) => write!(f, "{id:?}"),
        }
    }
}

/// A request: the client waits for its one response.
#[derive(Clone, Debug, PartialEq)]
pub struct Request {
    /// The id the response carries back.
    pub id: Id,
    /// The method's name.
    pub method: String,
    /// The named parameters; `None` when the request had none.
    pub params: Option<Map<String, Value>>,
}

/// A notification: never answered, not even with an error.
#[derive(Clone, Debug, PartialEq)]
pub struct Notification {
    /// The method's name.
    pub method: String,
    /// The named parameters; `None` when the notification had none.
    pub params: Option<Map<String, Value>>,
}

/// One message read from the client.
#[derive(Clone, Debug, PartialEq)]
pub enum Incoming {
    /// A request, to answer.
    Request(Request),
    /// A notification, never answered.
    Notification(Notification),
    /// A response, with its id if it had a readable one. MCP clients send
    /// none (duscope sends no requests): the transport logs it and goes on,
    /// since answering it could start an exchange of errors without end.
    Response(Option<Id>),
}

/// A JSON-RPC error object.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RpcError {
    /// One of the codes above, or a code the method defines.
    pub code: i64,
    /// One sentence on what was wrong.
    pub message: String,
    /// What else the client may need, such as a limit or a list.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl RpcError {
    /// An error with `code` and `message` and no data.
    pub fn new(code: i64, message: impl Into<String>) -> RpcError {
        RpcError {
            code,
            message: message.into(),
            data: None,
        }
    }

    /// The same error carrying `data`.
    #[must_use]
    pub fn with_data(mut self, data: Value) -> RpcError {
        self.data = Some(data);
        self
    }

    /// `-32601` for `method`.
    pub fn method_not_found(method: &str) -> RpcError {
        RpcError::new(METHOD_NOT_FOUND, format!("method not found: {method}"))
    }

    /// `-32602`: `message` says which parameter and why.
    pub fn invalid_params(message: impl fmt::Display) -> RpcError {
        RpcError::new(INVALID_PARAMS, format!("invalid params: {message}"))
    }

    /// `-32603`: `message` says what failed.
    pub fn internal(message: impl fmt::Display) -> RpcError {
        RpcError::new(INTERNAL_ERROR, format!("internal error: {message}"))
    }

    fn invalid_request(message: impl fmt::Display) -> RpcError {
        RpcError::new(INVALID_REQUEST, format!("invalid request: {message}"))
    }
}

impl fmt::Display for RpcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.message, self.code)
    }
}

/// A line the server cannot take as a message: the error it answers with,
/// and the request's id when one could be read.
#[derive(Clone, Debug, PartialEq)]
pub struct Rejection {
    /// The id the error response carries; `None` leaves it out, as MCP's
    /// schema has an error response do when the id could not be read.
    pub id: Option<Id>,
    /// The error.
    pub error: RpcError,
}

impl Rejection {
    fn without_id(error: RpcError) -> Rejection {
        Rejection { id: None, error }
    }
}

/// One line of input, without its newline, as a message.
///
/// The checks, in order: the line is JSON (else `-32700`); it is one object,
/// not a batch (else `-32600`); a response is recognised by its `result` or
/// `error` and no `method`, and never answered; a present `id` is a string or
/// an integer, `jsonrpc` is `"2.0"`, `method` is a string and `params`, when
/// present and not null, is an object (else `-32600`, with the id once it
/// was read; `-32602` for positional parameters, which no MCP method takes).
/// A request has an `id`; a notification has none. Other members are ignored.
pub fn parse(line: &[u8]) -> Result<Incoming, Rejection> {
    let value: Value = serde_json::from_slice(line).map_err(|e| {
        Rejection::without_id(RpcError::new(PARSE_ERROR, format!("parse error: {e}")))
    })?;
    let mut object = match value {
        Value::Object(object) => object,
        Value::Array(_) => {
            return Err(Rejection::without_id(RpcError::invalid_request(
                "batches are not supported; send one message per line",
            )));
        }
        _ => {
            return Err(Rejection::without_id(RpcError::invalid_request(
                "a message is a JSON object",
            )));
        }
    };
    if !object.contains_key("method")
        && (object.contains_key("result") || object.contains_key("error"))
    {
        return Ok(Incoming::Response(
            object.get("id").and_then(Id::from_value),
        ));
    }
    let id = match object.get("id") {
        None => None,
        Some(value) => Some(Id::from_value(value).ok_or_else(|| {
            Rejection::without_id(RpcError::invalid_request(
                "id must be a string or an integer",
            ))
        })?),
    };
    let refuse = |error: RpcError| Rejection {
        id: id.clone(),
        error,
    };
    if object.get("jsonrpc").and_then(Value::as_str) != Some(VERSION) {
        return Err(refuse(RpcError::invalid_request(
            r#"jsonrpc must be "2.0""#,
        )));
    }
    let method = match object.remove("method") {
        Some(Value::String(method)) => method,
        Some(_) => return Err(refuse(RpcError::invalid_request("method must be a string"))),
        None => return Err(refuse(RpcError::invalid_request("method is missing"))),
    };
    let params = match object.remove("params") {
        None | Some(Value::Null) => None,
        Some(Value::Object(params)) => Some(params),
        Some(Value::Array(_)) => {
            return Err(refuse(RpcError::invalid_params(
                "params must be an object of named parameters",
            )));
        }
        Some(_) => {
            return Err(refuse(RpcError::invalid_request(
                "params must be an object",
            )));
        }
    };
    Ok(match id {
        Some(id) => Incoming::Request(Request { id, method, params }),
        None => Incoming::Notification(Notification { method, params }),
    })
}

#[derive(Serialize)]
struct ResultResponse<'a> {
    jsonrpc: &'static str,
    id: &'a Id,
    result: &'a Value,
}

#[derive(Serialize)]
struct ErrorResponse<'a> {
    jsonrpc: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<&'a Id>,
    error: &'a RpcError,
}

#[derive(Serialize)]
struct OutgoingNotification<'a> {
    jsonrpc: &'static str,
    method: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<&'a Value>,
}

fn line(message: &impl Serialize) -> serde_json::Result<Vec<u8>> {
    let mut line = serde_json::to_vec(message)?;
    line.push(b'\n');
    Ok(line)
}

/// The line of the response to `id` with `result`, newline included.
pub fn result_line(id: &Id, result: &Value) -> serde_json::Result<Vec<u8>> {
    line(&ResultResponse {
        jsonrpc: VERSION,
        id,
        result,
    })
}

/// The line of an error response, newline included; `id` is `None` when the
/// request's id could not be read.
pub fn error_line(id: Option<&Id>, error: &RpcError) -> serde_json::Result<Vec<u8>> {
    line(&ErrorResponse {
        jsonrpc: VERSION,
        id,
        error,
    })
}

/// The line of a notification to the client, newline included.
pub fn notification_line(method: &str, params: Option<&Value>) -> serde_json::Result<Vec<u8>> {
    line(&OutgoingNotification {
        jsonrpc: VERSION,
        method,
        params,
    })
}
