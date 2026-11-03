//! P4-CORE-01: `clean` on a terminal shows the plan and asks once; only
//! `yes` typed in full moves anything.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use duscope_testkit::{TestEnv, duscope_bin};
use termlens::{Key, Terminal};

/// `duscope clean <path>` in a pseudo-terminal, run by `/bin/sh`, which
/// prints its exit status.
fn clean(env: &TestEnv, path: &Path) -> Terminal {
    let script = format!(
        "'{}' clean '{}'; echo \"exit=$?\"",
        duscope_bin().display(),
        path.display()
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

/// A file of 3,000 bytes in a tree with a saved scan, which clean plans
/// from.
fn saved_file(env: &TestEnv) -> PathBuf {
    let tree = fs::canonicalize(env.tmp_dir()).unwrap().join("tree");
    fs::create_dir_all(&tree).unwrap();
    let file = tree.join("a.bin");
    fs::write(&file, vec![7u8; 3000]).unwrap();
    let out = env
        .cmd()
        .arg("scan")
        .arg(&tree)
        .args(["--save", "--json"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    file
}

/// Answers the prompt with `answer` and returns the exit status and what
/// the terminal shows.
fn answer(terminal: &mut Terminal, answer: &str) -> (i32, String) {
    terminal
        .wait_until(|screen| screen.full_text().contains("Type yes"))
        .unwrap();
    terminal.send_str(answer).unwrap();
    terminal.send(Key::Enter).unwrap();
    terminal.wait_exit().unwrap();
    let text = terminal.screen().full_text();
    let status = text
        .lines()
        .find_map(|l| l.trim().strip_prefix("exit="))
        .unwrap_or_else(|| panic!("no exit status:\n{text}"));
    (status.trim().parse().unwrap(), text)
}

#[test]
fn p4_core_01_anything_but_yes_at_the_prompt_moves_nothing() {
    let env = TestEnv::new();
    let file = saved_file(&env);
    for typed in ["no", "y", "YES"] {
        let mut terminal = clean(&env, &file);
        let (status, text) = answer(&mut terminal, typed);
        assert_eq!(status, 1, "{text}");
        assert!(text.contains("1 item to move to the Trash"), "{text}");
        assert!(text.contains("the answer was not yes"), "{text}");
        assert!(file.exists());
    }
}

#[cfg(target_os = "linux")]
#[test]
fn p4_core_01_linux_yes_at_the_prompt_moves_the_plan() {
    let env = TestEnv::new();
    let file = saved_file(&env);
    let mut terminal = clean(&env, &file);
    let (status, text) = answer(&mut terminal, "yes");
    assert_eq!(status, 0, "{text}");
    assert!(text.contains("moved 1 item to the Trash"), "{text}");
    assert!(!file.exists());
    let trashed = env.xdg_data_home().join("Trash/files/a.bin");
    assert_eq!(fs::read(trashed).unwrap(), vec![7u8; 3000]);
}
