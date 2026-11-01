use std::io::Write;

use duscope_walk::{DirId, Kind, Skip};
use flate2::Compression;
use flate2::write::GzEncoder;

use super::{CoreError, DECOMPRESSED_CAP, ImportFormat, Imported, import, import_capped};
use crate::rollup::roll_up;

fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut gz = GzEncoder::new(Vec::new(), Compression::fast());
    gz.write_all(bytes).unwrap();
    gz.finish().unwrap()
}

/// The root's totals after the roll-up: apparent, allocated, files, dirs.
fn totals(imported: &mut Imported) -> (u64, u64, u32, u32) {
    roll_up(&mut imported.arena);
    let root = imported.arena.dir(DirId::ROOT);
    (
        root.cum_apparent,
        root.cum_allocated,
        root.n_files,
        root.n_dirs,
    )
}

fn names(imported: &Imported) -> Vec<Vec<u8>> {
    imported.arena.entries().map(|e| e.name.to_vec()).collect()
}

fn refused(input: &[u8], format: Option<ImportFormat>) -> String {
    match import(input, format) {
        Err(CoreError::ImportInvalid(why)) => why,
        Err(other) => panic!("not ImportInvalid: {other:?}"),
        Ok(_) => panic!("imported: {}", String::from_utf8_lossy(input)),
    }
}

const NCDU: &[u8] = br#"[1,2,{"progname":"ncdu","progver":"2.9.2","timestamp":1800000000,"extra":{"a":[1,{"b":null}]}},
[{"name":"/home/a","asize":4096,"dsize":4096,"dev":66},
{"name":"one","asize":100,"dsize":4096,"ino":7,"nlink":2,"mtime":1700000000},
{"name":"two","asize":100,"dsize":4096,"ino":7,"nlink":2},
{"name":"caf\u00e9 \"q\" \ud83d\ude00 \u0001","asize":5,"dsize":0},
{"name":"rawFF","notreg":true},
{"name":"gone","read_error":true},
{"name":"proc","excluded":"kernfs"},
[{"name":"sub","asize":4096,"dsize":4096,"read_error":true}],
[{"name":"other","asize":10,"dsize":4096,"dev":67},
{"name":"x","asize":1,"dsize":4096,"ino":7,"hlnkc":true}]]]
"#;

/// [`NCDU`], with a name that is not UTF-8.
fn ncdu() -> Vec<u8> {
    let at = NCDU.windows(5).position(|w| w == b"rawFF").unwrap();
    let mut bytes = NCDU.to_vec();
    bytes.splice(at + 3..at + 5, [0xff]);
    bytes
}

#[test]
fn p2_cli_03_ncdu_reads_into_the_events_of_a_walk() {
    let mut imported = import(&ncdu()[..], None).unwrap();
    assert_eq!(imported.format, ImportFormat::Ncdu);
    assert_eq!(imported.root, b"/home/a");
    assert_eq!(imported.scanned_at, Some(1_800_000_000_000_000_000));
    assert_eq!(
        (
            imported.unreadable_dirs,
            imported.entry_errors,
            imported.skipped_dirs
        ),
        (1, 1, 1)
    );
    let names = names(&imported);
    assert_eq!(names[0], b"a");
    assert!(names.contains(&"café \"q\" 😀 \x01".as_bytes().to_vec()));
    assert!(names.contains(&b"raw\xff".to_vec()));
    // The file that could not be read is an error, not an entry.
    assert!(!names.contains(&b"gone".to_vec()));
    let kinds: Vec<Kind> = imported.arena.entries().map(|e| e.kind).collect();
    assert_eq!(kinds.iter().filter(|k| **k == Kind::Dir).count(), 4);
    assert!(kinds.contains(&Kind::Unknown));
    assert!(
        imported
            .arena
            .skipped()
            .values()
            .any(|s| *s == Skip::Kernel)
    );
    // `one` and `two` share inode 7 on device 66 and count once; `x`, on
    // device 67, is inode 7 of another device.
    let (apparent, allocated, files, dirs) = totals(&mut imported);
    assert_eq!(files, 5);
    assert_eq!(dirs, 3);
    assert_eq!(apparent, 4096 + 100 + 5 + 4096 + 10 + 1);
    assert_eq!(allocated, 4096 + 4096 + 4096 + 4096 + 4096);
}

#[test]
fn p2_cli_03_gdu_directory_sizes_come_from_the_entries() {
    // gdu writes a directory's cumulative sizes and an item count.
    let gdu = br#"[1,2,{"progname":"gdu","progver":"v5.31.0","timestamp":1800000000},
[{"name":"/srv","asize":999999,"dsize":999999,"items":4,"mtime":1700000000},
{"name":"f","asize":10,"dsize":4096},
[{"name":"d","asize":888888,"dsize":888888,"items":2},
{"name":"g","asize":20,"dsize":4096},
{"name":"h","ino":5,"hlnkc":true,"asize":30,"dsize":4096},
{"name":"i","ino":5,"hlnkc":true,"asize":30,"dsize":4096}]]]"#;
    let mut imported = import(&gdu[..], None).unwrap();
    assert_eq!(imported.format, ImportFormat::Gdu);
    let (apparent, allocated, files, dirs) = totals(&mut imported);
    assert_eq!((files, dirs), (4, 1));
    assert_eq!(apparent, 10 + 20 + 30);
    assert_eq!(allocated, 3 * 4096);
    // Asked for as ncdu's, the directories keep their sizes.
    let mut as_ncdu = import(&gdu[..], Some(ImportFormat::Ncdu)).unwrap();
    assert_eq!(totals(&mut as_ncdu).0, 999_999 + 10 + 888_888 + 20 + 30);
}

const GPSCAN: &[u8] = br#"<?xml version="1.0" encoding="UTF-8"?>
<GrandPerspectiveScanDump appVersion="3.8.1" formatVersion="7">
<ScanInfo volumePath="/" volumeSize="1000" freeSpace="10" scanTime="2027-01-15T08:00:00Z" fileSizeMeasure="physical">
<Folder name="Users/a" created="2027-01-15T08:00:00Z" modified="2027-01-15T08:00:00Z">
<File name="say &quot;hi&quot; &amp; bye" size="8192" modified="2026-01-01T00:00:00Z"/>
<Unknown><File name="ignored" size="1"/></Unknown>
<Folder name="sub" modified="2027-01-15T08:00:00Z">
<File name="x" size="4096" modified="2027-01-15T08:00:00Z" flags="2"/>
</Folder>
<Folder name="empty"/>
</Folder>
</ScanInfo>
</GrandPerspectiveScanDump>
"#;

#[test]
fn p2_cli_03_a_gpscan_reads_gzipped_or_not() {
    for input in [GPSCAN.to_vec(), gzip(GPSCAN)] {
        let mut imported = import(&input[..], None).unwrap();
        assert_eq!(imported.format, ImportFormat::Gpscan);
        assert_eq!(imported.root, b"/Users/a");
        assert_eq!(imported.scanned_at, Some(1_800_000_000_000_000_000));
        let names = names(&imported);
        assert!(names.contains(&b"say \"hi\" & bye".to_vec()));
        assert!(!names.contains(&b"ignored".to_vec()));
        let (apparent, allocated, files, dirs) = totals(&mut imported);
        assert_eq!((files, dirs), (2, 2));
        assert_eq!((apparent, allocated), (8192 + 4096, 8192 + 4096));
    }
}

#[test]
fn p2_cli_03_gzip_is_read_whatever_the_format_and_capped() {
    let mut imported = import(&gzip(&ncdu())[..], None).unwrap();
    assert_eq!(totals(&mut imported).2, 5);
    // Past the cap: refused, not read on.
    let big = gzip(&vec![b' '; 4096]);
    match import_capped(&big[..], None, 1024) {
        Err(CoreError::ImportInvalid(why)) => assert!(why.contains("cap"), "{why}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(DECOMPRESSED_CAP, 1 << 30);
    // A damaged stream.
    let mut damaged = gzip(&ncdu());
    let mid = damaged.len() / 2;
    damaged[mid] ^= 0xff;
    damaged.truncate(mid + 4);
    assert!(matches!(
        import(&damaged[..], None),
        Err(CoreError::ImportInvalid(_))
    ));
}

#[test]
fn p2_cli_03_what_is_not_a_tree_is_refused() {
    let cases: &[(&[u8], &str)] = &[
        (b"", "empty"),
        (b"hello", "neither"),
        (br#"[2,0,{},[{"name":"/"}]]"#, "major"),
        (br#"[1,0,{},[{"name":"/"}]"#, "ends early"),
        (br#"[1,0,{},[{"name":"/"}]] x"#, "follows"),
        (br#"[1,0,{},{"name":"/"}]"#, "root"),
        (br#"[1,0,{},[{"asize":1}]]"#, "no name"),
        (br#"[1,0,{},[{"name":"/"},{"name":"a/b"}]]"#, "slash"),
        (br#"[1,0,{},[{"name":"/"},{"name":".."}]]"#, "not a name"),
        (br#"[1,0,{},[{"name":"/"},{"name":"a"},{"name":"a"}]]"#, "twice"),
        (br#"[1,0,{},[{"name":"/"},{"name":"a","asize":-1}]]"#, "0 or more"),
        (br#"[1,0,{},[{"name":"/"},{"name":"a","asize":1.5}]]"#, "0 or more"),
        (br#"[1,0,{},[{"name":"/","x":[[[}]]]]"#, "bracket"),
        (
            br#"[1,0,{},[{"name":"/"},{"name":"a","asize":9223372036854775807},{"name":"b","asize":1}]]"#,
            "8 EiB",
        ),
        (br#"[1,0,{},[{"name":"/\q"}]]"#, "escape"),
        (b"<x/>", "not a GrandPerspectiveScanDump"),
        (
            br#"<GrandPerspectiveScanDump><ScanInfo><Folder name="/"><File name="f" size="x"/></Folder></ScanInfo></GrandPerspectiveScanDump>"#,
            "whole number",
        ),
        (
            br#"<GrandPerspectiveScanDump><ScanInfo><Folder name="/" modified="yesterday"></Folder></ScanInfo></GrandPerspectiveScanDump>"#,
            "RFC 3339",
        ),
        (
            br#"<GrandPerspectiveScanDump><ScanInfo><Folder name="/"></ScanInfo></GrandPerspectiveScanDump>"#,
            "malformed",
        ),
    ];
    for (input, why) in cases {
        let got = refused(input, None);
        assert!(
            got.contains(why),
            "{}: {got}",
            String::from_utf8_lossy(input)
        );
    }
    // A deep tree reads without recursing.
    let depth = 20_000;
    let mut deep = b"[1,0,{},".to_vec();
    for i in 0..depth {
        deep.extend_from_slice(
            format!(r#"[{{"name":"{}"}},"#, if i == 0 { "/" } else { "d" }).as_bytes(),
        );
    }
    deep.extend_from_slice(br#"{"name":"leaf","asize":1}"#);
    deep.extend(std::iter::repeat_n(b']', depth + 1));
    let mut imported = import(&deep[..], None).unwrap();
    let (apparent, _, files, dirs) = totals(&mut imported);
    assert_eq!((apparent, files), (1, 1));
    assert_eq!(dirs as usize, depth - 1);
}
