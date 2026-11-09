//! P5-WALK-01: the marks a walk hands its volumes, and the reasons replays
//! and their errors give, on every platform.

use std::io;
use std::path::{Path, PathBuf};

use super::{Fallback, Replay, marking};
use crate::{
    FsEventsError, FsEventsMark, MemProvider, Provider, ScanEvent, Skip, VolumeInfo, WalkOptions,
    walk_with,
};

/// A mark for the device `dev` at `now`, as the platform would read one.
#[allow(clippy::unnecessary_wraps)] // the platform's reader may find none
fn mark_of(dev: u64, now: u64) -> Option<FsEventsMark> {
    Some(FsEventsMark {
        uuid: format!("UUID-{dev}"),
        event_id: now,
        os_build: "25G332".to_owned(),
    })
}

/// The marks of the volumes `events` carry through `marking(now, ...)`.
fn marks(now: Option<u64>, events: Vec<ScanEvent>) -> Vec<Option<FsEventsMark>> {
    let mut seen = Vec::new();
    let mut sink = marking(now, mark_of, |event| {
        if let ScanEvent::Volume(volume) = event {
            seen.push(volume.fsevents);
        }
    });
    for event in events {
        sink(event);
    }
    drop(sink);
    seen
}

#[test]
fn p5_walk_01_each_entered_volume_carries_the_mark_sampled_before_the_walk() {
    let mut tree = MemProvider::new("/mem");
    tree.file("a", 1).dir("d").file("d/b", 2);
    // A walk through the wrapper `walk` puts around its sink.
    let (mut volumes, mut entries) = (Vec::new(), 0);
    walk_with(
        &tree,
        Path::new("/mem"),
        &WalkOptions::default(),
        marking(Some(42), mark_of, |event| match event {
            ScanEvent::Volume(volume) => volumes.push(volume),
            ScanEvent::Entry(_) => entries += 1,
            _ => {}
        }),
    )
    .unwrap();
    assert_eq!(entries, 4, "every other event passes through");
    let [root]: [VolumeInfo; 1] = volumes.try_into().unwrap();
    assert_eq!(root.fsevents, mark_of(root.dev, 42));

    let entered = tree.volume(Path::new("/mem")).unwrap();
    let skipped = VolumeInfo {
        reason: Some(Skip::Network),
        ..entered.clone()
    };
    // A volume the walk did not enter carries none, nor does any volume of
    // a walk that had no event id to sample.
    assert_eq!(
        marks(
            Some(7),
            vec![
                ScanEvent::Volume(skipped),
                ScanEvent::Volume(entered.clone())
            ]
        ),
        vec![None, mark_of(entered.dev, 7)]
    );
    assert_eq!(marks(None, vec![ScanEvent::Volume(entered)]), vec![None]);
}

#[test]
fn p5_walk_01_replays_and_their_errors_name_their_fallback_reasons() {
    let root = PathBuf::from("/r");
    let done = Replay {
        root: root.clone(),
        dirs: vec![root.join("a")],
        must_scan_subdirs: vec![root.join("b")],
        ancestors_changed: true,
        history_done: true,
        ..Replay::default()
    };
    assert_eq!(done.fallback(), None, "narrowed, the root still checked");
    let with = |change: fn(&mut Replay)| {
        let mut replay = done.clone();
        change(&mut replay);
        replay.fallback()
    };
    assert_eq!(
        with(|r| {
            r.dropped = true;
            r.ids_wrapped = true;
            r.history_done = false;
        }),
        Some(Fallback::EventsDropped),
        "a loss outranks the rest"
    );
    assert_eq!(with(|r| r.ids_wrapped = true), Some(Fallback::HistoryGone));
    assert_eq!(
        with(|r| r.history_done = false),
        Some(Fallback::HistoryGone)
    );
    assert_eq!(
        with(|r| r.must_scan_subdirs.push(r.root.clone())),
        Some(Fallback::HistoryGone)
    );

    let errors = [
        (FsEventsError::Unsupported, None),
        (
            FsEventsError::Path {
                path: root.clone(),
                source: io::ErrorKind::NotFound.into(),
            },
            Some("volume_changed"),
        ),
        (
            FsEventsError::OtherDevice {
                path: root.clone(),
                dev: 1,
                found: 2,
            },
            Some("volume_changed"),
        ),
        (
            FsEventsError::Disabled {
                mount_point: root.clone(),
            },
            Some("fsevents_disabled"),
        ),
        (FsEventsError::NoHistory { dev: 1 }, Some("volume_changed")),
        (
            FsEventsError::OtherHistory {
                dev: 1,
                recorded: "A".to_owned(),
                found: "B".to_owned(),
            },
            Some("volume_changed"),
        ),
        (
            FsEventsError::OtherSystem {
                recorded: "25G332".to_owned(),
                found: "26A100".to_owned(),
            },
            Some("history_gone"),
        ),
        (
            FsEventsError::IdsBehind {
                since: 9,
                current: 3,
            },
            Some("history_gone"),
        ),
        (FsEventsError::Stream { dev: 1 }, Some("history_gone")),
    ];
    for (error, reason) in errors {
        assert_eq!(error.fallback().map(Fallback::as_str), reason, "{error}");
    }
    assert_eq!(
        [
            Fallback::VolumeChanged,
            Fallback::HistoryGone,
            Fallback::FseventsDisabled,
            Fallback::EventsDropped,
        ]
        .map(Fallback::as_str),
        [
            "volume_changed",
            "history_gone",
            "fsevents_disabled",
            "events_dropped"
        ]
    );
}

#[test]
fn p5_core_01_only_apfs_and_hfs_histories_stand_in_for_a_read() {
    use super::reusable;
    assert!(reusable("apfs") && reusable("hfs"));
    for fs_type in [
        "msdos", "exfat", "ntfs", "smbfs", "nfs", "afpfs", "webdav", "ext4", "",
    ] {
        assert!(!reusable(fs_type), "{fs_type}");
    }
}
