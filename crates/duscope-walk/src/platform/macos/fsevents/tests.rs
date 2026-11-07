//! P5-WALK-01: the `FSEvents` history of the boot volume replayed after a
//! scan, the marks a walk records, the checks that refuse a history, and the
//! event flags on recorded events.

use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use duscope_testkit::TestEnv;

use super::{
    Collector, EVENT_IDS_WRAPPED, HISTORY_DONE, KERNEL_DROPPED, MOUNT, MUST_SCAN_SUB_DIRS, Place,
    UNMOUNT, USER_DROPPED, current_event_id, locate, mark, no_log, spell_uuid, volume_uuid,
};
use crate::fsevents::{Fallback, REPLAY_TIMEOUT, Replay, replay};
use crate::{FsEventsError, FsEventsMark, ScanEvent, VolumeInfo, WalkOptions, walk};

/// The volumes a walk of `root` reported.
fn volumes(root: &Path) -> Vec<VolumeInfo> {
    let mut volumes = Vec::new();
    walk(root, &WalkOptions::default(), |event| {
        if let ScanEvent::Volume(volume) = event {
            volumes.push(volume);
        }
    })
    .unwrap();
    volumes
}

/// Replays `root` from `mark` until its directories include every one of
/// `want`, or the replay's timeout has passed: fseventsd numbers a change
/// when it hears of it, a moment after the change.
fn replay_until(dev: u64, mark: &FsEventsMark, root: &Path, want: &BTreeSet<PathBuf>) -> Replay {
    let deadline = Instant::now() + REPLAY_TIMEOUT;
    loop {
        let replayed = replay(dev, mark, root, REPLAY_TIMEOUT).unwrap();
        let dirs: BTreeSet<&PathBuf> = replayed.dirs.iter().collect();
        if want.iter().all(|dir| dirs.contains(dir)) || Instant::now() > deadline {
            return replayed;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn set(root: &Path, dirs: &[&str]) -> BTreeSet<PathBuf> {
    dirs.iter()
        .map(|d| {
            if d.is_empty() {
                root.to_owned()
            } else {
                root.join(d)
            }
        })
        .collect()
}

/// Waits until a replay from the system's latest event id holds nothing
/// below `root`, for at most the replay's timeout, making directories
/// elsewhere meanwhile; whether it came to pass. The full history replays
/// the whole stored chunk that holds a mark, so a mark taken while the
/// chunk still holds the tree's creation replays the creation too.
fn new_chunk(env: &TestEnv, dev: u64, root: &Path) -> bool {
    let elsewhere = env.tmp_dir().join("elsewhere");
    let deadline = Instant::now() + REPLAY_TIMEOUT;
    let mut round = 0;
    loop {
        let now = mark(dev, current_event_id().unwrap()).unwrap();
        let replayed = replay(dev, &now, root, REPLAY_TIMEOUT).unwrap();
        if replayed.dirs.is_empty() && replayed.must_scan_subdirs.is_empty() {
            return true;
        }
        if Instant::now() > deadline {
            return false;
        }
        // Each directory is a record of its own, which fills the chunk.
        round += 1;
        for i in 0..200 {
            fs::create_dir_all(elsewhere.join(round.to_string()).join(i.to_string())).unwrap();
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

#[test]
fn p5_walk_01_a_replay_after_a_scan_returns_exactly_the_touched_directories() {
    let env = TestEnv::new();
    let root = env.root().join("tree");
    fs::create_dir(&root).unwrap();
    let dev = locate(&root).unwrap().dev;
    let before = mark(dev, current_event_id().unwrap()).expect("the boot volume keeps a history");
    for dir in ["a/b", "c", "d", "e/f"] {
        fs::create_dir_all(root.join(dir)).unwrap();
    }
    for file in [
        "a/one",
        "a/b/two",
        "c/three",
        "c/four",
        "d/five",
        "e/six",
        "e/f/seven",
    ] {
        fs::write(root.join(file), file).unwrap();
    }
    let created = set(&root, &["", "a", "a/b", "c", "d", "e", "e/f"]);
    let settled = replay_until(dev, &before, &root, &created);
    assert_eq!(
        settled.dirs.iter().collect::<BTreeSet<_>>(),
        created.iter().collect(),
        "the tree's creation, replayed"
    );
    let fresh = new_chunk(&env, dev, &root);

    // The scan: the walk marks the volume of the root before reading it.
    let volume = volumes(&root).remove(0);
    assert_eq!(volume.dev, dev, "the walk's device is the replay's");
    let scanned = volume.fsevents.expect("the walk marks the boot volume");
    // What the scan's replay holds before any change: nothing once
    // fseventsd started a new chunk, else some of the tree's creation.
    let unchanged = replay(dev, &scanned, &root, REPLAY_TIMEOUT).unwrap();
    assert!(unchanged.history_done && unchanged.fallback().is_none());
    let earlier: BTreeSet<PathBuf> = unchanged.dirs.into_iter().collect();
    if fresh {
        assert!(earlier.is_empty(), "{earlier:?}");
    } else {
        eprintln!("fseventsd kept the tree's creation in its chunk: {earlier:?}");
        assert!(earlier.is_subset(&created), "{earlier:?}");
    }

    // Five changes in four directories; the root, e and e/f are left alone.
    fs::OpenOptions::new()
        .append(true)
        .open(root.join("a/one"))
        .unwrap()
        .write_all(b" more")
        .unwrap();
    fs::write(root.join("a/b/new"), b"new").unwrap();
    fs::remove_file(root.join("c/three")).unwrap();
    fs::create_dir(root.join("d/sub")).unwrap();
    fs::set_permissions(root.join("d/five"), fs::Permissions::from_mode(0o600)).unwrap();
    let touched = set(&root, &["a", "a/b", "c", "d"]);
    let replayed = replay_until(dev, &scanned, &root, &touched);
    assert_eq!(
        replayed.dirs.iter().cloned().collect::<BTreeSet<_>>(),
        &touched | &earlier
    );
    assert_eq!(replayed.root, root);
    assert!(replayed.must_scan_subdirs.is_empty(), "{replayed:?}");
    assert!(replayed.history_done && !replayed.dropped && !replayed.ids_wrapped);
    assert_eq!(replayed.fallback(), None);
}

#[test]
fn p5_walk_01_a_walk_marks_each_volume_it_enters_with_its_uuid_and_event_id() {
    let env = TestEnv::new();
    let root = env.root().join("tree");
    fs::create_dir_all(root.join("dir")).unwrap();
    let low = current_event_id().unwrap();
    let volume = volumes(&root).remove(0);
    let high = current_event_id().unwrap();
    let scanned = volume.fsevents.expect("the walk marks the boot volume");
    assert_eq!(Some(&scanned.uuid), volume_uuid(volume.dev).as_ref());
    assert_eq!(scanned.uuid.len(), 36, "{}", scanned.uuid);
    assert!(
        scanned
            .uuid
            .chars()
            .all(|c| c == '-' || c.is_ascii_hexdigit() && !c.is_ascii_lowercase()),
        "{}",
        scanned.uuid
    );
    assert!(
        (low..=high).contains(&scanned.event_id),
        "{low} <= {} <= {high}",
        scanned.event_id
    );
    let sysctl = std::process::Command::new("sysctl")
        .args(["-n", "kern.osversion"])
        .output()
        .unwrap();
    assert_eq!(
        scanned.os_build,
        String::from_utf8_lossy(&sysctl.stdout).trim()
    );
    // walk_with reads through any provider, and marks nothing.
    let mut marked = Vec::new();
    crate::walk_with(
        &super::super::MacProvider::new(),
        &root,
        &WalkOptions::default(),
        |event| {
            if let ScanEvent::Volume(volume) = event {
                marked.push(volume.fsevents);
            }
        },
    )
    .unwrap();
    assert_eq!(marked, vec![None]);
}

#[test]
fn p5_walk_01_a_history_other_than_the_marks_is_refused_with_its_reason() {
    let env = TestEnv::new();
    let root = env.root().join("tree");
    fs::create_dir(&root).unwrap();
    let dev = locate(&root).unwrap().dev;
    let good = mark(dev, current_event_id().unwrap()).unwrap();
    let reason = |dev: u64, mark: &FsEventsMark, under: &Path| {
        let error = replay(dev, mark, under, REPLAY_TIMEOUT).unwrap_err();
        (error.fallback().map(Fallback::as_str), error)
    };
    let other_uuid = FsEventsMark {
        uuid: "00000000-0000-0000-0000-000000000000".to_owned(),
        ..good.clone()
    };
    let (why, error) = reason(dev, &other_uuid, &root);
    assert_eq!(why, Some("volume_changed"), "{error}");
    assert!(
        matches!(error, FsEventsError::OtherHistory { .. }),
        "{error}"
    );
    let other_build = FsEventsMark {
        os_build: "1A1".to_owned(),
        ..good.clone()
    };
    let (why, error) = reason(dev, &other_build, &root);
    assert_eq!(why, Some("history_gone"), "{error}");
    assert!(
        matches!(error, FsEventsError::OtherSystem { .. }),
        "{error}"
    );
    let ahead = FsEventsMark {
        event_id: u64::MAX - 1,
        ..good.clone()
    };
    let (why, error) = reason(dev, &ahead, &root);
    assert_eq!(why, Some("history_gone"), "{error}");
    assert!(matches!(error, FsEventsError::IdsBehind { .. }), "{error}");
    let (why, error) = reason(dev + 1, &good, &root);
    assert_eq!(why, Some("volume_changed"), "{error}");
    assert!(
        matches!(error, FsEventsError::OtherDevice { .. }),
        "{error}"
    );
    let (why, error) = reason(dev, &good, &root.join("missing"));
    assert_eq!(why, Some("volume_changed"), "{error}");
    assert!(matches!(error, FsEventsError::Path { .. }), "{error}");
    // The mark itself reads back: nothing changed below the root since.
    let replayed = replay(dev, &good, &root, REPLAY_TIMEOUT).unwrap();
    assert!(replayed.history_done, "{replayed:?}");
    assert_eq!(replayed.fallback(), None);
}

#[test]
fn p5_walk_01_a_directory_is_placed_below_its_mount_point_without_firmlinks() {
    let env = TestEnv::new();
    let root = env.root().join("tree");
    fs::create_dir(&root).unwrap();
    let place = locate(&root).unwrap();
    let watched = Path::new(std::ffi::OsStr::from_bytes(&place.watched));
    assert!(watched.is_relative(), "{}", watched.display());
    // The mount point joined to the device-relative path names the same
    // directory, which /System/Volumes/Data/private/var/... spells for the
    // /private/var/... the test runs in.
    let joined = place.mount_point.join(watched);
    let (a, b) = (fs::metadata(&joined).unwrap(), fs::metadata(&root).unwrap());
    assert_eq!(
        (a.dev(), a.ino()),
        (b.dev(), b.ino()),
        "{}",
        joined.display()
    );
    assert!(watched.ends_with("tree"), "{}", watched.display());
    let slash = locate(Path::new("/")).unwrap();
    assert_eq!(
        (slash.mount_point.as_path(), slash.watched.as_slice()),
        (Path::new("/"), &b""[..])
    );
}

/// The mark's event id in the recorded events.
const SINCE: u64 = 100;

/// A replay of `events` for the directory `/scan/root`, which the device
/// spells `Users/me/root`, from the event id [`SINCE`]; each event is
/// numbered after it.
fn replayed(events: &[(&str, u32)]) -> Replay {
    replayed_at(
        &events
            .iter()
            .map(|&(p, f)| (p, f, SINCE + 1))
            .collect::<Vec<_>>(),
    )
}

/// As [`replayed`], each event with its own id.
fn replayed_at(events: &[(&str, u32, u64)]) -> Replay {
    let mut collector = Collector::new(
        SINCE,
        b"Users/me/root".to_vec(),
        PathBuf::from("/scan/root"),
    );
    for &(path, flags, id) in events {
        collector.take(path.as_bytes(), flags, id);
    }
    collector.finish()
}

#[test]
fn p5_walk_01_dropped_events_give_events_dropped() {
    for dropped in [KERNEL_DROPPED, USER_DROPPED] {
        // As fseventsd reports a loss: MustScanSubDirs on "/", with the
        // flag that says where the events were lost.
        let lost = replayed(&[
            ("Users/me/root/a/", 0),
            ("/", MUST_SCAN_SUB_DIRS | dropped),
            ("", HISTORY_DONE),
        ]);
        assert!(lost.dropped && lost.history_done, "{lost:?}");
        assert_eq!(lost.must_scan_subdirs, vec![PathBuf::from("/scan/root")]);
        assert!(lost.dirs.is_empty(), "inside the root's subtree: {lost:?}");
        assert_eq!(lost.fallback(), Some(Fallback::EventsDropped));
        assert_eq!(Fallback::EventsDropped.as_str(), "events_dropped");
        // The flag alone, on a directory below the root, is a loss too.
        let below = replayed(&[("Users/me/root/a/", dropped), ("", HISTORY_DONE)]);
        assert_eq!(below.fallback(), Some(Fallback::EventsDropped), "{below:?}");
    }
}

#[test]
fn p5_walk_01_no_log_gives_fsevents_disabled() {
    let env = TestEnv::new();
    let volume = env.root().join("volume");
    fs::create_dir_all(volume.join(".fseventsd")).unwrap();
    assert!(!no_log(&volume));
    fs::write(volume.join(".fseventsd/no_log"), b"").unwrap();
    assert!(no_log(&volume));
    let error = FsEventsError::Disabled {
        mount_point: volume,
    };
    assert_eq!(error.fallback(), Some(Fallback::FseventsDisabled));
    assert_eq!(Fallback::FseventsDisabled.as_str(), "fsevents_disabled");
    // An unreadable .fseventsd (the Data volume's, owned by root) is no
    // evidence either way.
    assert!(!no_log(Path::new("/System/Volumes/Data")) || is_root());
}

fn is_root() -> bool {
    crate::effective_uid() == 0
}

#[test]
fn p5_walk_01_event_flags_widen_or_void_the_replay() {
    // Plain events name the directories, spelled from the caller's root;
    // MustScanSubDirs, Mount and Unmount ask for the whole subtree.
    let mixed = replayed(&[
        ("Users/me/root/", 0),
        ("Users/me/root/a/", 0),
        ("Users/me/root/a/", 0),
        ("Users/me/root/b/c/", MUST_SCAN_SUB_DIRS),
        ("Users/me/root/b/c/d/", 0),
        ("Users/me/root/m/", MOUNT),
        ("Users/me/root/u/", UNMOUNT),
        ("Users/me/rootless/", 0),
        ("Users/other/", 0),
        ("", HISTORY_DONE),
    ]);
    let root = Path::new("/scan/root");
    assert_eq!(mixed.dirs, vec![root.to_owned(), root.join("a")]);
    assert_eq!(
        mixed.must_scan_subdirs,
        vec![root.join("b/c"), root.join("m"), root.join("u")]
    );
    assert!(!mixed.ancestors_changed);
    assert_eq!(mixed.fallback(), None);
    // The whole root to read again: MustScanSubDirs on it or above it, a
    // volume unmounted above it, or MustScanSubDirs on HistoryDone.
    for events in [
        &[("Users/me/root/", MUST_SCAN_SUB_DIRS), ("", HISTORY_DONE)][..],
        &[("Users/", MUST_SCAN_SUB_DIRS), ("", HISTORY_DONE)][..],
        &[("Users/me/", UNMOUNT), ("", HISTORY_DONE)][..],
        &[("", HISTORY_DONE | MUST_SCAN_SUB_DIRS)][..],
    ] {
        let whole = replayed(events);
        assert_eq!(whole.must_scan_subdirs, vec![root.to_owned()], "{events:?}");
        assert_eq!(whole.fallback(), Some(Fallback::HistoryGone), "{events:?}");
    }
    // A plain change above the root is flagged for the caller to check the
    // root, and narrows nothing.
    let above = replayed(&[
        ("Users/me/", 0),
        ("Users/me/root/a/", 0),
        ("", HISTORY_DONE),
    ]);
    assert!(above.ancestors_changed, "{above:?}");
    assert_eq!(above.dirs, vec![root.join("a")]);
    assert!(above.must_scan_subdirs.is_empty());
    assert_eq!(above.fallback(), None);
    // Records keep an id older than a change they hold: every one counts.
    let overlap = replayed_at(&[
        ("Users/me/root/old/", 0, 3),
        ("Users/me/root/older/", MUST_SCAN_SUB_DIRS, 4),
        ("", HISTORY_DONE, 5),
    ]);
    assert_eq!(overlap.dirs, vec![root.join("old")]);
    assert_eq!(overlap.must_scan_subdirs, vec![root.join("older")]);
    // A mount or unmount before the mark was in place for the walk: its
    // directory counts as changed, not its subtree; one after it widens.
    let mounted = replayed_at(&[
        ("Users/me/root/", MOUNT, SINCE),
        ("Users/me/root/m/", UNMOUNT, 7),
        ("Users/me/", MOUNT, 8),
        ("", HISTORY_DONE, 9),
    ]);
    assert_eq!(mounted.dirs, vec![root.to_owned(), root.join("m")]);
    assert!(mounted.must_scan_subdirs.is_empty() && mounted.ancestors_changed);
    assert_eq!(mounted.fallback(), None);
    let remounted = replayed_at(&[("Users/me/root/", MOUNT, SINCE + 1), ("", HISTORY_DONE, 9)]);
    assert_eq!(remounted.must_scan_subdirs, vec![root.to_owned()]);
    assert_eq!(remounted.fallback(), Some(Fallback::HistoryGone));
    // Wrapped ids void the history.
    let wrapped = replayed(&[("", EVENT_IDS_WRAPPED), ("", HISTORY_DONE)]);
    assert!(wrapped.ids_wrapped);
    assert_eq!(wrapped.fallback(), Some(Fallback::HistoryGone));
    assert_eq!(Fallback::HistoryGone.as_str(), "history_gone");
    // No HistoryDone: the replay timed out before the history's end.
    let cut = replayed(&[("Users/me/root/a/", 0)]);
    assert!(!cut.history_done);
    assert_eq!(cut.fallback(), Some(Fallback::HistoryGone));
    // What arrives after HistoryDone is not part of the replay.
    let late = replayed(&[("", HISTORY_DONE), ("Users/me/root/late/", 0)]);
    assert!(late.dirs.is_empty(), "{late:?}");
    // Watching a whole volume, paths are relative to its root.
    let mut whole_volume = Collector::new(SINCE, Vec::new(), PathBuf::from("/Volumes/X"));
    whole_volume.take(b"a/b/", 0, 1);
    whole_volume.take(b"", 0, 2);
    whole_volume.take(b"", HISTORY_DONE, 3);
    let whole_volume = whole_volume.finish();
    assert_eq!(
        whole_volume.dirs,
        vec![PathBuf::from("/Volumes/X"), PathBuf::from("/Volumes/X/a/b")]
    );
}

#[test]
fn p5_walk_01_a_replay_watches_the_volume_root_except_on_the_systems_own_volumes() {
    let env = TestEnv::new();
    let root = env.root().join("tree");
    fs::create_dir(&root).unwrap();
    // The test's directory is on the boot Data volume, and `/` is the
    // sealed System volume: the running system's own, watched below the
    // root.
    for dir in [root.as_path(), Path::new("/")] {
        let place = locate(dir).unwrap();
        assert!(place.system, "{}", dir.display());
        assert_eq!(place.stream_path(), place.watched.as_slice());
    }
    // Any other volume is watched from its root, where its mounts are.
    let other = Place {
        dev: 1,
        mount_point: PathBuf::from("/Volumes/X"),
        watched: b"Users/me/root".to_vec(),
        system: false,
    };
    assert_eq!(other.stream_path(), b"");
    // There the volume mounted again after the mark voids the replay of a
    // directory below its root; mounted before it, it was in place for the
    // walk.
    let remounted = replayed_at(&[("", MOUNT, SINCE + 1), ("", HISTORY_DONE, SINCE + 2)]);
    assert_eq!(
        remounted.must_scan_subdirs,
        vec![PathBuf::from("/scan/root")]
    );
    assert_eq!(remounted.fallback(), Some(Fallback::HistoryGone));
    let mounted = replayed_at(&[("", MOUNT, SINCE - 1), ("", HISTORY_DONE, SINCE + 2)]);
    assert!(mounted.must_scan_subdirs.is_empty() && mounted.ancestors_changed);
    assert_eq!(mounted.fallback(), None);
    // Beside the directory, the rest of the volume counts for nothing.
    let beside = replayed(&[
        ("Users/other/", 0),
        ("Users/me/rootless/", MUST_SCAN_SUB_DIRS),
        ("Users/me/rootless/a/", 0),
        ("", HISTORY_DONE),
    ]);
    assert_eq!(
        beside,
        Replay {
            root: PathBuf::from("/scan/root"),
            history_done: true,
            ..Replay::default()
        }
    );
}

/// A replay of `events` for the whole volume mounted at `/Volumes/X`, from
/// the event id [`SINCE`], each event with its own id.
fn replayed_volume(events: &[(&str, u32, u64)]) -> Replay {
    let mut collector = Collector::new(SINCE, Vec::new(), PathBuf::from("/Volumes/X"));
    for &(path, flags, id) in events {
        collector.take(path.as_bytes(), flags, id);
    }
    collector.finish()
}

#[test]
fn p5_walk_01_an_entry_of_the_volume_root_marks_the_root_as_changed() {
    // As fseventsd records them on an image: a change among the root's own
    // entries by the entry's name, every other by its directory, with a
    // trailing slash.
    let after = SINCE + 1;
    let volume = replayed_volume(&[
        ("", MOUNT, SINCE - 1),
        ("newfile", 0, after),
        ("renamedfile", 0, after),
        ("olddir", 0, after),
        ("sub/", 0, after),
        ("sub/deep/", 0, after),
        ("", HISTORY_DONE, after),
    ]);
    let x = Path::new("/Volumes/X");
    assert_eq!(
        volume.dirs,
        vec![x.to_owned(), x.join("sub"), x.join("sub/deep")]
    );
    assert!(volume.must_scan_subdirs.is_empty() && !volume.ancestors_changed);
    assert_eq!(volume.fallback(), None);
    // The root's entries alone changed: the root, and none of its entries.
    let only = replayed_volume(&[("newfile", 0, after), ("", HISTORY_DONE, after)]);
    assert_eq!(only.dirs, vec![x.to_owned()]);
    // A subtree to read again at an entry is the entry's own, beside its
    // directory's entries.
    let mounted = replayed_volume(&[("mnt", MOUNT, after), ("", HISTORY_DONE, after)]);
    assert_eq!(mounted.dirs, vec![x.to_owned()]);
    assert_eq!(mounted.must_scan_subdirs, vec![x.join("mnt")]);
    assert_eq!(mounted.fallback(), None);
    // Replaying a directory, an entry of the root, the root's own name
    // included, is a change above it.
    let root = Path::new("/scan/root");
    for entry in ["Users", "elsewhere"] {
        let above = replayed(&[(entry, 0), ("Users/me/root/a/", 0), ("", HISTORY_DONE)]);
        assert!(above.ancestors_changed, "{entry}: {above:?}");
        assert_eq!(above.dirs, vec![root.join("a")], "{entry}");
    }
    let named = replayed(&[("Users", MUST_SCAN_SUB_DIRS), ("", HISTORY_DONE)]);
    assert_eq!(named.must_scan_subdirs, vec![root.to_owned()]);
    assert_eq!(named.fallback(), Some(Fallback::HistoryGone));
    // The same rule below the root, should a record name an entry there.
    let below = replayed(&[
        ("Users/me/root/a/file", 0),
        ("Users/me/root", 0),
        ("", HISTORY_DONE),
    ]);
    assert_eq!(below.dirs, vec![root.join("a")]);
    assert!(below.ancestors_changed, "{below:?}");
}

#[test]
fn p5_walk_01_uuids_are_spelled_as_core_foundation_spells_them() {
    let bytes = [
        0x83, 0x32, 0xa5, 0xa0, 0x30, 0x24, 0x4b, 0xb2, 0xab, 0x99, 0xd8, 0xab, 0x8f, 0xc0, 0xae,
        0xcc,
    ];
    assert_eq!(spell_uuid(&bytes), "8332A5A0-3024-4BB2-AB99-D8AB8FC0AECC");
}

/// Detaches a disk image however the test ends.
struct Detach<'a>(&'a TestEnv, PathBuf);

impl Drop for Detach<'_> {
    fn drop(&mut self) {
        let _ = self
            .0
            .command("hdiutil")
            .args(["detach", "-force"])
            .arg(&self.1)
            .output();
    }
}

/// Waits, for at most the replay's timeout, until `ready` holds.
fn wait_for(what: &str, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + REPLAY_TIMEOUT;
    while !ready() {
        assert!(Instant::now() < deadline, "waited too long for {what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The `no_log` check against fseventsd itself: an image keeps a history,
/// and once `.fseventsd/no_log` is written and the image attached again it
/// keeps none, which the replay names `fsevents_disabled`.
#[test]
#[ignore = "attaches a disk image; DUSCOPE_TEST_PLATFORM=1"]
fn p5_walk_01_macos_no_log_on_an_image_gives_fsevents_disabled() {
    let env = TestEnv::new();
    if std::env::var_os("DUSCOPE_TEST_PLATFORM").is_none_or(|v| v != "1") {
        eprintln!("skipped: set DUSCOPE_TEST_PLATFORM=1");
        return;
    }
    let work = fs::canonicalize(env.tmp_dir()).unwrap().join("image");
    let mount = work.join("mnt");
    fs::create_dir_all(&mount).unwrap();
    let image = work.join("image.dmg");
    env.hdiutil(&[
        &"create",
        &"-ov",
        &"-size",
        &"16m",
        &"-fs",
        &"APFS",
        &"-volname",
        &"DUSCOPE",
        &image,
    ]);
    // Not -nobrowse: fseventsd keeps no history of a volume attached so.
    env.hdiutil(&[&"attach", &"-mountpoint", &mount, &image]);
    let detach = Detach(&env, mount.clone());
    let dev = locate(&mount).unwrap().dev;
    wait_for("the image's FSEvents history", || {
        volume_uuid(dev).is_some()
    });
    fs::create_dir(mount.join("dir")).unwrap();
    let volume = volumes(&mount).remove(0);
    let marked = volume.fsevents.expect("the walk marks the image's volume");
    fs::write(mount.join("dir/file"), b"bytes").unwrap();
    // fseventsd records a change among the root's own entries by the
    // entry's name: the root's entries changed, not the entry's.
    fs::write(mount.join("top"), b"bytes").unwrap();
    let dirs = set(&mount, &["", "dir"]);
    let replayed = replay_until(volume.dev, &marked, &mount, &dirs);
    assert!(replayed.dirs.contains(&mount.join("dir")), "{replayed:?}");
    assert!(replayed.dirs.contains(&mount), "{replayed:?}");
    assert!(!replayed.dirs.contains(&mount.join("top")), "{replayed:?}");
    assert_eq!(replayed.fallback(), None, "{replayed:?}");

    // Logging off, as `touch .fseventsd/no_log` turns it off, from the
    // next mount on.
    wait_for(".fseventsd", || mount.join(".fseventsd").is_dir());
    fs::write(mount.join(".fseventsd/no_log"), b"").unwrap();
    env.hdiutil(&[&"detach", &mount]);
    env.hdiutil(&[&"attach", &"-mountpoint", &mount, &image]);
    let dev = locate(&mount).unwrap().dev;
    assert_eq!(volume_uuid(dev), None, "a volume with logging off");
    let error = replay(dev, &marked, &mount, REPLAY_TIMEOUT).unwrap_err();
    assert!(matches!(error, FsEventsError::Disabled { .. }), "{error}");
    assert_eq!(
        error.fallback().map(Fallback::as_str),
        Some("fsevents_disabled")
    );
    assert_eq!(volumes(&mount).remove(0).fsevents, None, "nothing to mark");
    drop(detach);
}

/// A remount the history did not log, replayed from a directory below the
/// volume's root: the image is marked, changed while attached with
/// `-nobrowse` (fseventsd keeps no history of it then), and attached as
/// before. The UUID is the same; the one record of the gap is the `Mount`
/// event at the volume's root, which a stream watching the directory alone
/// never receives.
#[test]
#[ignore = "attaches a disk image; DUSCOPE_TEST_PLATFORM=1"]
fn p5_walk_01_macos_a_remount_since_the_mark_voids_a_replay_below_the_root() {
    let env = TestEnv::new();
    if std::env::var_os("DUSCOPE_TEST_PLATFORM").is_none_or(|v| v != "1") {
        eprintln!("skipped: set DUSCOPE_TEST_PLATFORM=1");
        return;
    }
    let work = fs::canonicalize(env.tmp_dir()).unwrap().join("image");
    let mount = work.join("mnt");
    fs::create_dir_all(&mount).unwrap();
    let image = work.join("image.dmg");
    env.hdiutil(&[
        &"create",
        &"-ov",
        &"-size",
        &"16m",
        &"-fs",
        &"APFS",
        &"-volname",
        &"DUSCOPE",
        &image,
    ]);
    env.hdiutil(&[&"attach", &"-mountpoint", &mount, &image]);
    let detach = Detach(&env, mount.clone());
    let dev = locate(&mount).unwrap().dev;
    wait_for("the image's FSEvents history", || {
        volume_uuid(dev).is_some()
    });
    let deep = mount.join("sub/deep");
    fs::create_dir_all(&deep).unwrap();
    let volume = volumes(&mount).remove(0);
    let marked = volume.fsevents.expect("the walk marks the image's volume");
    let place = locate(&deep).unwrap();
    assert!(!place.system, "an image is not the system's own volume");
    assert_eq!(place.watched, b"sub/deep");
    let unchanged = replay(volume.dev, &marked, &deep, REPLAY_TIMEOUT).unwrap();
    assert_eq!(unchanged.fallback(), None, "{unchanged:?}");

    // Changed while fseventsd logs nothing, then attached as before.
    env.hdiutil(&[&"detach", &mount]);
    env.hdiutil(&[&"attach", &"-nobrowse", &"-mountpoint", &mount, &image]);
    let dev = locate(&mount).unwrap().dev;
    assert_eq!(volume_uuid(dev), None, "no history under -nobrowse");
    fs::write(deep.join("offline"), b"bytes").unwrap();
    env.hdiutil(&[&"detach", &mount]);
    env.hdiutil(&[&"attach", &"-mountpoint", &mount, &image]);
    let dev = locate(&mount).unwrap().dev;
    wait_for("the image's FSEvents history", || {
        volume_uuid(dev).is_some()
    });
    assert_eq!(
        volume_uuid(dev).as_ref(),
        Some(&marked.uuid),
        "the same history"
    );
    // The replay of the mount point sees the remount once fseventsd has
    // numbered it; the replay of the directory below must see it too.
    wait_for("the remount in the history", || {
        replay(dev, &marked, &mount, REPLAY_TIMEOUT)
            .unwrap()
            .fallback()
            .is_some()
    });
    for under in [&mount, &deep] {
        let replayed = replay(dev, &marked, under, REPLAY_TIMEOUT).unwrap();
        assert_eq!(
            &replayed.must_scan_subdirs,
            &[under.clone()],
            "{replayed:?}"
        );
        assert_eq!(
            replayed.fallback(),
            Some(Fallback::HistoryGone),
            "{}: {replayed:?}",
            under.display()
        );
    }
    drop(detach);
}
