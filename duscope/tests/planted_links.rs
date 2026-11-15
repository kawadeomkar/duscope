//! P5-SEC-01: two writes a symbolic link planted by someone else turned
//! onto another of the user's files, since v0.2.0. Each test plants the
//! link as that person would and runs the binary.
//!
//! - `export -o FILE` writes the export to `.FILE.<pid>.part` beside the
//!   file first. It opened that name with `File::create`, which follows a
//!   link there, and the name is guessable, so in a directory others may
//!   write (`/tmp`) a link planted at it had the export overwrite the file
//!   it led to.
//! - `agent --skill`, a command made to run in a project folder, made its
//!   folders with `create_dir_all`, which goes through a linked `.claude`
//!   or `.agents` (or any folder below them), and wrote each file to
//!   `<file>.tmp-<pid>` with `fs::write`, which follows a link there. A
//!   folder someone else prepared (a cloned repository) can hold such
//!   links: the skill's text then overwrote any file the user may write,
//!   or its files were made wherever a link led, and the command exited 0.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::Output;

use duscope_testkit::{TestEnv, duscope_bin};
use serde_json::Value;

/// What the victim of each test holds, before and, for the test to pass,
/// after.
const MINE: &[u8] = b"another file of the user's";

/// A file of the user's outside the folders the command writes to.
fn victim(env: &TestEnv) -> PathBuf {
    let victim = env.tmp_dir().join("victim");
    fs::write(&victim, MINE).unwrap();
    victim
}

/// The error object of a failed command run with `--json`, after checking
/// the exit code.
fn failure(out: &Output, exit: i32) -> Value {
    assert_eq!(
        out.status.code(),
        Some(exit),
        "stdout {} stderr {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let envelope: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(envelope["ok"], false);
    envelope["error"].clone()
}

/// The skill's files found under `dir`, links not followed.
fn skill_files(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return found;
    };
    for entry in entries {
        let path = entry.unwrap().path();
        let meta = fs::symlink_metadata(&path).unwrap();
        if meta.is_dir() {
            found.extend(skill_files(&path));
        } else if meta.is_file() {
            found.push(path);
        }
    }
    found
}

#[test]
fn p5_sec_01_an_export_never_writes_through_a_link_at_its_temporary_name() {
    let env = TestEnv::new();
    let tree = env.tmp_dir().join("tree");
    fs::create_dir_all(tree.join("sub")).unwrap();
    fs::write(tree.join("sub/file.bin"), vec![b'x'; 10_000]).unwrap();
    let saved = env
        .cmd()
        .args(["scan", tree.to_str().unwrap(), "--save", "--json"])
        .output()
        .unwrap();
    assert!(saved.status.success());
    let shared = env.tmp_dir().join("shared");
    fs::create_dir_all(&shared).unwrap();
    let victim = victim(&env);
    // The shell plants the link at the name the export will take, its own
    // pid's, then becomes duscope, which keeps that pid.
    let out = env
        .command("/bin/sh")
        .arg("-c")
        .arg(
            "ln -s \"$1\" \"$2/.out.json.$$.part\" && \
             exec \"$0\" export \"$3\" --format ncdu -o \"$2/out.json\"",
        )
        .arg(duscope_bin())
        .arg(&victim)
        .arg(&shared)
        .arg(&tree)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        fs::read(&victim).unwrap(),
        MINE,
        "the export wrote through the link: {stderr}"
    );
    assert!(
        fs::symlink_metadata(shared.join("out.json")).is_err(),
        "out.json was made: {stderr}"
    );
    // The export fails, naming the temporary file whose name was taken,
    // not the output, which is not there.
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    let line = stderr.lines().next().unwrap_or_default();
    assert!(
        line.contains("/.out.json.") && line.contains(".part"),
        "{stderr}"
    );
}

/// `duscope agent --skill --json ARGS` run in `cwd`.
fn skill(env: &TestEnv, cwd: &Path, args: &[&str]) -> Output {
    env.cmd()
        .current_dir(cwd)
        .args(["agent", "--skill", "--json"])
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn p5_sec_01_agent_skill_never_writes_through_a_linked_folder() {
    let env = TestEnv::new();
    // Each folder the skill's files go in, below the folder given.
    for (n, linked) in [
        ".claude",
        ".agents",
        ".claude/skills",
        ".agents/skills/duscope",
        ".claude/skills/duscope/references",
    ]
    .into_iter()
    .enumerate()
    {
        let project = env.work_dir().join(format!("project-{n}"));
        let elsewhere = env.tmp_dir().join(format!("elsewhere-{n}"));
        fs::create_dir_all(project.join(linked).parent().unwrap()).unwrap();
        fs::create_dir_all(&elsewhere).unwrap();
        symlink(&elsewhere, project.join(linked)).unwrap();
        let out = skill(&env, &project, &[]);
        let written = skill_files(&elsewhere);
        assert!(written.is_empty(), "{linked}: wrote {written:?}");
        let error = failure(&out, 1);
        assert_eq!(error["code"], "refused_path", "{linked}: {error}");
        let message = error["message"].as_str().unwrap();
        assert!(
            message.contains(&project.join(linked).display().to_string()),
            "{linked}: {message}"
        );
        // Refused before anything was written, the other folder included.
        assert_eq!(skill_files(&project), Vec::<PathBuf>::new(), "{linked}");
        // --force replaces a skill, never a link.
        let out = skill(&env, &project, &["--force"]);
        assert_eq!(failure(&out, 1)["code"], "refused_path", "{linked}");
        assert!(skill_files(&elsewhere).is_empty(), "{linked}");
    }
}

#[test]
fn p5_sec_01_agent_skill_never_writes_through_a_link_at_its_temporary_name() {
    let env = TestEnv::new();
    for (n, file) in [
        ".agents/skills/duscope/SKILL.md",
        ".agents/skills/duscope/references/sql.md",
        ".claude/skills/duscope/SKILL.md",
        ".claude/skills/duscope/references/sql.md",
    ]
    .into_iter()
    .enumerate()
    {
        let project = env.work_dir().join(format!("project-{n}"));
        fs::create_dir_all(project.join(file).parent().unwrap()).unwrap();
        let victim = victim(&env);
        // The shell plants the link at the temporary name of the file, its
        // own pid's, then becomes duscope, which keeps that pid.
        let out = env
            .command("/bin/sh")
            .arg("-c")
            .arg("ln -s \"$1\" \"$2.tmp-$$\" && exec \"$0\" agent --skill --json")
            .arg(duscope_bin())
            .arg(&victim)
            .arg(file)
            .current_dir(&project)
            .output()
            .unwrap();
        assert_eq!(
            fs::read(&victim).unwrap(),
            MINE,
            "{file}: the skill was written through the link: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let target = fs::symlink_metadata(project.join(file));
        assert!(
            target.is_err() || target.is_ok_and(|m| m.is_file()),
            "{file} is the planted link"
        );
        // The error names the temporary file, whose name was taken.
        let error = failure(&out, 1);
        assert_eq!(error["code"], "io_error", "{file}: {error}");
        let message = error["message"].as_str().unwrap();
        assert!(message.contains(&format!("{file}.tmp-")), "{message}");
    }
}

#[test]
fn p5_sec_01_agent_skill_refuses_a_link_at_a_skill_file() {
    let env = TestEnv::new();
    let victim = victim(&env);
    for (n, file) in [
        ".claude/skills/duscope/SKILL.md",
        ".agents/skills/duscope/references/sql.md",
    ]
    .into_iter()
    .enumerate()
    {
        let project = env.work_dir().join(format!("project-{n}"));
        let target = project.join(file);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        symlink(&victim, &target).unwrap();
        for args in [&[][..], &["--force"][..]] {
            let out = skill(&env, &project, args);
            let error = failure(&out, 1);
            assert_eq!(error["code"], "refused_path", "{file} {args:?}: {error}");
            let message = error["message"].as_str().unwrap();
            assert!(message.contains(&target.display().to_string()), "{message}");
            assert_eq!(fs::read(&victim).unwrap(), MINE, "{file} {args:?}");
            assert_eq!(fs::read_link(&target).unwrap(), victim, "{file} {args:?}");
            assert_eq!(skill_files(&project), Vec::<PathBuf>::new(), "{file}");
        }
    }
}
