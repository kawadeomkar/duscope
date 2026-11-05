//! P4-TUI-07: the `pixels` feature's probe against mocked terminals.
//! termlens plays the terminal: one that answers ratatui-image's Kitty
//! graphics query, one whose device attributes name Sixel, iTerm2 (named
//! by `TERM_PROGRAM`, as the picker reads it), one with no image protocol,
//! and one that answers nothing. Each declares a cell of 10×20 pixels where
//! it draws images, so the picture of a 100×25-cell map is 1000×500. Inside
//! tmux the probe is not run, so no `tmux` command runs either.

#![cfg(feature = "pixels")]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::time::{Duration, Instant};

use duscope_testkit::{TestEnv, duscope_bin};
use termlens::{Graphics, GraphicsProtocol, Key, Screen, Terminal};

/// The fixture case drawn.
const CASE: &str = "extensions";

/// The terminal's size: the static map is its columns by its rows less
/// the frame's five, 100×25.
const SIZE: (u16, u16) = (100, 30);

/// A cell, in pixels.
const CELL: (u16, u16) = (10, 20);

/// The terminal a test mocks.
struct Mock {
    graphics: Graphics,
    cell: bool,
    answers: bool,
    extra: &'static [(&'static str, &'static str)],
}

impl Mock {
    fn new(graphics: Graphics) -> Mock {
        Mock {
            graphics,
            cell: true,
            answers: true,
            extra: &[],
        }
    }

    /// `duscope <args> <the case>` run as `program`: duscope itself, or
    /// `/bin/sh -c` with a script around it.
    fn builder(&self, env: &TestEnv) -> termlens::TerminalBuilder {
        let mut builder = Terminal::builder()
            .size(SIZE.0, SIZE.1)
            .timeout(Duration::from_secs(60))
            .graphics(self.graphics)
            .answer_queries(self.answers)
            .env_clear()
            .envs(env.vars())
            .envs(self.extra.iter().copied())
            .current_dir(env.work_dir());
        if self.cell {
            builder = builder.cell_size(CELL.0, CELL.1);
        }
        builder
    }

    /// `duscope scan --treemap pixels` on the case, run until it exits.
    fn scan(&self, env: &TestEnv) -> Screen {
        let tree = env.fixtures().case_root(CASE);
        let mut terminal = self
            .builder(env)
            .args(["scan", "--treemap", "pixels", "--threads", "4"])
            .arg(&tree)
            .spawn(duscope_bin())
            .unwrap();
        let status = terminal.wait_exit().unwrap();
        let screen = terminal.screen();
        assert!(status.success(), "{}", screen.full_text());
        screen
    }
}

/// Asserts the frame's lines follow the picture: the legend on the row
/// under the map's 25, the summary, the warning row, the `next` line.
fn assert_footer_under_the_map(screen: &Screen) {
    let next = screen.row_text(28);
    assert!(
        next.starts_with("next: duscope top"),
        "row 28 is {next:?}:\n{}",
        screen.full_text()
    );
    assert!(
        screen.row_text(26).contains(" allocated"),
        "{}",
        screen.full_text()
    );
}

#[test]
fn p4_tui_07_a_kitty_terminal_gets_the_map_in_pixels() {
    let env = TestEnv::new();
    let screen = Mock::new(Graphics::Kitty).scan(&env);
    let seen = screen.graphics();
    assert_eq!(
        (seen.kitty(), seen.sixel()),
        (1, 0),
        "{}",
        screen.full_text()
    );
    let image = seen.last().unwrap();
    assert_eq!(image.protocol(), GraphicsProtocol::Kitty);
    // The picture of the 100×25-cell map, from the top-left corner.
    assert_eq!(image.size(), Some((1000, 500)));
    assert_eq!(image.at(), (0, 0));
    // Every cell of the map is a placeholder in the colour that names the
    // image.
    let [_, r, g, b] = image.id().unwrap().to_be_bytes();
    for row in [0, 12, 24] {
        for col in [0, 50, 99] {
            let cell = screen.cell(row, col).unwrap();
            assert_eq!(cell.contents().chars().next(), Some('\u{10EEEE}'));
            assert_eq!(cell.style().fg, termlens::Color::Rgb(r, g, b));
        }
    }
    // The picture covers its area and is shaded, as render's is.
    let bitmap = image.decode().unwrap();
    assert_eq!((bitmap.width(), bitmap.height()), (1000, 500));
    let colours = bitmap.colours();
    assert!(colours.iter().all(|(rgba, _)| rgba[3] == 255));
    assert!(colours.len() > 100, "{} colours", colours.len());
    assert_footer_under_the_map(&screen);
}

#[test]
fn p4_tui_07_a_sixel_terminal_gets_the_map_in_pixels() {
    let env = TestEnv::new();
    let screen = Mock::new(Graphics::Sixel).scan(&env);
    let seen = screen.graphics();
    assert_eq!(
        (seen.kitty(), seen.sixel()),
        (0, 1),
        "{}",
        screen.full_text()
    );
    let image = seen.last().unwrap();
    assert_eq!(image.size(), Some((1000, 500)));
    assert_eq!(image.at(), (0, 0));
    let bitmap = image.decode().unwrap();
    assert!(bitmap.width() >= 1000 && bitmap.height() >= 500);
    assert!(bitmap.colours().len() > 16);
    assert_footer_under_the_map(&screen);
}

#[test]
fn p4_tui_07_iterm2_is_known_by_its_environment() {
    let env = TestEnv::new();
    let mock = Mock {
        extra: &[("TERM_PROGRAM", "iTerm.app")],
        ..Mock::new(Graphics::None)
    };
    let screen = mock.scan(&env);
    // An inline PNG of the map's size, which the emulator does not draw.
    let inline = screen
        .unsupported()
        .iter()
        .find(|shape| shape.starts_with("^[]1337;File=inline=1;"))
        .map(str::to_owned);
    let inline = inline.unwrap_or_else(|| panic!("no iTerm2 image:\n{}", screen.full_text()));
    assert!(
        inline.contains(";width=1000px;height=500px;"),
        "{inline:.120}"
    );
    assert!(screen.graphics().is_empty());
    assert_footer_under_the_map(&screen);
}

#[test]
fn p4_tui_07_a_terminal_without_images_gets_the_cell_map() {
    let env = TestEnv::new();
    // A terminal that answers but names no protocol, nor a cell size.
    let none = Mock {
        cell: false,
        ..Mock::new(Graphics::None)
    };
    // A Kitty terminal, with the colours off: a picture has colours.
    let no_color = Mock {
        extra: &[("NO_COLOR", "1")],
        ..Mock::new(Graphics::Kitty)
    };
    for (mock, why) in [
        (
            none,
            "the terminal answered no Kitty graphics, Sixel or iTerm2 probe",
        ),
        (
            no_color,
            "a picture is drawn in a theme's colours, and the map's colours are off or the terminal's own",
        ),
    ] {
        let screen = mock.scan(&env);
        let text = screen.full_text();
        assert!(screen.graphics().is_empty(), "{why}:\n{text}");
        // The warning, wrapped at the terminal's width.
        let words = text.split_whitespace().collect::<Vec<_>>().join(" ");
        let warning =
            format!("duscope: warn: --treemap pixels: {why}, so the map is drawn in cells");
        assert!(words.contains(&warning), "{text}");
        assert!(text.contains("next: duscope top"), "{text}");
    }
}

#[test]
fn p4_tui_07_inside_tmux_the_map_stays_in_cells_and_no_tmux_runs() {
    let env = TestEnv::new();
    let tree = env.fixtures().case_root(CASE);
    // A `tmux` first on PATH that records each run: inside tmux,
    // ratatui-image's picker runs `tmux set -p allow-passthrough on`
    // before it asks the terminal anything.
    let bin = env.tmp_dir().join("fake-bin");
    fs::create_dir_all(&bin).unwrap();
    let ran = env.tmp_dir().join("tmux-ran");
    let tmux = bin.join("tmux");
    fs::write(
        &tmux,
        format!("#!/bin/sh\necho \"$*\" >> '{}'\n", ran.display()),
    )
    .unwrap();
    fs::set_permissions(&tmux, fs::Permissions::from_mode(0o755)).unwrap();
    let path = env
        .vars()
        .into_iter()
        .find(|(name, _)| name == "PATH")
        .map(|(_, path)| path.into_string().unwrap())
        .unwrap_or_default();
    let path = format!("{}:{path}", bin.display());
    // The two variables the picker reads, and the one tmux sets in every
    // pane, each alone, in a terminal that would answer Kitty graphics.
    for (name, value) in [
        ("TERM", "tmux-256color"),
        ("TERM_PROGRAM", "tmux"),
        ("TMUX", "/tmp/tmux-501/default,4242,0"),
    ] {
        let mut terminal = Mock::new(Graphics::Kitty)
            .builder(&env)
            .env("PATH", &path)
            .env(name, value)
            .args(["scan", "--treemap", "pixels", "--threads", "4"])
            .arg(&tree)
            .spawn(duscope_bin())
            .unwrap();
        let status = terminal.wait_exit().unwrap();
        let screen = terminal.screen();
        let text = screen.full_text();
        assert!(status.success(), "{name}={value}:\n{text}");
        assert!(
            !ran.exists(),
            "{name}={value}: tmux ran: {:?}",
            fs::read_to_string(&ran)
        );
        assert!(screen.graphics().is_empty(), "{name}={value}:\n{text}");
        // The warning, wrapped at the terminal's width, wherever it breaks.
        let bare = |s: &str| s.split_whitespace().collect::<String>();
        let warning = "duscope: warn: --treemap pixels: inside tmux the probe would turn on \
                       tmux's allow-passthrough option, so the map is drawn in cells";
        assert!(
            bare(&text).contains(&bare(warning)),
            "{name}={value}:\n{text}"
        );
        assert!(
            text.contains("next: duscope top"),
            "{name}={value}:\n{text}"
        );
    }
}

#[test]
fn p4_tui_07_a_silent_terminal_costs_the_probe_wait_and_keeps_its_modes() {
    let env = TestEnv::new();
    let tree = env.fixtures().case_root(CASE);
    let mock = Mock {
        answers: false,
        ..Mock::new(Graphics::Kitty)
    };
    // Run by the shell, so that `stty -a` prints the modes once duscope has
    // exited.
    let script = format!(
        "'{}' scan --treemap pixels --threads 4 '{}'; echo \"exit=$?\"; stty -a",
        duscope_bin().display(),
        tree.display()
    );
    let start = Instant::now();
    let mut terminal = mock
        .builder(&env)
        .args(["-c", &script])
        .spawn("/bin/sh")
        .unwrap();
    terminal.wait_exit().unwrap();
    // The picker waits two seconds for an answer; the map follows.
    let took = start.elapsed();
    assert!(took < Duration::from_secs(20), "{took:?}");
    let screen = terminal.screen();
    let text = screen.full_text();
    assert!(text.contains("exit=0"), "{text}");
    assert!(screen.graphics().is_empty(), "{text}");
    assert!(text.contains("next: duscope top"), "{text}");
    // The probe turned echo and line editing off and got no answer to turn
    // them on again: they are back all the same.
    let words: Vec<&str> = text.split_whitespace().collect();
    for mode in ["icanon", "echo"] {
        assert!(words.contains(&mode), "{mode} not restored:\n{text}");
        assert!(!words.contains(&format!("-{mode}").as_str()), "{text}");
    }
}

#[test]
fn p4_tui_07_the_viewer_draws_its_map_in_pixels() {
    let env = TestEnv::new();
    let tree = env.fixtures().case_root(CASE);
    let script = format!(
        "'{}' scan -i --treemap pixels --threads 4 '{}'; echo \"exit=$?\"; stty -a",
        duscope_bin().display(),
        tree.display()
    );
    let mut terminal = Mock::new(Graphics::Kitty)
        .builder(&env)
        .args(["-c", &script])
        .spawn("/bin/sh")
        .unwrap();
    // The viewer is up, its map pane one image.
    terminal
        .wait_until(|s| s.alternate_screen() && s.graphics().kitty() == 1)
        .unwrap();
    let seen = terminal.screen().graphics();
    let (width, height) = seen.last().and_then(|image| image.size()).unwrap();
    assert_eq!((width % 10, height % 20), (0, 0), "{width}×{height}");
    assert!(width >= 700 && height >= 300, "{width}×{height}");
    terminal.send(Key::Char('q')).unwrap();
    terminal.wait_exit().unwrap();
    let text = terminal.screen().full_text();
    assert!(text.contains("exit=0"), "{text}");
    assert!(!terminal.screen().alternate_screen(), "{text}");
}

#[test]
fn p4_tui_03_a_zoom_draws_the_zoomed_folder_in_pixels() {
    let env = TestEnv::new();
    let home = env.fixtures().path("home");
    let script = format!(
        "'{}' scan -i --treemap pixels --threads 4 '{}'; echo \"exit=$?\"",
        duscope_bin().display(),
        home.display()
    );
    let mut terminal = Mock::new(Graphics::Kitty)
        .builder(&env)
        .args(["-c", &script])
        .spawn("/bin/sh")
        .unwrap();
    terminal
        .wait_until(|s| s.alternate_screen() && s.graphics().kitty() == 1)
        .unwrap();
    // `z` zooms to the folder under the cursor and the picture follows it, as
    // the cells under it do; `Z` comes back to the whole map, drawn again.
    terminal.send(Key::Char('z')).unwrap();
    terminal.wait_until(|s| s.graphics().kitty() == 2).unwrap();
    terminal.send(Key::Char('Z')).unwrap();
    terminal.wait_until(|s| s.graphics().kitty() == 3).unwrap();
    terminal.send(Key::Char('q')).unwrap();
    terminal.wait_exit().unwrap();
    let text = terminal.screen().full_text();
    assert!(text.contains("exit=0"), "{text}");
}
