//! P4-TUI-06: the mark pane and `r` in the binary. `d` and `D` show the plan
//! as `clean` prints it; only `yes` typed in full moves anything, into the
//! test's own Trash: on Linux the test's XDG Trash, on macOS a disk image's
//! (a platform test), since the startup volume's items go to the account's
//! Trash whatever `HOME` says, which the test environment refuses. Nothing
//! here reaches the real Trash. `r` rescans and reloads.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use duscope_testkit::{TestEnv, duscope_bin};
use termlens::{Key, Terminal};

/// `duscope view <tree>` in a `cols`×`rows` pseudo-terminal, run by the
/// shell, which prints its exit status after it.
fn viewer(env: &TestEnv, tree: &Path, cols: u16, rows: u16) -> Terminal {
    let script = format!(
        "'{}' view '{}'; echo \"exit=$?\"",
        duscope_bin().display(),
        tree.display()
    );
    Terminal::builder()
        .size(cols, rows)
        .timeout(Duration::from_secs(60))
        .env_clear()
        .envs(env.vars())
        .current_dir(env.work_dir())
        .args(["-c", &script])
        .spawn("/bin/sh")
        .unwrap()
}

fn wait(terminal: &mut Terminal, what: &str) {
    terminal
        .wait_until(|screen| screen.text().contains(what))
        .unwrap_or_else(|e| panic!("{what}: {e}:\n{}", terminal.screen().text()));
}

/// Waits for `what` to leave the screen: `Esc` has been read, not joined to
/// the next key as an Alt sequence.
fn wait_gone(terminal: &mut Terminal, what: &str) {
    terminal
        .wait_until(|screen| !screen.text().contains(what))
        .unwrap_or_else(|e| panic!("still {what}: {e}:\n{}", terminal.screen().text()));
}

fn send(terminal: &mut Terminal, text: &str) {
    for c in text.chars() {
        terminal.send(Key::Char(c)).unwrap();
    }
}

/// A tree of three files at the root with the given sizes, saved, the
/// largest first: `a.bin`, `b.bin`, `c.bin`.
fn tree(env: &TestEnv, root: &Path) {
    fs::create_dir_all(root).unwrap();
    fs::write(root.join("a.bin"), vec![1u8; 512 * 1024]).unwrap();
    fs::write(root.join("b.bin"), vec![2u8; 128 * 1024]).unwrap();
    fs::write(root.join("c.bin"), vec![3u8; 32 * 1024]).unwrap();
    saved(env, root);
}

fn saved(env: &TestEnv, root: &Path) {
    let out = env
        .cmd()
        .arg("scan")
        .arg(root)
        .args(["--save", "--json"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn work_tree(env: &TestEnv) -> PathBuf {
    fs::canonicalize(env.work_dir()).unwrap().join("tree")
}

/// The viewer on `root`, with `a.bin` and `c.bin` marked: the largest, on
/// which it starts, and the last sibling. Returns the terminal, the map and
/// the key line up.
fn marked_two(env: &TestEnv, root: &Path, cols: u16, rows: u16) -> Terminal {
    let mut terminal = viewer(env, root, cols, rows);
    wait(&mut terminal, "treemap · ");
    terminal.send(Key::Char('d')).unwrap();
    wait(&mut terminal, "1 marked");
    terminal.send(Key::Char('G')).unwrap();
    wait(&mut terminal, "c.bin");
    terminal.send(Key::Char('d')).unwrap();
    wait(&mut terminal, "2 marked");
    terminal
}

/// The rows of the screen, each cut of the frame's border and trailing
/// space.
fn rows(screen: &str) -> Vec<String> {
    screen
        .lines()
        .map(|line| {
            line.trim_end()
                .trim_start_matches('│')
                .trim_end_matches('│')
                .trim_end()
                .to_owned()
        })
        .collect()
}

#[test]
fn p4_tui_06_the_pane_prints_the_plan_exactly_as_clean_does() {
    let env = TestEnv::new();
    let root = work_tree(&env);
    tree(&env, &root);
    let a = root.join("a.bin");
    let c = root.join("c.bin");

    // `clean a c`, on a wide terminal: its table, the line under it, and
    // the question, answered with no.
    let script = format!(
        "'{}' clean '{}' '{}'; echo \"exit=$?\"",
        duscope_bin().display(),
        a.display(),
        c.display()
    );
    let mut clean = Terminal::builder()
        .size(180, 30)
        .timeout(Duration::from_secs(60))
        .env_clear()
        .envs(env.vars())
        .current_dir(env.work_dir())
        .args(["-c", &script])
        .spawn("/bin/sh")
        .unwrap();
    clean
        .wait_until(|screen| screen.full_text().contains("Type yes"))
        .unwrap();
    let printed = clean.screen().full_text();
    send(&mut clean, "no");
    clean.send(Key::Enter).unwrap();
    clean.wait_exit().unwrap();
    assert!(a.exists() && c.exists());
    let cleaned = rows(&printed);
    let table: Vec<&String> = cleaned
        .iter()
        .skip_while(|line| !line.contains("action"))
        .take_while(|line| !line.starts_with("Type yes"))
        .filter(|line| !line.is_empty())
        .collect();
    assert_eq!(table.len(), 4, "{printed}");

    // The same two marked in the viewer: the pane shows those lines
    // character for character, whatever order the rows are in.
    let mut terminal = marked_two(&env, &root, 180, 30);
    terminal.send(Key::Char('D')).unwrap();
    wait(&mut terminal, "mark pane · 2 marked");
    wait(&mut terminal, "Type yes to move them to the Trash");
    let shown = terminal.screen().text();
    let pane = rows(&shown);
    let mut in_pane: Vec<&String> = pane
        .iter()
        .skip_while(|line| !line.contains("action"))
        .take_while(|line| !line.starts_with("Type yes"))
        .filter(|line| !line.is_empty())
        .collect();
    let mut wanted = table.clone();
    in_pane.sort();
    wanted.sort();
    assert_eq!(in_pane, wanted, "{shown}\n---\n{printed}");
    terminal.send(Key::Esc).unwrap();
    wait_gone(&mut terminal, "mark pane");
    terminal.send(Key::Char('q')).unwrap();
    wait(&mut terminal, "exit=0");
    assert!(a.exists() && c.exists());
}

#[test]
fn p4_tui_06_anything_but_yes_in_the_pane_moves_nothing() {
    let env = TestEnv::new();
    let root = work_tree(&env);
    tree(&env, &root);
    let mut terminal = marked_two(&env, &root, 120, 40);
    terminal.send(Key::Char('D')).unwrap();
    wait(&mut terminal, "Type yes to move them");
    for answer in ["no", "YES", "ye"] {
        send(&mut terminal, answer);
        wait(&mut terminal, &format!("{answer}▌"));
        terminal.send(Key::Enter).unwrap();
        wait(&mut terminal, "the answer was not yes");
    }
    // Esc closes it and moves nothing; the marks are still there.
    terminal.send(Key::Esc).unwrap();
    wait_gone(&mut terminal, "mark pane");
    wait(&mut terminal, "2 marked");
    assert!(root.join("a.bin").exists() && root.join("c.bin").exists());
    terminal.send(Key::Char('q')).unwrap();
    wait(&mut terminal, "exit=0");
    let text = terminal.screen().full_text();
    assert!(!text.contains("moved"), "{text}");
}

#[test]
fn p4_tui_06_a_refused_item_stops_the_plan_and_moves_nothing() {
    let env = TestEnv::new();
    let root = work_tree(&env);
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("a.bin"), vec![1u8; 512 * 1024]).unwrap();
    std::os::unix::fs::symlink("a.bin", root.join("zz-link")).unwrap();
    saved(&env, &root);
    let mut terminal = viewer(&env, &root, 120, 40);
    wait(&mut terminal, "treemap · ");
    // The last sibling: the link.
    terminal.send(Key::Char('G')).unwrap();
    wait(&mut terminal, "zz-link");
    terminal.send(Key::Char('d')).unwrap();
    wait(&mut terminal, "1 marked");
    terminal.send(Key::Char('D')).unwrap();
    wait(&mut terminal, "refused: symlink");
    wait(&mut terminal, "1 of the 1 items cannot be moved");
    assert!(
        !terminal.screen().text().contains("Type yes"),
        "{}",
        terminal.screen().text()
    );
    send(&mut terminal, "yes");
    terminal.send(Key::Enter).unwrap();
    terminal.send(Key::Esc).unwrap();
    wait_gone(&mut terminal, "mark pane");
    wait(&mut terminal, "1 marked");
    assert!(fs::symlink_metadata(root.join("zz-link")).is_ok());
    terminal.send(Key::Char('q')).unwrap();
    wait(&mut terminal, "exit=0");
}

/// Marks `a.bin` and `c.bin` under `root`, confirms with `yes`, and checks
/// what the pane promised: both gone from the tree and whole in the
/// `trashed` place, the map's freed tiles, and the summary on the way out.
fn confirm_and_check(env: &TestEnv, root: &Path, trashed: &dyn Fn(&str) -> PathBuf) {
    let a = root.join("a.bin");
    let c = root.join("c.bin");
    let mut terminal = marked_two(env, root, 120, 40);
    terminal.send(Key::Char('D')).unwrap();
    wait(&mut terminal, "2 items to move to the Trash");
    wait(&mut terminal, "Type yes to move them");
    send(&mut terminal, "yes");
    wait(&mut terminal, "yes▌");
    terminal.send(Key::Enter).unwrap();
    wait(&mut terminal, "moved 2 items to the Trash");
    wait(&mut terminal, " freed ");
    assert!(!a.exists() && !c.exists());
    assert_eq!(fs::read(trashed("a.bin")).unwrap(), vec![1u8; 512 * 1024]);
    assert_eq!(fs::read(trashed("c.bin")).unwrap(), vec![3u8; 32 * 1024]);
    // `b.bin` is where it was; the viewer goes on.
    assert!(root.join("b.bin").exists());
    terminal.send(Key::Char('q')).unwrap();
    wait(&mut terminal, "exit=0");
    let text = terminal.screen().full_text();
    assert!(
        text.contains("moved 2 items to the Trash") && text.contains("(not yet freed)"),
        "{text}"
    );
    assert!(text.contains("the saved scans still hold"), "{text}");
}

#[cfg(target_os = "linux")]
#[test]
fn p4_tui_06_mark_two_items_confirm_and_both_are_in_the_trash() {
    let env = TestEnv::new();
    let root = work_tree(&env);
    tree(&env, &root);
    let trash = env.xdg_data_home().join("Trash/files");
    confirm_and_check(&env, &root, &|name| trash.join(name));
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "attaches a disk image; DUSCOPE_TEST_PLATFORM=1"]
fn p4_tui_06_macos_mark_two_items_confirm_and_both_are_in_the_images_trash() {
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
    tree(&env, &mount);
    // The volume's own Trash, this user's directory in it.
    let uid = std::process::Command::new("id")
        .arg("-u")
        .output()
        .unwrap()
        .stdout;
    let uid = String::from_utf8(uid).unwrap().trim().to_owned();
    let trash = mount.join(".Trashes").join(uid);
    confirm_and_check(&env, &mount, &|name| trash.join(name));
    drop(detach);
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

#[test]
fn p4_tui_06_r_rescans_the_fixture_tree_and_reloads() {
    let env = TestEnv::new();
    let root = env.fixtures().case_root("extensions");
    let out = env
        .cmd()
        .arg("scan")
        .arg(&root)
        .args(["--save", "--json"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let mut terminal = viewer(&env, &root, 120, 40);
    wait(&mut terminal, "treemap · ");
    terminal.send(Key::Char('r')).unwrap();
    wait(&mut terminal, "rescanned");
    // The viewer goes on: the map is back, the keys answer.
    wait(&mut terminal, "treemap · ");
    terminal.send(Key::Char('2')).unwrap();
    wait(&mut terminal, "list · ");
    terminal.send(Key::Char('q')).unwrap();
    wait(&mut terminal, "exit=0");
}

#[test]
fn p4_tui_06_r_shows_what_changed_keeping_the_view_and_the_selection() {
    let env = TestEnv::new();
    let root = work_tree(&env);
    fs::create_dir_all(root.join("dir")).unwrap();
    fs::write(root.join("dir/big.bin"), vec![1u8; 1024 * 1024]).unwrap();
    fs::write(root.join("dir/small.bin"), vec![2u8; 8 * 1024]).unwrap();
    let mut terminal = viewer(&env, &root, 120, 40);
    wait(&mut terminal, "treemap · ");
    // The list view of `dir`, its files listed by name.
    terminal.send(Key::Char('2')).unwrap();
    wait(&mut terminal, "list · ");
    wait(&mut terminal, "big.bin");
    // What the rescan should find: a new large file.
    fs::write(root.join("dir/fresh-new.bin"), vec![3u8; 2048 * 1024]).unwrap();
    // FSEvents' history, which an incremental rescan reads on macOS, trails
    // the filesystem by about a second.
    std::thread::sleep(Duration::from_millis(1500));
    terminal.send(Key::Char('r')).unwrap();
    wait(&mut terminal, "rescanned");
    // Still the list view, the new file in it; the cursor on `big.bin`.
    wait(&mut terminal, "list · ");
    wait(&mut terminal, "fresh-new.bin");
    let text = terminal.screen().text();
    assert!(text.contains("big.bin"), "{text}");
    terminal.send(Key::Char('q')).unwrap();
    wait(&mut terminal, "exit=0");
}
