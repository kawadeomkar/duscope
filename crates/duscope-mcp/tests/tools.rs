//! P3-MCP-03: `tools/list` and `tools/call` through the server, in both
//! protocol revisions: the result's shape on the wire, the capability both
//! handshakes announce, and rmcp's client (the conformance oracle) listing
//! and calling a tool.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::io::{self, Write};
use std::os::fd::OwnedFd;
use std::sync::{Arc, Mutex};

use duscope_mcp::discover::{PLACEHOLDER_INSTRUCTIONS, ServerConfig};
use duscope_mcp::roots::{Refusal, Roots};
use duscope_mcp::server::{Server, TOOLS_TTL_MS, Tools};
use duscope_mcp::tools::{
    Annotations, Context, Invocation, Outcome, Registry, Runner, Tool, path_argument,
};
use duscope_mcp::transport::{self, Outbox, Reader, Transport};
use duscope_testkit::TestEnv;
use rmcp::model::{
    CallToolRequestParams, ClientCapabilities, ClientConfig, Implementation, ProtocolVersion,
};
use rmcp::service::{ClientLifecycleMode, ClientServiceExt};
use serde_json::{Map, Value, json};
use tokio::net::unix::pipe;

/// The envelope the fake verb writes, its keys out of alphabetical order
/// as the verbs' are.
const ENVELOPE: &str = r#"{"ok":true,"schema_version":"0.2","entries":[],"returned_count":0}"#;

/// `duscope_echo`: `top PATH`, with a fake verb behind it.
struct Echo;

impl Tool for Echo {
    fn name(&self) -> &'static str {
        "duscope_echo"
    }

    fn description(&self) -> &'static str {
        "Echoes."
    }

    fn input_schema(&self) -> &'static str {
        r#"{"type":"object","properties":{"path":{"type":"string"}},"additionalProperties":false}"#
    }

    fn annotations(&self) -> Annotations {
        Annotations {
            read_only: true,
            destructive: false,
            idempotent: true,
        }
    }

    fn invocation(
        &self,
        arguments: &Map<String, Value>,
        context: &Context<'_>,
    ) -> Result<Invocation, Refusal> {
        let path = path_argument(arguments, "path", None, context.roots)?;
        Ok(Invocation {
            verb: "top",
            args: vec![path.into()],
        })
    }

    fn summary(&self, envelope: &Value) -> String {
        format!("{} rows", envelope["returned_count"])
    }
}

struct FakeVerb;

impl Runner for FakeVerb {
    fn run(&self, _: &Invocation) -> Outcome {
        Outcome {
            json: ENVELOPE.to_owned(),
            ok: true,
        }
    }

    fn refuse(&self, refusal: &Refusal) -> Outcome {
        Outcome {
            json: json!({"ok": false, "error": {"code": refusal.code, "message": refusal.message, "hint": refusal.hint}}).to_string(),
            ok: false,
        }
    }
}

/// A server offering `duscope_echo`, rooted at the environment's work
/// directory.
fn server(env: &TestEnv) -> Server {
    let mut registry = Registry::new(24_000, false);
    registry.register(Box::new(Echo)).unwrap();
    Server::new(ServerConfig::new(PLACEHOLDER_INSTRUCTIONS)).with_tools(Tools {
        registry,
        roots: Roots::new([env.work_dir()]).unwrap(),
        index_dir: env.index_dir(),
        runner: Box::new(FakeVerb),
    })
}

/// A shared byte buffer standing in for stdout.
#[derive(Clone, Default)]
struct Buffer(Arc<Mutex<Vec<u8>>>);

impl Write for Buffer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// `lines` through the transport to `server`: every line it wrote.
fn serve(mut server: Server, lines: &[Value]) -> Vec<String> {
    let mut input = String::new();
    for line in lines {
        input.push_str(&line.to_string());
        input.push('\n');
    }
    let out = Buffer::default();
    transport::run(
        Reader::new(input.as_bytes()),
        &Outbox::new(out.clone()),
        &mut server,
    )
    .unwrap();
    let bytes = out.0.lock().unwrap().clone();
    String::from_utf8(bytes)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect()
}

fn modern(id: u64, method: &str, extra: &Value) -> Value {
    let mut params = json!({
        "_meta": {
            "io.modelcontextprotocol/protocolVersion": "2026-07-28",
            "io.modelcontextprotocol/clientCapabilities": {},
        },
    });
    params
        .as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
}

fn legacy(id: u64, method: &str, params: &Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
}

fn initialize() -> Value {
    legacy(
        1,
        "initialize",
        &json!({"protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "t", "version": "1"}}),
    )
}

fn result(line: &str) -> Value {
    let message: Value = serde_json::from_str(line).unwrap();
    message
        .get("result")
        .cloned()
        .unwrap_or_else(|| panic!("no result: {line}"))
}

#[test]
fn p3_mcp_03_tools_list_says_how_long_it_holds_in_2026_07_28() {
    let env = TestEnv::new();
    let lines = serve(server(&env), &[modern(1, "tools/list", &json!({}))]);
    let listed = result(&lines[0]);
    assert_eq!(listed["ttlMs"], TOOLS_TTL_MS);
    assert_eq!(listed["cacheScope"], "public");
    assert_eq!(listed["resultType"], "complete");
    assert_eq!(
        listed["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
        "duscope"
    );
    assert_eq!(listed["tools"][0]["name"], "duscope_echo");
    // The schema goes out as written: its keys in the author's order.
    assert!(
        lines[0].contains(
            r#""inputSchema":{"type":"object","properties":{"path":{"type":"string"}},"additionalProperties":false}"#
        ),
        "{}",
        lines[0]
    );
}

#[test]
fn p3_mcp_03_tools_list_in_a_2025_11_25_session_is_as_that_revision_writes_it() {
    let env = TestEnv::new();
    let lines = serve(
        server(&env),
        &[initialize(), legacy(2, "tools/list", &json!({}))],
    );
    let init = result(&lines[0]);
    assert_eq!(init["capabilities"]["tools"], json!({"listChanged": false}));
    let listed = result(&lines[1]);
    assert_eq!(listed["tools"][0]["name"], "duscope_echo");
    for absent in ["ttlMs", "cacheScope", "resultType", "_meta"] {
        assert!(listed.get(absent).is_none(), "{absent}: {listed}");
    }
}

#[test]
fn p3_mcp_03_discovery_announces_the_tools_capability() {
    let env = TestEnv::new();
    let lines = serve(server(&env), &[modern(1, "server/discover", &json!({}))]);
    assert_eq!(
        result(&lines[0])["capabilities"]["tools"],
        json!({"listChanged": false})
    );
    // Without tools: no capability, and no tools/list.
    let bare = Server::new(ServerConfig::new(PLACEHOLDER_INSTRUCTIONS));
    let lines = serve(
        bare,
        &[
            modern(1, "server/discover", &json!({})),
            modern(2, "tools/list", &json!({})),
        ],
    );
    assert!(result(&lines[0])["capabilities"].get("tools").is_none());
    let refused: Value = serde_json::from_str(&lines[1]).unwrap();
    assert_eq!(refused["error"]["code"], -32601);
}

#[test]
fn p3_mcp_03_tools_call_carries_the_envelope_as_the_verb_wrote_it() {
    let env = TestEnv::new();
    let root = env.work_dir().canonicalize().unwrap();
    let call = json!({"name": "duscope_echo", "arguments": {"path": root}});
    let lines = serve(
        server(&env),
        &[
            modern(1, "tools/call", &call),
            initialize(),
            legacy(2, "tools/call", &call),
        ],
    );
    assert!(
        lines[0].contains(&format!(r#""structuredContent":{ENVELOPE}"#)),
        "{}",
        lines[0]
    );
    let modern = result(&lines[0]);
    assert_eq!(modern["isError"], false);
    assert_eq!(
        modern["content"],
        json!([{"type": "text", "text": "0 rows"}])
    );
    assert_eq!(modern["resultType"], "complete");
    let legacy = result(&lines[2]);
    assert_eq!(legacy["structuredContent"], modern["structuredContent"]);
    assert!(legacy.get("resultType").is_none(), "{legacy}");
}

/// Has rmcp's client, speaking `lifecycle`, list the tools and call
/// `duscope_echo` at the work directory and outside it.
fn rmcp_session(lifecycle: ClientLifecycleMode, version: ProtocolVersion) {
    let env = TestEnv::new();
    let root = env.work_dir().canonicalize().unwrap();
    let (server_in, client_out) = std::io::pipe().unwrap();
    let (client_in, server_out) = std::io::pipe().unwrap();
    let running_server = Transport::start(server_in, server_out, server(&env)).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async move {
        let reader = pipe::Receiver::from_owned_fd(OwnedFd::from(client_in)).unwrap();
        let writer = pipe::Sender::from_owned_fd(OwnedFd::from(client_out)).unwrap();
        let client = ClientConfig::new(
            ClientCapabilities::default(),
            Implementation::new("duscope-test", "0"),
        )
        .with_protocol_version(version)
        .serve_with_lifecycle((reader, writer), lifecycle)
        .await
        .unwrap();
        let listed = client.list_tools(None).await.unwrap();
        assert_eq!(listed.tools.len(), 1);
        let tool = &listed.tools[0];
        assert_eq!(tool.name, "duscope_echo");
        let annotations = tool.annotations.as_ref().unwrap();
        assert_eq!(annotations.read_only_hint, Some(true));
        assert_eq!(annotations.open_world_hint, Some(false));
        let arguments = json!({"path": root});
        let called = client
            .call_tool(
                CallToolRequestParams::new("duscope_echo")
                    .with_arguments(arguments.as_object().unwrap().clone()),
            )
            .await
            .unwrap();
        assert_eq!(called.is_error, Some(false));
        assert_eq!(
            called.structured_content,
            Some(serde_json::from_str::<Value>(ENVELOPE).unwrap())
        );
        let outside = client
            .call_tool(
                CallToolRequestParams::new("duscope_echo")
                    .with_arguments(json!({"path": "/"}).as_object().unwrap().clone()),
            )
            .await
            .unwrap();
        assert_eq!(outside.is_error, Some(true));
        assert_eq!(
            outside.structured_content.unwrap()["error"]["code"],
            "refused_path"
        );
        client.cancel().await.unwrap();
    });
    drop(runtime);
    running_server.wait().unwrap();
}

#[test]
fn p3_mcp_03_rmcp_lists_and_calls_a_tool_in_2026_07_28() {
    rmcp_session(
        ClientLifecycleMode::Discover {
            preferred_versions: vec![ProtocolVersion::V_2026_07_28],
        },
        ProtocolVersion::LATEST,
    );
}

#[test]
fn p3_mcp_03_rmcp_lists_and_calls_a_tool_after_initialize() {
    rmcp_session(
        ClientLifecycleMode::Initialize,
        ProtocolVersion::V_2025_11_25,
    );
}
