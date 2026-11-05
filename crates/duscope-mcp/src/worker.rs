//! The worker: one thread that runs the tool calls one at a time, in the
//! order they came, while the reader thread goes on reading.
//!
//! The reader answers `ping`, `server/discover` and `tools/list` itself, at
//! once, whatever a call is doing, and reads `notifications/cancelled`.
//! Every other request waits in one FIFO queue: a `tools/call` waits for the
//! calls before it, and so does the answer to any other request
//! (`initialize`, a method there is none of), so the requests other than
//! those three are answered in the order they came. Indexes are immutable
//! snapshots opened read-only, so a call reads without locking anything; two
//! `duscope_scan` calls of one root run one after the other, and the second
//! finds the first's scan under its `max_age`.
//!
//! A call's verb runs on a thread of its own while the worker watches it:
//!
//! - **Progress.** Every [`Timing::progress_every`] (two seconds), a call
//!   whose request named a `progressToken` is sent the entries its verb has
//!   counted so far ([`crate::progress`]): the entries a scan's walk has
//!   read.
//! - **Deadline.** A call still running at its tool's deadline
//!   ([`Tool::deadline`](crate::tools::Tool::deadline): [`SCAN_DEADLINE`]
//!   for `duscope_scan`, [`DEADLINE`] for the others) is answered
//!   `deadline_exceeded`, with `isError: true`, and its cancellation flag is
//!   set, which stops a scan's walk within one directory per walk worker. A
//!   verb that returns after its deadline has passed is answered the same
//!   way. `duscope_query`'s statement has the sandbox's own 5-second
//!   deadline, which its verb answers `sql_timeout`, as the command line
//!   does; the 30 seconds stand behind it.
//! - **Cancellation.** `notifications/cancelled` naming a queued call takes
//!   it out of the queue; naming the running call, it sets the same flag,
//!   which `SIGINT` sets for a scan run from a shell. Either way the request
//!   gets no response, as the protocol asks.
//!
//! The verbs that read saved scans have no point at which to stop, so one
//! cancelled or past its deadline runs to its end and its result is
//! dropped. The next call starts only once the last call's verb has
//! returned, so no two verbs ever run at once.
//!
//! When the input ends, the worker answers the calls already received, then
//! stops; when a write to the client fails, it drops the queue, cancels the
//! running call, and stops once its verb has returned.

use std::collections::VecDeque;
use std::fmt;
use std::io;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use log::{debug, info, warn};
use serde_json::value::RawValue;
use serde_json::{Map, Value};

use crate::discover::ServerConfig;
use crate::jsonrpc::{Id, Request, RpcError};
use crate::progress::{self, PROGRESS, PROGRESS_EVERY, Reporter};
use crate::roots::Refusal;
use crate::server::{self, Revision, Tools};
use crate::tools::{CallResult, Context, Prepared};
use crate::transport::Responder;

/// How long a tool call may run unless its tool says otherwise: 30 seconds.
pub const DEADLINE: Duration = Duration::from_secs(30);

/// How long `duscope_scan` may run: 10 minutes.
pub const SCAN_DEADLINE: Duration = Duration::from_secs(10 * 60);

/// The worker thread's name, which panics and debuggers show.
pub const WORKER_THREAD: &str = "duscope-mcp-worker";

/// The name of the thread a call's verb runs on.
pub const CALL_THREAD: &str = "duscope-mcp-call";

/// The shortest progress interval: a shorter one would keep the worker
/// spinning.
const MIN_PROGRESS_EVERY: Duration = Duration::from_millis(1);

/// When a running call reports its progress, and how long a call may run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timing {
    /// How often a running call sends its progress: [`PROGRESS_EVERY`].
    pub progress_every: Duration,
    /// Every tool's deadline, in place of its own; `None`, as the server
    /// runs, keeps each tool's. The tests shorten it.
    pub deadline: Option<Duration>,
}

impl Default for Timing {
    fn default() -> Timing {
        Timing {
            progress_every: PROGRESS_EVERY,
            deadline: None,
        }
    }
}

/// What a running call shares with its verb.
#[derive(Debug, Clone, Default)]
pub struct CallSignals {
    /// Set when the call is cancelled or runs past its deadline: a scan's
    /// walk polls it before each directory and stops.
    pub cancel: Arc<AtomicBool>,
    /// The entries the verb has counted so far, which the call's progress
    /// reports: a scan adds each entry its walk reads.
    pub entries: Arc<AtomicU64>,
}

/// A request waiting its turn.
enum Job {
    /// A request the reader has answered, whose answer waits for the
    /// requests before it.
    Answer(Responder, Result<Box<RawValue>, RpcError>),
    /// A tool call.
    Call(Arc<Call>),
}

/// A `tools/call`: the request, how it is answered, and what its verb
/// shares with the worker.
struct Call {
    id: Id,
    params: Option<Map<String, Value>>,
    revision: Revision,
    /// `None` once the call is answered, or abandoned for a cancellation.
    responder: Mutex<Option<Responder>>,
    signals: CallSignals,
}

impl Call {
    fn responder(&self) -> MutexGuard<'_, Option<Responder>> {
        lock(&self.responder)
    }

    /// Whether the call still owes a response.
    fn is_open(&self) -> bool {
        self.responder().is_some()
    }

    /// Ends the call without a response, then tells its verb to stop. In
    /// that order: a verb that stops can then no longer have it answered.
    fn cancel(&self) {
        if let Some(responder) = self.responder().take() {
            responder.abandon();
        }
        self.signals.cancel.store(true, Ordering::Relaxed);
    }
}

/// The queue and the worker's state, under one lock.
#[derive(Default)]
struct State {
    queue: VecDeque<Job>,
    /// The call the worker is running, for a cancellation to find.
    running: Option<Arc<Call>>,
    /// Whether the worker is busy with a job taken from the queue.
    busy: bool,
    /// No more jobs come: the worker stops once the queue is empty.
    closed: bool,
}

struct Shared {
    state: Mutex<State>,
    /// Signalled when a job is queued or the queue is closed.
    wake: Condvar,
}

impl Shared {
    fn state(&self) -> MutexGuard<'_, State> {
        lock(&self.state)
    }
}

/// What the worker runs a call with.
struct Setup {
    tools: Arc<Tools>,
    config: ServerConfig,
    timing: Timing,
}

/// The worker thread and its queue.
pub(crate) struct Worker {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl fmt::Debug for Worker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = self.shared.state();
        f.debug_struct("Worker")
            .field("queued", &state.queue.len())
            .field("busy", &state.busy)
            .field("closed", &state.closed)
            .finish_non_exhaustive()
    }
}

impl Worker {
    /// Starts the worker thread, which runs calls with `tools`, answers in
    /// the revisions `config` describes, and reports and times calls as
    /// `timing` says.
    pub(crate) fn start(
        tools: Arc<Tools>,
        config: ServerConfig,
        timing: Timing,
    ) -> io::Result<Worker> {
        let shared = Arc::new(Shared {
            state: Mutex::new(State::default()),
            wake: Condvar::new(),
        });
        let setup = Setup {
            tools,
            config,
            timing,
        };
        let worker_shared = Arc::clone(&shared);
        let thread = thread::Builder::new()
            .name(WORKER_THREAD.to_owned())
            .spawn(move || work(&worker_shared, &setup))?;
        Ok(Worker {
            shared,
            thread: Some(thread),
        })
    }

    /// Queues the tool call `request`, which speaks `revision`, behind the
    /// requests before it.
    pub(crate) fn call(&self, request: Request, revision: Revision, responder: Responder) {
        let call = Arc::new(Call {
            id: request.id,
            params: request.params,
            revision,
            responder: Mutex::new(Some(responder)),
            signals: CallSignals::default(),
        });
        let mut state = self.shared.state();
        let ahead = state.queue.len() + usize::from(state.busy);
        debug!("queued tools/call {} behind {ahead}", call.id);
        state.queue.push_back(Job::Call(call));
        drop(state);
        self.shared.wake.notify_one();
    }

    /// Sends a request's answer, `outcome`, now when nothing is ahead of it
    /// in the queue, else once the requests ahead have been answered.
    pub(crate) fn answer(&self, responder: Responder, outcome: Result<Box<RawValue>, RpcError>) {
        let mut state = self.shared.state();
        if state.queue.is_empty() && !state.busy {
            // Only the reader queues, and the reader is here: nothing can
            // come ahead of this answer before it is sent.
            drop(state);
            send(responder, outcome);
            return;
        }
        debug!(
            "request {} waits for the requests before it",
            responder.id()
        );
        state.queue.push_back(Job::Answer(responder, outcome));
        drop(state);
        self.shared.wake.notify_one();
    }

    /// Cancels the call `id` names: a queued one leaves the queue, the
    /// running one is told to stop; neither is answered. Whether `id` named a
    /// call.
    pub(crate) fn cancel(&self, id: &Id) -> bool {
        let mut state = self.shared.state();
        let queued = state
            .queue
            .iter()
            .position(|job| matches!(job, Job::Call(call) if call.id == *id));
        if let Some(Job::Call(call)) = queued.and_then(|at| state.queue.remove(at)) {
            drop(state);
            debug!("request {id} leaves the queue");
            call.cancel();
            return true;
        }
        if let Some(call) = state.running.clone().filter(|call| call.id == *id) {
            drop(state);
            debug!("request {id} is told to stop");
            call.cancel();
            return true;
        }
        false
    }

    /// Answers the requests already queued, then stops the worker thread.
    pub(crate) fn finish(mut self) {
        self.stop();
    }

    /// Drops the queued requests unanswered, cancels the running call, and
    /// stops the worker thread once its verb has returned.
    pub(crate) fn abort(mut self) {
        let mut state = self.shared.state();
        state.closed = true;
        let dropped: Vec<Job> = state.queue.drain(..).collect();
        let running = state.running.clone();
        drop(state);
        for job in dropped {
            match job {
                Job::Answer(responder, _) => responder.abandon(),
                Job::Call(call) => call.cancel(),
            }
        }
        if let Some(call) = running {
            call.cancel();
        }
        self.stop();
    }

    /// Closes the queue and waits for the worker thread, which first
    /// answers what is left in it.
    fn stop(&mut self) {
        self.shared.state().closed = true;
        self.shared.wake.notify_all();
        if let Some(thread) = self.thread.take() {
            if thread.join().is_err() {
                warn!("the MCP worker thread panicked");
            }
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The worker thread: takes each job in turn until the queue is closed and
/// empty.
fn work(shared: &Shared, setup: &Setup) {
    loop {
        let job = {
            let mut state = shared.state();
            loop {
                if let Some(job) = state.queue.pop_front() {
                    if let Job::Call(call) = &job {
                        state.running = Some(Arc::clone(call));
                    }
                    state.busy = true;
                    break job;
                }
                if state.closed {
                    return;
                }
                state = shared
                    .wake
                    .wait(state)
                    .unwrap_or_else(PoisonError::into_inner);
            }
        };
        match job {
            Job::Answer(responder, outcome) => send(responder, outcome),
            Job::Call(call) => setup.run(&call),
        }
        let mut state = shared.state();
        state.running = None;
        state.busy = false;
    }
}

impl Setup {
    /// Runs `call`'s verb on a thread of its own, watches it, and answers
    /// the call, unless it was cancelled.
    fn run(&self, call: &Call) {
        if !call.is_open() {
            debug!("request {} was cancelled before it ran", call.id);
            return;
        }
        let context = Context {
            roots: &self.tools.roots,
            index_dir: &self.tools.index_dir,
        };
        let prepared = match self.tools.registry.prepare(call.params.as_ref(), &context) {
            Ok(prepared) => prepared,
            Err(error) => return self.answer(call, Err(error)),
        };
        let deadline = self
            .timing
            .deadline
            .unwrap_or_else(|| prepared.tool().deadline());
        let started = Instant::now();
        // A deadline past what an instant can hold is none.
        let due = started.checked_add(deadline);
        let runner = self.tools.runner.as_ref();
        let (done, verb_done) = mpsc::channel::<()>();
        thread::scope(|scope| {
            let prepared = &prepared;
            let spawned = thread::Builder::new()
                .name(CALL_THREAD.to_owned())
                .spawn_scoped(scope, move || {
                    let outcome = prepared.run(runner, &call.signals);
                    // The worker's wait ends as `done` goes, here or as a
                    // panic unwinds.
                    drop(done);
                    outcome
                });
            let verb = match spawned {
                Ok(verb) => verb,
                Err(e) => {
                    let error = RpcError::internal(format!("cannot start the call's thread: {e}"));
                    return self.answer(call, Err(error));
                }
            };
            self.watch(call, prepared, deadline, due, started, &verb_done);
            let outcome = verb.join();
            if !call.is_open() {
                debug!("request {} ended: its answer was sent or not owed", call.id);
                return;
            }
            let result = if due.is_some_and(|due| Instant::now() >= due) {
                self.expired(prepared, deadline)
            } else {
                match outcome {
                    Ok(outcome) => prepared.result(outcome),
                    Err(_) => Err(RpcError::internal(format!(
                        "{}'s verb panicked",
                        prepared.tool().name()
                    ))),
                }
            };
            self.answer(call, result);
        });
    }

    /// Waits for the call's verb to return, sending the call's progress
    /// meanwhile, and answers the call `deadline_exceeded` at `due`.
    fn watch(
        &self,
        call: &Call,
        prepared: &Prepared<'_>,
        deadline: Duration,
        mut due: Option<Instant>,
        started: Instant,
        verb_done: &mpsc::Receiver<()>,
    ) {
        let every = self.timing.progress_every.max(MIN_PROGRESS_EVERY);
        let mut reporter = progress::token(call.params.as_ref())
            .map(|token| Reporter::new(token, Arc::clone(&call.signals.entries)));
        let mut tick = started + every;
        loop {
            let wake = match (&reporter, due) {
                (Some(_), Some(due)) => Some(tick.min(due)),
                (Some(_), None) => Some(tick),
                (None, due) => due,
            };
            let waited = match wake {
                Some(at) => verb_done.recv_timeout(at.saturating_duration_since(Instant::now())),
                None => verb_done.recv().map_err(|_| RecvTimeoutError::Disconnected),
            };
            // Nothing is ever sent: the channel disconnects as the verb ends.
            if !matches!(waited, Err(RecvTimeoutError::Timeout)) {
                return;
            }
            let now = Instant::now();
            if due.is_some_and(|due| now >= due) {
                self.expire(call, prepared, deadline);
                // Answered: nothing more goes out about the call.
                due = None;
                reporter = None;
                continue;
            }
            if let Some(reporter) = &mut reporter {
                if now >= tick {
                    report(call, reporter);
                    while tick <= now {
                        tick += every;
                    }
                }
            }
        }
    }

    /// The call ran past its deadline: tells its verb to stop, and answers
    /// it `deadline_exceeded`. The answer is taken first, so the verb, once
    /// stopped, cannot have the call answered.
    fn expire(&self, call: &Call, prepared: &Prepared<'_>, deadline: Duration) {
        let responder = call.responder().take();
        call.signals.cancel.store(true, Ordering::Relaxed);
        if let Some(responder) = responder {
            info!(
                "request {} ran past its deadline of {}",
                call.id,
                spoken(deadline)
            );
            let result = self.expired(prepared, deadline);
            send(responder, self.written(call.revision, result));
        }
    }

    /// The result of a call past its deadline: `deadline_exceeded`, as its
    /// verb would write the error.
    fn expired(&self, prepared: &Prepared<'_>, deadline: Duration) -> Result<CallResult, RpcError> {
        let tool = prepared.tool().name();
        let command = prepared
            .verb()
            .map_or_else(|| "duscope".to_owned(), |verb| format!("duscope {verb}"));
        let refusal = Refusal {
            code: "deadline_exceeded",
            message: format!("{tool} ran past its deadline of {}", spoken(deadline)),
            hint: format!(
                "Call {tool} again on a smaller directory, or run {command} in a terminal, \
                 where no deadline applies"
            ),
        };
        prepared.result(self.tools.runner.refuse(&refusal))
    }

    /// Answers `call` with `result`, unless it was answered or cancelled.
    fn answer(&self, call: &Call, result: Result<CallResult, RpcError>) {
        let responder = call.responder().take();
        let Some(responder) = responder else {
            return;
        };
        match &result {
            Ok(result) if result.is_error() => {
                debug!("tools/call {} answered with an error", call.id);
            }
            Ok(_) => debug!("tools/call {} answered", call.id),
            Err(error) => debug!("tools/call {} failed: {error}", call.id),
        }
        send(responder, self.written(call.revision, result));
    }

    /// `result` as `revision` writes it.
    fn written(
        &self,
        revision: Revision,
        result: Result<CallResult, RpcError>,
    ) -> Result<Box<RawValue>, RpcError> {
        result.and_then(|result| server::finish(&self.config, revision, &result))
    }
}

/// Sends the call's progress, when its count has grown since the last and
/// it is still owed an answer.
fn report(call: &Call, reporter: &mut Reporter) {
    let responder = call.responder();
    let Some(responder) = responder.as_ref() else {
        return;
    };
    if let Some(params) = reporter.next() {
        if let Err(e) = responder.notify(PROGRESS, Some(&params)) {
            debug!("could not send the progress of request {}: {e}", call.id);
        }
    }
}

/// Sends a response. A failed write stops the transport, which reports it.
fn send(responder: Responder, outcome: Result<Box<RawValue>, RpcError>) {
    let id = responder.id().clone();
    if let Err(e) = responder.respond_raw(outcome) {
        debug!("could not answer request {id}: {e}");
    }
}

/// `duration` in words: "10 minutes", "30 seconds", or milliseconds below a
/// whole second.
fn spoken(duration: Duration) -> String {
    let seconds = duration.as_secs();
    let plural = |n: u64| if n == 1 { "" } else { "s" };
    if seconds >= 60 && seconds % 60 == 0 && duration.subsec_nanos() == 0 {
        let minutes = seconds / 60;
        format!("{minutes} minute{}", plural(minutes))
    } else if seconds > 0 && duration.subsec_nanos() == 0 {
        format!("{seconds} second{}", plural(seconds))
    } else {
        format!("{} ms", duration.as_millis())
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests;
