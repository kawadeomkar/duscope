//! P4-TUI-04: the panes' facts in the viewer the binary runs. A scan with
//! `--attrs full` names the selection's owner in the mode row, from the
//! scan in memory and from the saved scan; `i` shows the full fact list,
//! `explain` of the scan `view` ran and saved or read, or of a fresh `stat`
//! for `scan -i`, which saves nothing.

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::time::Duration;

use duscope_testkit::{TestEnv, duscope_bin};
use termlens::{Key, Terminal};

/// `duscope <args> <tree>` in a 160×45 pseudo-terminal, run by the shell,
/// which prints its exit status after it.
fn run(env: &TestEnv, args: &str, tree: &Path) -> Terminal {
    let script = format!(
        "'{}' {args} '{}'; echo \"exit=$?\"",
        duscope_bin().display(),
        tree.display()
    );
    Terminal::builder()
        .size(160, 45)
        .timeout(Duration::from_secs(60))
        .env_clear()
        .envs(env.vars())
        .current_dir(env.work_dir())
        .args(["-c", &script])
        .spawn("/bin/sh")
        .unwrap()
}

#[test]
fn p4_tui_04_the_viewer_names_the_owner_and_i_shows_the_facts() {
    let env = TestEnv::new();
    let tree = fs::canonicalize(env.work_dir()).unwrap().join("tree");
    fs::create_dir_all(tree.join("docs")).unwrap();
    fs::write(tree.join("docs/notes.txt"), vec![b'x'; 64 * 1024]).unwrap();
    let meta = fs::metadata(tree.join("docs/notes.txt")).unwrap();
    let owner = format!("· {}:{}", meta.uid(), meta.gid());
    // `view` scans and saves the tree, then reads the scan it saved (which
    // `--no-scan` makes sure of), then `scan -i` scans it again without
    // saving.
    for (args, saved) in [
        ("view --attrs full", true),
        ("view --no-scan --attrs full", true),
        ("scan -i --attrs full", false),
    ] {
        let mut terminal = run(&env, args, &tree);
        // The viewer opens on the largest file, its owner in the mode row.
        terminal
            .wait_until(|screen| screen.text().contains(&owner))
            .unwrap_or_else(|e| panic!("{args}: {e}:\n{}", terminal.screen().text()));
        terminal.send(Key::Char('i')).unwrap();
        // The scan's threads are the list's last row: the frame is whole.
        terminal
            .wait_until(|screen| screen.text().contains("threads "))
            .unwrap_or_else(|e| panic!("{args}: {e}:\n{}", terminal.screen().text()));
        let text = terminal.screen().text();
        // `--attrs full`: the times in place of the hint.
        assert!(text.contains("accessed"), "{args}:\n{text}");
        assert!(!text.contains("rerun with --attrs full"), "{args}:\n{text}");
        // `view` saved the scan it ran, which `explain` reads; `scan -i`
        // saved nothing, and says so.
        assert_eq!(
            text.contains("rerun with --save"),
            !saved,
            "{args}:\n{text}"
        );
        assert!(text.contains("freeable "), "{args}:\n{text}");
        terminal.send(Key::Char('q')).unwrap();
        terminal.wait_exit().unwrap();
        let text = terminal.screen().full_text();
        assert!(text.contains("exit=0"), "{args}:\n{text}");
    }
}
