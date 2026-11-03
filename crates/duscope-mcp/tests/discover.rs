//! P3-MCP-02: `server/discover` and per-request versions (2026-07-28), the
//! `initialize` handshake (2025-11-25), and the versions refused, through the
//! transport in memory.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::io::{self, Write};
use std::sync::{Arc, Mutex};

use duscope_mcp::discover::{
    MAX_INSTRUCTIONS_CHARS, PLACEHOLDER_INSTRUCTIONS, SUPPORTED_VERSIONS, ServerConfig,
};
use duscope_mcp::server::Server;
use duscope_mcp::transport::{self, Outbox, Reader};
use serde_json::{Value, json};

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

/// `lines` through the transport to a server with the placeholder
/// instructions: every message it wrote.
fn serve(lines: &[Value]) -> Vec<Value> {
    serve_with(ServerConfig::new(PLACEHOLDER_INSTRUCTIONS), lines)
}

/// `lines` through the transport to a server described by `config`.
fn serve_with(config: ServerConfig, lines: &[Value]) -> Vec<Value> {
    let mut input = String::new();
    for line in lines {
        input.push_str(&line.to_string());
        input.push('\n');
    }
    let out = Buffer::default();
    transport::run(
        Reader::new(input.as_bytes()),
        &Outbox::new(out.clone()),
        &mut Server::new(config),
    )
    .unwrap();
    let bytes = out.0.lock().unwrap().clone();
    bytes
        .split(|&b| b == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice(line).unwrap())
        .collect()
}

/// A 2026-07-28 request: its version and the client's capabilities in
/// `_meta`.
fn modern(id: u64, method: &str, version: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": {
            "_meta": {
                "io.modelcontextprotocol/protocolVersion": version,
                "io.modelcontextprotocol/clientInfo": {"name": "test", "version": "1"},
                "io.modelcontextprotocol/clientCapabilities": {},
            },
        },
    })
}

fn request(id: u64, method: &str, params: &Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
}

fn initialize(id: u64, version: &str) -> Value {
    request(
        id,
        "initialize",
        &json!({
            "protocolVersion": version,
            "capabilities": {},
            "clientInfo": {"name": "test", "version": "1"},
        }),
    )
}

/// The error of a response to request `id`: its code and data.
fn error(message: &Value, id: u64) -> (i64, Value) {
    assert_eq!(message["id"], id, "{message}");
    assert!(message.get("result").is_none(), "{message}");
    let error = &message["error"];
    (error["code"].as_i64().unwrap(), error["data"].clone())
}

fn server_info() -> Value {
    json!({"name": "duscope", "version": env!("CARGO_PKG_VERSION")})
}

#[test]
fn p3_mcp_02_discover_names_the_versions_capabilities_instructions_and_server() {
    let answers = serve(&[modern(1, "server/discover", "2026-07-28")]);
    assert_eq!(
        answers,
        [json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "resultType": "complete",
                "supportedVersions": ["2026-07-28", "2025-11-25"],
                "capabilities": {},
                "instructions": PLACEHOLDER_INSTRUCTIONS,
                "ttlMs": 3_600_000,
                "cacheScope": "public",
                "_meta": {"io.modelcontextprotocol/serverInfo": server_info()},
            },
        })]
    );
}

#[test]
fn p3_mcp_02_each_request_is_answered_from_its_own_meta() {
    let no_capabilities = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "ping",
        "params": {"_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28"}},
    });
    let numeric_version = json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "ping",
        "params": {"_meta": {
            "io.modelcontextprotocol/protocolVersion": 2026,
            "io.modelcontextprotocol/clientCapabilities": {},
        }},
    });
    let answers = serve(&[
        modern(1, "server/discover", "2026-07-28"),
        // A version given once does not carry over: each request needs its
        // own, with the client's capabilities, or it is malformed.
        no_capabilities,
        numeric_version,
        request(4, "server/discover", &json!({})),
        modern(5, "ping", "2026-07-28"),
        modern(6, "tools/list", "2026-07-28"),
    ]);
    assert_eq!(answers.len(), 6, "{answers:?}");
    assert_eq!(answers[0]["result"]["resultType"], "complete");
    for (answer, id) in answers[1..4].iter().zip(2..) {
        assert_eq!(error(answer, id).0, -32602, "{answer}");
    }
    assert_eq!(
        answers[4]["result"],
        json!({
            "resultType": "complete",
            "_meta": {"io.modelcontextprotocol/serverInfo": server_info()},
        })
    );
    // No tool registry yet.
    assert_eq!(error(&answers[5], 6).0, -32601);
}

#[test]
fn p3_mcp_02_an_unknown_version_in_meta_gets_the_revisions_error_with_the_list() {
    let answers = serve(&[
        modern(1, "server/discover", "1900-01-01"),
        modern(2, "ping", "2024-11-05"),
        // 2025-11-25 is served over initialize alone.
        modern(3, "server/discover", "2025-11-25"),
        modern(4, "server/discover", "2026-07-28"),
    ]);
    for (answer, (id, requested)) in
        answers
            .iter()
            .zip([(1, "1900-01-01"), (2, "2024-11-05"), (3, "2025-11-25")])
    {
        assert_eq!(
            error(answer, id),
            (
                -32022,
                json!({"supported": ["2026-07-28", "2025-11-25"], "requested": requested})
            ),
            "{answer}"
        );
        assert_eq!(answer["error"]["message"], "Unsupported protocol version");
    }
    // The refusals changed nothing: the next request is served.
    assert_eq!(answers[3]["result"]["resultType"], "complete");
}

#[test]
fn p3_mcp_02_the_initialize_handshake_serves_2025_11_25() {
    let answers = serve(&[
        // ping is allowed ahead of the handshake.
        request(1, "ping", &json!({})),
        // Without the handshake or a version in _meta, a request is malformed.
        request(2, "tools/list", &json!({})),
        initialize(3, "2025-11-25"),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        request(4, "ping", &json!({})),
        // In the session, a request needs no version, and the methods so far
        // are ping alone.
        request(5, "tools/list", &json!({})),
        // A 2026-07-28 request is still answered on its own.
        modern(6, "server/discover", "2026-07-28"),
    ]);
    assert_eq!(
        answers.len(),
        6,
        "the notification is never answered: {answers:?}"
    );
    assert_eq!(answers[0]["result"], json!({}));
    assert_eq!(error(&answers[1], 2).0, -32602);
    assert_eq!(
        answers[2],
        json!({
            "jsonrpc": "2.0",
            "id": 3,
            "result": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "serverInfo": server_info(),
                "instructions": PLACEHOLDER_INSTRUCTIONS,
            },
        })
    );
    assert_eq!(answers[3], json!({"jsonrpc": "2.0", "id": 4, "result": {}}));
    assert_eq!(error(&answers[4], 5).0, -32601);
    assert_eq!(
        answers[5]["result"]["supportedVersions"],
        json!(SUPPORTED_VERSIONS)
    );
}

#[test]
fn p3_mcp_02_initialize_naming_a_version_without_a_handshake_gets_2025_11_25() {
    let answers = serve(&[initialize(1, "2026-07-28")]);
    assert_eq!(answers[0]["result"]["protocolVersion"], "2025-11-25");
}

#[test]
fn p3_mcp_02_a_client_sending_2024_11_05_is_refused_with_the_list() {
    let answers = serve(&[
        initialize(1, "2024-11-05"),
        initialize(2, "1.0.0"),
        request(3, "initialize", &json!({"capabilities": {}})),
        // A refused handshake opens no session.
        request(4, "tools/list", &json!({})),
    ]);
    for (answer, (id, requested)) in answers.iter().zip([(1, "2024-11-05"), (2, "1.0.0")]) {
        assert_eq!(
            error(answer, id),
            (
                -32602,
                json!({"supported": ["2026-07-28", "2025-11-25"], "requested": requested})
            ),
            "{answer}"
        );
        assert_eq!(answer["error"]["message"], "Unsupported protocol version");
    }
    assert_eq!(error(&answers[2], 3).0, -32602);
    let (code, data) = error(&answers[3], 4);
    assert_eq!((code, data), (-32602, Value::Null));
}

#[test]
fn p3_mcp_02_instructions_are_cut_to_4096_characters() {
    assert_eq!(MAX_INSTRUCTIONS_CHARS, 4096);
    let utf16 = |text: &str| text.encode_utf16().count();
    // Characters as a JavaScript client counts them: UTF-16 code units, so a
    // character outside the BMP counts two, and none is split.
    for (text, kept) in [
        ("é".repeat(5000), 4096),
        ("😀".repeat(3000), 2048),
        (format!("a{}", "😀".repeat(2048)), 2048),
    ] {
        let config = ServerConfig::new(&text);
        assert_eq!(
            config.instructions().chars().count(),
            kept,
            "{}",
            &text[..8]
        );
        assert!(utf16(config.instructions()) <= MAX_INSTRUCTIONS_CHARS);
    }
    assert_eq!(
        ServerConfig::new("a few words").instructions(),
        "a few words"
    );
}

#[test]
fn p3_mcp_02_capabilities_are_the_servers_to_set() {
    let capabilities = json!({"tools": {}});
    let config =
        ServerConfig::new("x").with_capabilities(capabilities.as_object().unwrap().clone());
    let answers = serve_with(
        config,
        &[
            modern(1, "server/discover", "2026-07-28"),
            initialize(2, "2025-11-25"),
        ],
    );
    assert_eq!(answers[0]["result"]["capabilities"], capabilities);
    assert_eq!(answers[1]["result"]["capabilities"], capabilities);
}
