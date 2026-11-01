//! `summary`: a scan's totals under a path (the scan's root by default),
//! as the scan counts them and as `du` does, each volume's share, what a
//! mount point's volume uses that the scan did not find, and how the scan
//! ran. The CLI twin of the `duscope_summary` tool.

use std::path::Path;

use duscope_core::envelope::{
    ObjectEnvelope, ObjectQuery, SCHEMA_VERSION, Summary, SummaryVolume, Tool, VolumeEntry,
};
use duscope_core::query::{self, SizeKind};

use super::Failure;
use super::scan;
use super::source;
use crate::cli::SummaryArgs;
use crate::out::{Output, thousands};

/// `summary`.
pub fn run(args: &SummaryArgs, index_dir: Option<&Path>, output: &Output) -> Result<(), Failure> {
    let source = source::source(&args.walk, &args.index, index_dir)?;
    let size = scan::size_kind(&args.walk);
    let block = source.scan_block(&args.walk, size);
    let view = source.view();
    let bytes = query::summary(&*view);
    let root = view.node(0);
    let path_bytes = std::os::unix::ffi::OsStrExt::as_bytes(view.root_path().as_os_str()).to_vec();
    let path = view.root_path().to_string_lossy().into_owned();
    drop(view);
    // Each volume's share, by its device; a device met twice (a bind
    // mount) has its bytes at the first.
    let devs = source.volume_devs();
    let mut shared: Vec<u64> = Vec::new();
    let volumes: Vec<SummaryVolume> = block
        .volumes
        .iter()
        .zip(devs.iter().copied().chain(std::iter::repeat(u64::MAX)))
        .filter(|(v, dev)| {
            source.at_root()
                || bytes.volumes.iter().any(|b| b.dev == *dev)
                || Path::new(&v.mount_point).starts_with(&path)
        })
        .map(|(v, dev)| {
            let share = bytes
                .volumes
                .iter()
                .find(|b| b.dev == dev && !shared.contains(&dev))
                .copied()
                .unwrap_or_default();
            shared.push(dev);
            volume(v, share)
        })
        .collect();
    // A mount point: what its volume uses less what the scan found there.
    let unaccounted = volumes
        .iter()
        .find(|v| v.entered && v.mount_point == path)
        .and_then(|v| {
            let used = v.total_bytes?.checked_sub(v.free_bytes?)?;
            Some(used.saturating_sub(v.allocated_bytes))
        });
    let result = Summary {
        path,
        path_bytes,
        volumes,
        allocated_bytes: bytes.allocated,
        apparent_bytes: bytes.apparent,
        du_compatible_bytes: bytes.du_compatible,
        unaccounted_bytes: unaccounted,
        dataless_bytes: bytes.dataless,
        file_count: root.file_count,
        dir_count: root.dir_count,
        error_count: if source.at_root() {
            block.error_count
        } else {
            source.unreadable_dirs()
        },
        vanished_count: block.vanished_count,
        attrs: block.attrs,
        incremental: block.incremental,
        parent_scan_id: source.parent_scan_id(),
        started_at: block.started_at.clone(),
        finished_at: block.finished_at.clone(),
        duration_ms: u64::try_from(
            source.finished_ns().saturating_sub(source.started_ns()) / 1_000_000,
        )
        .unwrap_or(0),
        threads: block.thread_count,
    };
    let warnings = source.warnings(&args.walk);
    let envelope = ObjectEnvelope {
        ok: true,
        schema_version: SCHEMA_VERSION,
        tool: Tool::CURRENT,
        query: ObjectQuery {
            command: "summary",
            path: result.path.clone(),
        },
        scan: Some(block),
        result,
        warnings,
        next: Vec::new(),
    };
    source.check()?;
    let lines = people(&envelope.result, output, size);
    output.object(&envelope, &plain(&envelope.result), &lines)?;
    source.finish(&args.walk, output)
}

/// A scan block's volume, with the bytes found on it.
fn volume(v: &VolumeEntry, share: query::VolumeBytes) -> SummaryVolume {
    SummaryVolume {
        mount_point: v.mount_point.clone(),
        fs_type: v.fs_type.clone(),
        total_bytes: (v.total_bytes > 0).then_some(v.total_bytes),
        free_bytes: (v.total_bytes > 0).then_some(v.free_bytes),
        allocated_bytes: share.allocated,
        apparent_bytes: share.apparent,
        du_compatible_bytes: share.du_compatible,
        entered: v.entered,
        reason: v.reason,
    }
}

/// The answer for people: one line each, name and value.
fn people(s: &Summary, output: &Output, size: SizeKind) -> Vec<(String, String)> {
    let bytes = |n: u64| output.units.size(n);
    let shown = |allocated: u64, apparent: u64| match size {
        SizeKind::Allocated => bytes(allocated),
        SizeKind::Apparent => bytes(apparent),
    };
    let mut lines = vec![
        ("path".to_owned(), s.path.clone()),
        (
            "size".to_owned(),
            format!(
                "{} allocated · {} apparent · {} as du counts it",
                bytes(s.allocated_bytes),
                bytes(s.apparent_bytes),
                bytes(s.du_compatible_bytes)
            ),
        ),
        (
            "contents".to_owned(),
            format!(
                "{} files · {} dirs",
                thousands(s.file_count),
                thousands(s.dir_count)
            ),
        ),
    ];
    if let Some(n) = s.unaccounted_bytes {
        lines.push(("not found".to_owned(), bytes(n)));
    }
    if s.dataless_bytes > 0 {
        lines.push(("dataless".to_owned(), bytes(s.dataless_bytes)));
    }
    for v in &s.volumes {
        let state = if v.entered {
            shown(v.allocated_bytes, v.apparent_bytes)
        } else {
            format!("not entered ({})", v.reason.unwrap_or("excluded"))
        };
        let room = match (v.total_bytes, v.free_bytes) {
            (Some(total), Some(free)) => format!(" · {} free of {}", bytes(free), bytes(total)),
            _ => String::new(),
        };
        lines.push((
            "volume".to_owned(),
            format!("{} ({}) · {state}{room}", v.mount_point, v.fs_type),
        ));
    }
    if s.error_count > 0 {
        lines.push(("unreadable".to_owned(), thousands(s.error_count)));
    }
    lines.push((
        "scan".to_owned(),
        format!(
            "{} · {} ms · {} threads · attrs {}",
            s.started_at, s.duration_ms, s.threads, s.attrs
        ),
    ));
    lines
}

/// The answer for `--plain`: name and value, raw bytes for the path.
fn plain(s: &Summary) -> Vec<(&'static str, Vec<u8>)> {
    let n = |v: u64| v.to_string().into_bytes();
    vec![
        ("path", s.path_bytes.clone()),
        ("allocated_bytes", n(s.allocated_bytes)),
        ("apparent_bytes", n(s.apparent_bytes)),
        ("du_compatible_bytes", n(s.du_compatible_bytes)),
        (
            "unaccounted_bytes",
            s.unaccounted_bytes.map(n).unwrap_or_default(),
        ),
        ("dataless_bytes", n(s.dataless_bytes)),
        ("file_count", n(s.file_count)),
        ("dir_count", n(s.dir_count)),
        ("error_count", n(s.error_count)),
        ("vanished_count", n(s.vanished_count)),
        ("duration_ms", n(s.duration_ms)),
    ]
}
