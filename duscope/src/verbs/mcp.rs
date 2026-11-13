//! `mcp`: the MCP server on stdin and stdout (P3-MCP-03).
//!
//! The server answers a client in either protocol revision duscope speaks
//! and offers the tools of `duscope_mcp::tools`, each bound to `--roots`.
//! A tool's verb runs in this process exactly as `duscope <verb> --json`
//! runs from a shell ([`crate::run_captured`]): the same flags, the same
//! configuration file, the same index directory and `--max-chars`, so the
//! result's `structuredContent` is the envelope that command prints, byte
//! for byte. The server reads the configuration file as every verb does,
//! and stops at start on one it cannot use; its index directory is
//! `--index-dir` or `DUSCOPE_INDEX_DIR`, then the file's `index_dir`, then
//! the default (P3-MCP-09). Nothing but protocol messages reaches stdout;
//! the log goes to stderr at `--log-level`.
//!
//! The calls run one at a time on the server's worker thread (P3-MCP-05). A
//! call's verb runs with the call's [`CallSignals`] at hand
//! ([`running_call`]): a scan stops its walk when the call is cancelled or
//! runs past its deadline, as it stops for Ctrl-C from a shell, and counts
//! the entries it reads, which the server reports as the call's progress.

use std::cell::RefCell;
use std::ffi::OsString;
use std::path::PathBuf;

use duscope_core::config::{Config, ConfigSource};
use duscope_core::index;
use duscope_mcp::discover::ServerConfig;
use duscope_mcp::roots::{Refusal, Roots};
use duscope_mcp::server::{Server, Tools};
use duscope_mcp::tools::{Invocation, Outcome, Registry, Runner};
use duscope_mcp::transport::Transport;
use duscope_mcp::worker::{CallSignals, Timing};

use super::{Exit, Failure};
use crate::cli::{GlobalArgs, McpArgs};
use crate::out::{DEFAULT_MAX_CHARS, OutputArgs};

/// Serves until the client closes stdin.
pub fn serve(args: &McpArgs, global: &GlobalArgs, output: &OutputArgs) -> Result<(), Failure> {
    // Read before anything else, as main reads it for every other verb.
    let source = ConfigSource::locate(global.config.as_deref(), |name| std::env::var_os(name));
    let config = Config::load(&source).map_err(|e| crate::config_failure(&e))?;
    let roots = Roots::new(&args.roots).map_err(|e| {
        Failure::new(Exit::Usage, "usage", e.to_string())
            .hint("Run duscope mcp with --roots naming directories there are")
    })?;
    let index_dir = index_dir(global.index_dir.clone(), &config, |name| {
        std::env::var_os(name)
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
    let runner: Box<dyn Runner> = Box::new(VerbRunner {
        index_dir: index_dir.clone(),
        max_chars: output.max_chars,
        config: global.config.clone(),
    });
    #[cfg(debug_assertions)]
    let runner: Box<dyn Runner> = Box::new(fixture::Panicking(runner));
    // A scan's Full Disk Access warning names the app that runs the server.
    super::scan::serving_mcp();
    let config = ServerConfig::new(&crate::agent::briefing());
    let server = Server::new(config)
        .with_tools(Tools {
            registry,
            roots,
            index_dir,
            runner,
        })
        .with_timing(timing());
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

/// The index directory the server reads and saves into, as every verb
/// chooses it (`cli::apply_config`): `given`, which is `--index-dir` or
/// `DUSCOPE_INDEX_DIR`, then the configuration's `index_dir`, then the
/// default under `env`'s `XDG_CACHE_HOME` or `HOME`.
fn index_dir(
    given: Option<PathBuf>,
    config: &Config,
    env: impl Fn(&str) -> Option<OsString>,
) -> Result<PathBuf, Failure> {
    given
        .or_else(|| {
            config
                .defaults
                .index_dir
                .as_ref()
                .map(|dir| dir.value.clone())
        })
        .or_else(|| index::default_dir(env))
        .ok_or_else(|| {
            Failure::new(
                Exit::Usage,
                "usage",
                "there is no index directory: HOME and XDG_CACHE_HOME are unset",
            )
            .hint("Run duscope mcp with --index-dir DIR")
        })
}

/// The tools the server offers.
fn tools() -> Vec<Box<dyn duscope_mcp::tools::Tool>> {
    let tools = duscope_mcp::tools::shipped();
    #[cfg(debug_assertions)]
    let tools = {
        let mut tools = tools;
        if let Some(fixtures) = std::env::var_os("DUSCOPE_TEST_MCP_TOOLS").filter(|v| !v.is_empty())
        {
            tools.extend(fixture::tools(fixtures == "panic"));
        }
        tools
    };
    tools
}

/// When the server reports a call's progress and how long a call may run:
/// every two seconds, and each tool's own deadline. In a debug build, which
/// the tests run, `DUSCOPE_TEST_MCP_PROGRESS_MS` and
/// `DUSCOPE_TEST_MCP_DEADLINE_MS` shorten them, the second for every tool.
fn timing() -> Timing {
    let millis = |name: &str| {
        std::env::var(name)
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .map(std::time::Duration::from_millis)
    };
    let mut timing = Timing::default();
    if cfg!(debug_assertions) {
        if let Some(every) = millis("DUSCOPE_TEST_MCP_PROGRESS_MS") {
            timing.progress_every = every;
        }
        timing.deadline = millis("DUSCOPE_TEST_MCP_DEADLINE_MS");
    }
    timing
}

thread_local! {
    /// The signals of the tool call whose verb runs on this thread.
    static RUNNING_CALL: RefCell<Option<CallSignals>> = const { RefCell::new(None) };
}

/// The signals of the tool call whose verb runs on this thread, if one
/// does: the flag that stops a scan, and the count its entries go to.
pub fn running_call() -> Option<CallSignals> {
    RUNNING_CALL.with(|call| call.borrow().clone())
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

    /// Runs `invocation`'s command line, as `duscope` with the server's
    /// flags, in this process: what it wrote, and whether it succeeded.
    fn run_verb(&self, invocation: &Invocation) -> Outcome {
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
}

impl Runner for VerbRunner {
    fn run(&self, invocation: &Invocation) -> Outcome {
        self.run_verb(invocation)
    }

    fn run_call(&self, invocation: &Invocation, call: &CallSignals) -> Outcome {
        let outer = RUNNING_CALL.with(|running| running.replace(Some(call.clone())));
        let outcome = self.run_verb(invocation);
        RUNNING_CALL.with(|running| running.replace(outer));
        outcome
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

#[cfg(test)]
mod mapping;
#[cfg(test)]
mod tests;

/// Tools for the tests alone, in debug builds under
/// `DUSCOPE_TEST_MCP_TOOLS`: two verbs behind the machinery every tool goes
/// through, kept apart from the shipped tools so that the machinery's tests
/// stay as they are while the tools change; and, when its value is `panic`,
/// `test_panic`, whose verb panics (P3-MCP-08).
#[cfg(debug_assertions)]
mod fixture {
    use std::ffi::OsString;
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    use duscope_mcp::roots::Refusal;
    use duscope_mcp::tools::{
        Annotations, Context, Invocation, Outcome, Runner, Tool, path_argument, scan_argument,
    };
    use duscope_mcp::worker::CallSignals;
    use serde_json::{Map, Value};

    /// The fixture tools, `test_panic` among them when `panic`.
    pub fn tools(panic: bool) -> Vec<Box<dyn Tool>> {
        let mut tools: Vec<Box<dyn Tool>> = vec![Box::new(Summary), Box::new(Top)];
        if panic {
            tools.push(Box::new(Panic));
        }
        tools
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

    /// The verb `test_panic` runs, which is no verb of the command line's:
    /// [`Panicking`] runs it.
    const PANIC_VERB: &str = "test-panic";

    /// How long `test_panic`'s verb waits for its file before it panics all
    /// the same.
    const PANIC_PATIENCE: Duration = Duration::from_secs(10);

    /// `test_panic AFTER`: a verb that panics, as one with a bug would, once
    /// the file `after` exists, which the test creates when the server holds
    /// the calls it means to queue behind it.
    struct Panic;

    impl Tool for Panic {
        fn name(&self) -> &'static str {
            "test_panic"
        }

        fn description(&self) -> &'static str {
            "Panics once a file exists (a test fixture)."
        }

        fn input_schema(&self) -> &'static str {
            r#"{
              "type": "object",
              "properties": {
                "after": { "type": "string", "description": "The file that makes the verb panic" }
              },
              "required": ["after"],
              "additionalProperties": false
            }"#
        }

        fn annotations(&self) -> Annotations {
            READ_ONLY
        }

        fn invocation(
            &self,
            arguments: &Map<String, Value>,
            _: &Context<'_>,
        ) -> Result<Invocation, Refusal> {
            let after = arguments.get("after").and_then(Value::as_str).unwrap_or("");
            Ok(Invocation {
                verb: PANIC_VERB,
                args: vec![after.into()],
            })
        }

        fn summary(&self, _: &Value) -> String {
            "the verb returned".to_owned()
        }
    }

    /// The server's runner in a debug build: `test_panic`'s verb, which it
    /// runs itself, and every other verb as the runner it holds runs them.
    pub struct Panicking(pub Box<dyn Runner>);

    impl std::fmt::Debug for Panicking {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("Panicking").finish_non_exhaustive()
        }
    }

    impl Runner for Panicking {
        fn run(&self, invocation: &Invocation) -> Outcome {
            panic_on(invocation);
            self.0.run(invocation)
        }

        fn run_call(&self, invocation: &Invocation, call: &CallSignals) -> Outcome {
            panic_on(invocation);
            self.0.run_call(invocation, call)
        }

        fn refuse(&self, refusal: &Refusal) -> Outcome {
            self.0.refuse(refusal)
        }
    }

    /// For `test_panic`'s verb, waits until its file exists, for
    /// [`PANIC_PATIENCE`] at most, and panics.
    fn panic_on(invocation: &Invocation) {
        if invocation.verb != PANIC_VERB {
            return;
        }
        let after = invocation.args.first().map(PathBuf::from);
        let started = Instant::now();
        while after.as_ref().is_some_and(|after| !after.exists())
            && started.elapsed() < PANIC_PATIENCE
        {
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("test_panic's verb panicked, as its test asked");
    }
}

#[cfg(test)]
mod index_dir_order {
    use std::ffi::OsString;
    use std::path::PathBuf;

    use duscope_core::config::Config;
    use duscope_testkit::TestEnv;

    use super::index_dir;

    /// The order, from the environment's own values: the test process's
    /// environment is not the test's to set.
    #[test]
    fn p3_mcp_09_the_configured_index_directory_comes_after_the_flag_and_the_variable() {
        let env = TestEnv::new();
        let configured = env.work_dir().join("configured");
        let config = Config::parse(
            &format!("[defaults]\nindex_dir = \"{}\"\n", configured.display()),
            &env.xdg_config_home().join("duscope/config.toml"),
        )
        .unwrap();
        let vars = |name: &str| -> Option<OsString> {
            match name {
                "HOME" => Some(env.home().into()),
                "XDG_CACHE_HOME" => Some(env.xdg_cache_home().into()),
                _ => None,
            }
        };
        // --index-dir, or DUSCOPE_INDEX_DIR, which clap reads into it.
        let given = env.index_dir();
        assert_eq!(
            index_dir(Some(given.clone()), &config, vars).unwrap(),
            given
        );
        // Then the configuration's.
        assert_eq!(index_dir(None, &config, vars).unwrap(), configured);
        // Then the default.
        assert_eq!(
            index_dir(None, &Config::default(), vars).unwrap(),
            env.xdg_cache_home().join("duscope")
        );
        let none = index_dir(None, &Config::default(), |_| None).unwrap_err();
        assert_eq!(none.code, "usage");
        // The configuration's needs neither HOME nor XDG_CACHE_HOME.
        assert_eq!(
            index_dir(None, &config, |_| None).unwrap(),
            PathBuf::from(&configured)
        );
    }
}
