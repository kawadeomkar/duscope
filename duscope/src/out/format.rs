//! Which format a command writes, and the machine formats' writers:
//! compact JSON, `ndjson` lines, `--plain` records, the error envelope, and
//! stdout itself.
//!
//! The decision, first match wins: an explicit flag; `DUSCOPE_FORMAT`; an
//! agent's environment (`CLAUDECODE`, `CODEX_THREAD_ID`/`CODEX_VERSION`,
//! `GEMINI_CLI`: JSON); `CI` (plain); a terminal (the verb's human format);
//! plain. A terminal alone is not enough: Codex CLI and Gemini CLI run
//! commands in a pseudo-terminal.

use std::borrow::Cow;
use std::ffi::OsString;
use std::io::{self, Write};

use duscope_core::envelope::{
    Envelope, ErrorEnvelope, ErrorObject, Listing, NdjsonLine, SCHEMA_VERSION, Tool,
};
use serde::Serialize;

use super::Format;
use crate::verbs::{Exit, Failure};

/// The environment the decisions read, captured once.
// One field per variable the decisions read.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EnvVars {
    /// `DUSCOPE_FORMAT`, when set.
    pub duscope_format: Option<String>,
    /// An agent's variable is set.
    pub agent: bool,
    /// `CI` is set (and not `0` or `false`).
    pub ci: bool,
    /// `NO_COLOR` is set and not empty.
    pub no_color: bool,
    /// `CLICOLOR_FORCE` or `FORCE_COLOR` asks for colour.
    pub force_color: bool,
    /// `TERM=dumb`.
    pub term_dumb: bool,
}

/// The variables an agent's shell sets: Claude Code, Codex CLI, Gemini CLI.
const AGENT_VARS: [&str; 4] = [
    "CLAUDECODE",
    "CODEX_THREAD_ID",
    "CODEX_VERSION",
    "GEMINI_CLI",
];

impl EnvVars {
    /// Reads the variables through `get` (`std::env::var_os` in the
    /// binary).
    pub fn read(get: impl Fn(&str) -> Option<OsString>) -> EnvVars {
        let value = |name: &str| get(name).map(|v| v.to_string_lossy().into_owned());
        let present = |name: &str| value(name).is_some_and(|v| !v.is_empty());
        let on =
            |name: &str| value(name).is_some_and(|v| !matches!(v.as_str(), "" | "0" | "false"));
        EnvVars {
            duscope_format: value("DUSCOPE_FORMAT").filter(|v| !v.is_empty()),
            agent: AGENT_VARS.iter().any(|name| present(name)),
            ci: on("CI"),
            no_color: present("NO_COLOR"),
            force_color: on("CLICOLOR_FORCE") || on("FORCE_COLOR"),
            term_dumb: value("TERM").as_deref() == Some("dumb"),
        }
    }
}

/// The format: `explicit` from the flags, `human` the verb's format for a
/// terminal. A `DUSCOPE_FORMAT` that names no format is an error.
pub fn decide(
    explicit: Option<Format>,
    env: &EnvVars,
    stdout_tty: bool,
    human: Format,
) -> Result<Format, String> {
    if let Some(format) = explicit {
        return Ok(format);
    }
    if let Some(name) = &env.duscope_format {
        return Format::from_name(name).ok_or_else(|| {
            format!(
                "DUSCOPE_FORMAT={name:?} is not a format: use json, plain, table, treemap or ndjson"
            )
        });
    }
    Ok(if env.agent {
        Format::Json
    } else if env.ci {
        Format::Plain
    } else if stdout_tty {
        human
    } else {
        Format::Plain
    })
}

/// One JSON value, compact, and a newline.
pub fn json(out: &mut dyn Write, value: &impl Serialize) -> io::Result<()> {
    serde_json::to_writer(&mut *out, value).map_err(io::Error::from)?;
    out.write_all(b"\n")
}

/// The `ndjson` lines of an envelope.
pub fn ndjson<T: Serialize>(out: &mut dyn Write, envelope: &Envelope<T>) -> io::Result<()> {
    let mut line = |value: &NdjsonLine<'_, T>| json(out, value);
    line(&NdjsonLine::Scan {
        schema_version: envelope.schema_version,
        tool: envelope.tool,
        scan: &envelope.scan,
        query: &envelope.query,
    })?;
    for entry in &envelope.entries {
        line(&NdjsonLine::Entry(entry))?;
    }
    line(&NdjsonLine::End {
        returned_count: envelope.returned_count,
        total_count: envelope.total_count,
        warnings: &envelope.warnings,
    })
}

/// The `ndjson` lines of a listing of saved scans.
pub fn ndjson_listing<T: Serialize>(out: &mut dyn Write, listing: &Listing<T>) -> io::Result<()> {
    let mut line = |value: &NdjsonLine<'_, T>| json(out, value);
    line(&NdjsonLine::Listing {
        schema_version: listing.schema_version,
        tool: listing.tool,
        query: &listing.query,
    })?;
    for entry in &listing.entries {
        line(&NdjsonLine::Entry(entry))?;
    }
    line(&NdjsonLine::End {
        returned_count: listing.returned_count,
        total_count: listing.total_count,
        warnings: &listing.warnings,
    })
}

/// `--plain`: one record per row, fields tab-separated, each record ended
/// by a newline or, with `-0`, a NUL.
pub fn plain<'a>(
    out: &mut dyn Write,
    fields: &[&str],
    rows: impl Iterator<Item = Vec<Cow<'a, [u8]>>>,
    null: bool,
    header: bool,
) -> io::Result<()> {
    let end = if null { b'\0' } else { b'\n' };
    if header {
        out.write_all(fields.join("\t").as_bytes())?;
        out.write_all(&[end])?;
    }
    for row in rows {
        for (i, field) in row.iter().enumerate() {
            if i > 0 {
                out.write_all(b"\t")?;
            }
            out.write_all(field)?;
        }
        out.write_all(&[end])?;
    }
    Ok(())
}

/// The error envelope of a failure.
pub fn error_envelope(failure: &Failure) -> ErrorEnvelope {
    // Whether running the command again may succeed, and who can fix it.
    let (retryable, fixable_by) = match failure.code {
        "io_error" | "deadline_exceeded" | "sql_timeout" | "index_write_failed" => (true, "user"),
        "permission_denied"
        | "confirmation_required"
        | "trash_unavailable"
        | "untrusted_index"
        | "not_compiled"
        | "index_corrupt"
        | "index_version_mismatch" => (false, "user"),
        "not_available" => (false, "none"),
        _ => (false, "agent"),
    };
    ErrorEnvelope {
        ok: false,
        schema_version: SCHEMA_VERSION,
        tool: Tool::CURRENT,
        error: ErrorObject {
            code: failure.code,
            message: failure.message.clone(),
            hint: failure.hint.clone(),
            retryable,
            fixable_by,
        },
    }
}

/// Writes to stdout through a buffer. A closed pipe (`duscope … | head`)
/// ends the output quietly with success.
pub fn emit(write: impl FnOnce(&mut dyn Write) -> io::Result<()>) -> Result<(), Failure> {
    let mut out = io::BufWriter::new(io::stdout().lock());
    let written = write(&mut out).and_then(|()| out.flush());
    quiet_on_broken_pipe(written)
}

/// A write's result: success, also when the reader went away.
pub fn quiet_on_broken_pipe(result: io::Result<()>) -> Result<(), Failure> {
    match result {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        Err(e) => Err(Failure::new(
            Exit::Runtime,
            "io_error",
            format!("cannot write the output: {e}"),
        )),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::ffi::OsString;
    use std::io;

    use super::{EnvVars, Format, decide, error_envelope, quiet_on_broken_pipe};
    use crate::verbs::{Exit, Failure};

    fn env(vars: &[(&str, &str)]) -> EnvVars {
        let map: HashMap<String, OsString> = vars
            .iter()
            .map(|&(k, v)| (k.to_owned(), OsString::from(v)))
            .collect();
        EnvVars::read(|name| map.get(name).cloned())
    }

    #[test]
    fn p1_cli_02_the_format_decision_follows_its_precedence() {
        let table = Format::Table;
        let decide =
            |explicit, vars: &[(&str, &str)], tty| decide(explicit, &env(vars), tty, table);
        // An explicit flag beats everything.
        let all = [
            ("DUSCOPE_FORMAT", "plain"),
            ("CLAUDECODE", "1"),
            ("CI", "true"),
        ];
        assert_eq!(decide(Some(Format::Ndjson), &all, true), Ok(Format::Ndjson));
        // DUSCOPE_FORMAT beats the agent, CI and the terminal.
        assert_eq!(decide(None, &all, true), Ok(Format::Plain));
        assert_eq!(
            decide(
                None,
                &[("DUSCOPE_FORMAT", "table"), ("CLAUDECODE", "1")],
                false
            ),
            Ok(Format::Table)
        );
        assert!(decide(None, &[("DUSCOPE_FORMAT", "yaml")], true).is_err());
        // Each agent's variable selects JSON, on a terminal too.
        for agent in [
            "CLAUDECODE",
            "CODEX_THREAD_ID",
            "CODEX_VERSION",
            "GEMINI_CLI",
        ] {
            assert_eq!(
                decide(None, &[(agent, "1"), ("CI", "1")], true),
                Ok(Format::Json)
            );
        }
        // CI selects plain; CI=false and CI=0 do not.
        assert_eq!(decide(None, &[("CI", "true")], true), Ok(Format::Plain));
        assert_eq!(decide(None, &[("CI", "false")], true), Ok(Format::Table));
        assert_eq!(decide(None, &[("CI", "0")], true), Ok(Format::Table));
        // Then the terminal.
        assert_eq!(decide(None, &[], true), Ok(Format::Table));
        assert_eq!(decide(None, &[], false), Ok(Format::Plain));
        // An empty variable is not set.
        assert_eq!(decide(None, &[("CLAUDECODE", "")], true), Ok(Format::Table));
    }

    #[test]
    fn p1_cli_01_a_closed_pipe_is_success() {
        let broken = Err(io::Error::from(io::ErrorKind::BrokenPipe));
        assert_eq!(quiet_on_broken_pipe(broken), Ok(()));
        let other = Err(io::Error::from(io::ErrorKind::PermissionDenied));
        assert_eq!(quiet_on_broken_pipe(other).unwrap_err().exit, Exit::Runtime);
    }

    #[test]
    fn p1_cli_02_the_error_envelope_carries_the_error_object() {
        let failure = Failure::new(Exit::NotFound, "scan_not_found", "/nope does not exist")
            .hint("Check the path");
        let json = serde_json::to_value(error_envelope(&failure)).unwrap();
        assert_eq!(json["ok"], false);
        assert_eq!(json["tool"]["name"], "duscope");
        assert_eq!(
            json["error"],
            serde_json::json!({
                "code": "scan_not_found",
                "message": "/nope does not exist",
                "hint": "Check the path",
                "retryable": false,
                "fixable_by": "agent",
            })
        );
        let json = serde_json::to_value(error_envelope(&Failure::new(
            Exit::Permission,
            "permission_denied",
            "no",
        )))
        .unwrap();
        assert_eq!(json["error"]["hint"], serde_json::Value::Null);
        assert_eq!(json["error"]["fixable_by"], "user");
    }
}
