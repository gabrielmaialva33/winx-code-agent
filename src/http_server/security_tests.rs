//! Regression tests for admission order and the single request-body limit.
//!
//! Behind a tunnel every caller shares one source IP, so these tests send all
//! traffic from one peer unless a test is about per-IP independence.

#![allow(clippy::expect_used)]

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::pin::Pin;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::task::{Context, Poll};

use axum::body::{Body, Bytes};
use axum::extract::{ConnectInfo, Request};
use axum::http::{header, HeaderValue, StatusCode};
use axum::middleware;
use axum::response::{IntoResponse as _, Response};
use axum::routing::any;
use axum::Router;
use http_body::Frame;
use tokio::sync::Semaphore;
use tower::Service as _;

use super::{
    build_app, enforce_route_security, HealthState, HttpPrincipal, HttpServerOptions,
    HttpSessionAffinity, LocalSessionManager, RateLimiter, RouteSecurity, SessionOwners,
    INVALID_AUTH_DELAY, MAX_BODY_BYTES,
};

const LEFT_TOKEN: &str = "LEFT-HTTP-TOKEN-0123456789abcdef0123";
const RIGHT_TOKEN: &str = "RIGHT-HTTP-TOKEN-0123456789abcdef012";
const BAD_TOKEN: &str = "WRONG-HTTP-TOKEN-0123456789abcdef012";
const TUNNEL_PEER: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

#[derive(Clone, Copy)]
struct Limits {
    failed_auth_per_ip: u32,
    per_principal: u32,
    concurrency: usize,
    allow_query: bool,
}

impl Default for Limits {
    fn default() -> Self {
        Self { failed_auth_per_ip: 100, per_principal: 100, concurrency: 1, allow_query: false }
    }
}

fn security(limits: Limits) -> Arc<RouteSecurity> {
    Arc::new(RouteSecurity {
        principals: Arc::new(vec![
            HttpPrincipal::new("left", LEFT_TOKEN, false).expect("valid principal"),
            HttpPrincipal::new("right", RIGHT_TOKEN, false).expect("valid principal"),
        ]),
        allow_query: limits.allow_query,
        session_affinity: HttpSessionAffinity::Workspace,
        concurrency: Arc::new(Semaphore::new(limits.concurrency)),
        auth_failure_limiter: Arc::new(RateLimiter::new(limits.failed_auth_per_ip)),
        principal_rate_limiter: Arc::new(RateLimiter::new(limits.per_principal)),
        session_owners: Arc::new(SessionOwners::default()),
        session_manager: Arc::new(LocalSessionManager::default()),
    })
}

/// The handler mints `mcp-session-id: <x-mint-session>` when asked, exactly
/// as the SDK does on a legacy `initialize` response.
fn router(security: Arc<RouteSecurity>) -> Router {
    Router::new()
        .route(
            "/mcp",
            any(|request: Request| async move {
                let mut response = StatusCode::OK.into_response();
                if let Some(minted) = request.headers().get("x-mint-session") {
                    response.headers_mut().insert("mcp-session-id", minted.clone());
                }
                response
            }),
        )
        .layer(middleware::from_fn_with_state(security, enforce_route_security))
}

fn request_from(ip: IpAddr, token: Option<&str>, session: Option<&str>) -> Request {
    let mut builder = Request::builder().method("POST").uri("/mcp");
    if let Some(token) = token {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    if let Some(session) = session {
        builder = builder.header("mcp-session-id", session);
    }
    let mut request = builder.body(Body::empty()).expect("test request");
    request.extensions_mut().insert(ConnectInfo(SocketAddr::new(ip, 40_000)));
    request
}

fn request(token: Option<&str>) -> Request {
    request_from(TUNNEL_PEER, token, None)
}

async fn send(router: &Router, request: Request) -> Response {
    router.clone().call(request).await.expect("infallible router")
}

/// Status and virtual elapsed time of one request (tests run on paused time).
async fn timed(router: &Router, request: Request) -> (StatusCode, std::time::Duration) {
    let started = tokio::time::Instant::now();
    let response = send(router, request).await;
    if response.status() == StatusCode::TOO_MANY_REQUESTS {
        assert_eq!(response.headers()[header::RETRY_AFTER], "1");
    }
    (response.status(), started.elapsed())
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn bad_token_flood_cannot_rate_limit_or_starve_a_valid_principal() {
    let security = security(Limits { failed_auth_per_ip: 3, concurrency: 1, ..Limits::default() });
    let router = router(security.clone());
    let started = tokio::time::Instant::now();
    let flood = (0..40)
        .map(|index| {
            let router = router.clone();
            let token = (index % 2 == 0).then_some(BAD_TOKEN);
            tokio::spawn(async move { timed(&router, request(token)).await })
        })
        .collect::<Vec<_>>();
    for _ in 0..64 {
        tokio::task::yield_now().await;
    }

    // The whole flood is parked in the failure delay, holding no permit, and
    // has already exhausted the shared IP's failed-auth window.
    assert_eq!(security.concurrency.available_permits(), 1, "failed auth holds no permit");
    let valid = send(&router, request(Some(LEFT_TOKEN))).await;
    assert_eq!(valid.status(), StatusCode::OK, "a valid principal is admitted mid-flood");
    assert!(started.elapsed() < INVALID_AUTH_DELAY, "the flood is still in flight");
    assert_eq!(security.concurrency.available_permits(), 0, "the valid request holds it");
    drop(valid);
    assert_eq!(security.concurrency.available_permits(), 1);

    let mut unauthorized = 0;
    let mut limited = 0;
    for task in flood {
        let (status, elapsed) = task.await.expect("flood task");
        assert!(elapsed >= INVALID_AUTH_DELAY, "every failure is delayed: {elapsed:?}");
        match status {
            StatusCode::UNAUTHORIZED => unauthorized += 1,
            StatusCode::TOO_MANY_REQUESTS => limited += 1,
            other => unreachable!("unexpected flood status {other}"),
        }
    }
    assert_eq!((unauthorized, limited), (3, 37), "failures spend the per-IP window");
    let after = send(&router, request(Some(LEFT_TOKEN))).await;
    assert_eq!(after.status(), StatusCode::OK, "still admitted after the window is spent");
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn failed_auth_is_delayed_and_budgeted_per_source_ip() {
    let router =
        router(security(Limits { failed_auth_per_ip: 2, allow_query: true, ..Limits::default() }));
    let other_peer = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 7));

    let missing = timed(&router, request(None)).await;
    assert_eq!(missing.0, StatusCode::UNAUTHORIZED);
    assert!(missing.1 >= INVALID_AUTH_DELAY);
    let mut bad_query = request(None);
    *bad_query.uri_mut() = format!("/mcp?token={BAD_TOKEN}").parse().expect("uri");
    let wrong = timed(&router, bad_query).await;
    assert_eq!(wrong.0, StatusCode::UNAUTHORIZED, "a bad query token is a failed attempt");
    assert!(wrong.1 >= INVALID_AUTH_DELAY);
    let over = timed(&router, request(Some(BAD_TOKEN))).await;
    assert_eq!(over.0, StatusCode::TOO_MANY_REQUESTS, "the budget is spent");
    assert!(over.1 >= INVALID_AUTH_DELAY, "over-budget failures are delayed too");

    let elsewhere = timed(&router, request_from(other_peer, Some(BAD_TOKEN), None)).await;
    assert_eq!(elsewhere.0, StatusCode::UNAUTHORIZED, "windows are per source IP");

    let valid = timed(&router, request(Some(LEFT_TOKEN))).await;
    assert_eq!(valid, (StatusCode::OK, std::time::Duration::ZERO), "success is never delayed");
    let mut good_query = request(None);
    *good_query.uri_mut() = format!("/mcp?token={RIGHT_TOKEN}").parse().expect("uri");
    assert_eq!(timed(&router, good_query).await.0, StatusCode::OK, "opt-in query token");
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn authenticated_requests_do_not_spend_the_failed_auth_window() {
    let router = router(security(Limits { failed_auth_per_ip: 1, ..Limits::default() }));
    for _ in 0..5 {
        assert_eq!(send(&router, request(Some(LEFT_TOKEN))).await.status(), StatusCode::OK);
    }
    assert_eq!(timed(&router, request(Some(BAD_TOKEN))).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(timed(&router, request(Some(BAD_TOKEN))).await.0, StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn weak_token_stays_locked_out_while_its_ip_is_over_budget() {
    const WEAK_TOKEN: &str = "weak-token";
    let mut security = security(Limits { failed_auth_per_ip: 1, ..Limits::default() });
    Arc::get_mut(&mut security).expect("unshared security").principals = Arc::new(vec![
        HttpPrincipal::new("weak", WEAK_TOKEN, true).expect("weak principal"),
        HttpPrincipal::new("left", LEFT_TOKEN, false).expect("valid principal"),
    ]);
    let router = router(security);
    assert_eq!(send(&router, request(Some(WEAK_TOKEN))).await.status(), StatusCode::OK);

    assert_eq!(timed(&router, request(Some(BAD_TOKEN))).await.0, StatusCode::UNAUTHORIZED);
    // A correct guess of a guessable token is indistinguishable from a wrong
    // one until the window resets; a strong token is not locked out.
    let locked = timed(&router, request(Some(WEAK_TOKEN))).await;
    assert_eq!(locked.0, StatusCode::TOO_MANY_REQUESTS);
    assert!(locked.1 >= INVALID_AUTH_DELAY, "delayed like a wrong guess: {:?}", locked.1);
    assert_eq!(send(&router, request(Some(LEFT_TOKEN))).await.status(), StatusCode::OK);
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn principal_limit_applies_before_shared_capacity() {
    let security = security(Limits { per_principal: 1, concurrency: 1, ..Limits::default() });
    let router = router(security.clone());
    let held = send(&router, request(Some(LEFT_TOKEN))).await;
    assert_eq!(held.status(), StatusCode::OK);
    assert_eq!(security.concurrency.available_permits(), 0);

    let over = send(&router, request(Some(LEFT_TOKEN))).await;
    assert_eq!(over.status(), StatusCode::TOO_MANY_REQUESTS, "principal window, not 503");
    assert_eq!(over.headers()[header::RETRY_AFTER], "1");
    let busy = send(&router, request(Some(RIGHT_TOKEN))).await;
    assert_eq!(busy.status(), StatusCode::SERVICE_UNAVAILABLE, "capacity is still bounded");
    drop(held);
    assert_eq!(security.concurrency.available_permits(), 1);
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn session_ids_stay_bound_to_their_creator_under_auth_failures() {
    let security = security(Limits { failed_auth_per_ip: 1, concurrency: 1, ..Limits::default() });
    let router = router(security.clone());
    let mut mint = request(Some(LEFT_TOKEN));
    mint.headers_mut().insert("x-mint-session", HeaderValue::from_static("session-left"));
    assert_eq!(send(&router, mint).await.status(), StatusCode::OK);

    // Spend the shared IP's failure window, including with the owned id.
    let bad = request_from(TUNNEL_PEER, Some(BAD_TOKEN), Some("session-left"));
    assert_eq!(timed(&router, bad).await.0, StatusCode::UNAUTHORIZED);
    let bad = request_from(TUNNEL_PEER, Some(BAD_TOKEN), Some("session-left"));
    assert_eq!(timed(&router, bad).await.0, StatusCode::TOO_MANY_REQUESTS);

    let held = send(&router, request(Some(LEFT_TOKEN))).await;
    assert_eq!(held.status(), StatusCode::OK, "occupy the only permit");
    let foreign = request_from(TUNNEL_PEER, Some(RIGHT_TOKEN), Some("session-left"));
    let foreign = send(&router, foreign).await;
    assert_eq!(foreign.status(), StatusCode::NOT_FOUND, "foreign id: 404 without a permit");
    drop(held);

    let owner = request_from(TUNNEL_PEER, Some(LEFT_TOKEN), Some("session-left"));
    assert_eq!(send(&router, owner).await.status(), StatusCode::OK, "the creator keeps it");
    let foreign = request_from(TUNNEL_PEER, Some(RIGHT_TOKEN), Some("session-left"));
    assert_eq!(send(&router, foreign).await.status(), StatusCode::NOT_FOUND);
    assert_eq!(security.concurrency.available_permits(), 1);
}

/// Delivers a body in fixed-size frames with no length hint, like a chunked
/// upload: only the SDK's streaming limit can bound it.
struct ChunkedBody {
    remaining: Bytes,
}

impl http_body::Body for ChunkedBody {
    type Data = Bytes;
    type Error = std::convert::Infallible;

    fn poll_frame(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let this = self.get_mut();
        if this.remaining.is_empty() {
            return Poll::Ready(None);
        }
        let frame = this.remaining.split_to(this.remaining.len().min(64 * 1024));
        Poll::Ready(Some(Ok(Frame::data(frame))))
    }
}

/// A valid JSON-RPC `initialize` padded with trailing whitespace to `size`.
fn initialize_body(size: usize) -> Bytes {
    let mut body = br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"body-limit-test","version":"1.0"}}}"#
        .to_vec();
    assert!(body.len() <= size);
    body.resize(size, b' ');
    Bytes::from(body)
}

fn mcp_post(body: Bytes, declared: bool) -> Request {
    let length = body.len();
    let body = if declared { Body::from(body) } else { Body::new(ChunkedBody { remaining: body }) };
    let mut builder = Request::builder()
        .method("POST")
        .uri("/mcp")
        .header(header::HOST, "localhost")
        .header(header::AUTHORIZATION, format!("Bearer {LEFT_TOKEN}"))
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json, text/event-stream");
    if declared {
        builder = builder.header(header::CONTENT_LENGTH, length);
    }
    let mut request = builder.body(body).expect("test request");
    request.extensions_mut().insert(ConnectInfo(SocketAddr::new(TUNNEL_PEER, 40_000)));
    request
}

#[tokio::test(flavor = "current_thread")]
async fn request_body_limit_is_one_consistent_413_boundary() {
    assert_eq!(MAX_BODY_BYTES, 16 * 1024 * 1024);
    let options = HttpServerOptions {
        principals: vec![HttpPrincipal::new("left", LEFT_TOKEN, false).expect("principal")],
        ..HttpServerOptions::single("127.0.0.1:0", LEFT_TOKEN, Vec::new(), false).expect("options")
    };
    let app = build_app(
        options,
        Arc::new(crate::runtime::EmbeddedShellRuntime),
        Arc::new(HealthState { ready: AtomicBool::new(true) }),
    );

    for declared in [true, false] {
        // Above rmcp's 4 MiB default: proves the configured limit reaches the SDK.
        let at_limit = send(&app, mcp_post(initialize_body(MAX_BODY_BYTES), declared)).await;
        assert!(
            at_limit.status().is_success(),
            "a body of exactly the limit is accepted (declared={declared}): {}",
            at_limit.status()
        );
        drop(at_limit);
        let over = send(&app, mcp_post(initialize_body(MAX_BODY_BYTES + 1), declared)).await;
        assert_eq!(
            over.status(),
            StatusCode::PAYLOAD_TOO_LARGE,
            "one byte over the limit answers 413 (declared={declared})"
        );
    }
}
