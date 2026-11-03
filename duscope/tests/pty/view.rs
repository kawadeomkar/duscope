//! P4-TUI-01: the viewer's terminal lifecycle. `view` on the fixture tree in
//! a pseudo-terminal: it takes the alternate screen and the mouse, and gives
//! the terminal back as it found it after `q`, `Ctrl-C` and a panic, and
//! across resizes; it refuses a terminal below 80×24 and a stdout that is not
//! a terminal. The checks read the terminal's state, not the viewer's
//! panes, which later packages draw.

use std::process::Output;
use std::time::Duration;

use duscope_core::rules::Class;
use duscope_testkit::{TestEnv, duscope_bin};
use duscope_treemap::packs::Pack;
use termlens::{Key, Screen, Terminal};

/// The fixture case the viewer opens.
const CASE: &str = "extensions";

/// `duscope view` on the fixture case in a `cols`×`rows` pseudo-terminal,
/// run by `/bin/sh` so that `stty -a` prints the terminal's modes once the
/// viewer has exited; `extra` joins the test environment.
fn viewer(env: &TestEnv, cols: u16, rows: u16, extra: &[(&str, &str)]) -> Terminal {
    let tree = env.fixtures().case_root(CASE);
    let script = format!(
        "'{}' view '{}'; echo \"exit=$?\"; stty -a",
        duscope_bin().display(),
        tree.display()
    );
    Terminal::builder()
        .size(cols, rows)
        .timeout(Duration::from_secs(60))
        .env_clear()
        .envs(env.vars())
        .envs(extra.iter().copied())
        .current_dir(env.work_dir())
        .args(["-c", &script])
        .spawn("/bin/sh")
        .unwrap()
}

/// Whether the viewer is up: on the alternate screen, with the mouse
/// reported and something drawn.
fn up(screen: &Screen) -> bool {
    screen.alternate_screen()
        && !screen.mouse_modes().is_empty()
        && !screen.text().trim().is_empty()
}

/// Waits for the shell to print the viewer's exit status and the modes, and
/// returns the status.
fn exited(terminal: &mut Terminal) -> i32 {
    terminal.wait_exit().unwrap();
    let screen = terminal.screen();
    let text = screen.full_text();
    let status = text
        .lines()
        .find_map(|l| l.trim().strip_prefix("exit="))
        .unwrap_or_else(|| panic!("no exit status:\n{text}"));
    status.trim().parse().unwrap()
}

/// Asserts the terminal is as the viewer found it: the main screen, the
/// cursor shown, no mouse reporting or bracketed paste, and the line
/// discipline back in canonical mode with echo (`stty -a` names `icanon`
/// and `echo`, not `-icanon` and `-echo`).
fn assert_restored(terminal: &Terminal) {
    let screen = terminal.screen();
    let text = screen.full_text();
    assert!(
        !screen.alternate_screen(),
        "still on the alternate screen:\n{text}"
    );
    assert!(screen.cursor_visible(), "cursor hidden:\n{text}");
    assert!(
        screen.mouse_modes().is_empty(),
        "mouse still reported:\n{text}"
    );
    assert!(
        !screen.bracketed_paste(),
        "bracketed paste left on:\n{text}"
    );
    let words: Vec<&str> = text.split_whitespace().collect();
    for mode in ["icanon", "echo"] {
        assert!(words.contains(&mode), "{mode} not restored:\n{text}");
        assert!(
            !words.contains(&format!("-{mode}").as_str()),
            "-{mode}:\n{text}"
        );
    }
}

#[test]
fn p4_tui_01_q_restores_the_terminal() {
    let env = TestEnv::new();
    let mut terminal = viewer(&env, 100, 30, &[]);
    terminal.wait_until(up).unwrap();
    terminal.send(Key::Char('q')).unwrap();
    assert_eq!(exited(&mut terminal), 0);
    assert_restored(&terminal);
}

#[test]
fn p4_tui_01_ctrl_c_quits_as_q_does() {
    let env = TestEnv::new();
    let mut terminal = viewer(&env, 100, 30, &[]);
    terminal.wait_until(up).unwrap();
    // In raw mode Ctrl-C reaches the viewer as a key, not as SIGINT.
    terminal.send(Key::Ctrl('c')).unwrap();
    assert_eq!(exited(&mut terminal), 0);
    assert_restored(&terminal);
}

#[test]
fn p4_tui_01_a_panic_restores_the_terminal_before_its_message() {
    let env = TestEnv::new();
    // DUSCOPE_TEST_PANIC (debug builds only) panics after the first frame.
    let mut terminal = viewer(&env, 100, 30, &[("DUSCOPE_TEST_PANIC", "1")]);
    let status = exited(&mut terminal);
    assert_ne!(status, 0);
    assert_restored(&terminal);
    // The message was printed after the restore, so it is on the main
    // screen, where people can read it.
    let text = terminal.screen().full_text();
    assert!(text.contains("panicked"), "{text}");
}

#[test]
fn p4_tui_01_a_resize_redraws_at_the_new_size() {
    let env = TestEnv::new();
    let mut terminal = viewer(&env, 100, 30, &[]);
    terminal.wait_until(up).unwrap();
    // Every frame reaches the last row, where the key line goes.
    let last_row_drawn =
        |rows: u16| move |s: &Screen| up(s) && !s.row_text(rows - 1).trim().is_empty();
    terminal.wait_until(last_row_drawn(30)).unwrap();
    for (cols, rows) in [(140, 45), (80, 24)] {
        terminal.resize(cols, rows).unwrap();
        terminal.wait_until(last_row_drawn(rows)).unwrap();
    }
    terminal.send(Key::Char('q')).unwrap();
    assert_eq!(exited(&mut terminal), 0);
    assert_restored(&terminal);
}

#[test]
fn p4_tui_01_a_terminal_below_80x24_is_refused() {
    let env = TestEnv::new();
    for (cols, rows) in [(79, 24), (80, 23)] {
        let mut terminal = viewer(&env, cols, rows, &[]);
        assert_eq!(exited(&mut terminal), 2, "{cols}×{rows}");
        let screen = terminal.screen();
        let text = screen.full_text();
        assert!(!text.contains("exit=0"), "{text}");
        assert!(text.contains("80×24"), "{text}");
        assert!(text.contains("hint: "), "{text}");
        assert_restored(&terminal);
    }
}

/// `duscope <args>` on the fixture case with stdout and stderr piped.
fn piped(env: &TestEnv, args: &[&str]) -> Output {
    let tree = env.fixtures().case_root(CASE);
    env.cmd().args(args).arg(&tree).output().unwrap()
}

#[test]
fn p4_tui_01_a_stdout_that_is_not_a_terminal_is_refused() {
    let env = TestEnv::new();
    for args in [&["view"][..], &["scan", "-i"]] {
        let out = piped(&env, args);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert!(out.stdout.is_empty(), "{args:?}");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains("terminal"), "{args:?}: {stderr}");
        assert!(stderr.contains("hint: "), "{args:?}: {stderr}");
    }
    // An agent gets the error as the envelope: a usage error it can fix by
    // running another verb.
    let out = piped(&env, &["view", "--json"]);
    assert_eq!(out.status.code(), Some(2));
    let envelope: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(envelope["ok"], false);
    assert_eq!(envelope["error"]["code"], "usage");
    assert_eq!(envelope["error"]["fixable_by"], "agent");
}

/// The colours of the cells the viewer drew, foreground and background, in
/// a terminal that answers OSC 11 with `background` and has the environment
/// `extra` (24-bit colour, so the cells carry the pack's own colours).
fn colours_drawn(
    env: &TestEnv,
    background: (u8, u8, u8),
    extra: &[(&str, &str)],
) -> Vec<termlens::Color> {
    let tree = env.fixtures().case_root(CASE);
    // Wide enough for the legend pane, whose squares are each class's
    // own colour (a tile's may be tinted toward the base).
    let mut terminal = Terminal::builder()
        .size(160, 45)
        .timeout(Duration::from_secs(60))
        .background_rgb(background.0, background.1, background.2)
        .env_clear()
        .envs(env.vars())
        .env("COLORTERM", "truecolor")
        .envs(extra.iter().copied())
        .current_dir(env.work_dir())
        .arg("view")
        .arg(&tree)
        .spawn(duscope_bin())
        .unwrap();
    // The frame is whole once its last row, the key line, is drawn.
    terminal
        .wait_until(|s| up(s) && !s.row_text(44).trim().is_empty())
        .unwrap();
    let screen = terminal.screen();
    let mut colours = Vec::new();
    for row in 0..screen.rows() {
        for col in 0..screen.cols() {
            if let Some(cell) = screen.cell(row, col) {
                colours.push(cell.style().fg);
                colours.push(cell.style().bg);
            }
        }
    }
    terminal.send(Key::Char('q')).unwrap();
    assert!(terminal.wait_exit().unwrap().success());
    colours
}

/// The colour `pack` gives archives and media, which the fixture case
/// holds, as the terminal shows it.
fn class_colours(pack: &str) -> Vec<termlens::Color> {
    let pack = Pack::named(pack).unwrap();
    [Class::Archives, Class::Media]
        .into_iter()
        .map(|class| match pack.class(class) {
            duscope_treemap::cells::Color::Rgb(r, g, b) => termlens::Color::Rgb(r, g, b),
            other => panic!("{other:?}"),
        })
        .collect()
}

#[test]
fn p4_tui_01_the_terminal_background_picks_dark_or_light() {
    let env = TestEnv::new();
    let (light, mocha) = (class_colours("light"), class_colours("mocha"));
    let drawn_in =
        |colours: &[termlens::Color], pack: &[termlens::Color], other: &[termlens::Color]| {
            pack.iter().all(|c| colours.contains(c)) && !other.iter().any(|c| colours.contains(c))
        };
    // Without COLORFGBG the viewer asks the terminal (OSC 11): a white
    // background gets the light pack, a black one Mocha.
    assert!(drawn_in(
        &colours_drawn(&env, (255, 255, 255), &[]),
        &light,
        &mocha
    ));
    assert!(drawn_in(
        &colours_drawn(&env, (0, 0, 0), &[]),
        &mocha,
        &light
    ));
    // COLORFGBG, when set, decides without asking.
    let colours = colours_drawn(&env, (0, 0, 0), &[("COLORFGBG", "0;15")]);
    assert!(drawn_in(&colours, &light, &mocha));
}

/// `duscope view <arg>` in an 100×30 pseudo-terminal, from the work
/// directory, run by the shell as [`viewer`] runs it.
fn viewer_of(env: &TestEnv, arg: &str) -> Terminal {
    let script = format!(
        "'{}' view '{arg}'; echo \"exit=$?\"; stty -a",
        duscope_bin().display()
    );
    Terminal::builder()
        .size(100, 30)
        .timeout(Duration::from_secs(60))
        .env_clear()
        .envs(env.vars())
        .current_dir(env.work_dir())
        .args(["-c", &script])
        .spawn("/bin/sh")
        .unwrap()
}

#[test]
fn p4_tui_01_view_opens_a_saved_scan_by_its_id() {
    let env = TestEnv::new();
    let tree = env.fixtures().case_root(CASE);
    let out = env
        .cmd()
        .args(["scan", "--save", "--json"])
        .arg(&tree)
        .output()
        .unwrap();
    assert!(out.status.success());
    let envelope: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let id = envelope["scan"]["id"].as_str().unwrap().to_owned();
    // The leading digits of the saved scan's id, and latest, name it.
    for arg in [&id[..6], "latest"] {
        let mut terminal = viewer_of(&env, arg);
        terminal.wait_until(up).unwrap();
        terminal.send(Key::Char('q')).unwrap();
        assert_eq!(exited(&mut terminal), 0, "{arg}");
        assert_restored(&terminal);
    }
    // A word that only looks like an id, naming neither a path nor a saved
    // scan, fails as the path it is.
    let mut terminal = viewer_of(&env, "cafe");
    let status = exited(&mut terminal);
    let text = terminal.screen().full_text();
    assert_eq!(status, 3, "{text}");
    assert!(
        text.contains("cafe") && !text.contains("scan_not_found"),
        "{text}"
    );
    assert_restored(&terminal);
}
