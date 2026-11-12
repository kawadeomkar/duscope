//! P5-CORE-01: `rescan`'s modes on this platform, and, among the platform
//! tests (macOS), the incremental rescan on the real `FSEvents` history: a
//! copy of the fixture tree on a disk image, changed by a script of creates,
//! deletes, renames, links, clones and `chmod`s, rescanned from its history
//! into an index equal to a full scan's in every row.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::fs;
use std::path::Path;
use std::process::Output;

use duscope_core::query::timegrammar;
use duscope_testkit::TestEnv;
use serde_json::Value;

fn run(env: &TestEnv, args: &[&str]) -> Output {
    env.cmd().args(args).output().unwrap()
}

fn ok(out: &Output) -> Value {
    assert!(
        out.status.success(),
        "exit {:?}: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

/// Saves a scan of `root` and returns its id.
fn save(env: &TestEnv, root: &Path) -> String {
    let saved = ok(&run(
        env,
        &["scan", root.to_str().unwrap(), "--save", "--json"],
    ));
    saved["scan"]["id"].as_str().unwrap().to_owned()
}

/// `rescan root` with `args`, `hours` after the environment's clock.
fn rescan(env: &TestEnv, root: &Path, hours: i64, args: &[&str]) -> Output {
    let now = timegrammar::parse_date(env.test_now()).unwrap().ns;
    env.cmd()
        .env(
            "DUSCOPE_TEST_NOW",
            timegrammar::iso8601(now + hours * 3_600_000_000_000),
        )
        .arg("rescan")
        .arg(root)
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn p5_core_01_the_rescan_modes_and_their_scan_block() {
    let env = TestEnv::new();
    let root = fs::canonicalize(env.tmp_dir()).unwrap().join("tree");
    for dir in ["a", "b/c"] {
        fs::create_dir_all(root.join(dir)).unwrap();
        fs::write(root.join(dir).join("f"), b"bytes").unwrap();
    }
    let parent = save(&env, &root);
    let history = duscope_walk::fsevents::current_event_id().is_some();
    // Asked for the whole tree: no reason to give.
    let full = ok(&rescan(&env, &root, 1, &["--full", "--json"]));
    assert_eq!(full["scan"]["parent_scan_id"], parent.as_str());
    assert_eq!(full["scan"]["incremental"], false);
    assert_eq!(full["scan"]["rescanned_dirs"], Value::Null);
    assert_eq!(full["scan"]["fallback_reason"], Value::Null);
    let incremental = rescan(&env, &root, 2, &["--incremental", "--json"]);
    let table = rescan(&env, &root, 2, &["--format", "table"]);
    assert!(table.status.success());
    let footer = String::from_utf8(table.stdout).unwrap();
    let footer = footer.lines().last().unwrap();
    if history {
        // A saved scan older than --max-age: the whole tree, and why; and
        // --incremental fails rather than read it. Each rescan renews the
        // newest scan of the root, the one before it.
        let old = ok(&rescan(&env, &root, 10, &["--max-age", "2h", "--json"]));
        assert_eq!(old["scan"]["incremental"], false);
        assert_eq!(old["scan"]["fallback_reason"], "index_too_old");
        let refused = rescan(
            &env,
            &root,
            40,
            &["--incremental", "--max-age", "1d", "--json"],
        );
        assert_eq!(refused.status.code(), Some(2));
        let error: Value = serde_json::from_slice(&refused.stdout).unwrap();
        assert_eq!(error["error"]["code"], "not_available");
        assert!(
            error["error"]["message"]
                .as_str()
                .unwrap()
                .contains("index_too_old"),
            "{error}"
        );
        let table = rescan(&env, &root, 60, &["--max-age", "1d", "--format", "table"]);
        let text = String::from_utf8(table.stdout).unwrap();
        assert!(
            text.contains(" · full rescan: index too old · rescan of saved scan "),
            "{text}"
        );
        // Within the age the footer says how much was read again, or why
        // the whole tree was: a tree this small and this new is often
        // still in the history's newest chunk, all of it reported changed.
        let read_again = footer
            .split(" · ")
            .find(|part| part.starts_with("rescanned "))
            .map(|part| part.split(' ').collect::<Vec<_>>());
        match read_again {
            Some(words) => {
                assert_eq!(words[2], "of", "{footer}");
                assert_eq!(words[4..6], ["directories", "in"], "{footer}");
                assert!(
                    footer.contains(" s (FSEvents) · rescan of saved scan "),
                    "{footer}"
                );
            }
            None => assert!(footer.contains(" · full rescan: "), "{footer}"),
        }
        // Within the age, --incremental reads what the history reports or
        // says which check failed; either way the block says which.
        if incremental.status.success() {
            let answer: Value = serde_json::from_slice(&incremental.stdout).unwrap();
            assert_eq!(answer["scan"]["incremental"], true);
            assert!(answer["scan"]["rescanned_dirs"].as_u64().unwrap() >= 1);
            assert_eq!(answer["scan"]["fallback_reason"], Value::Null);
        } else {
            assert_eq!(incremental.status.code(), Some(2));
        }
    } else {
        // No change history to read (Linux): --incremental is not
        // available, and a rescan reads the whole tree with no reason to
        // give, since no check failed.
        assert_eq!(incremental.status.code(), Some(2));
        let error: Value = serde_json::from_slice(&incremental.stdout).unwrap();
        assert_eq!(error["error"]["code"], "not_available");
        assert!(
            error["error"]["hint"]
                .as_str()
                .unwrap()
                .contains("duscope diff"),
            "{error}"
        );
        let auto = ok(&rescan(&env, &root, 3, &["--json"]));
        assert_eq!(auto["scan"]["incremental"], false);
        assert_eq!(auto["scan"]["fallback_reason"], Value::Null);
        assert_eq!(auto["scan"]["rescanned_dirs"], Value::Null);
        // The table's rescan renewed the newest scan, the --full one's.
        let renewed = footer
            .rsplit(" · ")
            .next()
            .and_then(|last| last.strip_prefix("rescan of saved scan "))
            .unwrap_or_default();
        assert!(
            renewed.len() >= 4
                && full["scan"]["id"].as_str().unwrap().starts_with(renewed)
                && !footer.contains("full rescan:")
                && !footer.contains("(FSEvents)"),
            "{footer}"
        );
    }
    // A wrong --max-age costs no scan.
    let out = rescan(&env, &root, 4, &["--max-age", "5m", "--json"]);
    assert_eq!(out.status.code(), Some(2));
}

#[cfg(target_os = "macos")]
mod macos {
    use std::collections::BTreeSet;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    use duscope_testkit::TestEnv;
    use duscope_walk::FsEventsMark;
    use duscope_walk::fsevents::{REPLAY_TIMEOUT, replay, volume_uuid};
    use rusqlite::types::Value as Sql;
    use rusqlite::{Connection, OpenFlags};
    use serde_json::Value;

    use super::{ok, rescan, run, save};

    fn platform_tests() -> bool {
        if std::env::var_os("DUSCOPE_TEST_PLATFORM").is_none_or(|v| v != "1") {
            eprintln!("skipped: set DUSCOPE_TEST_PLATFORM=1");
            return false;
        }
        true
    }

    /// Detaches a disk image however the test ends, forced, as the other
    /// image tests' detaches are.
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

    /// The device of the directory at `path`, as the walk reads it.
    fn dev_of(path: &Path) -> u64 {
        use std::os::unix::fs::MetadataExt;
        fs::metadata(path).unwrap().dev()
    }

    /// Waits, for at most the replay's timeout, until `ready` holds.
    fn wait_for(what: &str, mut ready: impl FnMut() -> bool) {
        let deadline = Instant::now() + REPLAY_TIMEOUT;
        while !ready() {
            assert!(Instant::now() < deadline, "waited too long for {what}");
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// Attaches `image` at `mount`, browsable (fseventsd keeps no history of
    /// a volume attached with `-nobrowse`), and waits for its history.
    fn attach(env: &TestEnv, image: &Path, mount: &Path) {
        env.hdiutil(&[&"attach", &"-mountpoint", &mount, &image]);
        wait_for("the image's FSEvents history", || {
            volume_uuid(dev_of(mount)).is_some()
        });
    }

    /// An APFS disk image of `size` at `work/image.sparseimage`.
    fn image(env: &TestEnv, work: &Path, size: &str) -> PathBuf {
        let image = work.join("image.sparseimage");
        env.hdiutil(&[
            &"create",
            &"-ov",
            &"-size",
            &size,
            &"-type",
            &"SPARSE",
            &"-fs",
            &"APFS",
            &"-volname",
            &"DUSCOPE",
            &work.join("image"),
        ]);
        image
    }

    /// The index file of the saved scan `id`.
    fn index(env: &TestEnv, id: &str) -> Connection {
        let path = env.index_dir().join("scans").join(format!("{id}.db"));
        Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap()
    }

    /// Every row of `table`, in `order`.
    fn rows(db: &Connection, table: &str, order: &str) -> Vec<Vec<Sql>> {
        let mut stmt = db
            .prepare(&format!("SELECT * FROM {table} ORDER BY {order}"))
            .unwrap();
        let width = stmt.column_count();
        stmt.query_map([], |row| (0..width).map(|i| row.get::<_, Sql>(i)).collect())
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    /// The names of `table`'s columns, in order.
    fn columns(db: &Connection, table: &str) -> Vec<String> {
        let stmt = db
            .prepare(&format!("SELECT * FROM {table} LIMIT 0"))
            .unwrap();
        stmt.column_names().map(str::to_owned).collect()
    }

    /// A value for a failure's message, a blob as text.
    fn shown(value: &Sql) -> String {
        match value {
            Sql::Blob(bytes) => format!("{:?}", String::from_utf8_lossy(bytes)),
            other => format!("{other:?}"),
        }
    }

    /// How `got` differs from `want`, rows of `table` with these columns:
    /// the row counts, then each differing row (at most twenty) by its id
    /// and name, with the columns that differ.
    fn differing(table: &str, columns: &[String], got: &[Vec<Sql>], want: &[Vec<Sql>]) -> String {
        let named = columns.iter().position(|c| c == "name");
        let mut lines = vec![format!(
            "{table}: {} rows against a full scan's {}",
            got.len(),
            want.len()
        )];
        let rows = got
            .iter()
            .zip(want)
            .enumerate()
            .filter(|(_, (a, b))| a != b);
        for (at, (a, b)) in rows.take(20) {
            let changed: Vec<String> = columns
                .iter()
                .zip(a.iter().zip(b))
                .filter(|(_, (x, y))| x != y)
                .map(|(column, (x, y))| format!("{column} {} against {}", shown(x), shown(y)))
                .collect();
            let name = named.map(|i| shown(&b[i])).unwrap_or_default();
            lines.push(format!(
                "  row {at} ({} {name}): {}",
                shown(&b[0]),
                changed.join("; ")
            ));
        }
        lines.join("\n")
    }

    /// The mark the saved scan `id` recorded for the volume on `dev`.
    fn mark_of(db: &Connection, dev: u64) -> FsEventsMark {
        db.query_row(
            "SELECT fsevents_uuid, fsevents_event_id, os_build FROM scan_volumes WHERE dev = ?1",
            [dev.cast_signed()],
            |row| {
                Ok(FsEventsMark {
                    uuid: row.get(0)?,
                    event_id: row.get::<_, i64>(1)?.cast_unsigned(),
                    os_build: row.get(2)?,
                })
            },
        )
        .unwrap()
    }

    fn sh(env: &TestEnv, program: &str, args: &[&Path]) {
        let out = env.command(program).args(args).output().unwrap();
        assert!(
            out.status.success(),
            "{program}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// The script: creates, deletes, renames, links, clones and chmods on
    /// the tree at `tree`; returns the directories `FSEvents` reports for
    /// them.
    fn change(env: &TestEnv, tree: &Path) -> BTreeSet<PathBuf> {
        let t = |rel: &str| tree.join(rel);
        fs::write(t("home/Documents/new.txt"), b"hello\n").unwrap();
        fs::create_dir_all(t("home/Documents/newproj/src")).unwrap();
        fs::write(t("home/Documents/newproj/src/main.rs"), b"fn main() {}\n").unwrap();
        fs::remove_file(t("home/Downloads/notes.txt")).unwrap();
        fs::remove_dir_all(t("cases/extensions")).unwrap();
        fs::rename(t("home/Music"), t("home/Documents/Music2")).unwrap();
        fs::rename(t("home/Documents/graph.md"), t("home/Documents/graph2.md")).unwrap();
        // A link of a linked file; a first link, elsewhere, of a file that
        // had one, in a directory nothing else changes (its directory's
        // entry changed, FSEvents says: its link count and ctime did); the
        // first link of a group removed; a write through one link of a
        // group, whose other links are read again.
        fs::hard_link(
            t("cases/hard-links/pair/one.txt"),
            t("home/Documents/one-link.txt"),
        )
        .unwrap();
        fs::hard_link(
            t("home/Projects/rust-cli/src/main.rs"),
            t("cases/empty/main-link.rs"),
        )
        .unwrap();
        fs::remove_file(t("cases/hard-links/a/original.bin")).unwrap();
        // The script grows no file: APFS gives a file that grows into a new
        // block the rest of a 64 KiB chunk, and releases it at a time of its
        // own (on a CI runner, between the rescan and the full scan after
        // it, 15 blocks apart). Writing in place and shortening allocate
        // nothing.
        let mut linked = fs::OpenOptions::new()
            .write(true)
            .open(t("cases/hard-links/b/other-dir.bin"))
            .unwrap();
        std::io::Write::write_all(&mut linked, b"more").unwrap();
        drop(linked);
        // A clone joining a group, a member leaving it, and a new group from
        // a file that shared nothing, in a directory nothing else changes
        // (FSEvents records the clone at both).
        sh(
            env,
            "cp",
            &[
                Path::new("-c"),
                &t("cases/clones/original.img"),
                &t("home/Movies/clone-3.img"),
            ],
        );
        fs::remove_file(t("cases/clones/clone-1.img")).unwrap();
        sh(
            env,
            "cp",
            &[
                Path::new("-c"),
                &t("home/Projects/php-app/composer.lock"),
                &t("cases/special/composer-clone.lock"),
            ],
        );
        fs::set_permissions(
            t("home/Documents/velvet.dat"),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        fs::set_permissions(t("home/Pictures/field"), fs::Permissions::from_mode(0o700)).unwrap();
        // A file whose size changes.
        let pdf = fs::OpenOptions::new()
            .write(true)
            .open(t("home/Downloads/invoice-2036-11.pdf"))
            .unwrap();
        pdf.set_len(pdf.metadata().unwrap().len() - 4).unwrap();
        drop(pdf);
        // The directories FSEvents reports for them, measured on macOS 26.6:
        // a link's or a clone's source among them, which the rescan reads
        // only because the history names it.
        [
            "home",
            "home/Documents",
            "home/Documents/newproj/src",
            "home/Downloads",
            "home/Pictures",
            "home/Movies",
            "home/Projects/rust-cli",
            "home/Projects/php-app",
            "cases",
            "cases/empty",
            "cases/clones",
            "cases/special",
            "cases/hard-links/a",
            "cases/hard-links/b",
        ]
        .iter()
        .map(|rel| t(rel))
        .collect()
    }

    /// The incremental rescan against the full scan, on the real history:
    /// the fixture tree built on an image, which is attached again so the
    /// history's current chunk holds none of the tree's creation, then
    /// scanned, changed by the script, rescanned from its history and
    /// scanned whole.
    #[test]
    #[ignore = "attaches a disk image and replays its FSEvents history; DUSCOPE_TEST_PLATFORM=1"]
    fn p5_core_01_macos_an_incremental_rescan_equals_a_full_scan_of_the_fixture_tree() {
        let env = TestEnv::new();
        if !platform_tests() {
            return;
        }
        let work = fs::canonicalize(env.tmp_dir()).unwrap().join("image");
        let mount = work.join("mnt");
        fs::create_dir_all(&mount).unwrap();
        let image = image(&env, &work, "1g");
        attach(&env, &image, &mount);
        let detach = Detach(&env, mount.clone());
        let started = Instant::now();
        let tree = duscope_testkit::build_fixture_tree(&mount.join("fixtures"));
        eprintln!(
            "fixture tree built in {:.1} s",
            started.elapsed().as_secs_f64()
        );
        env.hdiutil(&[&"detach", &"-force", &mount]);
        attach(&env, &image, &mount);
        let dev = dev_of(&mount);
        let parent = save(&env, &tree);

        let touched = change(&env, &tree);
        // fseventsd numbers a change a moment after it is made.
        let mark = mark_of(&index(&env, &parent), dev);
        wait_for("the script in the history", || {
            let replayed = replay(dev, &mark, &tree, REPLAY_TIMEOUT).unwrap();
            let dirs: BTreeSet<&PathBuf> = replayed.dirs.iter().collect();
            touched.iter().all(|dir| dirs.contains(dir))
        });

        let answer = ok(&rescan(&env, &tree, 1, &["--incremental", "--json"]));
        let scan = &answer["scan"];
        assert_eq!(scan["incremental"], true, "{scan}");
        assert_eq!(scan["parent_scan_id"], parent.as_str());
        assert_eq!(scan["fallback_reason"], Value::Null);
        let read = scan["rescanned_dirs"].as_u64().unwrap();
        let dirs = scan["dir_count"].as_u64().unwrap();
        assert!(
            read > 0 && read < dirs / 10,
            "{read} of {dirs} directories read"
        );
        let merged = scan["id"].as_str().unwrap().to_owned();
        let fresh = save(&env, &tree);
        let (merged, fresh) = (index(&env, &merged), index(&env, &fresh));
        let mut differences = Vec::new();
        for (table, order) in [
            ("dirs", "id"),
            ("files", "id"),
            ("hardlinks", "dev, ino"),
            ("clones", "dev, clone_id"),
            ("shared_members", "id"),
            ("scan_errors", "dir_id, name"),
            ("type_stats", "ext"),
        ] {
            let (got, want) = (rows(&merged, table, order), rows(&fresh, table, order));
            if got != want {
                differences.push(differing(table, &columns(&fresh, table), &got, &want));
            }
        }
        assert!(
            differences.is_empty(),
            "the incremental rescan's index differs from a full scan's:\n{}",
            differences.join("\n")
        );
        let incremental: i64 = merged
            .query_row(
                "SELECT value FROM scan_meta WHERE key = 'incremental'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(incremental, 1);
        drop(detach);
    }

    /// A volume whose history fseventsd does not keep (an image attached
    /// with `-nobrowse`): the scan records no mark, and the rescan reads
    /// the whole tree, `volume_changed`, or with `--incremental` fails.
    #[test]
    #[ignore = "attaches a disk image; DUSCOPE_TEST_PLATFORM=1"]
    fn p5_core_01_macos_a_volume_without_history_is_rescanned_whole() {
        let env = TestEnv::new();
        if !platform_tests() {
            return;
        }
        let work = fs::canonicalize(env.tmp_dir()).unwrap().join("image");
        let mount = work.join("mnt");
        fs::create_dir_all(&mount).unwrap();
        let image = image(&env, &work, "16m");
        env.hdiutil(&[&"attach", &"-nobrowse", &"-mountpoint", &mount, &image]);
        let detach = Detach(&env, mount.clone());
        fs::create_dir_all(mount.join("dir")).unwrap();
        fs::write(mount.join("dir/file"), b"bytes").unwrap();
        let parent = save(&env, &mount);
        let whole = ok(&rescan(&env, &mount, 1, &["--json"]));
        assert_eq!(whole["scan"]["incremental"], false);
        assert_eq!(whole["scan"]["fallback_reason"], "volume_changed");
        assert_eq!(whole["scan"]["parent_scan_id"], parent.as_str());
        let refused = rescan(&env, &mount, 2, &["--incremental", "--json"]);
        assert_eq!(refused.status.code(), Some(2));
        let error: Value = serde_json::from_slice(&refused.stdout).unwrap();
        assert!(
            error["error"]["message"]
                .as_str()
                .unwrap()
                .contains("volume_changed"),
            "{error}"
        );
        let _ = run(&env, &["scans", "--json"]);
        drop(detach);
    }
}
