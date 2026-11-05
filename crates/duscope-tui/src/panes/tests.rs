//! P4-TUI-02: the screen at the four sizes the layout rules name, drawn
//! from the canvas's example tree, and the panes' contents for its
//! selection.
//!
//! The example is the canvas's `~/Library`: its folders, files and sizes in
//! MiB, a name holding `/` made folders, each pooled entry (`… +1,842
//! more`) one file. The classes table adds what the canvas classes by hand:
//! the WAL and shared-memory files of a database are data.

use std::convert::Infallible;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use duscope_core::arena::Arena;
use duscope_core::interchange::import::{ImportFormat, import};
use duscope_core::query::{ArenaView, SizeKind};
use duscope_core::rollup::{RollUp, roll_up};
use duscope_core::rules::Classes;
use duscope_testkit::golden;
use duscope_treemap::cells::Depth;
use duscope_treemap::packs::Pack;
use duscope_treemap::paint::{ColorBy, Glyphs, PaintOpts};
use ratatui_core::backend::TestBackend;
use ratatui_core::terminal::Terminal;

use super::{list_rows, mode_text};
use crate::widgets::{full_size, thousands};
use crate::{App, run, snapshot};

/// One entry of the example: a file and its MiB, or a folder.
enum E {
    F(&'static str, u64),
    D(&'static str, Vec<E>),
}
use E::{D, F};

/// The canvas's tree, as its artboard lists it.
#[allow(clippy::too_many_lines)]
fn library() -> Vec<E> {
    vec![
        D(
            "Caches",
            vec![
                D(
                    "com.apple.Safari",
                    vec![
                        F("Cache.db-wal", 1884),
                        F("Cache.db", 932),
                        D(
                            "WebKitCache/Version 16",
                            vec![
                                D(
                                    "Blobs",
                                    vec![
                                        F("1B2F…A3", 142),
                                        F("7F3A…9C", 118),
                                        F("C04E…51", 96),
                                        F("55AD…E0", 77),
                                        F("9D1C…22", 64),
                                        F("… +1,842 more", 148),
                                    ],
                                ),
                                D("Records", vec![F("… 2,210 records", 38)]),
                            ],
                        ),
                        D("fsCachedData", vec![F("… +612 files", 112)]),
                        F("Cache.db-shm", 32),
                    ],
                ),
                D(
                    "Homebrew",
                    vec![
                        D(
                            "downloads",
                            vec![
                                F("llvm@19--19.1.2.arm64_sequoia.bottle.tar.gz", 736),
                                F("rust--1.82.0.arm64_sequoia.bottle.tar.gz", 412),
                                F("openjdk--23.0.1.arm64_sequoia.bottle.tar.gz", 338),
                                F("node--22.9.0.arm64_sequoia.bottle.tar.gz", 311),
                                F("go--1.23.2.arm64_sequoia.bottle.tar.gz", 262),
                                F("python@3.13--3.13.0.arm64_sequoia.bottle.tar.gz", 141),
                                F("ffmpeg--7.1.arm64_sequoia.bottle.tar.gz", 118),
                                F("… +37 more", 96),
                            ],
                        ),
                        D(
                            "api",
                            vec![
                                F("formula.jws.json", 190),
                                F("cask.jws.json", 84),
                                F("formula_tap_migrations.jws.json", 12),
                            ],
                        ),
                        D(
                            "Cask",
                            vec![F("docker--4.35.0,172550.dmg", 212), F("… +9 more", 40)],
                        ),
                    ],
                ),
                D(
                    "pip",
                    vec![
                        D(
                            "http-v2",
                            vec![
                                F("0-9", 231),
                                F("a", 212),
                                F("b", 198),
                                F("c", 186),
                                F("d", 171),
                                F("e", 160),
                                F("f", 149),
                            ],
                        ),
                        D(
                            "wheels",
                            vec![
                                F("torch-2.5.0-cp313-none-macosx_11_0_arm64.whl", 184),
                                F("numpy-2.1.2-cp313-cp313-macosx_14_0_arm64.whl", 61),
                                F("… +58 more", 72),
                            ],
                        ),
                    ],
                ),
                D(
                    "Google/Chrome/Default/Cache",
                    vec![
                        D(
                            "Cache_Data",
                            vec![
                                F("f_0004f2", 96),
                                F("f_00031a", 71),
                                F("f_0001c8", 58),
                                F("… +4,120 more", 812),
                                F("data_1", 128),
                                F("data_2", 64),
                                F("index", 12),
                            ],
                        ),
                        D("Code Cache", vec![F("js", 148), F("wasm", 22)]),
                        D("GPUCache", vec![F("data_1", 64)]),
                    ],
                ),
                D(
                    "ms-playwright",
                    vec![
                        D(
                            "chromium-1140",
                            vec![
                                F("Chromium Framework", 486),
                                F("Chromium", 14),
                                F("… +611 more", 96),
                            ],
                        ),
                        D(
                            "webkit-2083",
                            vec![F("WebKit.framework", 312), F("… +280 more", 48)],
                        ),
                        D("firefox-1465", vec![F("XUL", 228), F("… +90 more", 42)]),
                        D("ffmpeg-1010", vec![F("ffmpeg-mac-arm64", 86)]),
                    ],
                ),
                D(
                    "com.spotify.client/Data",
                    vec![F("… 2,311 audio chunks", 920)],
                ),
                D("Yarn/v6", vec![F("… 19,877 packages", 512)]),
                D(
                    "JetBrains/IntelliJIdea2024.2",
                    vec![F("caches", 240), F("index", 180)],
                ),
                D(
                    "node-gyp/22.9.0/include/node",
                    vec![F("v8 headers", 48), F("openssl", 22), F("uv", 6)],
                ),
                F("… +84 more caches", 690),
            ],
        ),
        D(
            "Application Support",
            vec![
                D(
                    "Code",
                    vec![
                        D(
                            "CachedExtensionVSIXs",
                            vec![
                                F("ms-python.python-2024.16.0", 212),
                                F("ms-vscode.cpptools-1.22.11", 198),
                                F("rust-lang.rust-analyzer-0.3.2146", 141),
                                F("… +31 more", 103),
                            ],
                        ),
                        F("Cache", 420),
                        F("User/workspaceStorage", 512),
                        F("CachedData/1.94.2", 310),
                        F("logs", 290),
                    ],
                ),
                D(
                    "Google/Chrome/Default",
                    vec![
                        F("Service Worker/CacheStorage", 712),
                        F("IndexedDB", 498),
                        D(
                            "Extensions",
                            vec![
                                F("cjpalhdlnbpafiamejdnhcphjbkeiagm", 44),
                                F("… +22 more", 612),
                            ],
                        ),
                        F("History", 112),
                        F("Favicons", 38),
                        F("Cookies", 22),
                    ],
                ),
                D(
                    "Slack",
                    vec![
                        F("Cache", 920),
                        F("Service Worker/CacheStorage", 512),
                        F("logs", 188),
                        F("storage", 64),
                    ],
                ),
                D(
                    "Steam",
                    vec![
                        F("Steam.AppBundle", 1324),
                        F("appcache", 412),
                        F("logs", 96),
                    ],
                ),
                D(
                    "Spotify/PersistentCache/Storage",
                    vec![F("… 4,201 chunks", 1130)],
                ),
                D(
                    "Adobe",
                    vec![F("Common/Media Cache Files", 820), F("… +410 more", 120)],
                ),
                D("Zoom", vec![F("data", 310), F("… +1,200 more", 400)]),
                D("discord", vec![F("Cache", 420), F("Code Cache", 80)]),
                F("… +212 more apps", 720),
            ],
        ),
        D(
            "Developer",
            vec![
                D(
                    "Xcode",
                    vec![
                        D(
                            "iOS DeviceSupport",
                            vec![
                                D(
                                    "18.0 (22A3354) arm64e/Symbols",
                                    vec![
                                        F("System/Library/Frameworks", 1420),
                                        F("usr/lib/dyld", 212),
                                        F("… +3,100 more", 48),
                                    ],
                                ),
                                D(
                                    "17.5 (21F79) arm64e/Symbols",
                                    vec![
                                        F("System/Library/Frameworks", 1380),
                                        F("usr/lib/dyld", 198),
                                        F("… +2,980 more", 34),
                                    ],
                                ),
                                F("16.7.10 (20H350) arm64e", 118),
                            ],
                        ),
                        D(
                            "DerivedData",
                            vec![
                                D(
                                    "MyApp-fkqzbwpxhgrmuld",
                                    vec![
                                        F("Build/Intermediates.noindex", 1380),
                                        F("Index.noindex/DataStore", 612),
                                        F("Build/Products", 210),
                                    ],
                                ),
                                F("ModuleCache.noindex", 720),
                                F("… +4 projects", 190),
                            ],
                        ),
                        D(
                            "Archives",
                            vec![
                                F("MyApp 2026-10-14 09.41.xcarchive", 298),
                                F("MyApp 2026-09-30 14.02.xcarchive", 320),
                            ],
                        ),
                        F("UserData", 180),
                    ],
                ),
                D(
                    "CoreSimulator/Caches/dyld",
                    vec![F("iOS 18.0 dyld_sim shared cache", 812)],
                ),
            ],
        ),
        D(
            "Containers",
            vec![
                D(
                    "com.docker.docker",
                    vec![
                        F("Data/vms/0/data/Docker.raw", 8912),
                        D("Data/log", vec![F("host", 212), F("vm", 96)]),
                    ],
                ),
                D("com.microsoft.teams2", vec![F("Data/Library/Caches", 612)]),
                D("com.apple.Safari", vec![F("Data/Library/Safari", 180)]),
                F("… +118 more containers", 420),
            ],
        ),
        D(
            "Mail/V10",
            vec![
                D(
                    "IMAP-me@icloud.com/INBOX.mbox",
                    vec![F("Messages (.emlx)", 1980), F("Attachments", 400)],
                ),
                D(
                    "MailData",
                    vec![F("Envelope Index", 220), F("Envelope Index-wal", 180)],
                ),
            ],
        ),
        D(
            "Group Containers",
            vec![
                D(
                    "group.com.apple.notes",
                    vec![F("NoteStore.sqlite", 180), F("Media", 640)],
                ),
                D(
                    "UBF8T346G9.Office",
                    vec![F("Outlook", 320), F("… +90 more", 160)],
                ),
            ],
        ),
        D("Metadata/CoreSpotlight", vec![F("index.spotlightV3", 580)]),
        D("Messages/Attachments", vec![F("… 3,310 attachments", 512)]),
        D(
            "Logs",
            vec![
                F("DiagnosticReports", 120),
                F("CoreSimulator", 210),
                F("… +40 more", 90),
            ],
        ),
        F("… +24 more folders", 310),
    ]
}

/// A folder of the example as built: its folders (each name once) and its
/// files, in the order the canvas lists them.
#[derive(Default)]
struct Dir {
    dirs: Vec<(String, Dir)>,
    files: Vec<(String, u64)>,
}

impl Dir {
    /// The folder at `path` under this one, made where missing.
    fn at(&mut self, path: &[&str]) -> &mut Dir {
        let Some((first, rest)) = path.split_first() else {
            return self;
        };
        let i = if let Some(i) = self.dirs.iter().position(|(n, _)| n == first) {
            i
        } else {
            self.dirs.push(((*first).to_owned(), Dir::default()));
            self.dirs.len() - 1
        };
        self.dirs[i].1.at(rest)
    }

    fn add(&mut self, entries: Vec<E>) {
        for entry in entries {
            match entry {
                F(name, mib) => {
                    let parts: Vec<&str> = name.split('/').collect();
                    let (file, dirs) = parts.split_last().unwrap();
                    self.at(dirs).files.push(((*file).to_owned(), mib << 20));
                }
                D(name, kids) => {
                    let parts: Vec<&str> = name.split('/').collect();
                    self.at(&parts).add(kids);
                }
            }
        }
    }

    /// The folder's children as ncdu's export writes them.
    fn json(&self, out: &mut String) {
        for (name, bytes) in &self.files {
            // The canvas's one dated file, the one it opens on.
            let mtime = if name == "Cache.db-wal" {
                ",\"mtime\":1792624440"
            } else {
                ""
            };
            let _ = write!(
                out,
                r#",{{"name":"{name}","asize":{bytes},"dsize":{bytes}{mtime}}}"#
            );
        }
        for (name, dir) in &self.dirs {
            let _ = write!(out, r#",[{{"name":"{name}"}}"#);
            dir.json(out);
            out.push(']');
        }
    }
}

/// The example scanned from `/Users/alex/Library`.
pub(crate) fn mockup() -> (Arena, RollUp) {
    let mut root = Dir::default();
    root.add(library());
    let mut json = String::from(r#"[1,2,{"progname":"ncdu"},[{"name":"/Users/alex/Library"}"#);
    root.json(&mut json);
    json.push_str("]]");
    let mut arena = import(json.as_bytes(), Some(ImportFormat::Ncdu))
        .unwrap()
        .arena;
    let rollup = roll_up(&mut arena);
    (arena, rollup)
}

pub(crate) fn classes() -> Classes {
    let user = Classes::parse(
        "[ext]\n\"db-wal\" = \"data\"\n\"db-shm\" = \"data\"\n",
        "test",
    )
    .unwrap();
    Classes::vendored().merged(user)
}

pub(crate) fn opts<'a>(pack: &'a Pack, classes: &'a Classes, color_by: ColorBy) -> PaintOpts<'a> {
    PaintOpts {
        pack,
        color_by,
        glyphs: Glyphs::Unicode,
        depth: None,
        min_tile: 1,
        pool_files: false,
        size: SizeKind::Allocated,
        now: 0,
        classes,
    }
}

/// The screen of `app` at `w`×`h`, as snapshot text.
fn screen(app: &mut App<'_>, w: u16, h: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
    let mut events = vec![Event::Key(KeyEvent::new(
        KeyCode::Char('q'),
        KeyModifiers::NONE,
    ))]
    .into_iter();
    run(&mut terminal, app, || -> Result<Event, Infallible> {
        Ok(events.next().unwrap())
    })
    .unwrap();
    snapshot(terminal.backend().buffer())
}

/// The status the binary writes for a scan of the example: its total,
/// its counts, how long the scan took.
pub(crate) fn status(view: &ArenaView<'_>) -> String {
    use duscope_core::query::TreeView;
    let root = view.node(0);
    format!(
        "{} · {} files · {} dirs · scanned in 1.92s ✓",
        full_size(root.allocated),
        thousands(root.file_count),
        thousands(root.dir_count - 1)
    )
}

#[test]
fn p4_tui_02_snapshots_at_the_four_sizes_follow_the_layout_rules() {
    let tree = mockup();
    let view = ArenaView::new(&tree.0, &tree.1, PathBuf::from("/Users/alex/Library"));
    let (mocha, classes) = (Pack::named("mocha").unwrap(), classes());
    for (w, h) in [(160, 45), (120, 40), (100, 30), (80, 24)] {
        let mut app = App::new(
            &view,
            opts(&mocha, &classes, ColorBy::Type),
            Depth::TrueColor,
            Some(Path::new("/Users/alex")),
            status(&view),
            None,
        );
        golden(
            &format!("view_mockup_{w}x{h}.snap"),
            &screen(&mut app, w, h),
        );
    }
}

/// The rows of a snapshot's text, without the styles.
fn rows(snapshot: &str) -> Vec<String> {
    snapshot
        .split("--- styles")
        .next()
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect()
}

/// Columns `from..to` of `row`, trimmed at the end.
fn cols(row: &str, from: usize, to: usize) -> String {
    row.chars()
        .skip(from)
        .take(to - from)
        .collect::<String>()
        .trim_end()
        .to_owned()
}

#[test]
fn p4_tui_02_the_mockups_tree_renders_with_its_pane_contents() {
    let tree = mockup();
    let view = ArenaView::new(&tree.0, &tree.1, PathBuf::from("/Users/alex/Library"));
    let (mocha, classes) = (Pack::named("mocha").unwrap(), classes());
    let mut app = App::new(
        &view,
        opts(&mocha, &classes, ColorBy::Type),
        Depth::TrueColor,
        Some(Path::new("/Users/alex")),
        status(&view),
        None,
    );
    let text = screen(&mut app, 160, 45);
    let rows = rows(&text);
    // The header: the badge, the path, the tabs; the status at the end.
    assert!(
        rows[0].starts_with(" duscope  ~/Library   1 treemap  2 list  3 largest  4 types "),
        "{}",
        rows[0]
    );
    assert!(
        rows[0].ends_with(" 48.66 GiB · 126 files · 105 dirs · scanned in 1.92s ✓ "),
        "{}",
        rows[0]
    );
    // The breadcrumbs to the canvas's selection, the largest file.
    assert_eq!(
        rows[1].trim_end(),
        " ~ › Library › Caches › com.apple.Safari › Cache.db-wal"
    );
    // The map's pane from column 0 to 113, the right column's from 115.
    assert!(
        rows[2].starts_with("╭─ treemap · ~/Library · color: file type ─"),
        "{}",
        rows[2]
    );
    assert_eq!(rows[2].chars().nth(113), Some('╮'));
    assert!(cols(&rows[2], 115, 160).starts_with("╭─ selected ─"));
    // The selected pane, as the canvas fills it for Cache.db-wal.
    let selected: Vec<String> = (3..13).map(|y| cols(&rows[y], 116, 159)).collect();
    assert_eq!(
        selected,
        [
            "Cache.db-wal",
            "~/Library/Caches/com.apple.Safari",
            "",
            "size      1.84 GiB",
            "files     1",
            "parent    ████████░░░░░░░░  51.7%",
            "scan      █░░░░░░░░░░░░░░░   3.8%",
            "type      ● data · db, cache, blobs, json",
            "modified  2026-10-21 23:14",
            "mode      —",
        ]
    );
    // The siblings: com.apple.Safari's children, largest first.
    assert!(
        cols(&rows[14], 115, 160).starts_with("╭─ in com.apple.Safari · 3.56 GiB · 11 fi… ─╮"),
        "{}",
        rows[14]
    );
    let siblings: Vec<String> = (15..20).map(|y| cols(&rows[y], 116, 159)).collect();
    assert_eq!(
        siblings,
        [
            "▸ ■ Cache.db-wal       1.8G ███░░░  52%",
            "  ■ Cache.db           932M ██░░░░  26%",
            "  ■ WebKitCache/       683M █░░░░░  19%",
            "  ■ fsCachedData/      112M ░░░░░░   3%",
            "  ■ Cache.db-shm        32M ░░░░░░   1%",
        ]
    );
    // The legend: the classes' shares of the scan, largest first. The
    // canvas's pooled entries carry no extension, so other leads.
    assert!(cols(&rows[34], 115, 160).starts_with("╭─ by type ─"));
    let legend: Vec<String> = (35..39).map(|y| cols(&rows[y], 116, 159)).collect();
    assert_eq!(
        legend,
        [
            "■ other         31G █████░░░  64%",
            "■ data          12G ██░░░░░░  25%",
            "■ binaries     3.0G ░░░░░░░░   6%",
            "■ archives     2.7G ░░░░░░░░   6%",
        ]
    );
    // The key line.
    assert_eq!(
        rows[44].trim_end(),
        " ←↓↑→ move   ⏎ open   ⌫ up   z zoom   Z out   q quit"
    );
}

#[test]
fn p4_tui_02_the_legend_follows_the_colour_mode() {
    let tree = mockup();
    let view = ArenaView::new(&tree.0, &tree.1, PathBuf::from("/Users/alex/Library"));
    let (mocha, classes) = (Pack::named("mocha").unwrap(), classes());
    let mut app = App::new(
        &view,
        opts(&mocha, &classes, ColorBy::Folder),
        Depth::TrueColor,
        Some(Path::new("/Users/alex")),
        status(&view),
        None,
    );
    let rows = rows(&screen(&mut app, 160, 45));
    assert!(
        rows[2].starts_with("╭─ treemap · ~/Library · color: top folder ─"),
        "{}",
        rows[2]
    );
    assert!(cols(&rows[34], 115, 160).starts_with("╭─ by top folder ─"));
    // The largest top-level folders first, seven and `… n more`.
    let legend: Vec<String> = (35..43).map(|y| cols(&rows[y], 116, 128)).collect();
    assert_eq!(
        legend,
        [
            "■ Caches",
            "■ Applicati…",
            "■ Containers",
            "■ Developer",
            "■ Mail",
            "■ Group Con…",
            "■ Metadata",
            "■ … 3 more",
        ]
    );
}

#[test]
fn p4_tui_02_a_list_shows_what_fits_then_n_more() {
    // Nine siblings at most, then `… n more`; in fewer rows, one row
    // fewer and `… n more`.
    assert_eq!(list_rows(5, 9, 18), 5);
    assert_eq!(list_rows(9, 9, 18), 9);
    assert_eq!(list_rows(14, 9, 18), 9);
    assert_eq!(list_rows(10, 9, 10), 9);
    assert_eq!(list_rows(14, 9, 4), 3);
    assert_eq!(list_rows(3, 9, 3), 3);
}

#[test]
fn p4_tui_02_the_mode_reads_as_ls_writes_it() {
    assert_eq!(mode_text(Some(0o644), false), "-rw-r--r--");
    assert_eq!(mode_text(Some(0o755), true), "drwxr-xr-x");
    assert_eq!(mode_text(Some(0o4755), false), "-rwsr-xr-x");
    assert_eq!(mode_text(Some(0o2644), false), "-rw-r-Sr--");
    assert_eq!(mode_text(Some(0o1777), true), "drwxrwxrwt");
    assert_eq!(mode_text(None, false), "—");
}

/// A small tree from `(path, MiB)` files, scanned from `root`.
fn small(root: &str, files: &[(&str, u64)]) -> (Arena, RollUp) {
    let mut dir = Dir::default();
    for &(path, mib) in files {
        let parts: Vec<&str> = path.split('/').collect();
        let (file, dirs) = parts.split_last().unwrap();
        dir.at(dirs).files.push(((*file).to_owned(), mib << 20));
    }
    let mut json = format!(r#"[1,2,{{"progname":"ncdu"}},[{{"name":"{root}"}}"#);
    dir.json(&mut json);
    json.push_str("]]");
    let mut arena = import(json.as_bytes(), Some(ImportFormat::Ncdu))
        .unwrap()
        .arena;
    let rollup = roll_up(&mut arena);
    (arena, rollup)
}

/// The legend `opts` gives `view`.
fn legend_of(
    view: &ArenaView<'_>,
    opts: &PaintOpts<'_>,
    classes: &Classes,
) -> (&'static str, Vec<(String, u64)>) {
    use super::{Context, legend_rows, ranks};
    use std::cell::RefCell;
    let class_of = duscope_treemap::paint::classes_of(view, classes);
    let ranks = ranks(view, opts);
    let dominant = RefCell::default();
    let (title, rows) = legend_rows(&Context {
        view,
        opts,
        class_of: &class_of,
        dominant: &dominant,
        ranks: &ranks,
        root: "/r",
        root_parent: "/",
    });
    (
        title,
        rows.into_iter().map(|r| (r.label, r.bytes >> 20)).collect(),
    )
}

#[test]
fn p4_tui_02_folder_colours_follow_the_maps_tiles_under_pool_files() {
    use super::ranks;
    use duscope_core::query::{TreeView, children};
    let tree = small(
        "/r",
        &[("a/x.bin", 10), ("b/y.bin", 5), ("f1", 3), ("f2", 3)],
    );
    let view = ArenaView::new(&tree.0, &tree.1, PathBuf::from("/r"));
    let (mocha, classes) = (Pack::named("mocha").unwrap(), Classes::vendored());
    let id = |name: &str| {
        children(&view, &view.node(0))
            .find(|&id| view.name(id).as_ref() == name.as_bytes())
            .unwrap()
    };
    // Each child a tile: a, b, then the files, by size.
    let plain = ranks(&view, &opts(&mocha, &classes, ColorBy::Folder));
    assert_eq!((plain.by_id[&id("a")], plain.by_id[&id("b")]), (0, 1));
    assert_eq!(plain.files, None);
    // Under --pool-files the files are one tile of 6 MiB, between a and b,
    // and the legend lists it as the map draws it.
    let pooled_opts = PaintOpts {
        pool_files: true,
        ..opts(&mocha, &classes, ColorBy::Folder)
    };
    let pooled = ranks(&view, &pooled_opts);
    assert_eq!((pooled.by_id[&id("a")], pooled.by_id[&id("b")]), (0, 2));
    assert_eq!(pooled.files, Some(1));
    assert!(!pooled.by_id.contains_key(&id("f1")));
    let (title, rows) = legend_of(&view, &pooled_opts, &classes);
    assert_eq!(title, "by top folder");
    assert_eq!(
        rows,
        [
            ("a".to_owned(), 10),
            ("files".to_owned(), 6),
            ("b".to_owned(), 5)
        ]
    );
}

#[test]
fn p4_tui_02_the_legend_shows_the_colours_ls_colors_gives() {
    use duscope_treemap::cells::Color;
    use duscope_treemap::paint::LsColors;
    let tree = small("/r", &[("a.dmg", 8), ("b.dmg", 2), ("c.txt", 4)]);
    let view = ArenaView::new(&tree.0, &tree.1, PathBuf::from("/r"));
    let (mocha, classes) = (Pack::named("mocha").unwrap(), Classes::vendored());
    let table = LsColors {
        executable: None,
        suffixes: vec![(".dmg".to_owned(), Color::Ansi(1))],
    };
    let ls = opts(&mocha, &classes, ColorBy::LsColors(table));
    let (title, rows) = legend_of(&view, &ls, &classes);
    assert_eq!(title, "by LS_COLORS");
    // The .dmg files in LS_COLORS' red, the text file in its class colour.
    assert_eq!(rows, [("*.dmg".to_owned(), 10), ("docs".to_owned(), 4)]);
}

#[test]
fn p4_tui_02_the_depth_legend_counts_below_the_cap_with_the_folder() {
    let tree = small(
        "/r",
        &[("a/b/c/deep.bin", 6), ("a/top.bin", 2), ("x.bin", 1)],
    );
    let view = ArenaView::new(&tree.0, &tree.1, PathBuf::from("/r"));
    let (mocha, classes) = (Pack::named("mocha").unwrap(), Classes::vendored());
    let level = opts(&mocha, &classes, ColorBy::Level);
    let (_, rows) = legend_of(&view, &level, &classes);
    assert_eq!(
        rows,
        [
            ("depth 4".to_owned(), 6),
            ("depth 2".to_owned(), 2),
            ("depth 1".to_owned(), 1)
        ]
    );
    // At --depth 2 the folder b is drawn whole, in the depth-2 colour.
    let capped = PaintOpts {
        depth: Some(2),
        ..level
    };
    let (_, rows) = legend_of(&view, &capped, &classes);
    assert_eq!(rows, [("depth 2".to_owned(), 8), ("depth 1".to_owned(), 1)]);
}

#[test]
fn p4_tui_02_the_roots_parent_is_the_folder_it_is_in() {
    use super::{Context, ranks, selected};
    use crate::widgets::Canvas;
    use ratatui_core::layout::Rect;
    use std::cell::RefCell;
    let tree = small("/Users/alex", &[("a.txt", 1)]);
    let view = ArenaView::new(&tree.0, &tree.1, PathBuf::from("/Users/alex"));
    let (mocha, classes) = (Pack::named("mocha").unwrap(), Classes::vendored());
    let opts = opts(&mocha, &classes, ColorBy::Type);
    let class_of = duscope_treemap::paint::classes_of(&view, &classes);
    let ranks = ranks(&view, &opts);
    let dominant = RefCell::default();
    let ctx = Context {
        view: &view,
        opts: &opts,
        class_of: &class_of,
        dominant: &dominant,
        ranks: &ranks,
        root: "~",
        root_parent: "/Users",
    };
    let mut canvas = Canvas::new(43, 10, &mocha, Glyphs::Unicode);
    selected(&mut canvas, Rect::new(0, 0, 43, 10), &ctx, 0);
    let row: String = (0..43)
        .filter_map(|x| canvas.cells.get(x, 1).map(|c| c.ch))
        .collect();
    assert_eq!(row.trim_end(), "/Users");
}
