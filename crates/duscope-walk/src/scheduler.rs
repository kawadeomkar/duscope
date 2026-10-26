// MIT License
//
// Copyright (c) 2019 Sebastian Thiel
//
// Permission is hereby granted, free of charge, to any person obtaining a copy
// of this software and associated documentation files (the "Software"), to deal
// in the Software without restriction, including without limitation the rights
// to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
// copies of the Software, and to permit persons to whom the Software is
// furnished to do so, subject to the following conditions:
//
// The above copyright notice and this permission notice shall be included in all
// copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
// IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
// OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
// SOFTWARE.

//! The work-stealing worker pool, vendored from dua-core: dua-cli's
//! `crates/dua-lib/src/lib.rs` at commit
//! 467f3ec158a8bdc3fd4f79d7d885841beacf6ea3 (dua-core 5.0.1), under the MIT
//! licence above. `VENDOR.md` has its row and the sync procedure.
//!
//! Every worker owns a LIFO deque, so it stays depth-first; an idle worker
//! takes new jobs from the shared injector first, then its own newest job,
//! then steals the oldest job of a peer. Each successful thief wakes one more
//! idle worker, so concurrency ramps up only while work remains stealable.
//!
//! Upstream ties the pool to its own directory reads; duscope keeps the pool
//! and owns the reads (D21, D33), so this file holds upstream's `PoolShared`,
//! `Pool`, `start_pool`, `worker_loop`, `find_job`, `schedule_jobs` and the
//! two wake functions. Their logic is unchanged; the changes, each marked
//! `duscope:` below, are these:
//!
//! - The pool is generic over its job and event types, named `Job` and `Event`
//!   after upstream's enums so the function bodies read as upstream's. The
//!   walk's own state (upstream's `active_roots`, `order`, `options` and
//!   `next_directory_id`) and its jobs live in `lib.rs`.
//! - Workers are scoped threads, so they can borrow the provider and the
//!   options; they are named `duscope-walk-<n>`, and a failed spawn is an error
//!   rather than a panic.
//! - `Pool::finish` shuts the pool down as upstream's `Drop` does and returns
//!   the first worker panic instead of discarding it; `inject`, `any_finished`
//!   and `PoolShared::stop` give the walk what upstream's walk reached through
//!   the struct's fields.
//! - The deque and parker come from the `crossbeam-deque` and `crossbeam-utils`
//!   crates rather than the `crossbeam` umbrella.

use crossbeam_deque::{Injector, Steal, Stealer, Worker};
use crossbeam_utils::sync::{Parker, Unparker};
use std::{
    io,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering as AtomicOrdering},
        mpsc::{Receiver, SyncSender, sync_channel},
    },
    thread::{self, Scope, ScopedJoinHandle},
};

// duscope: generic over `Job` and `Event`; the walk-specific fields moved to the walk.
pub(crate) struct PoolShared<Job, Event> {
    /// Shared queue for the walk's first job.
    injector: Injector<Job>,
    stealers: Vec<Stealer<Job>>,
    stop: AtomicBool,
    pub(crate) events: SyncSender<Event>,
    /// Handles used to wake workers, indexed by worker number.
    unparkers: Vec<Unparker>,
    /// Whether each worker has announced that it is idle, indexed like `unparkers`.
    /// `wake_worker` atomically claims one idle worker before unparking it.
    idle: Vec<AtomicBool>,
    /// A round-robin cursor for the first idle worker to inspect.
    next_wake: AtomicUsize,
}

// duscope: scoped join handles.
pub(crate) struct Pool<'scope, Job, Event> {
    shared: Arc<PoolShared<Job, Event>>,
    pub(crate) events: Receiver<Event>,
    handles: Vec<ScopedJoinHandle<'scope, ()>>,
}

impl<Job, Event> PoolShared<Job, Event> {
    /// Wake one worker that has announced it is idle.
    fn wake_worker(&self) {
        let len = self.idle.len();
        // This cursor only distributes scan starting points, so relaxed races affect fairness, not
        // correctness; the compare-exchange below exclusively claims the worker to wake.
        let start = self.next_wake.fetch_add(1, AtomicOrdering::Relaxed) % len;
        for offset in 0..len {
            let idx = (start + offset) % len;
            if self.idle[idx]
                .compare_exchange(true, false, AtomicOrdering::AcqRel, AtomicOrdering::Relaxed)
                .is_ok()
            {
                self.unparkers[idx].unpark();
                break;
            }
        }
    }

    /// Wake all threads unconditionally.
    fn wake_workers(&self) {
        for unparker in &self.unparkers {
            unparker.unpark();
        }
    }

    /// duscope: stop the pool from inside a job, as upstream's jobs do by storing `stop`, and wake
    /// parked workers so they exit without waiting for the pool to be dropped.
    pub(crate) fn stop(&self) {
        self.stop.store(true, AtomicOrdering::Relaxed);
        self.wake_workers();
    }
}

// duscope: scoped threads borrowing `run`, upstream's `run_job`; the channel capacity is a parameter
// rather than upstream's `Options` field, and a failed spawn returns the error (dropping the pool
// stops and joins the workers already started).
pub(crate) fn start_pool<'scope, 'env, Job, Event, Run>(
    scope: &'scope Scope<'scope, 'env>,
    threads: usize,
    queued_batches_per_worker: usize,
    run: &'scope Run,
) -> io::Result<Pool<'scope, Job, Event>>
where
    Job: Send + 'scope,
    Event: Send + 'scope,
    Run: Fn(Job, &Worker<Job>, &PoolShared<Job, Event>) + Sync,
{
    let workers: Vec<_> = (0..threads).map(|_| Worker::new_lifo()).collect();
    let parkers: Vec<_> = (0..threads).map(|_| Parker::new()).collect();
    let (event_tx, event_rx) = sync_channel(threads.saturating_mul(queued_batches_per_worker));
    let shared = Arc::new(PoolShared {
        injector: Injector::new(),
        stealers: workers.iter().map(Worker::stealer).collect(),
        stop: AtomicBool::new(false),
        events: event_tx,
        unparkers: parkers
            .iter()
            .map(|parker| parker.unparker().clone())
            .collect(),
        idle: (0..threads).map(|_| AtomicBool::new(false)).collect(),
        next_wake: AtomicUsize::new(0),
    });
    let mut pool = Pool {
        shared,
        events: event_rx,
        handles: Vec::with_capacity(threads),
    };
    for (idx, (worker, parker)) in workers.into_iter().zip(parkers).enumerate() {
        let shared = Arc::clone(&pool.shared);
        let handle = thread::Builder::new()
            .name(format!("duscope-walk-{idx}"))
            .spawn_scoped(scope, move || {
                worker_loop(idx, &worker, &parker, &shared, run);
            })?;
        pool.handles.push(handle);
    }
    Ok(pool)
}

// duscope: `run` in place of upstream's `run_job`; the worker, parker and shared state are borrowed.
fn worker_loop<Job, Event, Run>(
    idx: usize,
    worker: &Worker<Job>,
    parker: &Parker,
    shared: &PoolShared<Job, Event>,
    run: &Run,
) where
    Run: Fn(Job, &Worker<Job>, &PoolShared<Job, Event>),
{
    while !shared.stop.load(AtomicOrdering::Relaxed) {
        let found = if let Some(found) = find_job(worker, shared) {
            found
        } else {
            shared.idle[idx].store(true, AtomicOrdering::Release);
            let Some(found) = find_job(worker, shared) else {
                parker.park();
                shared.idle[idx].store(false, AtomicOrdering::Release);
                continue;
            };
            shared.idle[idx].store(false, AtomicOrdering::Release);
            found
        };
        let (job, stolen) = found;
        if stolen {
            // A successful steal proves peer work is available; wake one more worker so
            // concurrency ramps up only while work remains stealable.
            shared.wake_worker();
        }
        run(job, worker, shared);
    }
}

impl<Job, Event> Pool<'_, Job, Event> {
    /// duscope: queue the walk's first job, as upstream's `start_jobs` does for one root.
    pub(crate) fn inject(&self, job: Job) {
        self.shared.injector.push(job);
        self.shared.wake_worker();
    }

    /// duscope: whether a worker has exited. Workers exit only once the pool is stopped, so a
    /// finished worker in a running pool has panicked.
    pub(crate) fn any_finished(&self) -> bool {
        self.handles.iter().any(ScopedJoinHandle::is_finished)
    }

    /// duscope: upstream's `Drop`, returning the first worker panic so the walk can resume it.
    pub(crate) fn finish(mut self) -> thread::Result<()> {
        self.shut_down()
    }

    fn shut_down(&mut self) -> thread::Result<()> {
        self.shared.stop.store(true, AtomicOrdering::Relaxed);
        self.shared.wake_workers();
        // Workers blocked on the bounded output channel must be released before joining them.
        let (_, disconnected) = sync_channel(0);
        drop(std::mem::replace(&mut self.events, disconnected));
        let mut joined = Ok(());
        for handle in self.handles.drain(..) {
            if let Err(panic) = handle.join() {
                joined = joined.and(Err(panic));
            }
        }
        joined
    }
}

impl<Job, Event> Drop for Pool<'_, Job, Event> {
    fn drop(&mut self) {
        // duscope: a pool dropped without `finish` is unwinding already; its panics are dropped.
        let _ = self.shut_down();
    }
}

/// Find newly submitted roots, then local work, then work from peers.
///
/// Check submitted roots before local directory jobs so a large existing tree cannot starve
/// newly discovered roots. Local LIFO work still takes priority over stealing from peers.
///
/// Returns the selected job and whether it was stolen from another worker; the caller uses a
/// successful steal to wake another idle worker. Returns `None` when a full scan finds no work.
pub(crate) fn find_job<Job, Event>(
    worker: &Worker<Job>,
    shared: &PoolShared<Job, Event>,
) -> Option<(Job, bool)> {
    loop {
        match shared.injector.steal() {
            Steal::Success(job) => return Some((job, true)),
            Steal::Retry => continue,
            Steal::Empty => {}
        }
        if let Some(job) = worker.pop() {
            return Some((job, false));
        }

        let mut retry = false;
        for stealer in &shared.stealers {
            match stealer.steal() {
                Steal::Success(job) => return Some((job, true)),
                Steal::Retry => retry = true,
                Steal::Empty => {}
            }
        }
        if !retry {
            return None;
        }
    }
}

pub(crate) fn schedule_jobs<Job, Event>(
    jobs: Vec<Job>,
    worker: &Worker<Job>,
    shared: &PoolShared<Job, Event>,
) {
    let has_jobs = !jobs.is_empty();
    for job in jobs {
        worker.push(job);
    }
    if has_jobs {
        shared.wake_worker();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn p1_walk_01_workers_take_their_newest_job_and_steal_the_oldest() {
        let (own, peer) = (Worker::new_lifo(), Worker::new_lifo());
        let (events, _received) = sync_channel::<()>(0);
        let shared = PoolShared {
            injector: Injector::new(),
            stealers: vec![own.stealer(), peer.stealer()],
            stop: AtomicBool::new(false),
            events,
            unparkers: Vec::new(),
            idle: Vec::new(),
            next_wake: AtomicUsize::new(0),
        };
        for job in 1..=3 {
            own.push(job);
        }
        assert_eq!(find_job(&own, &shared), Some((3, false)), "own newest");
        assert_eq!(find_job(&peer, &shared), Some((1, true)), "peer's oldest");
        shared.injector.push(9);
        assert_eq!(find_job(&own, &shared), Some((9, true)), "injector first");
        assert_eq!(find_job(&own, &shared), Some((2, false)));
        assert_eq!(find_job(&peer, &shared), None);
    }
}
