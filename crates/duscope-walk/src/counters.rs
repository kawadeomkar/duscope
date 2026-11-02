//! What each worker does, counted only under [`WalkOptions::counters`]
//! (`-v`), so a plain walk reads no clock and touches no counter.
//!
//! A job adds to its thread's own tally ([`add`]); the provider adds its
//! listing and attribute times to the same tally, the walk its busy time and
//! the entries it sends. After each job the worker moves the tally into its
//! [`Slot`], which the walk sums into the [`WalkSummary`](crate::WalkSummary)
//! when it ends.
//!
//! [`WalkOptions::counters`]: crate::WalkOptions::counters

use std::cell::Cell;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// One worker's figures: the entries it delivered and the time it spent in
/// the provider's reads.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WorkerCounts {
    /// Entries it delivered.
    pub entries: u64,
    /// Time inside the provider's reads, less the time the walk spent taking
    /// a streaming read's chunks in between.
    pub busy: Duration,
}

/// A job's figures so far, on its worker's thread.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Tally {
    pub(crate) entries: u64,
    pub(crate) busy: Duration,
    /// Time listing directories, by a provider that times it apart.
    pub(crate) listing: Duration,
    /// Time reading entries' attributes, by the same.
    pub(crate) stat: Duration,
    /// Time the walk spent in a streaming read's flushes, which
    /// [`Timer::elapsed`] leaves out.
    flushing: Duration,
}

thread_local! {
    static TALLY: Cell<Tally> = const {
        Cell::new(Tally {
            entries: 0,
            busy: Duration::ZERO,
            listing: Duration::ZERO,
            stat: Duration::ZERO,
            flushing: Duration::ZERO,
        })
    };
}

/// Changes this thread's tally.
pub(crate) fn add(change: impl FnOnce(&mut Tally)) {
    TALLY.with(|tally| {
        let mut now = tally.get();
        change(&mut now);
        tally.set(now);
    });
}

/// This thread's tally, which starts again from nothing.
pub(crate) fn take() -> Tally {
    TALLY.take()
}

/// Times a stretch of a provider's read: the time since it started, less
/// the flushes ([`Flushing`]) in between, which are the walk's own work.
pub(crate) struct Timer {
    start: Instant,
    flushed: Duration,
}

impl Timer {
    pub(crate) fn start() -> Timer {
        Timer {
            start: Instant::now(),
            flushed: TALLY.get().flushing,
        }
    }

    pub(crate) fn elapsed(&self) -> Duration {
        let flushed = TALLY.get().flushing.saturating_sub(self.flushed);
        self.start.elapsed().saturating_sub(flushed)
    }
}

/// Times one flush of a streaming read, which [`Timer`]s leave out.
pub(crate) struct Flushing(Instant);

impl Flushing {
    pub(crate) fn start() -> Flushing {
        Flushing(Instant::now())
    }

    pub(crate) fn stop(self) {
        let took = self.0.elapsed();
        add(|tally| tally.flushing += took);
    }
}

/// One worker's figures for the whole walk. Only its own worker adds to it.
#[derive(Debug, Default)]
pub(crate) struct Slot {
    entries: AtomicU64,
    busy: AtomicU64,
    listing: AtomicU64,
    stat: AtomicU64,
}

/// A duration in whole nanoseconds; a walk does not last 584 years.
fn nanos(d: Duration) -> u64 {
    u64::try_from(d.as_nanos()).unwrap_or(u64::MAX)
}

impl Slot {
    /// Adds a job's tally.
    pub(crate) fn absorb(&self, tally: Tally) {
        self.entries.fetch_add(tally.entries, Ordering::Relaxed);
        self.busy.fetch_add(nanos(tally.busy), Ordering::Relaxed);
        self.listing
            .fetch_add(nanos(tally.listing), Ordering::Relaxed);
        self.stat.fetch_add(nanos(tally.stat), Ordering::Relaxed);
    }

    pub(crate) fn counts(&self) -> WorkerCounts {
        WorkerCounts {
            entries: self.entries.load(Ordering::Relaxed),
            busy: Duration::from_nanos(self.busy.load(Ordering::Relaxed)),
        }
    }

    /// The worker's time listing directories and reading attributes.
    pub(crate) fn phases(&self) -> (Duration, Duration) {
        (
            Duration::from_nanos(self.listing.load(Ordering::Relaxed)),
            Duration::from_nanos(self.stat.load(Ordering::Relaxed)),
        )
    }
}
