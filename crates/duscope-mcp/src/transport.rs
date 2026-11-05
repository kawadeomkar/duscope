//! The stdio transport: newline-delimited JSON-RPC over a byte stream, which
//! `duscope mcp` gives its stdin and stdout.
//!
//! A reader thread frames the input into lines of at most [`MAX_MESSAGE_BYTES`],
//! parses each one ([`jsonrpc::parse`]), answers what it cannot accept and
//! hands requests and notifications to a [`Handler`]. Every message out goes
//! through one [`Outbox`], which writes and flushes a whole line under its
//! lock, so messages written from several threads never interleave. The crate
//! writes nothing else to its output: logs go through the `log` facade, which
//! the binary points at stderr.

use std::fmt;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};

use log::{debug, warn};
use serde::Serialize;
use serde_json::value::RawValue;
use serde_json::{Value, json};

use crate::jsonrpc::{self, Id, Incoming, Notification, Request, RpcError};

/// The longest message read, newline excluded: 16 MiB. A longer line is
/// refused with `-32600` and skipped without being held in memory.
pub const MAX_MESSAGE_BYTES: usize = 16 * 1024 * 1024;

/// The reader thread's buffer.
const READ_BUFFER_BYTES: usize = 64 * 1024;

/// A line buffer this large is not kept for the next message.
const KEPT_BUFFER_BYTES: usize = 1024 * 1024;

/// The reader thread's name, which panics and debuggers show.
pub const READER_THREAD: &str = "duscope-mcp-reader";

/// What the server does with each message the transport accepts. Its
/// methods run on the reader thread, which reads nothing more until they
/// return.
pub trait Handler {
    /// A request. The handler answers through `responder`, at once or later
    /// from another thread; a responder dropped unanswered answers an
    /// internal error.
    fn request(&mut self, request: Request, responder: Responder);

    /// A notification, which is never answered.
    fn notification(&mut self, notification: Notification);

    /// The input has ended (`Ok`), or reading it or writing to the client
    /// failed (`Err`): the handler finishes the requests it still owes an
    /// answer before [`run`] returns. The default has nothing to finish.
    fn end(&mut self, ended: &io::Result<()>) {
        let _ = ended;
    }
}

/// The output side: every message to the client is written through it, a
/// whole line at a time. Clones share one stream.
///
/// After a write fails (the client closed its end), every later one fails
/// too without writing, since the stream may hold part of a line.
#[derive(Clone)]
pub struct Outbox {
    sink: Arc<Mutex<Sink>>,
}

struct Sink {
    out: Box<dyn Write + Send>,
    failed: Option<io::ErrorKind>,
}

impl Outbox {
    /// An outbox writing to `out`.
    pub fn new(out: impl Write + Send + 'static) -> Outbox {
        Outbox {
            sink: Arc::new(Mutex::new(Sink {
                out: Box::new(out),
                failed: None,
            })),
        }
    }

    /// Sends a notification, such as `notifications/progress`.
    pub fn notify(&self, method: &str, params: Option<&Value>) -> io::Result<()> {
        self.write(&jsonrpc::notification_line(method, params)?)?;
        debug!("sent notification {method}");
        Ok(())
    }

    fn send_result<R: Serialize + ?Sized>(&self, id: &Id, result: &R) -> io::Result<()> {
        let line = jsonrpc::result_line(id, result)?;
        self.write(&line)?;
        debug!("sent the result for request {id} ({} bytes)", line.len());
        Ok(())
    }

    fn send_error(&self, id: Option<&Id>, error: &RpcError) -> io::Result<()> {
        self.write(&jsonrpc::error_line(id, error)?)?;
        match id {
            Some(id) => debug!("sent an error for request {id}: {error}"),
            None => debug!("sent an error: {error}"),
        }
        Ok(())
    }

    /// The sink, locked. A thread that panicked holding it may have left part
    /// of a line on the stream, so the sink then counts as failed.
    fn sink(&self) -> MutexGuard<'_, Sink> {
        self.sink.lock().unwrap_or_else(|poisoned| {
            let mut sink = poisoned.into_inner();
            sink.failed.get_or_insert(io::ErrorKind::Other);
            sink
        })
    }

    fn write(&self, line: &[u8]) -> io::Result<()> {
        let mut guard = self.sink();
        let sink = &mut *guard;
        if let Some(kind) = sink.failed {
            return Err(io::Error::new(
                kind,
                "an earlier write to the client failed",
            ));
        }
        let written = sink.out.write_all(line).and_then(|()| sink.out.flush());
        if let Err(e) = &written {
            sink.failed = Some(e.kind());
        }
        written
    }

    fn failed(&self) -> Option<io::ErrorKind> {
        self.sink().failed
    }
}

impl fmt::Debug for Outbox {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Outbox")
            .field("failed", &self.failed())
            .finish_non_exhaustive()
    }
}

/// The one response a request is owed. It can move to another thread; if it
/// is dropped unanswered, it answers `-32603` so the client is not left
/// waiting.
#[derive(Debug)]
pub struct Responder {
    id: Id,
    outbox: Outbox,
    open: bool,
}

impl Responder {
    fn new(id: Id, outbox: Outbox) -> Responder {
        Responder {
            id,
            outbox,
            open: true,
        }
    }

    /// The request's id.
    pub fn id(&self) -> &Id {
        &self.id
    }

    /// Sends the response: the result, or the error.
    pub fn respond(mut self, outcome: Result<Value, RpcError>) -> io::Result<()> {
        self.open = false;
        match outcome {
            Ok(result) => self.outbox.send_result(&self.id, &result),
            Err(error) => self.outbox.send_error(Some(&self.id), &error),
        }
    }

    /// Sends the response with a result already written as JSON, which goes
    /// out byte for byte, or the error.
    pub fn respond_raw(mut self, outcome: Result<Box<RawValue>, RpcError>) -> io::Result<()> {
        self.open = false;
        match outcome {
            Ok(result) => self.outbox.send_result(&self.id, &*result),
            Err(error) => self.outbox.send_error(Some(&self.id), &error),
        }
    }

    /// Sends a notification about the request ahead of its response, such
    /// as its progress.
    pub fn notify(&self, method: &str, params: Option<&Value>) -> io::Result<()> {
        self.outbox.notify(method, params)
    }

    /// Ends the request with no message at all, as a cancelled request must
    /// end.
    pub fn abandon(mut self) {
        self.open = false;
        debug!("abandoned request {}: nothing is sent", self.id);
    }
}

impl Drop for Responder {
    fn drop(&mut self) {
        if !self.open {
            return;
        }
        warn!(
            "request {} ended without a response; answering with an internal error",
            self.id
        );
        let error = RpcError::internal("the request ended without a response");
        if let Err(e) = self.outbox.send_error(Some(&self.id), &error) {
            debug!("could not answer request {}: {e}", self.id);
        }
    }
}

/// The lines of an input, each at most a limit long.
#[derive(Debug)]
pub struct Reader<R> {
    input: R,
    limit: usize,
    line: Vec<u8>,
}

enum Frame<'a> {
    Line(&'a [u8]),
    Oversized,
}

impl<R: BufRead> Reader<R> {
    /// Lines of `input`, at most [`MAX_MESSAGE_BYTES`] each.
    pub fn new(input: R) -> Reader<R> {
        Reader::with_limit(input, MAX_MESSAGE_BYTES)
    }

    /// Lines of `input`, at most `limit` bytes each (tests and the fuzz
    /// target lower it).
    pub fn with_limit(input: R, limit: usize) -> Reader<R> {
        Reader {
            input,
            limit,
            line: Vec::new(),
        }
    }

    /// The next line without its newline, or `Oversized` once a line longer
    /// than the limit has been read through to its end; `None` at the end of
    /// the input. A last line without a newline still counts.
    fn next_frame(&mut self) -> io::Result<Option<Frame<'_>>> {
        if self.line.capacity() > KEPT_BUFFER_BYTES {
            self.line = Vec::new();
        }
        self.line.clear();
        let mut oversized = false;
        loop {
            let available = match self.input.fill_buf() {
                Ok(available) => available,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            };
            if available.is_empty() {
                return Ok(match (oversized, self.line.is_empty()) {
                    (true, _) => Some(Frame::Oversized),
                    (false, true) => None,
                    (false, false) => Some(Frame::Line(&self.line)),
                });
            }
            let newline = available.iter().position(|&b| b == b'\n');
            let chunk = &available[..newline.unwrap_or(available.len())];
            if !oversized {
                if self.line.len() + chunk.len() > self.limit {
                    oversized = true;
                    self.line = Vec::new();
                } else {
                    self.line.extend_from_slice(chunk);
                }
            }
            let used = newline.map_or(available.len(), |at| at + 1);
            self.input.consume(used);
            if newline.is_some() {
                return Ok(Some(if oversized {
                    Frame::Oversized
                } else {
                    Frame::Line(&self.line)
                }));
            }
        }
    }
}

/// Reads messages until the input ends, handing each request and
/// notification to `handler` and answering what it cannot accept: a line
/// that is not one valid message gets its error response, a response from
/// the client is logged and dropped, and an empty line is skipped.
///
/// Once the input ends, or a read or a write fails, the handler finishes
/// what it owes ([`Handler::end`]) before this returns.
///
/// Returns `Ok` at the end of the input, or the error of a failed read or of
/// a failed write to `outbox` (after which nothing more could reach the
/// client).
pub fn run<R: BufRead, H: Handler>(
    reader: Reader<R>,
    outbox: &Outbox,
    handler: &mut H,
) -> io::Result<()> {
    let ended = read(reader, outbox, handler);
    handler.end(&ended);
    match (ended, outbox.failed()) {
        (Ok(()), Some(kind)) => Err(io::Error::new(kind, "a write to the client failed")),
        (ended, _) => ended,
    }
}

/// [`run`]'s reading, until the input ends or a read or a write fails.
fn read<R: BufRead, H: Handler>(
    mut reader: Reader<R>,
    outbox: &Outbox,
    handler: &mut H,
) -> io::Result<()> {
    let limit = reader.limit;
    while let Some(frame) = reader.next_frame()? {
        match frame {
            Frame::Oversized => {
                warn!("refused a message longer than {limit} bytes");
                let error = RpcError::new(
                    jsonrpc::INVALID_REQUEST,
                    format!("invalid request: the message is longer than {limit} bytes"),
                )
                .with_data(json!({ "max_message_bytes": limit }));
                outbox.send_error(None, &error)?;
            }
            Frame::Line(line) if line.iter().all(|b| matches!(b, b' ' | b'\t' | b'\r')) => {
                debug!("skipped an empty line");
            }
            Frame::Line(line) => match jsonrpc::parse(line) {
                // The client's method names are logged with control
                // characters escaped, so a name cannot forge a log line.
                Ok(Incoming::Request(request)) => {
                    debug!(
                        "received request {} {} ({} bytes)",
                        request.id,
                        request.method.escape_debug(),
                        line.len()
                    );
                    let responder = Responder::new(request.id.clone(), outbox.clone());
                    handler.request(request, responder);
                }
                Ok(Incoming::Notification(notification)) => {
                    debug!(
                        "received notification {}",
                        notification.method.escape_debug()
                    );
                    handler.notification(notification);
                }
                Ok(Incoming::Response(id)) => {
                    let id = id.map_or_else(|| "no id".to_owned(), |id| format!("id {id}"));
                    warn!("ignored a response from the client ({id}): duscope sends no requests");
                }
                Ok(Incoming::PositionalNotification(method)) => {
                    warn!(
                        "ignored notification {}: its params are positional, not named",
                        method.escape_debug()
                    );
                }
                Err(rejection) => {
                    warn!("refused a message: {}", rejection.error);
                    outbox.send_error(rejection.id.as_ref(), &rejection.error)?;
                }
            },
        }
        if let Some(kind) = outbox.failed() {
            return Err(io::Error::new(kind, "a write to the client failed"));
        }
    }
    debug!("the input ended");
    Ok(())
}

/// A running transport: its reader thread and the outbox it answers through.
#[derive(Debug)]
pub struct Transport {
    outbox: Outbox,
    reader: JoinHandle<io::Result<()>>,
}

impl Transport {
    /// Starts the reader thread on `input`, with `handler`, answering on
    /// `output`.
    pub fn start<R, W, H>(input: R, output: W, mut handler: H) -> io::Result<Transport>
    where
        R: Read + Send + 'static,
        W: Write + Send + 'static,
        H: Handler + Send + 'static,
    {
        let outbox = Outbox::new(output);
        let reader_outbox = outbox.clone();
        let reader = thread::Builder::new()
            .name(READER_THREAD.to_owned())
            .spawn(move || {
                let input = BufReader::with_capacity(READ_BUFFER_BYTES, input);
                run(Reader::new(input), &reader_outbox, &mut handler)
            })?;
        Ok(Transport { outbox, reader })
    }

    /// The outbox, for messages sent from outside the handler.
    pub fn outbox(&self) -> &Outbox {
        &self.outbox
    }

    /// Waits for the reader thread: `Ok` once the input has ended.
    pub fn wait(self) -> io::Result<()> {
        match self.reader.join() {
            Ok(ended) => ended,
            Err(panic) => std::panic::resume_unwind(panic),
        }
    }
}
