//! P4-CORE-01: `clean` on a terminal shows the plan and asks once; only
//! `yes` typed in full moves anything.
//!
//! P5-CORE-04: a folder holding a mounted volume stays out of the Trash,
//! one mounted between the plan and `yes` included (a platform test, on
//! disk images).

use std::fs;
#[cfg(target_os = "macos")]
use std::os::unix::fs::PermissionsExt;
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

/// The disk images a test attached, by their whole disks, which name them
/// however their mount points move; detached when dropped.
#[cfg(target_os = "macos")]
struct Images<'a> {
    env: &'a TestEnv,
    disks: Vec<String>,
}

#[cfg(target_os = "macos")]
impl Images<'_> {
    /// Makes an 8 MB HFS+ image at `image` and attaches it at `point`,
    /// owners honoured, as a person without root can.
    fn attach(&mut self, image: &Path, point: &Path) {
        self.env.hdiutil(&[
            &"create",
            &"-ov",
            &"-size",
            &"8m",
            &"-fs",
            &"HFS+",
            &"-volname",
            &"DUSCOPE",
            &image,
        ]);
        let mut tries = Vec::new();
        for _ in 0..3 {
            let out = self
                .env
                .command("hdiutil")
                .args(["attach", "-owners", "on", "-nobrowse", "-mountpoint"])
                .arg(point)
                .arg(image)
                .output()
                .unwrap();
            let text = String::from_utf8_lossy(&out.stdout).into_owned();
            if out.status.success()
                && let Some(disk) = text
                    .split_whitespace()
                    .next()
                    .filter(|d| d.starts_with("/dev/"))
            {
                self.disks.push(disk.to_owned());
                return;
            }
            tries.push(format!("{text}{}", String::from_utf8_lossy(&out.stderr)));
            std::thread::sleep(Duration::from_secs(2));
        }
        panic!("hdiutil attach {}: {tries:?}", image.display());
    }

    /// Detaches them, the last attached first, as the test environment's
    /// `hdiutil` retries a detach.
    fn detach(mut self) {
        while let Some(disk) = self.disks.pop() {
            self.env.hdiutil(&[&"detach", &disk]);
        }
    }

    /// Whether `hdiutil info` still lists `disk`.
    fn listed(&self, disk: &str) -> bool {
        self.env
            .command("hdiutil")
            .arg("info")
            .output()
            .map(|out| {
                String::from_utf8_lossy(&out.stdout)
                    .lines()
                    .any(|line| line.split_whitespace().next() == Some(disk))
            })
            .unwrap_or(true)
    }
}

/// A test that fails on the way leaves no image attached: a detach can
/// unmount a volume and fail to eject its disk (one mounted inside another
/// image is let go when that one goes), so each pass tries every disk
/// still listed, the last attached first.
#[cfg(target_os = "macos")]
impl Drop for Images<'_> {
    fn drop(&mut self) {
        for _ in 0..3 {
            for disk in self.disks.iter().rev() {
                let _ = self
                    .env
                    .command("hdiutil")
                    .args(["detach", "-force", disk])
                    .output();
            }
            let left: Vec<String> = self
                .disks
                .iter()
                .filter(|disk| self.listed(disk))
                .cloned()
                .collect();
            if left.is_empty() {
                return;
            }
            self.disks = left;
            std::thread::sleep(Duration::from_secs(2));
        }
    }
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "attaches disk images; DUSCOPE_TEST_PLATFORM=1"]
fn p5_core_04_macos_a_folder_holding_a_mounted_image_stays_out_of_the_trash() {
    let env = TestEnv::new();
    if std::env::var_os("DUSCOPE_TEST_PLATFORM").is_none_or(|v| v != "1") {
        eprintln!("skipped: set DUSCOPE_TEST_PLATFORM=1");
        return;
    }
    let work = fs::canonicalize(env.tmp_dir()).unwrap().join("images");
    let outer = work.join("outer");
    fs::create_dir_all(&outer).unwrap();
    let mut images = Images {
        env: &env,
        disks: Vec::new(),
    };
    images.attach(&work.join("outer.dmg"), &outer);
    // On the outer image, whose Trash takes what is moved: a folder with an
    // image mounted two levels down, and one an image is mounted in later.
    let (held, late) = (outer.join("a"), outer.join("b"));
    for dir in [held.join("sub/m"), late.join("m")] {
        fs::create_dir_all(dir).unwrap();
    }
    fs::write(held.join("f.bin"), vec![1u8; 2000]).unwrap();
    fs::write(late.join("g.bin"), vec![2u8; 2000]).unwrap();
    images.attach(&work.join("inner.dmg"), &held.join("sub/m"));
    fs::write(held.join("sub/m/on-inner.bin"), vec![3u8; 1000]).unwrap();
    let out = env
        .cmd()
        .arg("scan")
        .arg(&outer)
        .args(["--save", "--json"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // Planned, and with --yes: refused, so nothing moves. By the mount
    // table; by the walk where the path is spelled otherwise than the table
    // has it (the image's volume ignores case); and by the table where the
    // walk cannot reach the mount, under a folder it cannot read.
    let refused = |folder: &Path| {
        for yes in [false, true] {
            let mut cmd = env.cmd();
            cmd.arg("clean").arg(folder).arg("--json");
            if yes {
                cmd.arg("--yes");
            }
            let out = cmd.output().unwrap();
            let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
            assert_eq!(out.status.code(), Some(1), "{value}");
            assert_eq!(value["error"]["code"], "refused_path", "{value}");
            let item = &value["result"]["plan"]["items"][0];
            assert_eq!(
                (&item["action"], &item["reason"]),
                (&"refused".into(), &"mount_point".into()),
                "{value}"
            );
            assert_eq!(value["result"]["trashed_count"], 0, "{value}");
        }
    };
    refused(&held);
    refused(&outer.join("A"));
    let mode = |bits| fs::set_permissions(held.join("sub"), fs::Permissions::from_mode(bits));
    mode(0o000).unwrap();
    refused(&held);
    mode(0o755).unwrap();
    // A volume mounted after the plan is shown and before `yes`: the mount
    // table, read again before the move, stops it.
    let mut terminal = clean(&env, &late);
    terminal
        .wait_until(|screen| screen.full_text().contains("Type yes"))
        .unwrap();
    images.attach(&work.join("late.dmg"), &late.join("m"));
    fs::write(late.join("m/on-late.bin"), vec![4u8; 1000]).unwrap();
    terminal.send_str("yes").unwrap();
    terminal.send(Key::Enter).unwrap();
    terminal.wait_exit().unwrap();
    let text = terminal.screen().full_text();
    assert!(text.contains("exit=1"), "{text}");
    assert!(text.contains("1 not moved"), "{text}");
    // Nothing went to the outer image's Trash; the folders, the mounts and
    // what is on them are where they were.
    let uid = std::process::Command::new("id").arg("-u").output().unwrap();
    let trash = outer
        .join(".Trashes")
        .join(String::from_utf8_lossy(&uid.stdout).trim());
    for name in ["a", "b"] {
        assert!(!trash.join(name).exists(), "{name} is in the Trash");
    }
    for (file, bytes) in [
        (held.join("f.bin"), vec![1u8; 2000]),
        (held.join("sub/m/on-inner.bin"), vec![3u8; 1000]),
        (late.join("g.bin"), vec![2u8; 2000]),
        (late.join("m/on-late.bin"), vec![4u8; 1000]),
    ] {
        assert_eq!(fs::read(&file).unwrap(), bytes, "{}", file.display());
    }
    images.detach();
}
