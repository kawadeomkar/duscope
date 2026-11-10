//! P4-TUI-05: the views and the keys in the viewer the binary runs: the
//! tabs and the tables, the colour and pack keys, the sort, the filter
//! prompt, the help, the focus, `y`'s key line and `o` through its test
//! knob, which appends the command lines it would start to a file instead
//! of starting them.

use std::fs;
use std::path::Path;
use std::time::Duration;

use duscope_testkit::{TestEnv, duscope_bin};
use termlens::{Key, Terminal};

/// `duscope view <tree>` in a 120×40 pseudo-terminal, with `DUSCOPE_TEST_OPENER`
/// naming `opened`; run by the shell, which prints its exit status after it.
fn viewer(env: &TestEnv, tree: &Path, opened: &Path) -> Terminal {
    let script = format!(
        "'{}' view '{}'; echo \"exit=$?\"",
        duscope_bin().display(),
        tree.display()
    );
    let opened = opened.display().to_string();
    Terminal::builder()
        .size(120, 40)
        .timeout(Duration::from_secs(60))
        .env_clear()
        .envs(env.vars())
        .envs([("DUSCOPE_TEST_OPENER", opened.as_str())])
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

fn wait_gone(terminal: &mut Terminal, what: &str) {
    terminal
        .wait_until(|screen| !screen.text().contains(what))
        .unwrap_or_else(|e| panic!("still {what}: {e}:\n{}", terminal.screen().text()));
}

#[test]
fn p4_tui_05_the_tabs_the_modes_and_the_keys_in_the_binary() {
    let env = TestEnv::new();
    let tree = fs::canonicalize(env.work_dir()).unwrap().join("tree");
    fs::create_dir_all(tree.join("data")).unwrap();
    fs::create_dir_all(tree.join("src")).unwrap();
    fs::write(tree.join("data/big.bin"), vec![b'x'; 256 * 1024]).unwrap();
    fs::write(tree.join("src/main.rs"), vec![b'x'; 64 * 1024]).unwrap();
    fs::write(tree.join("notes.txt"), vec![b'x'; 32 * 1024]).unwrap();
    let opened = env.work_dir().join("opened");
    let mut terminal = viewer(&env, &tree, &opened);
    wait(&mut terminal, "treemap · ");

    // The tabs: the tables stand where the map did.
    terminal.send(Key::Char('2')).unwrap();
    wait(&mut terminal, "list · ");
    terminal.send(Key::Char('3')).unwrap();
    wait(&mut terminal, "largest · ");
    wait(&mut terminal, "big.bin");
    terminal.send(Key::Char('4')).unwrap();
    wait(&mut terminal, "types · ");
    terminal.send(Key::Char('1')).unwrap();
    wait(&mut terminal, "treemap · ");

    // The colour mode and the pack; the map's title and the key line say so.
    terminal.send(Key::Char('c')).unwrap();
    wait(&mut terminal, "color: top folder");
    terminal.send(Key::Char('p')).unwrap();
    wait(&mut terminal, "theme: tokyo-night");

    // The sort, shown on the siblings pane's title.
    terminal.send(Key::Char('s')).unwrap();
    wait(&mut terminal, "by name · in");

    // The filter: typed on the key line, applied with ⏎, cleared with Esc.
    terminal.send(Key::Char('/')).unwrap();
    wait(&mut terminal, ">  ");
    for c in "*.bin".chars() {
        terminal.send(Key::Char(c)).unwrap();
    }
    wait(&mut terminal, "*.bin▌");
    terminal.send(Key::Enter).unwrap();
    wait(&mut terminal, "filter: *.bin");
    terminal.send(Key::Esc).unwrap();
    wait_gone(&mut terminal, "filter: *.bin");

    // The help, then back to the map.
    terminal.send(Key::Char('?')).unwrap();
    wait(&mut terminal, "this help");
    wait(&mut terminal, "cycle the theme pack");
    terminal.send(Key::Char('?')).unwrap();
    wait_gone(&mut terminal, "this help");

    // Focus: the key line names what the siblings pane answers to.
    terminal.send(Key::Tab).unwrap();
    wait(&mut terminal, "⇥ pane");
    terminal.send(Key::BackTab).unwrap();
    wait(&mut terminal, "z zoom");

    // `y` puts the path on the key line.
    terminal.send(Key::Char('y')).unwrap();
    wait(&mut terminal, "copied ");
    let text = terminal.screen().text();
    assert!(text.contains("big.bin"), "{text}");

    // `o` goes through the knob the test set, which writes down the command
    // lines that show the selection in the file manager; none starts. They
    // are this platform's (`duscope-walk` tests each platform's), and none
    // hands the file itself to an opener: only `open -R` names it.
    let big = tree.join("data/big.bin");
    let expected: String = duscope_walk::reveal_commands(&big)
        .iter()
        .map(|line| {
            let args: Vec<_> = line.iter().map(|arg| arg.to_str().unwrap()).collect();
            args.join(" ") + "\n"
        })
        .collect();
    terminal.send(Key::Char('o')).unwrap();
    terminal
        .wait_until(|_| fs::read_to_string(&opened).is_ok_and(|log| log == expected))
        .unwrap_or_else(|e| panic!("{e}: {:?}", fs::read_to_string(&opened)));
    let named = format!(" {}", big.display());
    for line in expected.lines() {
        assert!(
            !line.ends_with(&named) || line.starts_with("open -R "),
            "{line}"
        );
    }

    terminal.send(Key::Char('q')).unwrap();
    wait(&mut terminal, "exit=0");
}
