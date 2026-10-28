//! `completions SHELL`: the completion script for bash, zsh, fish, elvish
//! or `PowerShell`, generated from the clap tree, so a new flag cannot miss
//! it. Hidden (test-only) flags are left out: `clap_complete` writes them,
//! so their lines and words are taken out of its script. A flag is taken
//! out by how it is typed, wherever it appears, so a flag hidden on one
//! verb is never visible on another (a test holds that).

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
    let mut flags: Vec<String> = typed(command)
        .into_iter()
        .filter(|&(_, hidden)| hidden)
        .map(|(flag, _)| flag)
        .collect();
    flags.sort();
    flags.dedup();
    flags
}

/// Every flag of `command` and its verbs as it is typed, with whether it
/// is hidden there.
fn typed(command: &clap::Command) -> Vec<(String, bool)> {
    let mut flags: Vec<(String, bool)> = command
        .get_arguments()
        .flat_map(|arg| {
            let long = arg.get_long().map(|l| format!("--{l}"));
            let short = arg.get_short().map(|c| format!("-{c}"));
            long.into_iter()
                .chain(short)
                .map(|flag| (flag, arg.is_hide_set()))
        })
        .collect();
    for verb in command.get_subcommands() {
        flags.extend(typed(verb));
    }
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
            // `'--provider=[…]:…' \`, with a `(…)` of the flags it
            // conflicts with and a `*` when it repeats before the flag, and
            // a list of values that may continue over several lines.
            Shell::Zsh => {
                let flag_at = trimmed
                    .strip_prefix('\'')
                    .map(|rest| match rest.strip_prefix('(') {
                        Some(group) => group.split_once(')').map_or(rest, |(_, after)| after),
                        None => rest,
                    })
                    .map(|rest| rest.strip_prefix('*').unwrap_or(rest));
                let hides = flag_at.is_some_and(|rest| {
                    hidden.iter().any(|flag| {
                        rest.strip_prefix(flag.as_str())
                            .is_some_and(|after| after.starts_with(['=', '[', '+', ':', '\'']))
                    })
                });
                if hides {
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
            // `-l provider` lines (`-s i` for a flag with only a short
            // form), with the rest of a value list whose quoted `-a "…"`
            // continues over several lines, and the argparse spec fish
            // parses the command line with: `provider= i/interactive …`.
            Shell::Fish => {
                if hidden.iter().any(|flag| match flag.strip_prefix("--") {
                    Some(long) => line.contains(&format!(" -l {long} ")),
                    None => flag
                        .strip_prefix('-')
                        .is_some_and(|short| line.contains(&format!(" -s {short} "))),
                }) {
                    let mut open = opens_fish_string(line, false);
                    while open {
                        match lines.next() {
                            Some(next) => open = opens_fish_string(next, true),
                            None => break,
                        }
                    }
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

/// Whether a double-quoted fish string is still open at the end of `line`,
/// given whether one was open at its start. A backslash escapes the next
/// character (`\"`, `\'`), and a double quote inside single quotes is
/// text, as is a single quote inside double quotes.
fn opens_fish_string(line: &str, mut double: bool) -> bool {
    let mut single = false;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                chars.next();
            }
            '"' if !single => double = !double,
            '\'' if !double => single = !single,
            _ => {}
        }
    }
    double
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;
    use clap_complete::Shell;

    use super::{hidden_flags, opens_fish_string, script, strip, typed};
    use crate::cli::Cli;

    #[test]
    fn p1_cli_04_no_flag_is_hidden_on_one_verb_and_visible_on_another() {
        let mut command = Cli::command();
        command.build();
        let flags = typed(&command);
        for (flag, hidden) in &flags {
            assert!(
                flags.iter().all(|(other, h)| other != flag || h == hidden),
                "{flag} is hidden in one place and visible in another"
            );
        }
    }

    #[test]
    fn p1_cli_04_every_form_of_a_hidden_flag_is_taken_out() {
        let hidden = ["--secret".to_owned(), "-z".to_owned()];
        let zsh = "'(--plain)--secret[x]' \\\n'*--secret=[x]:X:_files' \\\n'-z+[x]' \\\n'--secret-not[x]' \\\n";
        assert_eq!(strip(Shell::Zsh, zsh, &hidden), "'--secret-not[x]' \\\n");
        let fish = "complete -c d -s z -d 'x'\ncomplete -c d -s y -d 'y'\n";
        assert_eq!(
            strip(Shell::Fish, fish, &hidden),
            "complete -c d -s y -d 'y'\n"
        );
        // A value list continues over lines inside its quotes: all of it
        // goes, and nothing after it does.
        let fish = "complete -c d -l secret -d 'It\\'s' -r -f -a \"one\\t'One'\ntwo\\t'Two'\nthree\\t'Three'\"\ncomplete -c d -l kept -d 'k'\n";
        assert_eq!(
            strip(Shell::Fish, fish, &hidden),
            "complete -c d -l kept -d 'k'\n"
        );
    }

    #[test]
    fn p1_cli_04_fish_strings_are_followed_over_lines() {
        assert!(opens_fish_string("complete -a \"one\\t'One'", false));
        assert!(opens_fish_string("two\\t'Two'", true));
        assert!(!opens_fish_string("three\\t'Three'\"", true));
        assert!(!opens_fish_string(
            "complete -d 'say \"hi\"' -a \"x\"",
            false
        ));
        assert!(!opens_fish_string("complete -d 'It\\'s \"' -l x", false));
    }

    #[test]
    fn p1_cli_04_theme_completes_the_pack_names() {
        for shell in [Shell::Bash, Shell::Zsh, Shell::Fish] {
            let text = String::from_utf8(script(shell)).unwrap();
            assert!(text.contains("tokyo-night"), "{shell}");
        }
    }

    /// Whether `script` offers `flag` (`--save`, `-i`): as a word of its
    /// own in any shell's syntax, as fish's `-l save` or `-s i`, or as a
    /// name in fish's argparse spec (`save`, `cursor=`, `i/interactive`).
    fn offers(script: &str, flag: &str) -> bool {
        let long = flag.strip_prefix("--");
        let short = flag.strip_prefix('-').filter(|_| long.is_none());
        script.lines().any(|line| {
            let words: Vec<&str> = line
                .split(|c: char| c.is_whitespace() || "'\"()[]=:+*,{}|;\\".contains(c))
                .filter(|w| !w.is_empty())
                .collect();
            let fish = words.windows(2).any(|pair| match pair {
                ["-l", name] => Some(*name) == long,
                ["-s", name] => Some(*name) == short,
                _ => false,
            });
            let spec = line.trim_start().starts_with("string join")
                && line.split_whitespace().any(|spec| {
                    spec.trim_end_matches('=')
                        .split('/')
                        .any(|name| Some(name) == long || Some(name) == short)
                });
            words.contains(&flag) || fish || spec
        })
    }

    #[test]
    fn p1_cli_04_completions_leave_hidden_flags_out() {
        let mut command = Cli::command();
        command.build();
        // Every flag the clap tree hides, so a flag hidden later is checked
        // too; the test-only and later-phase ones are among them.
        let hidden = hidden_flags(&command);
        for flag in [
            "--provider",
            "--test-now",
            "--cursor",
            "--interactive",
            "-i",
            "--save",
        ] {
            assert!(hidden.iter().any(|h| h == flag), "{flag}: {hidden:?}");
        }
        for shell in [
            Shell::Bash,
            Shell::Zsh,
            Shell::Fish,
            Shell::Elvish,
            Shell::PowerShell,
        ] {
            let text = String::from_utf8(script(shell)).unwrap();
            // The check finds a visible flag in each shell's spelling.
            assert!(offers(&text, "--follow-symlinks"), "{shell}");
            assert!(offers(&text, "-x"), "{shell}");
            for flag in &hidden {
                assert!(!offers(&text, flag), "{shell} offers {flag}");
            }
        }
    }
}
