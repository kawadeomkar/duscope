//! `mktree`: builds the reference fixture tree and prints the manifest hash.
//!
//! ```text
//! mktree [--out DIR] [--force]   build DIR/tree unless it matches the manifest
//! mktree --print-hash            print the manifest hash, build nothing
//! mktree --print-plan            print the expanded manifest the hash covers
//! mktree --list [--out DIR]      print the deterministic listing of DIR/tree
//! mktree --verify [--out DIR]    compare DIR/tree with the manifest
//! mktree --arm | --disarm        apply or restore the test modes
//! ```
//!
//! The hash goes to stdout; everything else to stderr. DIR defaults to
//! `target/fixtures.noindex` of the workspace (`$CARGO_TARGET_DIR/fixtures.noindex`).

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use mktree::{OutDir, Plan, Status};

const USAGE: &str = "usage: mktree [--out DIR] [--force | --print-hash | --print-plan | --list | --verify | --arm | --disarm]";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Action {
    Build,
    PrintHash,
    PrintPlan,
    List,
    Verify,
    Arm,
    Disarm,
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(message) => {
            eprintln!("mktree: {message}");
            ExitCode::FAILURE
        }
    }
}

#[allow(clippy::too_many_lines)] // argument parsing, then one arm per action
fn run() -> Result<ExitCode, String> {
    let mut action = Action::Build;
    let mut force = false;
    let mut out = None;
    let mut args = std::env::args_os().skip(1);
    while let Some(arg) = args.next() {
        let set = |a: Action, action: &mut Action| -> Result<(), String> {
            if *action != Action::Build {
                return Err(USAGE.to_owned());
            }
            *action = a;
            Ok(())
        };
        match arg.to_str() {
            Some("--out") => out = Some(PathBuf::from(args.next().ok_or(USAGE)?)),
            Some("--force") => force = true,
            Some("--print-hash") => set(Action::PrintHash, &mut action)?,
            Some("--print-plan") => set(Action::PrintPlan, &mut action)?,
            Some("--list") => set(Action::List, &mut action)?,
            Some("--verify") => set(Action::Verify, &mut action)?,
            Some("--arm") => set(Action::Arm, &mut action)?,
            Some("--disarm") => set(Action::Disarm, &mut action)?,
            Some("-h" | "--help") => {
                eprintln!("{USAGE}");
                return Ok(ExitCode::SUCCESS);
            }
            _ => {
                return Err(format!(
                    "unexpected argument {}\n{USAGE}",
                    arg.to_string_lossy()
                ));
            }
        }
    }
    if force && action != Action::Build {
        return Err(USAGE.to_owned());
    }
    let out = out.map_or_else(OutDir::workspace_default, OutDir::new);
    let plan = Plan::embedded().map_err(|e| e.to_string())?;
    let hash = plan.hash();
    let mut stdout = io::stdout().lock();
    let print = |stdout: &mut io::StdoutLock<'_>, text: &str| -> Result<(), String> {
        stdout
            .write_all(text.as_bytes())
            .and_then(|()| stdout.flush())
            .map_err(|e| format!("stdout: {e}"))
    };
    if hash != plan.recorded_hash {
        eprintln!(
            "mktree: warning: the manifest records hash {}, the expanded manifest hashes to {hash}; \
             if the change to the tree is intended, update `hash` in tests/fixtures/mktree/manifest.toml",
            plan.recorded_hash
        );
    }
    match action {
        Action::PrintHash => print(&mut stdout, &format!("{hash}\n"))?,
        Action::PrintPlan => print(&mut stdout, &plan.text())?,
        Action::List => print(
            &mut stdout,
            &mktree::listing(&out.tree()).map_err(|e| e.to_string())?,
        )?,
        Action::Verify => {
            let record = out
                .record()
                .map_err(|e| e.to_string())?
                .ok_or("no record: run `just fixtures` first")?;
            let report = mktree::verify(&out.tree(), &plan, &record).map_err(|e| e.to_string())?;
            for p in &report.problems {
                eprintln!("mktree: {p}");
            }
            if !report.read_after_build.is_empty() {
                eprintln!(
                    "mktree: {} entries were read after the build (atime moved), e.g. {}",
                    report.read_after_build.len(),
                    report.read_after_build[0]
                );
            }
            if !report.problems.is_empty() {
                return Ok(ExitCode::FAILURE);
            }
            eprintln!(
                "mktree: {} matches the manifest ({} entries)",
                out.tree().display(),
                record.entries
            );
        }
        Action::Arm => out.arm(&plan).map_err(|e| e.to_string())?,
        Action::Disarm => out.disarm(&plan).map_err(|e| e.to_string())?,
        Action::Build => {
            let (record, built) = out.ensure(&plan, force).map_err(|e| e.to_string())?;
            if built {
                eprintln!(
                    "mktree: built {} entries in {:.1} s at {} ({}, {})",
                    record.entries,
                    std::time::Duration::from_millis(record.build_ms).as_secs_f64(),
                    out.tree().display(),
                    record.platform,
                    record.filesystem
                );
            } else {
                eprintln!(
                    "mktree: {} is up to date ({} entries)",
                    out.tree().display(),
                    record.entries
                );
            }
            for case in record.cases.iter().filter(|c| c.status == Status::Skipped) {
                eprintln!(
                    "mktree: skipped {}: {} (instead: {})",
                    case.name, case.detail, case.fallback
                );
            }
            print(&mut stdout, &format!("{hash}\n"))?;
        }
    }
    Ok(ExitCode::SUCCESS)
}
