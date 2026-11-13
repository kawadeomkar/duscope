//! P4-CORE-01: `clean` to the Trash. Plans on the fixture tree (goldens),
//! the refusal matrix, `confirmation_required` without a terminal, handles,
//! and the Trash round trip: on Linux into the test's own XDG Trash, on macOS
//! on a disk image (a platform test), since the startup volume's items go to
//! the account's Trash whatever `HOME` says, which the test environment
//! refuses (`DUSCOPE_TEST_TRASH`). Nothing here moves a file of the shared
//! fixture tree: its plans are dry runs.
//!
//! P5-CORE-03: `--permanent` deletes the test's own trees, and `du` before
//! and after measures what went; `--expect-bytes` at the edges of its 1%.
//! The race `--permanent` guards against is the library's test, between
//! the plan and the deletion (`duscope-core`'s `clean` tests).
//!
//! P5-CORE-04: a tree changed since its scan is planned from what is there
//! now, so `--expect-bytes` is held to the live bytes; an unchanged one keeps
//! the scan's figures and goldens.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::fs;
use std::io::Write as _;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::Output;

use duscope_testkit::{TestEnv, golden, replace_values};
use serde_json::Value;

fn run(env: &TestEnv, args: &[&str]) -> Output {
    env.cmd().args(args).output().unwrap()
}

fn json(out: &Output) -> Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "{e}: {}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    })
}

fn ok(out: &Output) -> Value {
    assert!(
        out.status.success(),
        "exit {:?}: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    json(out)
}

/// The `clean` envelope of a run that ends with `code` and `exit`.
fn failed(out: &Output, exit: i32, code: &str) -> Value {
    assert_eq!(
        out.status.code(),
        Some(exit),
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let value = json(out);
    assert_eq!(value["ok"], false, "{value}");
    assert_eq!(value["error"]["code"], code, "{value}");
    value
}

fn s(path: &Path) -> &str {
    path.to_str().unwrap()
}

/// A tree under the test's temporary directory: a file of 3,000 bytes, a
/// directory holding one of 5,000, and a symbolic link to the file.
fn tree(env: &TestEnv) -> PathBuf {
    let root = fs::canonicalize(env.tmp_dir()).unwrap().join("tree");
    fs::create_dir_all(root.join("dir")).unwrap();
    fs::write(root.join("a.bin"), vec![7u8; 3000]).unwrap();
    fs::write(root.join("dir/b.bin"), vec![9u8; 5000]).unwrap();
    symlink("a.bin", root.join("link")).unwrap();
    root
}

/// Saves a scan of `root`; its id.
fn save(env: &TestEnv, root: &Path) -> String {
    let saved = ok(&run(env, &["scan", s(root), "--save", "--json"]));
    saved["scan"]["id"].as_str().unwrap().to_owned()
}

/// The rescan `next` suggests after a clean of the saved scan `id` of
/// `root`, in the index directory the test environment names.
fn rescan(root: &Path, id: &str) -> String {
    format!("duscope rescan {} --scan {id} --json", s(root))
}

/// Each item's `action` and `reason`, as `action:reason`.
fn actions(value: &Value) -> Vec<String> {
    value["result"]["plan"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| {
            format!(
                "{}:{}",
                item["action"].as_str().unwrap(),
                item["reason"].as_str().unwrap_or("")
            )
        })
        .collect()
}

/// `text` without what differs between runs and machines, as `explain`'s
/// goldens normalise it.
fn normalize(text: &str, root: &Path, id: &str) -> String {
    let mut text = duscope_testkit::normalize(text, root);
    text = text.replace(id, "<id>");
    for key in ["total_allocated_bytes", "total_apparent_bytes"] {
        replace_values(&mut text, &format!("\"{key}\":"), ',', "<total>");
    }
    text
}

#[test]
fn p4_core_01_golden_plans_on_the_fixture_tree() {
    let env = TestEnv::new();
    let root = fs::canonicalize(env.fixtures().root()).unwrap();
    let saved = ok(&run(
        &env,
        &["scan", s(&root), "--save", "--json", "--threads", "4"],
    ));
    let id = saved["scan"]["id"].as_str().unwrap().to_owned();
    let os = std::env::consts::OS;
    // A cache directory with its marker, and a file whose other links lie
    // outside it, so it frees nothing: a dry run, as every plan of the shared
    // fixture tree here is.
    let items = [
        root.join("home/Projects/npm-app/node_modules"),
        root.join("cases/hard-links/a/original.bin"),
    ];
    let mut args = vec!["clean"];
    args.extend(items.iter().map(|p| s(p)));
    for (format, flag) in [("json", "--json"), ("plain", "--plain")] {
        let mut asked = args.clone();
        asked.push(flag);
        let out = run(&env, &asked);
        assert_eq!(
            out.status.code(),
            Some(1),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let text = String::from_utf8(out.stdout).unwrap();
        golden(
            &format!("clean_{format}_plan@{os}.snap"),
            &normalize(&text, &root, &id),
        );
    }
    let mut asked = args.clone();
    asked.push("--json");
    let plan = failed(&run(&env, &asked), 1, "confirmation_required");
    let items = &plan["result"]["plan"]["items"];
    assert_eq!(items[0]["regenerable_by"], "npm install");
    assert_eq!(items[1]["frees_bytes"], 0);
    assert_eq!(items[1]["frees_kind"], "lower_bound");
    assert_eq!(items[1]["links_outside_subtree"], 3);
    assert_eq!(plan["result"]["plan"]["confirmation_required"], true);
    assert_eq!(plan["result"]["freed_bytes"], 0);
    assert_eq!(plan["scan"]["id"], id.as_str());
}

#[test]
fn p4_core_01_the_refusal_matrix() {
    let env = TestEnv::new();
    let root = tree(&env);
    let id = save(&env, &root);
    let outside = fs::canonicalize(env.tmp_dir()).unwrap().join("outside.txt");
    fs::write(&outside, b"not under the scan's root").unwrap();
    // The test's own entries with --yes: refused, so nothing moves.
    for (path, reason) in [(root.join("link"), "symlink"), (root.clone(), "root")] {
        let out = run(&env, &["clean", s(&path), "--yes", "--json"]);
        let value = failed(&out, 1, "refused_path");
        assert_eq!(
            actions(&value),
            [format!("refused:{reason}")],
            "{}",
            path.display()
        );
        assert_eq!(value["result"]["trashed_count"], 0);
        assert!(path.exists());
    }
    // The system's, planned only, against the test's scan: each is refused
    // for what it is before it is outside that scan's root.
    let mut cases: Vec<(&str, &str)> = vec![("/", "root"), ("/dev", "mount_point")];
    if cfg!(target_os = "macos") {
        // A firmlink of the System volume, and the sealed System volume.
        cases.push(("/Applications", "firmlink"));
        cases.push(("/usr/share", "read_only_volume"));
    }
    for (path, reason) in cases {
        let out = run(&env, &["clean", path, "--scan", &id, "--json"]);
        let value = failed(&out, 1, "refused_path");
        assert_eq!(actions(&value), [format!("refused:{reason}")], "{path}");
    }
    // Outside the scan --scan names.
    let out = run(&env, &["clean", s(&outside), "--scan", &id, "--json"]);
    let value = failed(&out, 1, "refused_path");
    assert_eq!(actions(&value), ["refused:outside_root"]);
    // One refused item makes the whole run a dry run, --yes or not.
    let out = run(
        &env,
        &[
            "clean",
            s(&root.join("a.bin")),
            s(&root.join("link")),
            "--yes",
            "--json",
        ],
    );
    let value = failed(&out, 1, "refused_path");
    assert_eq!(actions(&value), ["trash:", "refused:symlink"]);
    assert!(value["result"]["plan"]["items"][0]["outcome"].is_null());
    assert!(root.join("a.bin").exists() && root.join("link").exists());
    // --skip-refused plans the rest, which waits for --yes.
    let out = run(
        &env,
        &[
            "clean",
            s(&root.join("a.bin")),
            s(&root.join("link")),
            "--skip-refused",
            "--json",
        ],
    );
    let value = failed(&out, 1, "confirmation_required");
    assert_eq!(
        value["result"]["plan"]["total_allocated_bytes"],
        value["result"]["plan"]["items"][0]["allocated_bytes"]
    );
}

#[test]
fn p4_core_01_without_a_terminal_nothing_moves_without_yes() {
    let env = TestEnv::new();
    let root = tree(&env);
    let id = save(&env, &root);
    let file = root.join("a.bin");
    for extra in [&[][..], &["--no-input"][..]] {
        let mut args = vec!["clean", s(&file), "--json"];
        args.extend_from_slice(extra);
        let value = failed(&run(&env, &args), 1, "confirmation_required");
        assert_eq!(value["result"]["plan"]["confirmation_required"], true);
        assert_eq!(value["result"]["trashed_count"], 0);
        assert_eq!(value["error"]["fixable_by"], "user");
        let next = value["next"][0].as_str().unwrap();
        // The same items against the same saved scan.
        assert!(
            next.starts_with("duscope clean ")
                && next.contains(&format!(" --scan {id}"))
                && next.ends_with(" --yes"),
            "{next}"
        );
        assert!(file.exists());
    }
    // People read the plan and the line under it; the failure is on stderr.
    let out = run(&env, &["clean", s(&file)]);
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("clean moves items to the Trash only with --yes"),
        "{stderr}"
    );
}

#[test]
fn p4_core_01_an_item_twice_inside_another_or_missing_fails_the_run() {
    let env = TestEnv::new();
    let root = tree(&env);
    let file = root.join("a.bin");
    // No saved scan holds it: clean plans from one.
    let out = run(&env, &["clean", s(&file), "--json"]);
    assert_eq!(out.status.code(), Some(3));
    let value = json(&out);
    assert_eq!(value["error"]["code"], "scan_not_found");
    assert!(
        value["error"]["hint"]
            .as_str()
            .unwrap()
            .starts_with("Run duscope scan --save "),
        "{value}"
    );
    save(&env, &root);
    let out = run(
        &env,
        &[
            "clean",
            s(&root.join("dir")),
            s(&root.join("dir/b.bin")),
            "--json",
        ],
    );
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(json(&out)["error"]["code"], "usage");
    let out = run(&env, &["clean", s(&file), s(&file), "--json"]);
    assert_eq!(out.status.code(), Some(2));
    let out = run(&env, &["clean", s(&root.join("gone")), "--json"]);
    assert_eq!(out.status.code(), Some(3));
    assert_eq!(json(&out)["error"]["code"], "scan_not_found");
    // At least one item.
    assert_eq!(run(&env, &["clean"]).status.code(), Some(2));
}

#[test]
fn p4_core_01_a_handle_names_the_entry_explain_examined() {
    let env = TestEnv::new();
    let root = tree(&env);
    save(&env, &root);
    let file = root.join("a.bin");
    let facts = ok(&run(&env, &["explain", s(&file), "--json"]));
    let handle = facts["result"]["handle"].as_str().unwrap().to_owned();
    let value = failed(
        &run(&env, &["clean", "--from", &handle, "--json"]),
        1,
        "confirmation_required",
    );
    assert_eq!(value["query"]["handle_count"], 1);
    assert_eq!(value["result"]["plan"]["items"][0]["path"], s(&file));
    assert_eq!(value["scan"]["id"], facts["scan"]["id"]);
    // The hint names the item by its handle again.
    let next = value["next"][0].as_str().unwrap();
    assert!(
        next.contains(" --from ") && next.contains(&handle) && !next.contains(s(&file)),
        "{next}"
    );
    // Another file at the path: another inode, made while the first exists.
    let other = root.join("a.new");
    fs::write(&other, vec![1u8; 3000]).unwrap();
    fs::rename(&other, &file).unwrap();
    let out = run(&env, &["clean", "--from", &handle, "--yes", "--json"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(json(&out)["error"]["code"], "path_changed");
    assert!(file.exists());
    // A handle duscope did not sign.
    // A character of the MAC: not the last, whose low bits may be padding
    // that decodes the same whatever they are.
    let mut forged = handle.into_bytes();
    let at = forged.len() - 2;
    forged[at] = if forged[at] == b'A' { b'B' } else { b'A' };
    let out = run(
        &env,
        &[
            "clean",
            "--from",
            std::str::from_utf8(&forged).unwrap(),
            "--json",
        ],
    );
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(json(&out)["error"]["code"], "handle_invalid");
}

/// Moves `trashed` back to `to` and checks it holds `bytes`: the Trash
/// keeps the item as it was.
fn restore(trashed: &Path, to: &Path, bytes: &[u8]) {
    assert_eq!(fs::read(trashed).unwrap(), bytes, "{}", trashed.display());
    fs::rename(trashed, to).unwrap();
    assert_eq!(fs::read(to).unwrap(), bytes);
}

#[cfg(target_os = "linux")]
#[test]
fn p4_core_01_linux_a_trashed_file_round_trips() {
    let env = TestEnv::new();
    let root = tree(&env);
    let id = save(&env, &root);
    let file = root.join("a.bin");
    let dir = root.join("dir");
    // The symbolic link is refused and left; the rest moves.
    let out = run(
        &env,
        &[
            "clean",
            s(&file),
            s(&dir),
            s(&root.join("link")),
            "--skip-refused",
            "--yes",
            "--json",
        ],
    );
    let value = ok(&out);
    assert_eq!(actions(&value), ["trash:", "trash:", "refused:symlink"]);
    let items = &value["result"]["plan"]["items"];
    assert_eq!(items[0]["outcome"], "trashed");
    assert_eq!(items[1]["outcome"], "trashed");
    assert!(items[2]["outcome"].is_null());
    assert_eq!(value["result"]["trashed_count"], 2);
    assert_eq!(value["result"]["freed_bytes"], 0);
    assert_eq!(value["result"]["plan"]["confirmation_required"], false);
    assert_eq!(value["warnings"][0]["code"], "index_untouched");
    assert_eq!(value["next"][0], rescan(&root, &id));
    // The link itself stays; it dangles now that a.bin, which it names, is
    // in the Trash, so `exists` (which follows it) cannot tell.
    assert!(!file.exists() && !dir.exists());
    assert!(fs::symlink_metadata(root.join("link")).is_ok());
    // In the test's home Trash, with what the specification asks beside it.
    let trash = env.xdg_data_home().join("Trash");
    let info = fs::read_to_string(trash.join("info/a.bin.trashinfo")).unwrap();
    assert!(
        info.starts_with("[Trash Info]\n") && info.contains(&format!("Path={}", s(&file))),
        "{info}"
    );
    restore(&trash.join("files/a.bin"), &file, &[7u8; 3000]);
    restore(
        &trash.join("files/dir/b.bin"),
        &root.join("b.bin"),
        &[9u8; 5000],
    );
}

#[cfg(target_os = "macos")]
#[test]
fn p4_core_01_the_test_environment_moves_nothing_on_the_startup_volume() {
    // NSFileManager puts an item of the startup volume in the account's own
    // Trash, so the test environment refuses: the item is left where it is,
    // and the run says why.
    let env = TestEnv::new();
    let root = tree(&env);
    save(&env, &root);
    let file = root.join("a.bin");
    let value = failed(
        &run(&env, &["clean", s(&file), "--yes", "--json"]),
        1,
        "trash_unavailable",
    );
    let item = &value["result"]["plan"]["items"][0];
    assert_eq!(item["outcome"], "failed");
    assert_eq!(item["error"]["code"], "trash_unavailable");
    assert_eq!(value["result"]["trashed_count"], 0);
    assert!(file.exists());
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "attaches a disk image; DUSCOPE_TEST_PLATFORM=1"]
fn p4_core_01_macos_a_trashed_file_round_trips_on_an_image() {
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
    env.hdiutil(&[&"attach", &"-nobrowse", &"-mountpoint", &mount, &image]);
    let detach = Detach(&env, mount.clone());
    let file = mount.join("a.bin");
    let dir = mount.join("dir");
    fs::create_dir_all(&dir).unwrap();
    fs::write(&file, vec![7u8; 3000]).unwrap();
    fs::write(dir.join("b.bin"), vec![9u8; 5000]).unwrap();
    save(&env, &mount);
    let value = ok(&run(&env, &["clean", s(&file), s(&dir), "--yes", "--json"]));
    assert_eq!(value["result"]["trashed_count"], 2);
    assert!(!file.exists() && !dir.exists());
    // The volume's own Trash, this user's directory in it.
    let trash = mount.join(".Trashes").join(effective_uid());
    restore(&trash.join("a.bin"), &file, &[7u8; 3000]);
    restore(&trash.join("dir/b.bin"), &mount.join("b.bin"), &[9u8; 5000]);
    drop(detach);
}

/// The effective user id, as `id -u` prints it.
#[cfg(target_os = "macos")]
fn effective_uid() -> String {
    let out = std::process::Command::new("id").arg("-u").output().unwrap();
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

/// Detaches the disk image mounted at its path when dropped.
#[cfg(target_os = "macos")]
struct Detach<'a>(&'a TestEnv, PathBuf);

#[cfg(target_os = "macos")]
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

/// `du -sk` of `path`, in bytes.
fn du(env: &TestEnv, path: &Path) -> u64 {
    let out = env.command("du").arg("-sk").arg(path).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8(out.stdout).unwrap();
    let kib: u64 = text.split_whitespace().next().unwrap().parse().unwrap();
    kib * 1024
}

/// `n` with a comma between each group of three digits, as messages print
/// it.
fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[test]
fn p5_core_03_du_before_and_after_equals_freed_bytes() {
    let env = TestEnv::new();
    let root = tree(&env);
    // Under dir: a directory two deep, two links of one file, and a file
    // whose other link stays beside it, outside the plan.
    fs::create_dir_all(root.join("dir/sub/deep")).unwrap();
    fs::write(root.join("dir/sub/deep/c.bin"), vec![3u8; 70_000]).unwrap();
    fs::write(root.join("dir/h1.bin"), vec![4u8; 20_000]).unwrap();
    fs::hard_link(root.join("dir/h1.bin"), root.join("dir/sub/h2.bin")).unwrap();
    fs::write(root.join("dir/kept.bin"), vec![5u8; 30_000]).unwrap();
    fs::hard_link(root.join("dir/kept.bin"), root.join("kept-link.bin")).unwrap();
    let id = save(&env, &root);
    let file = root.join("a.bin");
    let dir = root.join("dir");
    // Without --yes, the plan says what it would delete, and nothing goes.
    let plan = failed(
        &run(&env, &["clean", s(&file), s(&dir), "--permanent", "--json"]),
        1,
        "confirmation_required",
    );
    assert_eq!(plan["query"]["permanent"], true);
    assert_eq!(plan["result"]["plan"]["method"], "permanent");
    assert_eq!(actions(&plan), ["permanent:", "permanent:"]);
    assert_eq!(
        plan["error"]["message"],
        "nothing was deleted: clean deletes items only with --yes"
    );
    let next = plan["next"][0].as_str().unwrap();
    assert!(next.ends_with(" --permanent --yes"), "{next}");
    assert!(file.exists() && dir.exists());
    let planned = plan["result"]["plan"]["total_frees_bytes"]
        .as_u64()
        .unwrap();

    let before = du(&env, &root);
    let value = ok(&run(
        &env,
        &["clean", s(&file), s(&dir), "--permanent", "--yes", "--json"],
    ));
    let after = du(&env, &root);
    let freed = value["result"]["freed_bytes"].as_u64().unwrap();
    assert_eq!(before - after, freed, "du {before} then {after}");
    assert!(freed > 0);
    // The scan is the tree as it was: the plan's figure is what went.
    assert_eq!(freed, planned);
    assert_eq!(value["result"]["deleted_count"], 2);
    assert_eq!(value["result"]["trashed_count"], 0);
    let items = &value["result"]["plan"]["items"];
    assert_eq!(items[0]["outcome"], "deleted");
    assert_eq!(items[1]["outcome"], "deleted");
    assert!(fs::symlink_metadata(&file).is_err() && fs::symlink_metadata(&dir).is_err());
    assert_eq!(
        fs::read(root.join("kept-link.bin")).unwrap(),
        vec![5u8; 30_000]
    );
    assert_eq!(value["warnings"][0]["code"], "index_untouched");
    assert_eq!(value["next"][0], rescan(&root, &id));
    // A refused item stops a permanent run as it does a move.
    let out = run(
        &env,
        &[
            "clean",
            s(&root.join("kept-link.bin")),
            s(&root.join("link")),
            "--permanent",
            "--yes",
            "--json",
        ],
    );
    let value = failed(&out, 1, "refused_path");
    assert_eq!(
        value["error"]["message"],
        "1 of the 2 items cannot be deleted, so nothing was deleted"
    );
    assert!(root.join("kept-link.bin").exists());
}

#[test]
fn p5_core_03_expect_bytes_within_one_percent_or_nothing_is_touched() {
    let env = TestEnv::new();
    let root = tree(&env);
    fs::write(root.join("c.bin"), vec![3u8; 70_000]).unwrap();
    let id = save(&env, &root);
    let frees = |path: &Path| {
        let plan = failed(
            &run(&env, &["clean", s(path), "--permanent", "--json"]),
            1,
            "confirmation_required",
        );
        plan["result"]["plan"]["total_frees_bytes"]
            .as_u64()
            .unwrap()
    };
    let expect = |path: &Path, n: u64| {
        run(
            &env,
            &[
                "clean",
                s(path),
                "--permanent",
                "--expect-bytes",
                &n.to_string(),
                "--yes",
                "--json",
            ],
        )
    };
    // The smallest and the largest N whose 1% holds the plan's figure, and
    // a byte past each.
    let file = root.join("a.bin");
    let total = frees(&file);
    let lowest = (100 * total).div_ceil(101);
    let highest = 100 * total / 99;
    for n in [lowest - 1, highest + 1] {
        let value = failed(&expect(&file, n), 1, "expectation_mismatch");
        let message = value["error"]["message"].as_str().unwrap();
        assert!(
            message.contains(&thousands(total)) && message.contains(&thousands(n)),
            "{message}"
        );
        // The person approved another figure: theirs to look at again.
        assert_eq!(value["error"]["fixable_by"], "user");
        assert_eq!(value["query"]["expect_bytes"], n);
        assert_eq!(value["result"]["deleted_count"], 0);
        assert_eq!(value["result"]["plan"]["confirmation_required"], false);
        assert_eq!(value["next"], serde_json::json!([]));
        assert!(file.exists());
    }
    let value = ok(&expect(&file, highest));
    assert_eq!(value["result"]["deleted_count"], 1);
    assert!(!file.exists());
    let other = root.join("c.bin");
    let total = frees(&other);
    let value = ok(&expect(&other, (100 * total).div_ceil(101)));
    assert_eq!(value["result"]["freed_bytes"], total);
    assert!(!other.exists());

    // The Trash too, and a dry run: the figure is checked before anything,
    // and a plan that meets it waits for --yes as any does.
    let dir = root.join("dir");
    let value = failed(
        &run(
            &env,
            &["clean", s(&dir), "--expect-bytes", "1", "--yes", "--json"],
        ),
        1,
        "expectation_mismatch",
    );
    assert_eq!(value["result"]["trashed_count"], 0);
    assert!(dir.exists());
    let total = failed(
        &run(&env, &["clean", s(&dir), "--json"]),
        1,
        "confirmation_required",
    )["result"]["plan"]["total_frees_bytes"]
        .as_u64()
        .unwrap();
    let n = total.to_string();
    let value = failed(
        &run(&env, &["clean", s(&dir), "--expect-bytes", &n, "--json"]),
        1,
        "confirmation_required",
    );
    assert_eq!(
        value["next"][0],
        format!(
            "duscope clean {} --scan {id} --expect-bytes {n} --yes",
            s(&dir)
        )
    );
    let out = run(
        &env,
        &["clean", s(&dir), "--expect-bytes", &n, "--yes", "--json"],
    );
    if cfg!(target_os = "macos") {
        // The test environment refuses the startup volume's Trash; the hint
        // names the other way.
        let value = failed(&out, 1, "trash_unavailable");
        let hint = value["error"]["hint"].as_str().unwrap();
        assert!(hint.contains("--permanent"), "{hint}");
        assert!(dir.exists());
    } else {
        assert_eq!(ok(&out)["result"]["trashed_count"], 1);
        assert!(!dir.exists());
    }
}

#[test]
fn p4_core_01_the_rescan_a_clean_suggests_runs_as_written() {
    let env = TestEnv::new();
    let root = tree(&env);
    // Saved where a command without --index-dir does not look: the hint and
    // `next` name the directory, as the scan was found.
    let dir = env.work_dir().join("elsewhere");
    let flag = s(&dir);
    let saved = ok(&run(
        &env,
        &["scan", s(&root), "--save", "--index-dir", flag, "--json"],
    ));
    let id = saved["scan"]["id"].as_str().unwrap();
    let file = root.join("a.bin");
    let value = ok(&run(
        &env,
        &[
            "clean",
            s(&file),
            "--permanent",
            "--yes",
            "--index-dir",
            flag,
            "--json",
        ],
    ));
    let renew = [
        "rescan",
        s(&root),
        "--index-dir",
        flag,
        "--scan",
        id,
        "--json",
    ];
    assert_eq!(
        value["next"],
        Value::from(vec![format!("duscope {}", renew.join(" "))])
    );
    assert_eq!(
        value["warnings"][0]["hint"],
        format!("Run duscope rescan --index-dir {flag} --scan {id} to measure the tree again")
    );
    // The command `next` holds runs, and saves a new scan.
    let again = ok(&run(&env, &renew));
    assert!(
        again["scan"]["id"].as_str().is_some_and(|new| new != id),
        "{again}"
    );
}

#[test]
fn p4_core_01_a_clean_that_changed_nothing_suggests_no_rescan() {
    let env = TestEnv::new();
    let root = tree(&env);
    save(&env, &root);
    // A directory that cannot be written: its file is not deleted.
    let dir = root.join("dir");
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o555)).unwrap();
    let out = run(
        &env,
        &[
            "clean",
            s(&dir.join("b.bin")),
            "--permanent",
            "--yes",
            "--json",
        ],
    );
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
    let value = json(&out);
    assert_eq!(out.status.code(), Some(1), "{value}");
    assert_eq!(value["result"]["plan"]["items"][0]["outcome"], "failed");
    assert_eq!(value["result"]["deleted_count"], 0);
    assert!(dir.join("b.bin").exists());
    // Nothing went: the saved scan still matches, and the item's error says
    // what to do.
    assert_eq!(value["warnings"], Value::Array(Vec::new()), "{value}");
    assert_eq!(value["next"], Value::Array(Vec::new()), "{value}");
}

#[test]
fn p4_core_01_after_a_clean_an_import_is_renewed_by_a_new_scan() {
    let env = TestEnv::new();
    let root = tree(&env);
    save(&env, &root);
    let file = env.tmp_dir().join("tree.json");
    let exported = run(
        &env,
        &["export", s(&root), "--format", "ncdu", "-o", s(&file)],
    );
    assert!(exported.status.success(), "{exported:?}");
    let imported = ok(&run(
        &env,
        &["import", s(&file), "--format", "ncdu", "--json"],
    ));
    let id = imported["scan"]["id"].as_str().unwrap();
    let value = ok(&run(
        &env,
        &[
            "clean",
            s(&root.join("a.bin")),
            "--scan",
            id,
            "--permanent",
            "--yes",
            "--json",
        ],
    ));
    assert_eq!(value["result"]["deleted_count"], 1, "{value}");
    // A rescan refuses an import, which has no scan options to scan with.
    assert_eq!(value["next"], Value::Array(Vec::new()), "{value}");
    assert_eq!(
        value["warnings"][0]["hint"],
        format!(
            "Run duscope scan --save {} to measure the tree again",
            s(&root)
        )
    );
}

// P5-MCP-01: the two flags `duscope_delete` runs `clean` with, never typed
// by a person (the tool's own tests are in `crates/duscope-mcp`).

/// `explain --json` of `path`: its handle and `freeable_now_bytes`.
fn handle(env: &TestEnv, path: &Path) -> (String, u64) {
    let facts = ok(&run(env, &["explain", s(path), "--json"]));
    (
        facts["result"]["handle"].as_str().unwrap().to_owned(),
        facts["result"]["freeable_now_bytes"].as_u64().unwrap(),
    )
}

#[test]
fn p5_mcp_01_a_dry_run_answers_the_plan_with_no_confirmation_left_open() {
    let env = TestEnv::new();
    let root = tree(&env);
    save(&env, &root);
    let file = root.join("a.bin");
    let (token, frees) = handle(&env, &file);
    let expect = format!("--expect-item-bytes={frees}");
    let value = ok(&run(
        &env,
        &["clean", "--from", &token, &expect, "--dry-run", "--json"],
    ));
    assert_eq!(value["ok"], true);
    assert_eq!(value["error"], Value::Null);
    assert_eq!(value["next"], serde_json::json!([]));
    assert_eq!(value["result"]["plan"]["confirmation_required"], true);
    assert_eq!(value["result"]["trashed_count"], 0);
    assert!(file.exists());
    // Without --expect-item-bytes too, and refused all the same with
    // --yes beside it, which it cannot be.
    assert_eq!(
        run(&env, &["clean", "--from", &token, "--dry-run", "--json"])
            .status
            .code(),
        Some(0)
    );
    assert_eq!(
        run(&env, &["clean", "--from", &token, "--dry-run", "--yes"])
            .status
            .code(),
        Some(2)
    );
    assert!(file.exists());
}

#[test]
fn p5_mcp_01_an_item_off_its_figure_is_refused_and_nothing_moves() {
    let env = TestEnv::new();
    let root = tree(&env);
    save(&env, &root);
    let (file, dir) = (root.join("a.bin"), root.join("dir"));
    let (token_a, frees_a) = handle(&env, &file);
    let (token_b, frees_b) = handle(&env, &dir);
    let wrong = format!("--expect-item-bytes={}", frees_b * 3 + 8192);
    let right = format!("--expect-item-bytes={frees_a}");
    for mode in [["--dry-run", "--json"], ["--yes", "--json"]] {
        let out = run(
            &env,
            &[
                "clean", "--from", &token_a, "--from", &token_b, &right, &wrong, mode[0], mode[1],
            ],
        );
        let value = failed(&out, 1, "expectation_mismatch");
        let items = &value["result"]["plan"]["items"];
        assert_eq!(items[0]["action"], "trash", "{value}");
        assert_eq!(items[1]["action"], "refused", "{value}");
        assert_eq!(items[1]["reason"], "expectation_mismatch", "{value}");
        assert_eq!(value["result"]["plan"]["confirmation_required"], false);
        assert_eq!(value["result"]["trashed_count"], 0);
        assert!(file.exists() && dir.exists());
    }
    // A refused item of another kind is reported first, as the plan says.
    let link = root.join("link");
    let out = run(
        &env,
        &[
            "clean",
            "--from",
            &token_a,
            s(&link),
            "--expect-bytes=1",
            "--json",
        ],
    );
    assert_eq!(json(&out)["error"]["code"], "refused_path");
    // One figure per handle, or the command line is wrong.
    let out = run(
        &env,
        &[
            "clean",
            "--from",
            &token_a,
            "--from",
            &token_b,
            "--expect-item-bytes=1",
            "--json",
        ],
    );
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(json(&out)["error"]["code"], "usage");
    assert_eq!(
        run(&env, &["clean", "--expect-item-bytes=1"]).status.code(),
        Some(2)
    );
}

#[test]
fn p5_mcp_01_an_item_whose_bytes_no_scan_knows_is_refused_whatever_its_figure() {
    let env = TestEnv::new();
    let root = tree(&env);
    save(&env, &root);
    let file = root.join("a.bin");
    let (token_a, frees_a) = handle(&env, &file);
    // A directory made since the scan: what it frees is not known.
    let fresh = root.join("made-since");
    fs::create_dir(&fresh).unwrap();
    fs::write(fresh.join("big.bin"), vec![5u8; 200_000]).unwrap();
    let facts = ok(&run(&env, &["explain", s(&fresh), "--json"]));
    assert_eq!(
        facts["result"]["freeable_now_bytes"],
        Value::Null,
        "{facts}"
    );
    let token_d = facts["result"]["handle"].as_str().unwrap().to_owned();
    let right = format!("--expect-item-bytes={frees_a}");
    for figure in ["0", "1", "200000"] {
        let unknown = format!("--expect-item-bytes={figure}");
        for mode in ["--dry-run", "--yes"] {
            let out = run(
                &env,
                &[
                    "clean", "--from", &token_a, &right, "--from", &token_d, &unknown, mode,
                    "--json",
                ],
            );
            let value = failed(&out, 1, "expectation_mismatch");
            let items = &value["result"]["plan"]["items"];
            assert_eq!(items[0]["action"], "trash", "{value}");
            assert_eq!(items[1]["action"], "refused", "{value}");
            assert_eq!(items[1]["reason"], "expectation_mismatch", "{value}");
            // P5-CORE-04: measured by the plan's own walk, which no figure
            // from a saved scan is held to.
            assert!(
                items[1]["frees_bytes"].as_u64().unwrap() >= 200_000,
                "{value}"
            );
            assert_eq!(items[1]["frees_kind"], "upper_bound", "{value}");
            assert_eq!(value["result"]["trashed_count"], 0, "{value}");
            let message = value["error"]["message"].as_str().unwrap();
            assert!(
                message.contains("the saved scan does not hold") && message.contains("made-since"),
                "{message}"
            );
            assert!(
                value["error"]["hint"].as_str().unwrap().contains("Rescan"),
                "{value}"
            );
            assert!(file.exists() && fresh.join("big.bin").exists());
        }
    }
}

/// The message of the first item in the plan that failed.
fn first_failure(value: &Value) -> &str {
    value["result"]["plan"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find_map(|item| item["error"]["message"].as_str())
        .unwrap_or_else(|| panic!("no item failed: {value}"))
}

#[test]
fn p5_mcp_01_a_run_that_acted_on_some_items_says_how_many_of_how_many() {
    // An item in a directory that cannot be written fails, the others go:
    // the run's message, all an MCP client may show, says how many of the
    // items went before the first failure.
    let env = TestEnv::new();
    let root = tree(&env);
    for name in ["e.bin", "f.bin"] {
        fs::write(root.join(name), vec![4u8; 2000]).unwrap();
    }
    fs::write(root.join("dir/c.bin"), vec![6u8; 2000]).unwrap();
    save(&env, &root);
    let dir = root.join("dir");
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o555)).unwrap();
    let delete = |items: &[&str]| {
        let mut args = vec!["clean"];
        args.extend_from_slice(items);
        args.extend(["--permanent", "--yes", "--json"]);
        run(&env, &args)
    };
    let gone = [root.join("a.bin"), root.join("e.bin"), root.join("f.bin")];
    let kept = [dir.join("b.bin"), dir.join("c.bin")];
    let one = delete(&[s(&gone[0]), s(&kept[0])]);
    let two = delete(&[s(&gone[1]), s(&gone[2]), s(&kept[0]), s(&kept[1])]);
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();

    let value = json(&one);
    assert_eq!(one.status.code(), Some(1), "{value}");
    assert_eq!(value["result"]["deleted_count"], 1, "{value}");
    let message = value["error"]["message"].as_str().unwrap();
    let item = first_failure(&value);
    assert_eq!(
        message,
        format!("1 of the 2 items was deleted; one was not: {item}")
    );
    assert!(!gone[0].exists() && kept[0].exists());

    let value = json(&two);
    assert_eq!(two.status.code(), Some(1), "{value}");
    assert_eq!(value["result"]["deleted_count"], 2, "{value}");
    let message = value["error"]["message"].as_str().unwrap();
    let item = first_failure(&value);
    assert_eq!(
        message,
        format!("2 of the 4 items were deleted; 2 were not, the first: {item}")
    );
    assert!(gone.iter().all(|path| !path.exists()));
    assert!(kept.iter().all(|path| path.exists()));
}

// P5-CORE-04: what changed since the scan is priced from the tree.

/// Writes `n` zero bytes to a new file at `path`, a mebibyte at a time.
fn zeros(path: &Path, n: usize) {
    let mut file = fs::File::create(path).unwrap();
    let chunk = vec![0u8; 1 << 20];
    let mut left = n;
    while left > 0 {
        let take = left.min(chunk.len());
        file.write_all(&chunk[..take]).unwrap();
        left -= take;
    }
}

#[test]
fn p5_core_04_expect_bytes_fails_against_live_bytes() {
    // The audit's reproduction: a plan and --expect-bytes of 4,096 bytes,
    // 50 MiB written since the scan.
    let env = TestEnv::new();
    let v = fs::canonicalize(env.tmp_dir()).unwrap().join("v");
    let d = v.join("d");
    fs::create_dir_all(&d).unwrap();
    fs::write(d.join("a"), vec![0u8; 4096]).unwrap();
    let saved = ok(&run(
        &env,
        &["scan", s(&v), "--save", "--json", "--treemap", "never"],
    ));
    let id = saved["scan"]["id"].as_str().unwrap().to_owned();
    zeros(&d.join("big"), 52_428_800);
    let out = run(
        &env,
        &[
            "clean",
            s(&d),
            "--scan",
            &id,
            "--permanent",
            "--expect-bytes",
            "4096",
            "--yes",
            "--json",
        ],
    );
    let value = failed(&out, 1, "expectation_mismatch");
    assert!(d.join("a").exists() && d.join("big").exists());
    assert_eq!(value["result"]["deleted_count"], 0);
    assert_eq!(value["result"]["freed_bytes"], 0);
    // The plan is the tree's: what the walk found, at most.
    let plan = &value["result"]["plan"];
    let total = plan["total_frees_bytes"].as_u64().unwrap();
    assert!(total >= 52_428_800 + 4096, "{value}");
    assert_eq!(plan["items"][0]["frees_kind"], "upper_bound", "{value}");
    assert_eq!(plan["items"][0]["frees_bytes"], total, "{value}");
    let message = value["error"]["message"].as_str().unwrap();
    assert!(
        message.contains(&thousands(total)) && message.contains("4,096"),
        "{message}"
    );
    // The warning says why, and how to measure the tree again.
    let warning = &value["warnings"][0];
    assert_eq!(warning["code"], "changed_since_scan", "{value}");
    assert_eq!(warning["count"], 1, "{value}");
    let hint = warning["hint"].as_str().unwrap();
    assert!(
        hint.starts_with("Run duscope rescan")
            && hint.contains(&id)
            && hint.ends_with("to measure the tree again"),
        "{hint}"
    );
    // The live figure is the one a plan meets; a dry run waits for --yes.
    let out = run(
        &env,
        &[
            "clean",
            s(&d),
            "--scan",
            &id,
            "--permanent",
            "--expect-bytes",
            &total.to_string(),
            "--json",
        ],
    );
    failed(&out, 1, "confirmation_required");

    // A directory made since the scan counts its walk too: 0 no longer
    // passes for it.
    let fresh = v.join("made-since");
    fs::create_dir(&fresh).unwrap();
    fs::write(fresh.join("c"), vec![1u8; 64 * 1024]).unwrap();
    let out = run(
        &env,
        &[
            "clean",
            s(&fresh),
            "--scan",
            &id,
            "--permanent",
            "--expect-bytes",
            "0",
            "--yes",
            "--json",
        ],
    );
    let value = failed(&out, 1, "expectation_mismatch");
    assert!(fresh.join("c").exists());
    assert!(
        value["result"]["plan"]["total_frees_bytes"]
            .as_u64()
            .unwrap()
            >= 64 * 1024,
        "{value}"
    );
    // Made since, not changed: no warning for it.
    assert_eq!(value["warnings"], serde_json::json!([]), "{value}");
}

#[test]
fn p5_core_04_an_unchanged_fixture_tree_keeps_index_figures() {
    let env = TestEnv::new();
    let root = fs::canonicalize(env.fixtures().root()).unwrap();
    let saved = ok(&run(
        &env,
        &["scan", s(&root), "--save", "--json", "--threads", "4"],
    ));
    let id = saved["scan"]["id"].as_str().unwrap().to_owned();
    let os = std::env::consts::OS;
    // P4-CORE-01's two plans, as their goldens hold them.
    let items = [
        root.join("home/Projects/npm-app/node_modules"),
        root.join("cases/hard-links/a/original.bin"),
    ];
    for (format, flag) in [("json", "--json"), ("plain", "--plain")] {
        let mut args = vec!["clean"];
        args.extend(items.iter().map(|p| s(p)));
        args.push(flag);
        let out = run(&env, &args);
        assert_eq!(out.status.code(), Some(1));
        let text = String::from_utf8(out.stdout).unwrap();
        golden(
            &format!("clean_{format}_plan@{os}.snap"),
            &normalize(&text, &root, &id),
        );
    }
    // Every folder two levels down, each planned alone, the fixture tree's
    // cases among them (links, clones, sparse and compressed files, an
    // unreadable folder, odd names, times and file types): none has changed
    // since the scan, so none is priced from the tree.
    let mut planned = 0;
    for top in ["home", "cases"] {
        for entry in fs::read_dir(root.join(top)).unwrap() {
            let path = entry.unwrap().path();
            if !fs::symlink_metadata(&path).unwrap().is_dir() {
                continue;
            }
            let value = json(&run(&env, &["clean", s(&path), "--scan", &id, "--json"]));
            let warnings = value["warnings"].as_array().unwrap();
            assert!(
                warnings.iter().all(|w| w["code"] != "changed_since_scan"),
                "{}: {value}",
                path.display()
            );
            planned += 1;
        }
    }
    assert!(planned > 20, "{planned}");
    // Every file in the cases, as items of one plan: a file is compared
    // through its `lstat`, where the scan read it through the directory
    // listing, and the two must agree on clones, compressed and sparse
    // files and file types. `wide` is the same file a hundred thousand
    // times over.
    let mut files = Vec::new();
    let mut folders = vec![root.join("cases")];
    while let Some(folder) = folders.pop() {
        for entry in fs::read_dir(&folder).unwrap() {
            let Ok(entry) = entry else {
                continue;
            };
            let path = entry.path();
            // A name that is not UTF-8, as the Linux tree holds, is no
            // argument of this command line.
            if path.to_str().is_none() {
                continue;
            }
            // A folder that may be listed but not entered holds entries no
            // one can stat.
            let Ok(meta) = fs::symlink_metadata(&path) else {
                continue;
            };
            let kind = meta.file_type();
            if kind.is_dir() {
                if path.file_name().unwrap() != "wide" && fs::read_dir(&path).is_ok() {
                    folders.push(path);
                }
            } else if !kind.is_symlink() {
                files.push(path);
            }
        }
    }
    assert!(files.len() > 50, "{}", files.len());
    let mut args = vec!["clean", "--scan", &id, "--dry-run", "--json"];
    args.extend(files.iter().map(|p| s(p)));
    let value = json(&run(&env, &args));
    assert_eq!(
        value["result"]["plan"]["items"].as_array().unwrap().len(),
        files.len(),
        "{value}"
    );
    let warnings = value["warnings"].as_array().unwrap();
    assert!(
        warnings.iter().all(|w| w["code"] != "changed_since_scan"),
        "{warnings:?}"
    );
}

#[test]
fn p5_core_04_clean_from_an_entry_changed_since_the_scan_is_path_changed() {
    let env = TestEnv::new();
    let root = tree(&env);
    save(&env, &root);
    let dir = root.join("dir");
    let (token, _) = handle(&env, &dir);
    // Its file grown in place: the same directory, the same inode, but not
    // what the scan saw and explain signed.
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(dir.join("b.bin"))
        .unwrap();
    file.write_all(&vec![1u8; 30_000]).unwrap();
    drop(file);
    for mode in ["--dry-run", "--yes"] {
        let value = failed(
            &run(&env, &["clean", "--from", &token, mode, "--json"]),
            1,
            "path_changed",
        );
        let message = value["error"]["message"].as_str().unwrap();
        assert!(
            message.contains("has changed since the saved scan"),
            "{message}"
        );
        let hint = value["error"]["hint"].as_str().unwrap();
        assert!(hint.contains("duscope rescan"), "{hint}");
        assert!(dir.join("b.bin").exists());
    }
    // By path, the plan is the tree's, and says so.
    let value = failed(
        &run(&env, &["clean", s(&dir), "--json"]),
        1,
        "confirmation_required",
    );
    assert_eq!(
        value["warnings"][0]["code"], "changed_since_scan",
        "{value}"
    );
    assert_eq!(
        value["result"]["plan"]["items"][0]["frees_kind"],
        "upper_bound"
    );
}

/// Bytes on disk of `path` itself, as a scan counts them.
fn on_disk(path: &Path) -> u64 {
    use std::os::unix::fs::MetadataExt;
    fs::symlink_metadata(path).unwrap().blocks() * 512
}

#[test]
fn p5_core_04_expect_bytes_counts_what_the_scan_did_not_enter() {
    // The audit's reproduction, through an exclude: 50 MiB under a folder
    // the scan listed and did not enter, nothing changed since. On APFS the
    // plan read 4,096 bytes and the deletion freed 52,432,896.
    let env = TestEnv::new();
    let top = fs::canonicalize(env.tmp_dir()).unwrap().join("proj");
    let (app, cache) = (top.join("app"), top.join("app/cache"));
    fs::create_dir_all(&cache).unwrap();
    fs::write(app.join("main.rs"), b"fn main() {}\n").unwrap();
    zeros(&cache.join("blob"), 52_428_800);
    let saved = ok(&run(
        &env,
        &[
            "scan",
            s(&top),
            "--exclude",
            "app/cache",
            "--save",
            "--json",
        ],
    ));
    let id = saved["scan"]["id"].as_str().unwrap().to_owned();
    let scanned = on_disk(&app) + on_disk(&app.join("main.rs")) + on_disk(&cache);
    let live = scanned + on_disk(&cache.join("blob"));
    let delete = |figure: u64| {
        let figure = figure.to_string();
        run(
            &env,
            &[
                "clean",
                s(&app),
                "--scan",
                &id,
                "--permanent",
                "--expect-bytes",
                &figure,
                "--yes",
                "--json",
            ],
        )
    };
    let value = failed(&delete(scanned), 1, "expectation_mismatch");
    assert!(cache.join("blob").exists(), "{value}");
    assert_eq!(value["result"]["deleted_count"], 0);
    let item = &value["result"]["plan"]["items"][0];
    assert_eq!(item["frees_bytes"], live, "{value}");
    assert_eq!(item["frees_kind"], "upper_bound", "{value}");
    assert!(
        value["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .all(|w| w["code"] != "changed_since_scan"),
        "{value}"
    );
    // What is there is met, and is what the deletion frees.
    let value = ok(&delete(live));
    assert!(!app.exists());
    assert_eq!(value["result"]["freed_bytes"], live, "{value}");
}

#[test]
fn p5_core_04_a_tool_item_holding_what_the_scan_did_not_enter_is_refused() {
    let env = TestEnv::new();
    let top = fs::canonicalize(env.tmp_dir()).unwrap().join("proj");
    let (app, cache) = (top.join("app"), top.join("app/cache"));
    fs::create_dir_all(&cache).unwrap();
    fs::write(app.join("main.rs"), b"fn main() {}\n").unwrap();
    fs::write(cache.join("blob"), vec![6u8; 64 * 1024]).unwrap();
    ok(&run(
        &env,
        &[
            "scan",
            s(&top),
            "--exclude",
            "app/cache",
            "--save",
            "--json",
        ],
    ));
    let (token, frees) = handle(&env, &app);
    let from = format!("--from={token}");
    let figure = format!("--expect-item-bytes={frees}");
    let value = failed(
        &run(&env, &["clean", &from, &figure, "--dry-run", "--json"]),
        1,
        "expectation_mismatch",
    );
    let message = value["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("does not hold everything under"),
        "{value}"
    );
    assert!(cache.join("blob").exists());
}

#[test]
fn p5_core_04_a_sparse_file_made_dense_with_its_time_restored_fails_expect_bytes() {
    let env = TestEnv::new();
    let top = fs::canonicalize(env.tmp_dir()).unwrap().join("sparse");
    let dir = top.join("d");
    fs::create_dir_all(&dir).unwrap();
    let file = dir.join("s.bin");
    let len = 52_428_800u64;
    fs::File::create(&file).unwrap().set_len(len).unwrap();
    let saved = ok(&run(&env, &["scan", s(&top), "--save", "--json"]));
    let id = saved["scan"]["id"].as_str().unwrap().to_owned();
    let scanned = on_disk(&dir) + on_disk(&file);
    let was = fs::metadata(&file).unwrap().modified().unwrap();
    let mut open = fs::OpenOptions::new().write(true).open(&file).unwrap();
    let chunk = vec![5u8; 1 << 20];
    for _ in 0..50 {
        open.write_all(&chunk).unwrap();
    }
    open.sync_all().unwrap();
    open.set_modified(was).unwrap();
    drop(open);
    let figure = scanned.to_string();
    let value = failed(
        &run(
            &env,
            &[
                "clean",
                s(&dir),
                "--scan",
                &id,
                "--permanent",
                "--expect-bytes",
                &figure,
                "--yes",
                "--json",
            ],
        ),
        1,
        "expectation_mismatch",
    );
    assert!(file.exists(), "{value}");
    assert_eq!(
        value["warnings"][0]["code"], "changed_since_scan",
        "{value}"
    );
    let live = value["result"]["plan"]["total_frees_bytes"]
        .as_u64()
        .unwrap();
    assert!(live >= len, "{value}");
    assert_eq!(live, on_disk(&dir) + on_disk(&file), "{value}");
}

#[test]
fn p5_core_04_a_links_other_half_removed_fails_expect_bytes() {
    let env = TestEnv::new();
    let top = fs::canonicalize(env.tmp_dir()).unwrap().join("links");
    let (d, e) = (top.join("d"), top.join("e"));
    fs::create_dir_all(&d).unwrap();
    fs::create_dir_all(&e).unwrap();
    fs::write(d.join("a"), vec![4u8; 1 << 20]).unwrap();
    fs::hard_link(d.join("a"), e.join("b")).unwrap();
    let saved = ok(&run(&env, &["scan", s(&top), "--save", "--json"]));
    let id = saved["scan"]["id"].as_str().unwrap().to_owned();
    let plan = ok(&run(
        &env,
        &["clean", s(&d), "--scan", &id, "--dry-run", "--json"],
    ));
    let scanned = plan["result"]["plan"]["total_frees_bytes"]
        .as_u64()
        .unwrap();
    assert!(scanned < 1 << 20, "{plan}");
    fs::remove_file(e.join("b")).unwrap();
    let figure = scanned.to_string();
    let value = failed(
        &run(
            &env,
            &[
                "clean",
                s(&d),
                "--scan",
                &id,
                "--permanent",
                "--expect-bytes",
                &figure,
                "--yes",
                "--json",
            ],
        ),
        1,
        "expectation_mismatch",
    );
    assert!(d.join("a").exists(), "{value}");
    assert_eq!(
        value["warnings"][0]["code"], "changed_since_scan",
        "{value}"
    );
    assert!(
        value["result"]["plan"]["total_frees_bytes"]
            .as_u64()
            .unwrap()
            >= 1 << 20,
        "{value}"
    );
}

#[test]
fn p5_core_04_a_linked_file_appended_to_is_not_priced_at_nothing() {
    let env = TestEnv::new();
    let top = fs::canonicalize(env.tmp_dir()).unwrap().join("linked");
    let x = top.join("x");
    fs::create_dir_all(&x).unwrap();
    let (p, q) = (x.join("p"), x.join("q"));
    fs::write(&p, vec![4u8; 1 << 20]).unwrap();
    fs::hard_link(&p, &q).unwrap();
    let saved = ok(&run(&env, &["scan", s(&top), "--save", "--json"]));
    let id = saved["scan"]["id"].as_str().unwrap().to_owned();
    let mut file = fs::OpenOptions::new().append(true).open(&p).unwrap();
    file.write_all(b"more").unwrap();
    drop(file);
    let delete = |figure: u64| {
        let figure = figure.to_string();
        run(
            &env,
            &[
                "clean",
                s(&p),
                s(&q),
                "--scan",
                &id,
                "--permanent",
                "--expect-bytes",
                &figure,
                "--yes",
                "--json",
            ],
        )
    };
    let value = failed(&delete(0), 1, "expectation_mismatch");
    assert!(p.exists() && q.exists(), "{value}");
    let live = on_disk(&p);
    assert_eq!(
        value["result"]["plan"]["total_frees_bytes"], live,
        "{value}"
    );
    let value = ok(&delete(live));
    assert!(!p.exists() && !q.exists());
    assert_eq!(value["result"]["freed_bytes"], live, "{value}");
}

/// Whether the test runs as root, whom no mode stops.
fn is_root(env: &TestEnv) -> bool {
    let uid = env.command("id").arg("-u").output().unwrap();
    String::from_utf8_lossy(&uid.stdout).trim() == "0"
}

#[test]
fn p5_core_04_a_directory_the_walk_cannot_read_meets_no_figure() {
    let env = TestEnv::new();
    if is_root(&env) {
        return;
    }
    let top = fs::canonicalize(env.tmp_dir()).unwrap().join("proj");
    let (d, secret) = (top.join("d"), top.join("d/secret"));
    fs::create_dir_all(&secret).unwrap();
    fs::write(d.join("a"), vec![1u8; 4096]).unwrap();
    zeros(&secret.join("blob"), 1 << 20);
    let saved = ok(&run(&env, &["scan", s(&top), "--save", "--json"]));
    let id = saved["scan"]["id"].as_str().unwrap().to_owned();
    let mode = |bits| fs::set_permissions(&secret, fs::Permissions::from_mode(bits)).unwrap();
    mode(0o000);
    // The plan names no figure for it, and says why.
    let value = ok(&run(
        &env,
        &["clean", s(&d), "--scan", &id, "--dry-run", "--json"],
    ));
    let plan = &value["result"]["plan"];
    assert_eq!(plan["items"][0]["frees_bytes"], Value::Null, "{value}");
    assert_eq!(plan["items"][0]["frees_kind"], Value::Null, "{value}");
    let codes: Vec<&str> = value["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["code"].as_str().unwrap())
        .collect();
    assert!(codes.contains(&"partial_read"), "{value}");
    assert!(codes.contains(&"changed_since_scan"), "{value}");
    // Not the total's own figure, nor the one the scan gave.
    let total = plan["total_frees_bytes"].as_u64().unwrap().to_string();
    for figure in ["0", total.as_str()] {
        let out = run(
            &env,
            &[
                "clean",
                s(&d),
                "--scan",
                &id,
                "--permanent",
                "--expect-bytes",
                figure,
                "--yes",
                "--json",
            ],
        );
        let value = failed(&out, 1, "expectation_mismatch");
        let message = value["error"]["message"].as_str().unwrap();
        assert!(message.contains("could not be read whole"), "{value}");
        assert!(d.join("a").exists(), "{value}");
    }
    mode(0o755);
    assert!(secret.join("blob").exists());
}

#[test]
fn p5_core_04_a_tool_item_a_walk_cannot_read_is_refused() {
    let env = TestEnv::new();
    if is_root(&env) {
        return;
    }
    let top = fs::canonicalize(env.tmp_dir()).unwrap().join("proj");
    let (d, secret) = (top.join("d"), top.join("d/secret"));
    fs::create_dir_all(&secret).unwrap();
    fs::write(secret.join("blob"), vec![1u8; 4096]).unwrap();
    let mode = |bits| fs::set_permissions(&secret, fs::Permissions::from_mode(bits)).unwrap();
    // Unreadable at the scan as now: nothing has changed, so the handle
    // plans, and no figure is held to account by what was never read.
    mode(0o000);
    ok(&run(&env, &["scan", s(&top), "--save", "--json"]));
    let (token, _) = handle(&env, &d);
    let from = format!("--from={token}");
    let value = failed(
        &run(
            &env,
            &[
                "clean",
                &from,
                "--expect-item-bytes=0",
                "--dry-run",
                "--json",
            ],
        ),
        1,
        "expectation_mismatch",
    );
    let message = value["error"]["message"].as_str().unwrap();
    assert!(message.contains("could not be read whole"), "{value}");
    mode(0o755);
    assert!(secret.join("blob").exists());
}

#[test]
fn p5_core_04_handles_from_two_saved_scans_are_a_usage_error() {
    let env = TestEnv::new();
    let base = fs::canonicalize(env.tmp_dir()).unwrap();
    let (one, two) = (base.join("one"), base.join("two"));
    for root in [&one, &two] {
        fs::create_dir_all(root).unwrap();
        fs::write(root.join("a.bin"), vec![7u8; 3000]).unwrap();
        save(&env, root);
    }
    let (first, _) = handle(&env, &one.join("a.bin"));
    let (second, _) = handle(&env, &two.join("a.bin"));
    // The plan is made against one saved scan, which never saw the other
    // handle's entry: neither is planned, and nothing moves.
    let out = run(
        &env,
        &[
            "clean",
            &format!("--from={first}"),
            &format!("--from={second}"),
            "--yes",
            "--json",
        ],
    );
    let value = failed(&out, 2, "usage");
    let message = value["error"]["message"].as_str().unwrap();
    assert!(message.contains("two saved scans"), "{value}");
    assert!(one.join("a.bin").exists() && two.join("a.bin").exists());
    // Handles from one scan plan as before.
    let value = failed(
        &run(&env, &["clean", &format!("--from={first}"), "--json"]),
        1,
        "confirmation_required",
    );
    assert_eq!(actions(&value), ["trash:"]);
}

/// A folder that holds a mount point and is not one, from the system's own
/// mounts: on macOS `/System/Volumes`, where the Data volume and the
/// others the system mounts are; on Linux the folder above one of the
/// mount table's mount points that is not one itself (`/sys/fs` above
/// `/sys/fs/cgroup`). `None` where the table has none.
fn folder_holding_a_mount() -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        return Some(PathBuf::from("/System/Volumes"));
    }
    let table = fs::read("/proc/self/mountinfo").ok()?;
    // The mount point is the fifth field; one with an escaped byte is left
    // out rather than unescaped.
    let points: Vec<PathBuf> = table
        .split(|&b| b == b'\n')
        .filter_map(|line| line.split(|&b| b == b' ').nth(4))
        .filter(|point| !point.contains(&b'\\'))
        .map(|point| PathBuf::from(String::from_utf8_lossy(point).into_owned()))
        .collect();
    points
        .iter()
        .filter_map(|point| point.parent())
        .find(|folder| {
            *folder != Path::new("/") && !points.iter().any(|p| p == folder) && folder.is_dir()
        })
        .map(Path::to_owned)
}

#[test]
fn p5_core_04_a_folder_holding_a_mount_point_is_refused_for_the_trash() {
    let env = TestEnv::new();
    let root = tree(&env);
    let id = save(&env, &root);
    let Some(folder) = folder_holding_a_mount() else {
        duscope_testkit::skip("no folder here holds a mount point");
        return;
    };
    // The Trash would take the volumes mounted in it along: refused for
    // what it holds before it is outside the scan's root. Planned only.
    let value = failed(
        &run(&env, &["clean", s(&folder), "--scan", &id, "--json"]),
        1,
        "refused_path",
    );
    assert_eq!(
        actions(&value),
        ["refused:mount_point"],
        "{}",
        folder.display()
    );
    // A deletion stops at a mount instead, leaving it and the folders
    // above it: what the folder holds does not refuse it.
    let value = failed(
        &run(
            &env,
            &["clean", s(&folder), "--scan", &id, "--permanent", "--json"],
        ),
        1,
        "refused_path",
    );
    assert_ne!(
        actions(&value),
        ["refused:mount_point"],
        "{}",
        folder.display()
    );
}

/// Unmounts what a test mounted, innermost first, however it ends.
#[cfg(target_os = "linux")]
struct Mounted<'a> {
    env: &'a TestEnv,
    points: Vec<PathBuf>,
}

#[cfg(target_os = "linux")]
impl Drop for Mounted<'_> {
    fn drop(&mut self) {
        for point in self.points.iter().rev() {
            let _ = self.env.command("umount").arg(point).output();
        }
    }
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "mounts a tmpfs and a bind mount; root; DUSCOPE_TEST_PLATFORM=1"]
fn p5_core_04_linux_a_folder_holding_a_mount_stays_out_of_the_trash() {
    let env = TestEnv::new();
    if std::env::var_os("DUSCOPE_TEST_PLATFORM").is_none_or(|v| v != "1") {
        eprintln!("skipped: set DUSCOPE_TEST_PLATFORM=1");
        return;
    }
    if !is_root(&env) {
        duscope_testkit::skip("mounting needs root");
        return;
    }
    let root = tree(&env);
    // A tmpfs two folders down, another device; and a bind mount of a
    // folder of the same device, which no device number tells.
    let (tmpfs, bound) = (root.join("t"), root.join("b"));
    let outside = fs::canonicalize(env.tmp_dir()).unwrap().join("outside");
    for dir in [tmpfs.join("deep/m"), bound.join("m"), outside.clone()] {
        fs::create_dir_all(dir).unwrap();
    }
    fs::write(tmpfs.join("kept.bin"), vec![1u8; 2000]).unwrap();
    fs::write(outside.join("target.bin"), vec![2u8; 3000]).unwrap();
    let mut mounted = Mounted {
        env: &env,
        points: Vec::new(),
    };
    for args in [
        vec![
            "-t".to_owned(),
            "tmpfs".to_owned(),
            "-o".to_owned(),
            "size=1m".to_owned(),
            "tmpfs".to_owned(),
            s(&tmpfs.join("deep/m")).to_owned(),
        ],
        vec![
            "--bind".to_owned(),
            s(&outside).to_owned(),
            s(&bound.join("m")).to_owned(),
        ],
    ] {
        let point = PathBuf::from(args.last().unwrap());
        let out = env.command("mount").args(&args).output().unwrap();
        assert!(
            out.status.success(),
            "mount {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        mounted.points.push(point);
    }
    fs::write(tmpfs.join("deep/m/on-tmpfs.bin"), vec![3u8; 1000]).unwrap();
    let id = save(&env, &root);
    for folder in [&tmpfs, &bound] {
        let value = failed(
            &run(&env, &["clean", s(folder), "--scan", &id, "--json"]),
            1,
            "refused_path",
        );
        assert_eq!(actions(&value), ["refused:mount_point"], "{value}");
        let value = failed(
            &run(
                &env,
                &["clean", s(folder), "--scan", &id, "--yes", "--json"],
            ),
            1,
            "refused_path",
        );
        assert_eq!(actions(&value), ["refused:mount_point"], "{value}");
        assert_eq!(value["result"]["trashed_count"], 0);
    }
    // Nothing moved: the folders, the mounts and what is on them.
    assert_eq!(fs::read(tmpfs.join("kept.bin")).unwrap(), [1u8; 2000]);
    assert_eq!(
        fs::read(tmpfs.join("deep/m/on-tmpfs.bin")).unwrap(),
        [3u8; 1000]
    );
    assert_eq!(fs::read(bound.join("m/target.bin")).unwrap(), [2u8; 3000]);
    assert!(!env.xdg_data_home().join("Trash/files/t").exists());
    assert!(!env.xdg_data_home().join("Trash/files/b").exists());
    drop(mounted);
}

/// Runs `mount` with `args`, its mount point last, and keeps the point in
/// `mounted` to unmount it; false when it fails, the reason on stderr.
#[cfg(target_os = "linux")]
fn mounted_with(mounted: &mut Mounted<'_>, args: &[&str]) -> bool {
    let out = mounted.env.command("mount").args(args).output().unwrap();
    if !out.status.success() {
        eprintln!("mount {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        return false;
    }
    mounted.points.push(PathBuf::from(args[args.len() - 1]));
    true
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "mounts two tmpfs layers, an overlay of them and mounts inside it; root; \
            DUSCOPE_TEST_PLATFORM=1"]
fn p5_core_10_linux_an_overlays_files_go_to_its_trash_and_its_mounts_stay() {
    use std::os::unix::fs::MetadataExt;

    let env = TestEnv::new();
    if std::env::var_os("DUSCOPE_TEST_PLATFORM").is_none_or(|v| v != "1") {
        eprintln!("skipped: set DUSCOPE_TEST_PLATFORM=1");
        return;
    }
    if !is_root(&env) {
        duscope_testkit::skip("mounting needs root");
        return;
    }
    let layers = fs::canonicalize(env.tmp_dir()).unwrap().join("layers");
    let (lower, upper) = (layers.join("lower"), layers.join("upper"));
    for dir in [&lower, &upper] {
        fs::create_dir_all(dir).unwrap();
    }
    let mut mounted = Mounted {
        env: &env,
        points: Vec::new(),
    };
    // Each layer on a filesystem of its own, so that without xino the
    // overlay reports a file on its layer's device.
    for layer in [&lower, &upper] {
        let point = s(layer);
        assert!(mounted_with(
            &mut mounted,
            &["-t", "tmpfs", "-o", "size=4m", "tmpfs", point]
        ));
    }
    fs::create_dir_all(lower.join("merged")).unwrap();
    fs::write(lower.join("merged/lower.bin"), vec![1u8; 3000]).unwrap();
    for dir in ["data/merged", "work"] {
        fs::create_dir_all(upper.join(dir)).unwrap();
    }
    fs::write(upper.join("data/merged/upper.bin"), vec![2u8; 5000]).unwrap();
    // The overlay is the test's XDG_DATA_HOME: the home Trash, which takes
    // an item on its own filesystem, is then the overlay's, in a debug
    // build (whose test guard moves only such items) as in a release one.
    let overlay = env.xdg_data_home();
    let options = format!(
        "lowerdir={},upperdir={},workdir={},xino=off",
        s(&lower),
        s(&upper.join("data")),
        s(&upper.join("work"))
    );
    if !mounted_with(
        &mut mounted,
        &["-t", "overlay", "-o", &options, "overlay", s(&overlay)],
    ) {
        duscope_testkit::skip_fs("overlay", "an overlay could not be mounted");
        return;
    }
    let merged = overlay.join("merged");
    let dev = |path: &Path| fs::symlink_metadata(path).unwrap().dev();
    // What the device rule misread: a merged directory on the overlay's
    // device, its files on their layers'.
    let (upper_file, lower_file) = (merged.join("upper.bin"), merged.join("lower.bin"));
    assert_ne!(dev(&upper_file), dev(&merged));
    assert_ne!(dev(&lower_file), dev(&merged));
    assert_ne!(dev(&upper_file), dev(&lower_file));
    // Real mounts in the overlay: a tmpfs, a folder holding one, a file of
    // the overlay bound over another (on the upper layer's device, as the
    // overlay's own files are), and a folder bound over another (on the
    // overlay's device, as its folder is).
    for dir in ["tmpfs", "holder/inner", "plain", "bound"] {
        fs::create_dir_all(overlay.join(dir)).unwrap();
    }
    fs::write(overlay.join("source.bin"), vec![3u8; 1000]).unwrap();
    fs::write(merged.join("bound.bin"), vec![4u8; 1000]).unwrap();
    fs::write(overlay.join("plain/kept.bin"), vec![5u8; 1000]).unwrap();
    let (tmpfs, holder, bound_file, bound_dir) = (
        overlay.join("tmpfs"),
        overlay.join("holder"),
        merged.join("bound.bin"),
        overlay.join("bound"),
    );
    for args in [
        vec!["-t", "tmpfs", "-o", "size=1m", "tmpfs", s(&tmpfs)],
        vec![
            "-t",
            "tmpfs",
            "-o",
            "size=1m",
            "tmpfs",
            s(&holder.join("inner")),
        ],
        vec!["--bind", s(&overlay.join("source.bin")), s(&bound_file)],
        vec!["--bind", s(&overlay.join("plain")), s(&bound_dir)],
    ] {
        assert!(mounted_with(&mut mounted, &args));
    }
    fs::write(tmpfs.join("on-tmpfs.bin"), vec![6u8; 1000]).unwrap();
    assert_eq!(dev(&bound_file), dev(&upper_file));
    assert_eq!(dev(&bound_dir), dev(&overlay));
    let id = save(&env, &overlay);

    // The two files are planned for the Trash, and --yes moves them there.
    let files = [s(&upper_file), s(&lower_file)];
    let value = failed(
        &run(
            &env,
            &["clean", files[0], files[1], "--scan", &id, "--json"],
        ),
        1,
        "confirmation_required",
    );
    assert_eq!(actions(&value), ["trash:", "trash:"], "{value}");
    let value = ok(&run(
        &env,
        &[
            "clean", files[0], files[1], "--scan", &id, "--yes", "--json",
        ],
    ));
    assert_eq!(actions(&value), ["trash:", "trash:"], "{value}");
    assert_eq!(value["result"]["trashed_count"], 2, "{value}");
    for item in value["result"]["plan"]["items"].as_array().unwrap() {
        assert_eq!(item["outcome"], "trashed", "{value}");
    }
    assert!(fs::symlink_metadata(&upper_file).is_err());
    assert!(fs::symlink_metadata(&lower_file).is_err());
    let trash = overlay.join("Trash");
    assert_eq!(
        fs::read(trash.join("files/upper.bin")).unwrap(),
        [2u8; 5000]
    );
    assert_eq!(
        fs::read(trash.join("files/lower.bin")).unwrap(),
        [1u8; 3000]
    );
    for name in ["upper.bin", "lower.bin"] {
        let info = fs::read_to_string(trash.join(format!("info/{name}.trashinfo"))).unwrap();
        assert!(
            info.contains(&format!("Path={}", s(&merged.join(name)))),
            "{info}"
        );
    }
    // The lower layer is never written: its file is hidden, not moved.
    assert_eq!(
        fs::read(lower.join("merged/lower.bin")).unwrap(),
        [1u8; 3000]
    );

    // Each real mount stays where it is, planned or with --yes, for the
    // Trash, and a mount point planned for --permanent too.
    for item in [&tmpfs, &holder, &bound_file, &bound_dir] {
        let value = failed(
            &run(&env, &["clean", s(item), "--scan", &id, "--yes", "--json"]),
            1,
            "refused_path",
        );
        assert_eq!(
            actions(&value),
            ["refused:mount_point"],
            "{}: {value}",
            item.display()
        );
        assert_eq!(value["result"]["trashed_count"], 0);
    }
    for item in [&tmpfs, &bound_file, &bound_dir] {
        let value = failed(
            &run(
                &env,
                &["clean", s(item), "--scan", &id, "--permanent", "--json"],
            ),
            1,
            "refused_path",
        );
        assert_eq!(
            actions(&value),
            ["refused:mount_point"],
            "{}: {value}",
            item.display()
        );
    }
    assert_eq!(fs::read(tmpfs.join("on-tmpfs.bin")).unwrap(), [6u8; 1000]);
    assert!(holder.join("inner").is_dir());
    assert_eq!(fs::read(&bound_file).unwrap(), [3u8; 1000]);
    assert_eq!(fs::read(bound_dir.join("kept.bin")).unwrap(), [5u8; 1000]);
    for name in ["tmpfs", "holder", "bound.bin", "bound"] {
        assert!(!trash.join("files").join(name).exists(), "{name}");
    }
    drop(mounted);
}
