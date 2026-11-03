//! P3-MCP-02: rmcp's client (the conformance oracle, D36) against the server
//! on the stdio transport, over a pair of OS pipes in this process: the
//! 2026-07-28 discovery, the 2025-11-25 `initialize` handshake, a version the
//! server refuses and retries from its list, and a 2024-11-05 client refused
//! with that list.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::os::fd::OwnedFd;

use duscope_mcp::discover::{PLACEHOLDER_INSTRUCTIONS, ServerConfig};
use duscope_mcp::server::Server;
use duscope_mcp::transport::Transport;
use rmcp::model::{
    ClientCapabilities, ClientConfig, ErrorCode, Implementation, ProtocolVersion, ServerPeerInfo,
};
use rmcp::service::{ClientInitializeError, ClientLifecycleMode, ClientServiceExt};
use serde_json::{Value, json};
use tokio::net::unix::pipe;

/// The client's identity, speaking `version` in an `initialize`.
fn client(version: ProtocolVersion) -> ClientConfig {
    ClientConfig::new(
        ClientCapabilities::default(),
        Implementation::new("duscope-test", "0"),
    )
    .with_protocol_version(version)
}

/// Starts the server on fresh pipes and has rmcp's client open `lifecycle`
/// on the other ends: the server's information as the client recorded it,
/// or the client's error. The client then closes its end, and the server
/// must end cleanly.
fn handshake(
    version: ProtocolVersion,
    lifecycle: ClientLifecycleMode,
) -> Result<ServerPeerInfo, Box<ClientInitializeError>> {
    let (server_in, client_out) = std::io::pipe().unwrap();
    let (client_in, server_out) = std::io::pipe().unwrap();
    let server = Transport::start(
        server_in,
        server_out,
        Server::new(ServerConfig::new(PLACEHOLDER_INSTRUCTIONS)),
    )
    .unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let outcome = runtime.block_on(async move {
        let reader = pipe::Receiver::from_owned_fd(OwnedFd::from(client_in)).unwrap();
        let writer = pipe::Sender::from_owned_fd(OwnedFd::from(client_out)).unwrap();
        let running = client(version)
            .serve_with_lifecycle((reader, writer), lifecycle)
            .await
            .map_err(Box::new)?;
        let info = running.peer_info().map(|info| (*info).clone());
        running.cancel().await.unwrap();
        Ok(info.expect("the client recorded no server information"))
    });
    drop(runtime);
    server.wait().unwrap();
    outcome
}

fn server_info(info: &ServerPeerInfo) -> Value {
    serde_json::to_value(&info.server_info).unwrap()
}

#[test]
fn p3_mcp_02_rmcp_completes_the_2026_07_28_discovery() {
    let info = handshake(
        ProtocolVersion::LATEST,
        ClientLifecycleMode::Discover {
            preferred_versions: vec![ProtocolVersion::V_2026_07_28],
        },
    )
    .unwrap();
    assert_eq!(info.protocol_version, ProtocolVersion::V_2026_07_28);
    assert_eq!(
        server_info(&info),
        json!({"name": "duscope", "version": env!("CARGO_PKG_VERSION")})
    );
    assert_eq!(info.instructions.as_deref(), Some(PLACEHOLDER_INSTRUCTIONS));
}

#[test]
fn p3_mcp_02_rmcp_completes_the_2025_11_25_initialize_handshake() {
    let info = handshake(
        ProtocolVersion::V_2025_11_25,
        ClientLifecycleMode::Initialize,
    )
    .unwrap();
    assert_eq!(info.protocol_version, ProtocolVersion::V_2025_11_25);
    assert_eq!(
        server_info(&info),
        json!({"name": "duscope", "version": env!("CARGO_PKG_VERSION")})
    );
    assert_eq!(info.instructions.as_deref(), Some(PLACEHOLDER_INSTRUCTIONS));
}

#[test]
fn p3_mcp_02_rmcp_retries_a_refused_version_from_the_servers_list() {
    // The client asks first for a version the server does not know: the
    // refusal's list sends it to 2026-07-28, where any other error would have
    // sent it to the legacy handshake.
    let future: ProtocolVersion = serde_json::from_value(json!("2099-01-01")).unwrap();
    let info = handshake(
        ProtocolVersion::LATEST,
        ClientLifecycleMode::Discover {
            preferred_versions: vec![future, ProtocolVersion::V_2026_07_28],
        },
    )
    .unwrap();
    assert_eq!(info.protocol_version, ProtocolVersion::V_2026_07_28);
}

#[test]
fn p3_mcp_02_rmcp_probing_for_the_era_stays_on_2026_07_28() {
    let info = handshake(
        ProtocolVersion::V_2025_11_25,
        ClientLifecycleMode::Auto {
            preferred_versions: vec![ProtocolVersion::V_2026_07_28],
            legacy_version: Some(ProtocolVersion::V_2025_11_25),
        },
    )
    .unwrap();
    assert_eq!(info.protocol_version, ProtocolVersion::V_2026_07_28);
}

#[test]
fn p3_mcp_02_rmcp_sending_2024_11_05_is_refused_with_the_list() {
    let refused = handshake(
        ProtocolVersion::V_2024_11_05,
        ClientLifecycleMode::Initialize,
    )
    .unwrap_err();
    let ClientInitializeError::JsonRpcError(error) = *refused else {
        panic!("not refused with a JSON-RPC error: {refused:?}");
    };
    assert_eq!(error.code, ErrorCode::INVALID_PARAMS);
    assert_eq!(
        error.data,
        Some(json!({"supported": ["2026-07-28", "2025-11-25"], "requested": "2024-11-05"}))
    );
}
