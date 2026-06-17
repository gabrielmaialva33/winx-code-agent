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
//!   the `Authorization: Bearer <token>` header (header-only — a `?token=` query
//!   parameter would leak the secret into proxy/tunnel logs and browser history);
//! - bind to a loopback address and put an authenticated HTTPS tunnel in front —
//!   never expose this straight to `0.0.0.0` on an untrusted network;
//! - turn it off when you're done testing.

// Module docs name products (ChatGPT, OAuth, cloudflared) — prose, not code idents.
#![allow(clippy::doc_markdown)]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::{
    extract::{ConnectInfo, Request, State},
    http::StatusCode,
    middleware::{self, Next},
    response::{IntoResponse, Response},
    Router,
};
use rmcp::transport::streamable_http_server::{
    session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
};
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::timeout::TimeoutLayer;

use crate::server::{SessionIsolation, WinxService};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// Reject request bodies larger than this — caps an unbounded-POST DoS on the
/// network-reachable RCE endpoint. Sized above the 50 MB per-file ceiling
/// (`MAX_FILE_SIZE` in `read_files`/`file_write_or_edit`) so `FileWriteOrEdit`,
/// which carries the file content inline in the JSON-RPC body, isn't rejected
/// for large-but-legitimate writes over HTTP — the stdio transport has no such
/// limit.
const MAX_BODY_BYTES: usize = 64 * 1024 * 1024;

/// Per-request wall-clock budget. Long-running shell commands run in the
/// background (BashCommand `is_background`/`status_check`), so the HTTP request
/// itself is always short-lived; a stuck request shouldn't pin a connection
/// forever.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

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

    let mut config = StreamableHttpServerConfig::default();
    config.stateful_mode = true;
    config.allowed_hosts.extend(extra_hosts);

    // One shared WinxService across every request. Remote clients like ChatGPT
    // are effectively stateless — they don't reuse the MCP session between tool
    // calls — so a per-session service would throw away the shell that
    // `Initialize` created before `BashCommand` runs ("Bash state not
    // initialized"). Sharing one instance keeps shells alive for the server's
    // lifetime; isolation between logical sessions is provided per `thread_id`
    // by the service's internal session registry.
    //
    // Strict isolation: with many clients behind one shared token, an empty
    // `thread_id` must NOT fall back to whoever was last active (that would land
    // one client in another's shell). Strict mode disables that fallback. Two
    // clients that deliberately reuse the same explicit `thread_id` still share a
    // shell — real multi-tenant isolation needs per-client tokens, which the
    // single shared-token model doesn't provide.
    let shared = WinxService::with_isolation(SessionIsolation::Strict);
    let mcp_service = StreamableHttpService::new(
        move || Ok(shared.clone()),
        Arc::new(LocalSessionManager::default()),
        config,
    );

    // Layer order: the LAST `.layer()` is the outermost, so the body-size limit
    // runs first (it can reject an oversized POST before auth even looks at it),
    // then the timeout, then the token check, then the MCP service.
    let app = Router::new()
        .nest_service("/mcp", mcp_service)
        .layer(middleware::from_fn_with_state(Arc::new(token), require_token))
        .layer(TimeoutLayer::with_status_code(StatusCode::REQUEST_TIMEOUT, REQUEST_TIMEOUT))
        .layer(RequestBodyLimitLayer::new(MAX_BODY_BYTES));

    let listener = tokio::net::TcpListener::bind(bind).await?;
    tracing::warn!(
        "winx remote MCP transport on http://{bind}/mcp — shell/file access is now \
         network-reachable. Keep it behind an HTTPS tunnel and shut it down when done."
    );
    // `into_make_service_with_connect_info` puts the peer address in request
    // extensions so the auth middleware can log who is hammering the endpoint.
    axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await?;
    Ok(())
}

/// Reject any request that doesn't carry the shared token.
async fn require_token(
    State(token): State<Arc<String>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    request: Request,
    next: Next,
) -> Response {
    if request_has_token(&request, &token) {
        next.run(request).await
    } else {
        // Log the peer (never the token) so brute-force attempts on this
        // RCE-adjacent endpoint are visible.
        tracing::warn!(
            "rejected request from {peer} to {} — missing or invalid token",
            request.uri().path()
        );
        (StatusCode::UNAUTHORIZED, "missing or invalid token\n").into_response()
    }
}

/// True if the request presents the token via `Authorization: Bearer`.
///
/// Header-only by design: a `?token=` query parameter would leak the secret into
/// proxy/tunnel access logs, browser history, and `Referer` headers. Clients
/// that need it (ChatGPT connectors, etc.) all support the `Authorization`
/// header.
fn request_has_token(request: &Request, expected: &str) -> bool {
    request
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .is_some_and(|presented| constant_time_eq(presented.trim(), expected))
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
    #![allow(clippy::unwrap_used)]
    use super::{constant_time_eq, request_has_token};
    use axum::body::Body;
    use axum::extract::Request;

    #[test]
    fn token_comparison() {
        assert!(constant_time_eq("s3cret", "s3cret"));
        assert!(!constant_time_eq("s3cret", "s3creT"));
        assert!(!constant_time_eq("s3cret", "s3cret-longer"));
        assert!(!constant_time_eq("", "x"));
    }

    fn req(uri: &str, auth: Option<&str>) -> Request {
        let mut b = Request::builder().uri(uri);
        if let Some(a) = auth {
            b = b.header(axum::http::header::AUTHORIZATION, a);
        }
        b.body(Body::empty()).unwrap()
    }

    #[test]
    fn accepts_valid_bearer_header() {
        assert!(request_has_token(&req("/mcp", Some("Bearer s3cret")), "s3cret"));
    }

    #[test]
    fn rejects_missing_and_wrong_header() {
        assert!(!request_has_token(&req("/mcp", None), "s3cret"));
        assert!(!request_has_token(&req("/mcp", Some("Bearer nope")), "s3cret"));
        assert!(!request_has_token(&req("/mcp", Some("s3cret")), "s3cret")); // no "Bearer "
    }

    #[test]
    fn query_token_is_rejected_now() {
        // Header-only: a `?token=` query param must NOT authenticate (it would
        // leak the secret into proxy/tunnel logs).
        assert!(!request_has_token(&req("/mcp?token=s3cret", None), "s3cret"));
    }
}
