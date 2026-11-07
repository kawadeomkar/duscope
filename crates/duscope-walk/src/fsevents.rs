//! A volume's `FSEvents` history: where it stood when a scan started, and
//! what changed since, for an incremental rescan.
//!
//! macOS keeps a persistent, per-volume log of the directories that changed,
//! numbered by one system-wide event id. [`walk`](crate::walk) samples the
//! id before it reads the root and gives every volume it enters an
//! [`FsEventsMark`] (`VolumeInfo::fsevents`), which the index keeps in
//! `scan_volumes`. A rescan hands the mark to [`replay`], which reads the log
//! back from that id and returns the directories changed since, or the
//! [`Fallback`] that says why the log cannot be trusted.
//!
//! Elsewhere there is no such log: [`current_event_id`] and [`volume_uuid`]
//! return `None` and [`replay`] fails with [`FsEventsError::Unsupported`], so
//! no caller needs a `cfg`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::{FsEventsError, FsEventsMark, ScanEvent, platform};

/// How long [`replay`] waits for the end of the history: the 30 seconds of
/// the incremental-rescan design. Past it the replay is incomplete
/// ([`Replay::history_done`] is `false`).
pub const REPLAY_TIMEOUT: Duration = Duration::from_secs(30);

/// The latest event id of the system (`FSEventsGetCurrentEventId`), shared
/// by every volume; `None` off macOS or when `fseventsd` gives none.
pub fn current_event_id() -> Option<u64> {
    platform::fsevents_now()
}

/// The UUID of the `FSEvents` database of the volume on device `dev`
/// (`FSEventsCopyUUIDForDevice`), upper-case and hyphenated; `None` off
/// macOS and for a volume whose history is not kept (logging off, a disk
/// image mounted with `-nobrowse`).
pub fn volume_uuid(dev: u64) -> Option<String> {
    platform::fsevents_uuid(dev)
}

/// Reads back the `FSEvents` history of the volume on device `dev` from the
/// event id of `mark`, limited to the directory `under` (the scan's root,
/// or a mount point below it), and returns the directories that changed
/// since, spelled from `under`.
///
/// It first checks that the history is the one `mark` was taken from: the
/// directory is still on `dev`, logging is on (no `.fseventsd/no_log` at
/// the volume's root), the database has `mark`'s UUID, the system has
/// `mark`'s build and its event ids have not gone back. It then opens a
/// stream on the device from that id with the full history and collects
/// until `HistoryDone`, for at most `timeout` ([`REPLAY_TIMEOUT`]).
///
/// The stream watches the volume's root, where fseventsd records the
/// volume being mounted: a volume attached again since the mark, perhaps
/// after changes nothing logged (attached with `-nobrowse`, or on another
/// system), gives [`Fallback::HistoryGone`] whatever directory `under`
/// is. On the running system's own volumes (`/` and its Data volume),
/// which are mounted again only by a restart and whose whole history is
/// too long to read, it watches `under` alone.
///
/// The full history delivers the whole stored chunk that holds the mark,
/// and a record there can keep an id older than a change it holds, so every
/// record counts: a directory changed shortly before the scan may be listed
/// too, and none changed after it is left out.
///
/// # Errors
///
/// A failed check, a stream that cannot be opened, or
/// [`FsEventsError::Unsupported`] off macOS. Each but the last names its
/// [`Fallback`] ([`FsEventsError::fallback`]).
pub fn replay(
    dev: u64,
    mark: &FsEventsMark,
    under: &Path,
    timeout: Duration,
) -> Result<Replay, FsEventsError> {
    platform::fsevents_replay(dev, mark, under, timeout)
}

/// What a volume's `FSEvents` history says changed below a directory since
/// a mark.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
// Four facts of the history, each read on its own.
#[allow(clippy::struct_excessive_bools)]
pub struct Replay {
    /// The directory the replay was limited to, as given.
    pub root: PathBuf,
    /// The directories whose own entries changed since the mark (and some
    /// shortly before it), each once, sorted, below or at `root` and
    /// spelled from it. A directory inside one of `must_scan_subdirs` is
    /// left out.
    pub dirs: Vec<PathBuf>,
    /// The directories whose whole subtree must be read again: a
    /// `MustScanSubDirs` event (events coalesced up the tree, or lost), or a
    /// volume mounted or unmounted there after the mark (`root`, for the
    /// replayed volume itself mounted again). `root` itself among them
    /// means the replay cannot narrow the rescan.
    pub must_scan_subdirs: Vec<PathBuf>,
    /// A directory above `root` changed its own entries. The history
    /// holds the directories on the way down to `root` too, and such a
    /// change can be `root` itself renamed, removed or put in place of
    /// another, which nothing below it records: the caller checks that
    /// `root` is still the directory the scan read (its `ctime`). Other
    /// changes there, the usual case, leave the subtree as it was.
    pub ancestors_changed: bool,
    /// `FSEvents` lost events (`KernelDropped` or `UserDropped`).
    pub dropped: bool,
    /// The event-id counter wrapped (`EventIdsWrapped`): the mark's id no
    /// longer orders anything.
    pub ids_wrapped: bool,
    /// The history was read to its end (`HistoryDone`) within the timeout.
    pub history_done: bool,
}

impl Replay {
    /// Why the replay cannot stand in for a full rescan, if it cannot: lost
    /// events, or a history it could not read whole (wrapped ids, no
    /// `HistoryDone` in time, the whole root to read again).
    #[must_use]
    pub fn fallback(&self) -> Option<Fallback> {
        if self.dropped {
            Some(Fallback::EventsDropped)
        } else if self.ids_wrapped
            || !self.history_done
            || self.must_scan_subdirs.contains(&self.root)
        {
            Some(Fallback::HistoryGone)
        } else {
            None
        }
    }
}

/// The reasons a replay gives for a full rescan: four of the rescan's
/// `fallback_reason` values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Fallback {
    /// The volume's history is not the one the scan recorded: another
    /// device, another `FSEvents` database or none.
    VolumeChanged,
    /// The history no longer reaches back to the scan's event id.
    HistoryGone,
    /// `FSEvents` logging is off on the volume (`.fseventsd/no_log`).
    FseventsDisabled,
    /// `FSEvents` dropped events since the scan.
    EventsDropped,
}

impl Fallback {
    /// The `fallback_reason` value.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Fallback::VolumeChanged => "volume_changed",
            Fallback::HistoryGone => "history_gone",
            Fallback::FseventsDisabled => "fsevents_disabled",
            Fallback::EventsDropped => "events_dropped",
        }
    }
}

/// The mark of the volume on `dev` when the system's event id was `now`:
/// `None` where the volume keeps no history.
pub(crate) fn mark(dev: u64, now: u64) -> Option<FsEventsMark> {
    platform::fsevents_mark(dev, now)
}

/// Wraps `sink` so that every volume the walk entered carries its mark at
/// the event id `now`, read by `mark`; a volume it did not enter, and every
/// volume when `now` is `None`, carries none.
pub(crate) fn marking(
    now: Option<u64>,
    mark: impl Fn(u64, u64) -> Option<FsEventsMark>,
    mut sink: impl FnMut(ScanEvent),
) -> impl FnMut(ScanEvent) {
    move |event| match event {
        ScanEvent::Volume(mut volume) if volume.entered() => {
            volume.fsevents = now.and_then(|now| mark(volume.dev, now));
            sink(ScanEvent::Volume(volume));
        }
        event => sink(event),
    }
}

#[cfg(test)]
mod tests;
