//! `completions SHELL`: the completion script for bash, zsh, fish, elvish
//! or `PowerShell`, generated from the clap tree, so a new flag cannot miss
//! it. Hidden (test-only) flags are left out.

use clap::CommandFactory;
use clap_complete::Shell;

use super::Failure;
use crate::cli::Cli;
use crate::out::emit_bytes;

/// Prints the script for `shell`.
pub fn run(shell: Shell) -> Result<(), Failure> {
    emit_bytes(&script(shell))
}

/// The script for `shell`, without the hidden flags `clap_complete`
/// writes for bash, zsh, elvish and `PowerShell`.
pub fn script(shell: Shell) -> Vec<u8> {
    let mut command = Cli::command();
    let mut out = Vec::new();
    clap_complete::generate(shell, &mut command, "duscope", &mut out);
    let hidden = hidden_flags(&command);
    strip(shell, &String::from_utf8_lossy(&out), &hidden).into_bytes()
}

/// The hidden flags of `command` and its verbs, as they are typed
/// (`--provider`, `-i`).
fn hidden_flags(command: &clap::Command) -> Vec<String> {
    let mut flags: Vec<String> = command
        .get_arguments()
        .filter(|arg| arg.is_hide_set())
        .flat_map(|arg| {
            let long = arg.get_long().map(|l| format!("--{l}"));
            let short = arg.get_short().map(|c| format!("-{c}"));
            long.into_iter().chain(short)
        })
        .collect();
    for verb in command.get_subcommands() {
        flags.extend(hidden_flags(verb));
    }
    flags.sort();
    flags.dedup();
    flags
}

/// `script` without the lines and words that offer one of `hidden`.
fn strip(shell: Shell, script: &str, hidden: &[String]) -> String {
    let mut out = String::with_capacity(script.len());
    let mut lines = script.lines();
    while let Some(line) = lines.next() {
        let trimmed = line.trim_start();
        let offers = |prefix: &str, suffixes: &[char]| {
            hidden.iter().any(|flag| {
                trimmed
                    .strip_prefix(prefix)
                    .and_then(|rest| rest.strip_prefix(flag.as_str()))
                    .is_some_and(|rest| rest.starts_with(suffixes))
            })
        };
        match shell {
            // `opts="… --provider …"`, and a `case` branch per flag that
            // takes a value: `--provider)` down to `;;`.
            Shell::Bash => {
                if offers("", &[')']) {
                    for rest in lines.by_ref() {
                        if rest.trim() == ";;" {
                            break;
                        }
                    }
                    continue;
                }
                if trimmed.starts_with("opts=\"") {
                    let kept: Vec<&str> = line
                        .split(' ')
                        .filter(|word| !hidden.iter().any(|flag| word.trim_matches('"') == flag))
                        .collect();
                    let mut joined = kept.join(" ");
                    if !joined.ends_with('"') {
                        joined.push('"');
                    }
                    out.push_str(&joined);
                    out.push('\n');
                    continue;
                }
            }
            // `'--provider=[…]:…' \`, which a list of values may continue
            // over several lines.
            Shell::Zsh => {
                if offers("'", &['=', '[', '+', ':', '\'']) {
                    let mut end = line;
                    while !end.trim_end().ends_with('\\') {
                        match lines.next() {
                            Some(next) => end = next,
                            None => break,
                        }
                    }
                    continue;
                }
            }
            // `-l provider` lines, and the argparse spec fish parses the
            // command line with: `provider= i/interactive …`.
            Shell::Fish => {
                if hidden.iter().any(|flag| {
                    flag.strip_prefix("--")
                        .is_some_and(|long| line.contains(&format!(" -l {long} ")))
                }) {
                    continue;
                }
                if trimmed.starts_with("string join") {
                    let kept: Vec<&str> = line
                        .split(' ')
                        .filter(|spec| {
                            let names = spec.trim_end_matches('=');
                            !names.split('/').any(|name| {
                                let flag = if name.len() == 1 {
                                    format!("-{name}")
                                } else {
                                    format!("--{name}")
                                };
                                hidden.contains(&flag)
                            }) || spec.is_empty()
                        })
                        .collect();
                    out.push_str(&kept.join(" "));
                    out.push('\n');
                    continue;
                }
            }
            Shell::Elvish if offers("cand ", &[' ']) => continue,
            Shell::PowerShell if offers("[CompletionResult]::new('", &['\'']) => continue,
            _ => {}
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use clap_complete::Shell;

    use super::script;

    #[test]
    fn p1_cli_04_completions_leave_hidden_flags_out() {
        for shell in [
            Shell::Bash,
            Shell::Zsh,
            Shell::Fish,
            Shell::Elvish,
            Shell::PowerShell,
        ] {
            let text = String::from_utf8(script(shell)).unwrap();
            // fish spells a long flag `-l follow-symlinks`.
            assert!(text.contains("follow-symlinks"), "{shell}");
            for hidden in ["provider", "test-now", "cursor", "interactive", "save"] {
                // As each shell offers a flag: `--save`, fish's `-l save`
                // and its argparse spec's `save` or `cursor=`.
                let forms = [
                    format!("--{hidden}"),
                    format!(" -l {hidden}"),
                    format!(" {hidden}="),
                    format!(" {hidden} "),
                    format!("/{hidden} "),
                ];
                for form in &forms {
                    assert!(!text.contains(form.as_str()), "{shell} offers {form:?}");
                }
            }
        }
    }
}
