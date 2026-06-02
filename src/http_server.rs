//! Optional remote MCP transport over Streamable HTTP.
//!
//! This is what lets a cloud client (e.g. ChatGPT "developer mode" custom
//! connectors) talk to winx. The default stdio transport is for local clients
//! (Claude Desktop, Cursor); this one serves the MCP protocol over HTTP at
//! `/mcp` so it can sit behind an HTTPS tunnel (cloudflared/ngrok).
//!
//! # SECURITY
//! winx exposes arbitrary shell execution and filesystem access. Serving it over
//! the network is effectively remote code execution on this machine. Therefore:
//! - a non-empty bearer token is **required**; every request must present it via
//!   `Authorization: Bearer <token>` or a `?token=<token>` query parameter;
//! - bind to a loopback address and put an authenticated HTTPS tunnel in front —
//!   never expose this straight to `0.0.0.0` on an untrusted network;
//! - turn it off when you're done testing.

// Module docs name products (ChatGPT, OAuth, cloudflared) — prose, not code idents.
#![allow(clippy::doc_markdown)]

use std::sync::Arc;

use axum::{
    extract::{Request, State},
    http::StatusCode,
    middleware::{self, Next},
    response::{IntoResponse, Response},
    Router,
};
use rmcp::transport::streamable_http_server::{
    session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
};

use crate::server::WinxService;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// Start the Streamable HTTP MCP server.
///
/// - `bind`: socket address to listen on, e.g. `127.0.0.1:8000`.
/// - `token`: shared secret required on every request (must be non-empty).
/// - `extra_hosts`: additional `Host` authorities to accept beyond loopback —
///   add your tunnel hostname here (e.g. `abc.trycloudflare.com`), otherwise the
///   built-in DNS-rebinding guard rejects requests coming through the tunnel.
pub async fn start_http_server(
    bind: &str,
    token: String,
    extra_hosts: Vec<String>,
) -> Result<(), BoxError> {
    if token.trim().is_empty() {
        return Err("refusing to start HTTP transport without a token (RCE exposure)".into());
    }

    // Each MCP session gets its own WinxService (its own shell state).
    let mut config = StreamableHttpServerConfig::default();
    config.stateful_mode = true;
    config.allowed_hosts.extend(extra_hosts);

    // One shared WinxService — and thus one shared bash_state / live PTY — across
    // every request. Remote clients like ChatGPT are effectively stateless: they
    // don't reuse the MCP session between tool calls, so a per-session service
    // would throw away the shell that `Initialize` created before `BashCommand`
    // ever runs ("Bash state not initialized"). Sharing one instance keeps the
    // initialized shell alive for the whole lifetime of the server.
    let shared = WinxService::new();
    let mcp_service = StreamableHttpService::new(
        move || Ok(shared.clone()),
        Arc::new(LocalSessionManager::default()),
        config,
    );

    let app = Router::new()
        .nest_service("/mcp", mcp_service)
        .layer(middleware::from_fn_with_state(Arc::new(token), require_token));

    let listener = tokio::net::TcpListener::bind(bind).await?;
    tracing::warn!(
        "winx remote MCP transport on http://{bind}/mcp — shell/file access is now \
         network-reachable. Keep it behind an HTTPS tunnel and shut it down when done."
    );
    axum::serve(listener, app).await?;
    Ok(())
}

/// Reject any request that doesn't carry the shared token.
async fn require_token(State(token): State<Arc<String>>, request: Request, next: Next) -> Response {
    if request_has_token(&request, &token) {
        next.run(request).await
    } else {
        (StatusCode::UNAUTHORIZED, "missing or invalid token\n").into_response()
    }
}

/// True if the request presents the token via `Authorization: Bearer` or `?token=`.
fn request_has_token(request: &Request, expected: &str) -> bool {
    let header_match = request
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .is_some_and(|presented| constant_time_eq(presented.trim(), expected));

    let query_match = request.uri().query().is_some_and(|query| {
        query
            .split('&')
            .filter_map(|pair| pair.split_once('='))
            .any(|(key, value)| key == "token" && constant_time_eq(value, expected))
    });

    header_match || query_match
}

/// Length-aware byte comparison that avoids early-exit on the first mismatch.
fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::constant_time_eq;

    #[test]
    fn token_comparison() {
        assert!(constant_time_eq("s3cret", "s3cret"));
        assert!(!constant_time_eq("s3cret", "s3creT"));
        assert!(!constant_time_eq("s3cret", "s3cret-longer"));
        assert!(!constant_time_eq("", "x"));
    }
}
