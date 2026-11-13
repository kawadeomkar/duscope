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
//! A verb that changes what it cannot take back (`clean` acting for
//! `duscope_delete`) first claims the call's point of no return
//! ([`CallSignals::commit`]), and the deadline, a cancellation and an
//! abandoned client each claim the call's stop; the call takes whichever
//! comes first, and never both. A call stopped first moves nothing: its
//! verb finds it stopped and acts on nothing. A call committed first runs
//! to its end and is always answered with its verb's result, after its
//! deadline too: the deadline does not answer it `deadline_exceeded`, and a
//! cancellation of it is ignored, as the protocol allows for a request that
//! can no longer be cancelled.
//!
//! When the input ends, the worker answers the calls already received, then
//! stops. A client whose input has ended may have gone, so from then on
//! none of those calls may pass its point of no return: the running call,
//! unless it is past it already, and each call still queued find their
//! call stopped when they reach it, act on nothing, and are answered with
//! what their verbs did; a call that never reaches one, a dry run or a
//! read, is answered as before.
//!
//! Once a write to the client has failed, from any thread, nothing more
//! can reach it: the worker starts no request still queued, dropping
//! each unanswered, and a running call whose progress cannot be sent is
//! cancelled, as is the running call when the reader stops on the failure
//! (or meets the end of the input after it), unless it is past its point of
//! no return. The worker stops once the running call's verb has returned.
//!
//! A panic ends the process in a release build, which aborts on one
//! (DEV-87): nothing unwinds, so neither the worker's answer to a verb that
//! panicked nor the internal error a dropped responder sends is ever written.
//! The binary's panic hook calls [`answer_before_abort`] first, in that build
//! alone. The running call is stopped, unless it is past its point of no
//! return, and answered `-32603`, which says, for a call past that point,
//! that some of what it asked for may be done; then each request queued
//! behind it is answered in order, one the reader answered with that answer
//! and a call `-32603`, since it never runs. The hook waits on no lock for
//! more than a moment, since the thread that panicked may hold it: a response
//! whose lock stays held is not sent, and once a response cannot be written,
//! none after it is. A request read once the queue has been answered, or the
//! one the reader was handling if it panicked, gets no answer. Where panics
//! unwind, as in the tests, the server survives a verb's panic: the call is
//! answered `-32603`, saying the same of a call past its point of no return,
//! and the next one runs.

use std::collections::VecDeque;
use std::fmt;
use std::io;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, TryLockError, Weak};
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
use crate::transport::{Responder, lock_briefly};

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
    /// Where the call stands against its point of no return: [`RUNNING`],
    /// [`COMMITTED`] or [`STOPPED`], each change claimed by one
    /// compare-and-swap so that the verb and the worker cannot both win.
    stage: Arc<AtomicU8>,
}

/// The worker of the server this process runs, for [`answer_before_abort`]:
/// the last one started. The binary runs one server; the tests start many.
static SERVING: Mutex<Weak<Shared>> = Mutex::new(Weak::new());

/// A call neither committed nor stopped.
const RUNNING: u8 = 0;
/// A call whose verb has passed its point of no return.
const COMMITTED: u8 = 1;
/// A call cancelled, past its deadline, or whose client has gone.
const STOPPED: u8 = 2;

impl CallSignals {
    /// Claims the call's point of no return, for a verb about to change
    /// what it cannot take back: true when the call is committed, now or
    /// before, and is then answered with the verb's result whatever comes
    /// after; false when the call was stopped first (cancelled, past its
    /// deadline, its client gone), and the verb must change nothing.
    pub fn commit(&self) -> bool {
        self.claim(COMMITTED)
    }

    /// Whether the call's verb has passed its point of no return.
    pub fn committed(&self) -> bool {
        self.stage.load(Ordering::Acquire) == COMMITTED
    }

    /// Stops the call unless its verb has passed its point of no return,
    /// setting [`cancel`](CallSignals::cancel): true when the call is
    /// stopped, now or before; false when it is committed, and runs to its
    /// end.
    pub(crate) fn stop(&self) -> bool {
        let stopped = self.claim(STOPPED);
        if stopped {
            self.cancel.store(true, Ordering::Relaxed);
        }
        stopped
    }

    /// Stops the call's point of no return, not the call: its verb runs on,
    /// and the call is answered with its result, but a [`commit`] now fails
    /// and the verb changes nothing. True when it is stopped, now or before;
    /// false when the call is committed, and runs to its end.
    ///
    /// [`commit`]: CallSignals::commit
    fn forbid_commit(&self) -> bool {
        self.claim(STOPPED)
    }

    /// Moves a running call to `stage`, by one compare-and-swap: whether
    /// the call is at `stage` now, by this claim or an earlier one.
    fn claim(&self, stage: u8) -> bool {
        self.stage
            .compare_exchange(RUNNING, stage, Ordering::AcqRel, Ordering::Acquire)
            .map_or_else(|now| now == stage, |_| true)
    }
}

/// A request waiting its turn.
enum Job {
    /// A request the reader has answered, whose answer waits for the
    /// requests before it.
    Answer(Responder, Result<Box<RawValue>, RpcError>),
    /// A tool call.
    Call(Arc<Call>),
}

impl Job {
    /// Whether a write to the client has failed, so that the job's answer
    /// could not reach it. A cancelled call owes none.
    fn output_failed(&self) -> bool {
        match self {
            Job::Answer(responder, _) => responder.output_failed(),
            Job::Call(call) => call
                .responder()
                .as_ref()
                .is_some_and(Responder::output_failed),
        }
    }

    /// Ends the job unanswered, without starting it.
    fn drop_unanswered(self) {
        match self {
            Job::Answer(responder, _) => {
                debug!("dropped the answer to request {}", responder.id());
                responder.abandon();
            }
            Job::Call(call) => {
                debug!("dropped tools/call {} before it ran", call.id);
                // Never run, so never committed: it stops.
                call.cancel();
            }
        }
    }
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

    /// The tool the request names.
    fn tool(&self) -> &str {
        self.params
            .as_ref()
            .and_then(|params| params.get("name"))
            .and_then(Value::as_str)
            .unwrap_or("the tool")
    }

    /// For the panic hook: the response, if the call still owes one, taken
    /// with the call stopped, unless its verb is past its point of no return
    /// (`true` beside the response then), so that a verb still running acts
    /// on nothing once its call is answered. `Err` while the response's lock
    /// stays held.
    fn take_before_abort(&self) -> Result<Option<(Responder, bool)>, ()> {
        let mut owed = match lock_briefly(&self.responder) {
            Ok(owed) => owed,
            Err(TryLockError::Poisoned(owed)) => owed.into_inner(),
            Err(TryLockError::WouldBlock) => return Err(()),
        };
        Ok(owed
            .take()
            .map(|responder| (responder, !self.signals.stop())))
    }

    /// Ends the call without a response and tells its verb to stop, unless
    /// its verb is past its point of no return: whether it stopped. The
    /// response is held while the call is stopped, so a verb that stops can
    /// no longer have it answered; a committed call keeps it, to be
    /// answered with its result.
    fn cancel(&self) -> bool {
        let mut owed = self.responder();
        if !self.signals.stop() {
            return false;
        }
        if let Some(responder) = owed.take() {
            responder.abandon();
        }
        true
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

    /// [`answer_before_abort`], for this worker.
    fn answer_before_abort(&self) {
        let mut state = match lock_briefly(&self.state) {
            Ok(state) => state,
            Err(TryLockError::Poisoned(state)) => state.into_inner(),
            Err(TryLockError::WouldBlock) => {
                warn!("the MCP worker's queue stayed locked: its requests go unanswered");
                return;
            }
        };
        // Held to the end, so that nothing joins the queue while it is
        // answered.
        let running = state.running.clone();
        let queued: Vec<Job> = state.queue.drain(..).collect();
        let on_call_thread = thread::current().name() == Some(CALL_THREAD);
        let mut owed: Vec<(Responder, Result<Box<RawValue>, RpcError>)> = Vec::new();
        if let Some(call) = running {
            match call.take_before_abort() {
                Ok(Some((responder, committed))) => {
                    let tool = call.tool();
                    let subject = if on_call_thread {
                        format!("{tool}'s verb panicked")
                    } else {
                        format!("the server panicked while {tool} ran")
                    };
                    let message =
                        format!("{}; the server is stopping", panicked(&subject, committed));
                    owed.push((responder, Err(RpcError::internal(message))));
                }
                Ok(None) => debug!("request {} was answered already", call.id),
                Err(()) => warn!(
                    "request {}'s response stayed locked: it goes unanswered",
                    call.id
                ),
            }
        }
        for job in queued {
            match job {
                Job::Answer(responder, outcome) => owed.push((responder, outcome)),
                Job::Call(call) => match call.take_before_abort() {
                    Ok(Some((responder, _))) => {
                        let message = format!(
                            "the server is stopping after a panic: {} did not run",
                            call.tool()
                        );
                        owed.push((responder, Err(RpcError::internal(message))));
                    }
                    Ok(None) => debug!("request {} was cancelled", call.id),
                    Err(()) => warn!(
                        "request {}'s response stayed locked: it goes unanswered",
                        call.id
                    ),
                },
            }
        }
        let mut owed = owed.into_iter();
        for (responder, outcome) in owed.by_ref() {
            let id = responder.id().clone();
            if let Err(e) = responder.respond_before_abort(outcome) {
                warn!("could not answer request {id} before the server stops: {e}");
                break;
            }
        }
        // Nothing more can be written, so nothing more is tried: a responder
        // dropped unanswered would wait for the output.
        for (responder, _) in owed {
            responder.abandon();
        }
        drop(state);
    }
}

/// Answers what the server this process runs still owes, for the panic hook
/// of a build that aborts on a panic, before the process ends: the running
/// call `-32603`, then each request queued behind it, in order (the
/// module's documentation says how). It waits on no lock for more than a
/// moment, and does nothing when no server runs. A build that unwinds must
/// not call it: there the server survives the panic and the queued requests
/// are answered as usual.
pub fn answer_before_abort() {
    let serving = match lock_briefly(&SERVING) {
        Ok(serving) => serving.upgrade(),
        Err(TryLockError::Poisoned(serving)) => serving.into_inner().upgrade(),
        Err(TryLockError::WouldBlock) => None,
    };
    if let Some(shared) = serving {
        shared.answer_before_abort();
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
        *lock(&SERVING) = Arc::downgrade(&shared);
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
    /// call that stopped: the running call past its point of no return does
    /// not, and is answered with its result.
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
            if call.cancel() {
                debug!("request {id} is told to stop");
                return true;
            }
            info!(
                "request {id} is past its point of no return: the cancellation is ignored, and \
                 the call is answered with its result"
            );
        }
        false
    }

    /// Answers the requests already queued, then stops the worker thread.
    /// The client's input has ended, so it may have gone: neither the
    /// running call nor a queued one may pass its point of no return from
    /// now on, and a verb that tries acts on nothing. Each is still run and
    /// answered; the running call past its point of no return runs to its
    /// end.
    pub(crate) fn finish(mut self) {
        let state = self.shared.state();
        let queued = state.queue.iter().filter_map(|job| match job {
            Job::Call(call) => Some(call),
            Job::Answer(..) => None,
        });
        for call in state.running.iter().chain(queued) {
            if !call.signals.forbid_commit() {
                debug!(
                    "request {} is past its point of no return: it runs to its end",
                    call.id
                );
            }
        }
        drop(state);
        debug!("the input ended: no call left may pass its point of no return");
        self.stop();
    }

    /// Drops the queued requests unanswered, cancels the running call
    /// unless it is past its point of no return, and stops the worker
    /// thread once its verb has returned.
    pub(crate) fn abort(mut self) {
        let mut state = self.shared.state();
        state.closed = true;
        let dropped: Vec<Job> = state.queue.drain(..).collect();
        let running = state.running.clone();
        drop(state);
        for job in dropped {
            job.drop_unanswered();
        }
        if let Some(call) = running
            && !call.cancel()
        {
            debug!(
                "request {} is past its point of no return: it runs to its end",
                call.id
            );
        }
        self.stop();
    }

    /// Closes the queue and waits for the worker thread, which first
    /// answers what is left in it.
    fn stop(&mut self) {
        self.shared.state().closed = true;
        self.shared.wake.notify_all();
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            warn!("the MCP worker thread panicked");
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The worker thread: takes each job in turn until the queue is closed and
/// empty. Once a write to the client has failed, it starts none: each is
/// dropped unanswered.
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
            job if job.output_failed() => job.drop_unanswered(),
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
            // A call past its point of no return is answered with what its
            // verb did, however long it took.
            let late = due.is_some_and(|due| Instant::now() >= due);
            let result = if late && !call.signals.committed() {
                self.expired(prepared, deadline)
            } else {
                match outcome {
                    Ok(outcome) => prepared.result(outcome),
                    Err(_) => Err(RpcError::internal(panicked(
                        &format!("{}'s verb panicked", prepared.tool().name()),
                        call.signals.committed(),
                    ))),
                }
            };
            self.answer(call, result);
        });
    }

    /// Waits for the call's verb to return, sending the call's progress
    /// meanwhile, and answers the call `deadline_exceeded` at `due` unless
    /// its verb is past its point of no return by then.
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
                // Answered, nothing more goes out about the call; or past its
                // point of no return, still reporting until its verb returns.
                if self.expire(call, prepared, deadline) {
                    reporter = None;
                }
                due = None;
                continue;
            }
            if let Some(reporter) = &mut reporter
                && now >= tick
            {
                report(call, reporter);
                while tick <= now {
                    tick += every;
                }
            }
        }
    }

    /// The call ran past its deadline: tells its verb to stop, and answers
    /// it `deadline_exceeded`; whether it did. A call past its point of no
    /// return is neither stopped nor answered here: it is answered with its
    /// result once its verb returns. The answer is held while the call is
    /// stopped, so the verb, once stopped, cannot have the call answered.
    fn expire(&self, call: &Call, prepared: &Prepared<'_>, deadline: Duration) -> bool {
        let mut owed = call.responder();
        if !call.signals.stop() {
            info!(
                "request {} ran past its deadline of {}, past its point of no return: it is \
                 answered with its result once its verb returns",
                call.id,
                spoken(deadline)
            );
            return false;
        }
        let responder = owed.take();
        drop(owed);
        if let Some(responder) = responder {
            info!(
                "request {} ran past its deadline of {}",
                call.id,
                spoken(deadline)
            );
            let result = self.expired(prepared, deadline);
            send(responder, self.written(call.revision, result));
        }
        true
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
/// it is still owed an answer. A progress that cannot be sent means its
/// answer could not be either: the call is cancelled, unless it is past its
/// point of no return.
fn report(call: &Call, reporter: &mut Reporter) {
    let owed = call.responder();
    let Some(responder) = owed.as_ref() else {
        return;
    };
    let Some(params) = reporter.next() else {
        return;
    };
    let Err(e) = responder.notify(PROGRESS, Some(&params)) else {
        return;
    };
    // The cancellation takes the responder.
    drop(owed);
    if call.cancel() {
        info!(
            "request {} is told to stop: its progress could not be sent ({e})",
            call.id
        );
    } else {
        info!(
            "request {}'s progress could not be sent ({e}); past its point of no return, it \
             runs to its end",
            call.id
        );
    }
}

/// Sends a response. A failed write stops the transport, which reports it.
fn send(responder: Responder, outcome: Result<Box<RawValue>, RpcError>) {
    let id = responder.id().clone();
    if let Err(e) = responder.respond_raw(outcome) {
        debug!("could not answer request {id}: {e}");
    }
}

/// Why a call failed when its verb, or the server, panicked: `subject`, and,
/// for a call past its point of no return, that some of what it asked for
/// may be done, which nothing can now report.
fn panicked(subject: &str, committed: bool) -> String {
    if committed {
        format!(
            "{subject} past its point of no return: some of what the call asked for may be done"
        )
    } else {
        subject.to_owned()
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
