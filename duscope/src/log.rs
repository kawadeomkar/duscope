//! The stderr logger behind the `log` facade.
//!
//! A filter in `RUST_LOG`'s grammar picks the records: comma-separated
//! directives, each a level (`debug`), a target (`duscope_walk`, every
//! level) or both (`duscope_walk=trace`); a target covers its submodules,
//! the longest match wins, and a directive that is neither is ignored. The
//! filter is `-q`, `-v`, `-vv` or `--debug` when one is given, else
//! `DUSCOPE_LOG`, else `RUST_LOG`, else warnings and errors. Records go to
//! stderr, never stdout; a release build never logs `trace`.

use log::{Level, LevelFilter, Log, Metadata, Record};

use crate::out::stderr_line;

/// The target of the `-vv` figures (per volume, per worker), which `-v`
/// leaves out.
pub const DETAIL: &str = "duscope::detail";

/// How much the verbosity flags ask for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verbosity {
    /// `-q`: errors only.
    Quiet,
    /// No flag: the environment's filter, or warnings and errors.
    Normal,
    /// `-v`: phase timings.
    Verbose,
    /// `-vv`: phase timings, per volume and per worker.
    VeryVerbose,
    /// `--debug`: everything, each directory's errors as they happen.
    Debug,
}

impl Verbosity {
    /// The filter a flag stands for; `None` without one.
    fn filter(self) -> Option<String> {
        match self {
            Verbosity::Quiet => Some("error".to_owned()),
            Verbosity::Normal => None,
            Verbosity::Verbose => Some("info".to_owned()),
            Verbosity::VeryVerbose => Some(format!("info,{DETAIL}=debug")),
            Verbosity::Debug => Some("debug".to_owned()),
        }
    }
}

/// A parsed filter: a default level and per-target levels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Filter {
    default: LevelFilter,
    targets: Vec<(String, LevelFilter)>,
}

impl Filter {
    /// Parses `spec`; with no level directive the default is `warn`.
    pub fn parse(spec: &str) -> Filter {
        let mut filter = Filter {
            default: LevelFilter::Warn,
            targets: Vec::new(),
        };
        for directive in spec.split(',').map(str::trim).filter(|d| !d.is_empty()) {
            match directive.split_once('=') {
                Some((target, level)) => {
                    if let Ok(level) = level.trim().parse() {
                        filter.targets.push((target.trim().to_owned(), level));
                    }
                }
                None => match directive.parse() {
                    Ok(level) => filter.default = level,
                    Err(_) => filter
                        .targets
                        .push((directive.to_owned(), LevelFilter::Trace)),
                },
            }
        }
        // Longest first, so the most specific target matches first.
        filter
            .targets
            .sort_by_key(|(target, _)| std::cmp::Reverse(target.len()));
        filter
    }

    /// The level `target` logs at.
    pub fn level(&self, target: &str) -> LevelFilter {
        let covers = |prefix: &str| {
            target == prefix
                || target
                    .strip_prefix(prefix)
                    .is_some_and(|rest| rest.starts_with("::"))
        };
        let level = self
            .targets
            .iter()
            .find(|(prefix, _)| covers(prefix))
            .map_or(self.default, |&(_, level)| level);
        release_cap(level)
    }

    /// The most any target logs.
    fn max(&self) -> LevelFilter {
        let most = self
            .targets
            .iter()
            .map(|&(_, level)| level)
            .fold(self.default, Ord::max);
        release_cap(most)
    }
}

/// `level`, but at most `debug` in a release build.
fn release_cap(level: LevelFilter) -> LevelFilter {
    if cfg!(debug_assertions) {
        level
    } else {
        level.min(LevelFilter::Debug)
    }
}

/// The filter for `verbosity` and the environment `env` reads.
pub fn filter(verbosity: Verbosity, env: impl Fn(&str) -> Option<String>) -> Filter {
    let set = |name: &str| env(name).filter(|v| !v.trim().is_empty());
    let spec = verbosity
        .filter()
        .or_else(|| set("DUSCOPE_LOG"))
        .or_else(|| set("RUST_LOG"))
        .unwrap_or_default();
    Filter::parse(&spec)
}

/// The logger: one line per record on stderr.
#[derive(Debug)]
struct StderrLogger {
    filter: Filter,
}

impl Log for StderrLogger {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.level() <= self.filter.level(metadata.target())
    }

    fn log(&self, record: &Record<'_>) {
        if self.enabled(record.metadata()) {
            stderr_line(&line(
                record.level(),
                record.target(),
                &record.args().to_string(),
            ));
        }
    }

    fn flush(&self) {}
}

/// A record as it is written: `duscope: <message>` for the figures people
/// asked for with `-v` and `-vv`, the level before the message otherwise,
/// and the target too at `debug` and `trace`, which a filter can name.
fn line(level: Level, target: &str, message: &str) -> String {
    if target == DETAIL {
        return format!("duscope: {message}");
    }
    match level {
        Level::Info => format!("duscope: {message}"),
        Level::Error | Level::Warn => {
            format!(
                "duscope: {}: {message}",
                level.as_str().to_ascii_lowercase()
            )
        }
        Level::Debug | Level::Trace => format!(
            "duscope: {}: {target}: {message}",
            level.as_str().to_ascii_lowercase()
        ),
    }
}

/// Installs the logger with `filter`, for the rest of the process. A second
/// call changes nothing.
pub fn init(filter: Filter) {
    let max = filter.max();
    if log::set_logger(Box::leak(Box::new(StderrLogger { filter }))).is_ok() {
        log::set_max_level(max);
    }
}

#[cfg(test)]
mod tests {
    use log::{Level, LevelFilter};

    use super::{DETAIL, Filter, Verbosity, filter, line};

    #[test]
    fn p2_core_02_the_filter_reads_rust_logs_grammar() {
        let f =
            Filter::parse("info, duscope_walk=debug, duscope_walk::platform=off, nonsense=loud");
        assert_eq!(f.level("duscope::verbs::scan"), LevelFilter::Info);
        assert_eq!(f.level("duscope_walk"), LevelFilter::Debug);
        assert_eq!(f.level("duscope_walk::lib"), LevelFilter::Debug);
        assert_eq!(f.level("duscope_walk::platform::linux"), LevelFilter::Off);
        // A target covers its submodules only, not names it prefixes.
        assert_eq!(f.level("duscope_walker"), LevelFilter::Info);
        // A bare target is every level of it.
        let bare = Filter::parse("duscope_core");
        assert_eq!(
            bare.level("duscope_core::rollup"),
            LevelFilter::Trace.min(cap())
        );
        assert_eq!(bare.level("duscope"), LevelFilter::Warn);
        assert_eq!(Filter::parse("").level("x"), LevelFilter::Warn);
    }

    /// What a build logs at most: `trace` only in a debug build.
    fn cap() -> LevelFilter {
        if cfg!(debug_assertions) {
            LevelFilter::Trace
        } else {
            LevelFilter::Debug
        }
    }

    #[test]
    fn p2_core_02_a_flag_beats_duscope_log_which_beats_rust_log() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                pairs
                    .iter()
                    .find(|(k, _)| *k == name)
                    .map(|(_, v)| (*v).to_owned())
            }
        };
        let both: &[(&str, &str)] = &[("DUSCOPE_LOG", "debug"), ("RUST_LOG", "error")];
        assert_eq!(
            filter(Verbosity::Normal, env(both)).level("duscope"),
            LevelFilter::Debug
        );
        assert_eq!(
            filter(Verbosity::Normal, env(&[("RUST_LOG", "error")])).level("duscope"),
            LevelFilter::Error
        );
        // An empty DUSCOPE_LOG is unset.
        let empty: &[(&str, &str)] = &[("DUSCOPE_LOG", " "), ("RUST_LOG", "info")];
        assert_eq!(
            filter(Verbosity::Normal, env(empty)).level("duscope"),
            LevelFilter::Info
        );
        assert_eq!(
            filter(Verbosity::Quiet, env(both)).level("duscope"),
            LevelFilter::Error
        );
        assert_eq!(
            filter(Verbosity::Verbose, env(both)).level("duscope"),
            LevelFilter::Info
        );
        let vv = filter(Verbosity::VeryVerbose, env(both));
        assert_eq!(vv.level("duscope::verbs::scan"), LevelFilter::Info);
        assert_eq!(vv.level(DETAIL), LevelFilter::Debug);
        assert_eq!(
            filter(Verbosity::Debug, env(&[])).level("duscope_walk"),
            LevelFilter::Debug
        );
        assert_eq!(
            filter(Verbosity::Normal, env(&[])).level("duscope"),
            LevelFilter::Warn
        );
    }

    #[test]
    fn p2_core_02_records_read_duscope_level_message() {
        assert_eq!(
            line(Level::Info, "duscope", "walk: 1 s"),
            "duscope: walk: 1 s"
        );
        assert_eq!(
            line(Level::Debug, DETAIL, "workers: 4"),
            "duscope: workers: 4"
        );
        assert_eq!(line(Level::Warn, "duscope_walk", "x"), "duscope: warn: x");
        assert_eq!(
            line(Level::Debug, "duscope::verbs::scan", "/t/a: denied"),
            "duscope: debug: duscope::verbs::scan: /t/a: denied"
        );
    }
}
