//! P4-TEST-01: the navigation eval. The ten tasks of `nav_tasks.toml`, each a
//! scripted session of `duscope view` in a pseudo-terminal: the keys a person
//! would press and what the screen must show after each. Each must complete
//! without `?`: a script that presses it, or types it, is refused before it
//! runs, and a step that finds the help on the screen fails.
//!
//! The Trash is never the machine's own. On Linux a task with a Trash step
//! moves its file into the test's XDG Trash; on macOS, under the test
//! environment's guard, an item of the startup volume cannot be moved at all
//! (`clean`'s guard, `duscope/tests/clean.rs`), so such a task runs its
//! `no-trash` steps here and its `trash` steps on a disk image, a platform
//! test (`DUSCOPE_TEST_PLATFORM=1`).

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use duscope_testkit::{TestEnv, duscope_bin};
use serde::Deserialize;
use termlens::{Key, Screen, Terminal};

/// The scripts, in the row's order.
const SCRIPTS: &str = include_str!("nav_tasks.toml");

/// How long one step may wait for what it expects.
const STEP_WAIT: Duration = Duration::from_secs(60);
/// The first wait of a resize step, the one timing-sensitive kind.
const RESIZE_WAIT: Duration = Duration::from_secs(10);

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Scripts {
    task: Vec<Task>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Task {
    id: String,
    title: String,
    tree: Tree,
    #[serde(default = "default_size")]
    size: (u16, u16),
    step: Vec<Step>,
}

fn default_size() -> (u16, u16) {
    (120, 40)
}

/// The tree a task runs on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Tree {
    /// [`nav_tree`], built for the eval.
    Nav,
    /// A copy of the fixture tree's `extensions` case: nothing moves out of
    /// the shared fixture tree.
    Fixture,
}

/// Where a step runs: where the test has a Trash of its own, or where it
/// has none (macOS's startup volume, under the guard).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum Only {
    Trash,
    NoTrash,
}

/// One step: what it sends, then what it waits for on the screen.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct Step {
    /// Keys, each pressed in turn: a character (`"d"`, `"G"`, `"/"`) or a
    /// name (`"enter"`, `"esc"`, `"tab"`, `"backtab"`, `"backspace"`,
    /// `"up"`, `"down"`, `"left"`, `"right"`, `"home"`, `"end"`, `"pgup"`,
    /// `"pgdn"`).
    press: Vec<String>,
    /// Text typed a character at a time, after the keys.
    text: Option<String>,
    /// A new terminal size, columns and rows, instead of keys.
    resize: Option<(u16, u16)>,
    /// What must appear on the screen.
    see: Vec<String>,
    /// What must leave the screen.
    gone: Vec<String>,
    /// What the session must have printed on the main screen.
    printed: Vec<String>,
    /// What it must not have printed there.
    unprinted: Vec<String>,
    /// Runs only where the Trash is the test's own, or only where it is not.
    only: Option<Only>,
}

impl Step {
    fn runs(&self, own_trash: bool) -> bool {
        match self.only {
            None => true,
            Some(Only::Trash) => own_trash,
            Some(Only::NoTrash) => !own_trash,
        }
    }
}

/// Reads `text`, refusing what the eval does not allow: a script that
/// presses or types `?`, and a step that waits for nothing.
fn parse(text: &str) -> Result<Vec<Task>, String> {
    let scripts: Scripts = toml::from_str(text).map_err(|e| e.to_string())?;
    for task in &scripts.task {
        check(task)?;
    }
    Ok(scripts.task)
}

fn check(task: &Task) -> Result<(), String> {
    for step in &task.step {
        let sent = step
            .press
            .iter()
            .map(String::as_str)
            .chain(step.text.as_deref());
        if sent.into_iter().any(|keys| keys.contains('?')) {
            return Err(format!(
                "{}: a task must complete without `?`, and a step presses it: {:?}",
                task.id, step.press
            ));
        }
        for key in &step.press {
            key_of(key)?;
        }
        if step.see.is_empty()
            && step.gone.is_empty()
            && step.printed.is_empty()
            && step.unprinted.is_empty()
        {
            return Err(format!(
                "{}: a step of {:?} checks nothing",
                task.id, step.press
            ));
        }
    }
    Ok(())
}

/// The key `name` stands for in a script.
fn key_of(name: &str) -> Result<Key, String> {
    let mut chars = name.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        return Ok(Key::Char(c));
    }
    Ok(match name {
        "enter" => Key::Enter,
        "esc" => Key::Esc,
        "tab" => Key::Tab,
        "backtab" => Key::BackTab,
        "backspace" => Key::Backspace,
        "up" => Key::Up,
        "down" => Key::Down,
        "left" => Key::Left,
        "right" => Key::Right,
        "home" => Key::Home,
        "end" => Key::End,
        "pgup" => Key::PageUp,
        "pgdn" => Key::PageDown,
        other => return Err(format!("no such key in a script: {other:?}")),
    })
}

/// The ten tasks, checked.
fn tasks() -> Vec<Task> {
    parse(SCRIPTS).unwrap()
}

fn task(id: &str) -> Task {
    tasks()
        .into_iter()
        .find(|task| task.id == id)
        .unwrap_or_else(|| panic!("no task {id}"))
}

/// `duscope view <root>` at the task's size, run by the shell, which prints
/// its exit status after it.
fn viewer(env: &TestEnv, root: &Path, size: (u16, u16)) -> Terminal {
    let script = format!(
        "'{}' view '{}'; echo \"exit=$?\"",
        duscope_bin().display(),
        root.display()
    );
    Terminal::builder()
        .size(size.0, size.1)
        .timeout(STEP_WAIT)
        .env_clear()
        .envs(env.vars())
        .current_dir(env.work_dir())
        .args(["-c", &script])
        .spawn("/bin/sh")
        .unwrap()
}

/// Whether a screen is what `step` waits for. A resize step waits for the
/// frame drawn at the new size: the grid takes the size at once and keeps
/// the old frame, clipped or padded, until the viewer repaints, so the key
/// line (`… q quit`) must be on the new last row.
fn met(screen: &Screen, step: &Step) -> bool {
    let text = screen.text();
    let full = screen.full_text();
    step.see.iter().all(|want| text.contains(want.as_str()))
        && !step.gone.iter().any(|want| text.contains(want.as_str()))
        && step.printed.iter().all(|want| full.contains(want.as_str()))
        && !step
            .unprinted
            .iter()
            .any(|want| full.contains(want.as_str()))
        && step.resize.is_none_or(|(cols, rows)| {
            screen.size() == (cols, rows) && screen.row_text(rows - 1).contains("q quit")
        })
}

/// Performs `step` and waits for what it expects; a resize step, the one
/// timing-sensitive kind, gets the one retry the spec allows.
fn perform(terminal: &mut Terminal, step: &Step) {
    if let Some((cols, rows)) = step.resize {
        terminal.resize(cols, rows).unwrap();
        let settled = terminal.wait_until_for(|screen| met(screen, step), RESIZE_WAIT);
        if settled.is_err() {
            // One retry, a size away first: the kernel signals only a change.
            terminal.resize(cols + 1, rows).unwrap();
            terminal.resize(cols, rows).unwrap();
            terminal
                .wait_until(|screen| met(screen, step))
                .unwrap_or_else(|e| panic!("resize {cols}×{rows}: {e}:\n{}", shown(terminal)));
        }
        return;
    }
    for key in &step.press {
        terminal.send(key_of(key).unwrap()).unwrap();
    }
    if let Some(text) = &step.text {
        for c in text.chars() {
            terminal.send(Key::Char(c)).unwrap();
        }
    }
    terminal
        .wait_until(|screen| met(screen, step))
        .unwrap_or_else(|e| panic!("{step:?}: {e}:\n{}", shown(terminal)));
}

fn shown(terminal: &Terminal) -> String {
    terminal.screen().full_text()
}

/// Runs `task` on `root`: each step in turn, the help never on the screen.
fn run(env: &TestEnv, root: &Path, task: &Task, own_trash: bool) -> Terminal {
    let mut terminal = viewer(env, root, task.size);
    terminal
        .wait_until(|screen| screen.text().contains("treemap · "))
        .unwrap_or_else(|e| panic!("{}: {e}:\n{}", task.title, shown(&terminal)));
    for step in task.step.iter().filter(|step| step.runs(own_trash)) {
        perform(&mut terminal, step);
        assert!(
            !terminal.screen().text().contains("this help"),
            "{}: the help is on the screen:\n{}",
            task.title,
            terminal.screen().text()
        );
    }
    terminal
}

/// The tree the eval navigates, saved: folders two deep, the largest entry a
/// file under the largest folder, every file's size a whole number of
/// 4 KiB blocks, so each filesystem rounds them alike.
fn nav_tree(env: &TestEnv) -> PathBuf {
    let root = fs::canonicalize(env.work_dir()).unwrap().join("tree");
    nav_tree_at(env, &root)
}

/// [`nav_tree`] at `root`.
fn nav_tree_at(env: &TestEnv, root: &Path) -> PathBuf {
    let files: &[(&str, usize)] = &[
        ("media/movie.mkv", 1024 * 1024),
        ("media/photos/a.jpg", 128 * 1024),
        ("media/photos/b.jpg", 64 * 1024),
        ("archive.tar", 512 * 1024),
        ("projects/docs/guide.md", 96 * 1024),
        ("projects/docs/notes.md", 32 * 1024),
        ("projects/app/main.rs", 40 * 1024),
        ("projects/app/lib.rs", 8 * 1024),
        ("readme.txt", 4 * 1024),
    ];
    for &(name, size) in files {
        let path = root.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, vec![b'x'; size]).unwrap();
    }
    saved(env, root);
    root.to_path_buf()
}

/// A copy of the fixture tree's `extensions` case, saved.
fn fixture_tree(env: &TestEnv) -> PathBuf {
    let root = fs::canonicalize(env.work_dir()).unwrap().join("tree");
    copy_tree(&env.fixtures().case_root("extensions"), &root);
    saved(env, &root);
    root
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).unwrap();
        }
    }
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

fn tree_of(env: &TestEnv, task: &Task) -> PathBuf {
    match task.tree {
        Tree::Nav => nav_tree(env),
        Tree::Fixture => fixture_tree(env),
    }
}

/// Runs the task named `id` on its own tree, with no Trash of the test's
/// own, and checks the shell's status after it.
fn eval(id: &str) {
    let env = TestEnv::new();
    let task = task(id);
    let root = tree_of(&env, &task);
    let terminal = run(&env, &root, &task, false);
    assert_finished(&terminal, &task);
}

/// The viewer has given the terminal back, `q` having ended the session.
fn assert_finished(terminal: &Terminal, task: &Task) {
    assert_restored(terminal, &task.title);
}

/// The terminal is back on the main screen, the shell having printed the
/// status of a viewer that exited 0, with the cursor shown and the mouse off.
fn assert_restored(terminal: &Terminal, what: &str) {
    let screen = terminal.screen();
    let text = screen.full_text();
    assert!(text.contains("exit=0"), "{what}: {text}");
    assert!(!screen.alternate_screen(), "{what}: {text}");
    assert!(screen.cursor_visible(), "{what}: {text}");
    assert!(screen.mouse_modes().is_empty(), "{what}: {text}");
}

/// termlens's resize storm: fifty sizes at once, with keys among them and
/// never a wait, down past the viewer's minimum and up to 200×60. The
/// viewer draws the last size and gives the terminal back.
#[test]
fn p4_test_01_a_resize_storm_ends_at_its_last_size() {
    let env = TestEnv::new();
    let root = nav_tree(&env);
    let mut terminal = viewer(&env, &root, (120, 40));
    terminal
        .wait_until(|screen| screen.text().contains("treemap · "))
        .unwrap_or_else(|e| panic!("{e}:\n{}", shown(&terminal)));
    let sizes = [
        (200, 60),
        (80, 24),
        (60, 20),
        (133, 41),
        (140, 45),
        (110, 33),
    ];
    for (i, &(cols, rows)) in sizes.iter().cycle().take(50).enumerate() {
        terminal.resize(cols, rows).unwrap();
        if i % 3 == 0 {
            terminal.send(Key::Char('s')).unwrap();
        }
    }
    let last = Step {
        resize: Some((150, 44)),
        see: vec!["treemap · ".to_owned()],
        ..Step::default()
    };
    // The frame drawn at 150×44, its key line on the last row.
    perform(&mut terminal, &last);
    terminal.send(Key::Char('q')).unwrap();
    // crossterm returns a resize without reading the terminal when both wake
    // one poll, and the poll reports a byte once, so a key on a storm's heels
    // can wait for the next byte: the one retry the spec allows.
    let quit = terminal.wait_until_for(|screen| screen.text().contains("exit=0"), RESIZE_WAIT);
    if quit.is_err() {
        terminal.send(Key::Char('q')).unwrap();
        terminal
            .wait_until(|screen| screen.text().contains("exit=0"))
            .unwrap_or_else(|e| panic!("{e}:\n{}", shown(&terminal)));
    }
    assert_restored(&terminal, "a resize storm");
}

/// The ids of the tasks, in the row's order: a task added, renamed or
/// reordered in the file must be added here too, with its test.
const IDS: [&str; 10] = [
    "reach_largest_file",
    "open_typed_path",
    "filter_one_class",
    "zoom_two_levels",
    "sort_by_count",
    "mark_three_total",
    "trash_marked_file",
    "switch_layout_view",
    "resize_and_back",
    "quit_with_summary",
];

#[test]
fn p4_test_01_the_scripts_are_the_ten_tasks_of_the_row() {
    let listed: Vec<String> = tasks().into_iter().map(|task| task.id).collect();
    assert_eq!(listed, IDS);
}

#[test]
fn p4_test_01_a_script_that_presses_or_types_help_is_refused() {
    let pressed = r#"
[[task]]
id = "x"
title = "x"
tree = "nav"
[[task.step]]
press = ["?"]
see = ["anything"]
"#;
    let error = parse(pressed).unwrap_err();
    assert!(error.contains("without `?`"), "{error}");
    let typed = pressed.replace("press = [\"?\"]", "text = \"*?\"");
    let error = parse(&typed).unwrap_err();
    assert!(error.contains("without `?`"), "{error}");
    let checked = pressed.replace("press = [\"?\"]", "press = [\"d\"]");
    assert!(parse(&checked).is_ok());
    let unknown = pressed.replace("press = [\"?\"]", "press = [\"frobnicate\"]");
    assert!(parse(&unknown).unwrap_err().contains("no such key"));
}

#[test]
fn p4_test_01_a_step_that_checks_nothing_is_refused() {
    let script = r#"
[[task]]
id = "x"
title = "x"
tree = "nav"
[[task.step]]
press = ["d"]
"#;
    let error = parse(script).unwrap_err();
    assert!(error.contains("checks nothing"), "{error}");
}

#[test]
fn p4_test_01_nav_task_reach_largest_file() {
    eval("reach_largest_file");
}

#[test]
fn p4_test_01_nav_task_open_typed_path() {
    eval("open_typed_path");
}

#[test]
fn p4_test_01_nav_task_filter_one_class() {
    eval("filter_one_class");
}

#[test]
fn p4_test_01_nav_task_zoom_two_levels() {
    eval("zoom_two_levels");
}

#[test]
fn p4_test_01_nav_task_sort_by_count() {
    eval("sort_by_count");
}

#[test]
fn p4_test_01_nav_task_mark_three_total() {
    eval("mark_three_total");
}

#[test]
fn p4_test_01_nav_task_switch_layout_view() {
    eval("switch_layout_view");
}

#[test]
fn p4_test_01_nav_task_resize_and_back() {
    eval("resize_and_back");
}

/// `trash_marked_file` where the test has a Trash of its own: into the
/// test's XDG Trash on Linux, whole.
#[cfg(target_os = "linux")]
#[test]
fn p4_test_01_nav_task_trash_marked_file() {
    let env = TestEnv::new();
    let task = task("trash_marked_file");
    let root = tree_of(&env, &task);
    let bytes = fs::read(root.join("tool")).unwrap();
    let terminal = run(&env, &root, &task, true);
    assert_finished(&terminal, &task);
    assert!(!root.join("tool").exists());
    let trashed = env.xdg_data_home().join("Trash/files/tool");
    assert_eq!(fs::read(trashed).unwrap(), bytes);
}

/// `trash_marked_file` on macOS, where the startup volume has no Trash a
/// test may touch: the plan is read, the answer is no, the file stays. The
/// platform test moves it for real on a disk image.
#[cfg(target_os = "macos")]
#[test]
fn p4_test_01_nav_task_trash_marked_file_stops_short_of_the_trash() {
    let env = TestEnv::new();
    let task = task("trash_marked_file");
    let root = tree_of(&env, &task);
    let terminal = run(&env, &root, &task, false);
    assert_finished(&terminal, &task);
    assert!(root.join("tool").exists());
}

#[cfg(target_os = "linux")]
#[test]
fn p4_test_01_nav_task_quit_with_summary() {
    let env = TestEnv::new();
    let task = task("quit_with_summary");
    let root = tree_of(&env, &task);
    let terminal = run(&env, &root, &task, true);
    assert_finished(&terminal, &task);
}

/// On macOS the session moves nothing here: `q` prints no summary, the
/// viewer's rule. The platform test moves something first.
#[cfg(target_os = "macos")]
#[test]
fn p4_test_01_nav_task_quit_with_summary_when_nothing_moved() {
    eval("quit_with_summary");
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "attaches a disk image; DUSCOPE_TEST_PLATFORM=1"]
fn p4_test_01_macos_the_trash_tasks_move_a_file_on_an_image() {
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
    let uid = String::from_utf8(
        std::process::Command::new("id")
            .arg("-u")
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_owned();
    let trash = mount.join(".Trashes").join(uid);

    // On the image: the fixture case for `trash_marked_file`, the eval's
    // tree for `quit_with_summary`, each its own volume folder.
    let root = mount.join("trash");
    copy_tree(&env.fixtures().case_root("extensions"), &root);
    saved(&env, &root);
    let task_trash = task("trash_marked_file");
    let bytes = fs::read(root.join("tool")).unwrap();
    let terminal = run(&env, &root, &task_trash, true);
    assert_finished(&terminal, &task_trash);
    assert!(!root.join("tool").exists());
    assert_eq!(fs::read(trash.join("tool")).unwrap(), bytes);

    let root = mount.join("summary");
    let task_summary = task("quit_with_summary");
    let source = nav_tree_at(&env, &root);
    let terminal = run(&env, &source, &task_summary, true);
    assert_finished(&terminal, &task_summary);
    assert!(trash.join("readme.txt").exists());
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
