//! Whether to colour what people read: `--color` > `NO_COLOR` >
//! `CLICOLOR_FORCE` or `FORCE_COLOR` > `TERM=dumb` > a terminal. The machine
//! formats are never coloured, whatever this decides.

use super::args::ColorWhen;
use super::format::EnvVars;

/// Whether to colour what people read.
pub fn decide(explicit: Option<ColorWhen>, env: &EnvVars, stdout_tty: bool) -> bool {
    match explicit {
        Some(ColorWhen::Always) => true,
        Some(ColorWhen::Never) => false,
        Some(ColorWhen::Auto) | None => {
            !env.no_color && (env.force_color || (!env.term_dumb && stdout_tty))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::ffi::OsString;

    use super::{ColorWhen, EnvVars, decide as color};

    fn env(vars: &[(&str, &str)]) -> EnvVars {
        let map: HashMap<String, OsString> = vars
            .iter()
            .map(|&(k, v)| (k.to_owned(), OsString::from(v)))
            .collect();
        EnvVars::read(|name| map.get(name).cloned())
    }

    #[test]
    fn p1_cli_02_the_colour_decision_follows_its_precedence() {
        let all = [("NO_COLOR", "1"), ("FORCE_COLOR", "1"), ("TERM", "dumb")];
        assert!(color(Some(ColorWhen::Always), &env(&all), false));
        assert!(!color(Some(ColorWhen::Never), &env(&[]), true));
        // NO_COLOR beats the forcing variables.
        assert!(!color(None, &env(&all), true));
        assert!(!color(
            Some(ColorWhen::Auto),
            &env(&[("NO_COLOR", "1")]),
            true
        ));
        // An empty NO_COLOR is not set.
        assert!(color(None, &env(&[("NO_COLOR", "")]), true));
        // CLICOLOR_FORCE and FORCE_COLOR beat TERM=dumb and a pipe.
        assert!(color(
            None,
            &env(&[("CLICOLOR_FORCE", "1"), ("TERM", "dumb")]),
            false
        ));
        assert!(color(None, &env(&[("FORCE_COLOR", "3")]), false));
        assert!(!color(None, &env(&[("CLICOLOR_FORCE", "0")]), false));
        // TERM=dumb beats the terminal.
        assert!(!color(None, &env(&[("TERM", "dumb")]), true));
        assert!(color(None, &env(&[("TERM", "xterm-256color")]), true));
        assert!(!color(None, &env(&[]), false));
    }
}
