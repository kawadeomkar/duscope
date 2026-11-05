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

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::fs;
use std::os::unix::fs::symlink;
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
    assert_eq!(value["next"][0], format!("duscope rescan --scan {id}"));
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
    assert_eq!(value["next"][0], format!("duscope rescan --scan {id}"));
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
    let id = save(&env, &root);
    let file = root.join("a.bin");
    let value = ok(&run(
        &env,
        &["clean", s(&file), "--permanent", "--yes", "--json"],
    ));
    // `rescan` reads a bare argument as a path, so the saved scan is named
    // with --scan, in the hint and in `next`.
    let next = format!("duscope rescan --scan {id}");
    assert_eq!(value["next"][0], next);
    let hint = value["warnings"][0]["hint"].as_str().unwrap();
    assert!(hint.contains(&next), "{hint}");
    let mut args: Vec<&str> = next.split(' ').skip(1).collect();
    args.push("--json");
    let again = ok(&run(&env, &args));
    assert!(
        again["scan"]["id"].as_str().is_some_and(|new| new != id),
        "{again}"
    );
}
