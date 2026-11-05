//! `mcp`: the MCP server on stdin and stdout (P3-MCP-03).
//!
//! The server answers a client in either protocol revision duscope speaks
//! and offers the tools of `duscope_mcp::tools`, each bound to `--roots`.
//! A tool's verb runs in this process exactly as `duscope <verb> --json`
//! runs from a shell ([`crate::run_captured`]): the same flags, the same
//! configuration file, the same index directory and `--max-chars`, so the
//! result's `structuredContent` is the envelope that command prints, byte
//! for byte. Nothing but protocol messages reaches stdout; the log goes to
//! stderr at `--log-level`.

use std::ffi::OsString;
use std::path::PathBuf;

use duscope_core::index;
use duscope_mcp::discover::ServerConfig;
use duscope_mcp::roots::{Refusal, Roots};
use duscope_mcp::server::{Server, Tools};
use duscope_mcp::tools::{Invocation, Outcome, Registry, Runner};
use duscope_mcp::transport::Transport;

use super::{Exit, Failure};
use crate::cli::{GlobalArgs, McpArgs};
use crate::out::{DEFAULT_MAX_CHARS, OutputArgs};

/// Serves until the client closes stdin.
pub fn serve(args: &McpArgs, global: &GlobalArgs, output: &OutputArgs) -> Result<(), Failure> {
    let roots = Roots::new(&args.roots).map_err(|e| {
        Failure::new(Exit::Usage, "usage", e.to_string())
            .hint("Run duscope mcp with --roots naming directories there are")
    })?;
    let index_dir = global
        .index_dir
        .clone()
        .or_else(|| index::default_dir(|name| std::env::var_os(name)))
        .ok_or_else(|| {
            Failure::new(
                Exit::Usage,
                "usage",
                "there is no index directory: HOME and XDG_CACHE_HOME are unset",
            )
            .hint("Run duscope mcp with --index-dir DIR")
        })?;
    let max_chars = output.max_chars.unwrap_or(DEFAULT_MAX_CHARS);
    let mut registry = Registry::new(max_chars, args.allow_delete);
    for tool in tools() {
        registry.register(tool).map_err(|e| {
            Failure::new(
                Exit::Runtime,
                "io_error",
                format!("cannot offer a tool: {e}"),
            )
        })?;
    }
    let runner = VerbRunner {
        index_dir: index_dir.clone(),
        max_chars: output.max_chars,
        config: global.config.clone(),
    };
    let config = ServerConfig::new(&crate::agent::briefing());
    let server = Server::new(config).with_tools(Tools {
        registry,
        roots,
        index_dir,
        runner: Box::new(runner),
    });
    log::info!("serving on stdin and stdout");
    let failed = |e: std::io::Error| {
        Failure::new(
            Exit::Runtime,
            "io_error",
            format!("the MCP transport failed: {e}"),
        )
    };
    Transport::start(std::io::stdin(), std::io::stdout(), server)
        .and_then(Transport::wait)
        .map_err(failed)
}

/// The tools the server offers.
fn tools() -> Vec<Box<dyn duscope_mcp::tools::Tool>> {
    let tools = Vec::new();
    #[cfg(debug_assertions)]
    let tools = {
        let mut tools = tools;
        if std::env::var_os("DUSCOPE_TEST_MCP_TOOLS").is_some_and(|v| !v.is_empty()) {
            tools.extend(fixture::tools());
        }
        tools
    };
    tools
}

/// Runs a tool's verb in this process, as the shell command would.
#[derive(Debug)]
struct VerbRunner {
    index_dir: PathBuf,
    max_chars: Option<usize>,
    config: Option<OsString>,
}

impl VerbRunner {
    /// The flags every tool's verb gets: JSON, the server's index
    /// directory, and the `--max-chars` and configuration file the server
    /// was given. Nothing else: the next-page hint a verb prints repeats
    /// its command line, which should read as a person would type it.
    fn server_flags(&self) -> Vec<OsString> {
        let mut flags: Vec<OsString> = vec![
            "--json".into(),
            "--index-dir".into(),
            self.index_dir.clone().into(),
        ];
        if let Some(max_chars) = self.max_chars {
            flags.push("--max-chars".into());
            flags.push(max_chars.to_string().into());
        }
        if let Some(config) = &self.config {
            flags.push("--config".into());
            flags.push(config.clone());
        }
        flags
    }
}

impl Runner for VerbRunner {
    fn run(&self, invocation: &Invocation) -> Outcome {
        let mut argv: Vec<OsString> = vec!["duscope".into(), invocation.verb.into()];
        argv.extend(invocation.args.iter().cloned());
        argv.extend(self.server_flags());
        log::debug!("running {} for a tool", invocation.verb);
        let (code, written) = crate::run_captured(argv);
        let json = String::from_utf8_lossy(&written).trim_end().to_owned();
        Outcome {
            json,
            ok: code == Exit::Success.code(),
        }
    }

    fn refuse(&self, refusal: &Refusal) -> Outcome {
        let exit = match refusal.code {
            "usage" => Exit::Usage,
            "scan_not_found" => Exit::NotFound,
            _ => Exit::Runtime,
        };
        let failure =
            Failure::new(exit, refusal.code, refusal.message.clone()).hint(refusal.hint.clone());
        let written = crate::out::error_json(&failure);
        Outcome {
            json: written,
            ok: false,
        }
    }
}

/// Tools for the tests alone, in debug builds under
/// `DUSCOPE_TEST_MCP_TOOLS`: two verbs behind the machinery every tool goes
/// through, while no shipped tool exists yet to test it with.
#[cfg(debug_assertions)]
mod fixture {
    use std::ffi::OsString;

    use duscope_mcp::roots::Refusal;
    use duscope_mcp::tools::{
        Annotations, Context, Invocation, Tool, path_argument, scan_argument,
    };
    use serde_json::{Map, Value};

    /// The fixture tools.
    pub fn tools() -> Vec<Box<dyn Tool>> {
        vec![Box::new(Summary), Box::new(Top)]
    }

    const READ_ONLY: Annotations = Annotations {
        read_only: true,
        destructive: false,
        idempotent: true,
    };

    /// `summary [--scan ID] PATH`.
    struct Summary;

    impl Tool for Summary {
        fn name(&self) -> &'static str {
            "test_summary"
        }

        fn description(&self) -> &'static str {
            "A saved scan's totals under a path (a test fixture)."
        }

        fn input_schema(&self) -> &'static str {
            r#"{
              "type": "object",
              "properties": {
                "path": { "type": "string", "description": "A path under a root" },
                "scan_id": { "type": "string", "description": "latest, or a saved scan's id" }
              },
              "additionalProperties": false
            }"#
        }

        fn annotations(&self) -> Annotations {
            READ_ONLY
        }

        fn invocation(
            &self,
            arguments: &Map<String, Value>,
            context: &Context<'_>,
        ) -> Result<Invocation, Refusal> {
            let scan = scan_argument(arguments, "scan_id", context)?;
            let path = path_argument(arguments, "path", scan.as_ref(), context.roots)?;
            let mut args: Vec<OsString> = vec!["--no-scan".into()];
            if let Some(scan) = scan {
                args.extend(["--scan".into(), scan.selector.into()]);
            }
            args.push(path.into());
            Ok(Invocation {
                verb: "summary",
                args,
            })
        }

        fn summary(&self, envelope: &Value) -> String {
            format!(
                "{} bytes under {}",
                envelope["result"]["allocated_bytes"],
                envelope["result"]["path"].as_str().unwrap_or("?")
            )
        }
    }

    /// `top PATH [--limit N]`.
    struct Top;

    impl Tool for Top {
        fn name(&self) -> &'static str {
            "test_top"
        }

        fn description(&self) -> &'static str {
            "The largest directories under a path (a test fixture)."
        }

        fn input_schema(&self) -> &'static str {
            r#"{
              "type": "object",
              "properties": {
                "path": { "type": "string" },
                "limit": { "type": "integer", "minimum": 1, "maximum": 100 }
              },
              "required": ["path"],
              "additionalProperties": false
            }"#
        }

        fn annotations(&self) -> Annotations {
            READ_ONLY
        }

        fn invocation(
            &self,
            arguments: &Map<String, Value>,
            context: &Context<'_>,
        ) -> Result<Invocation, Refusal> {
            let path = path_argument(arguments, "path", None, context.roots)?;
            let mut args: Vec<OsString> = vec!["--no-scan".into()];
            if let Some(limit) = arguments.get("limit").and_then(Value::as_u64) {
                args.extend(["--limit".into(), limit.to_string().into()]);
            }
            args.push(path.into());
            Ok(Invocation { verb: "top", args })
        }

        fn summary(&self, envelope: &Value) -> String {
            format!(
                "{} of {} rows",
                envelope["returned_count"], envelope["total_count"]
            )
        }
    }
}
