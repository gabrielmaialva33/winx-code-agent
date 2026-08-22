//! Optional remote MCP transport over Streamable HTTP.
//!
//! The endpoint exposes shell and filesystem capabilities, so remote mode is
//! deliberately fail-closed: loopback bind by default, strong file-backed or
//! explicit bearer credentials, bounded request concurrency/rate, and isolated
//! daemon namespaces for independently authenticated principals.

#![allow(clippy::doc_markdown)]

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::{
    extract::{ConnectInfo, Request, State},
    http::{header, HeaderValue, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    Router,
};
use rand::RngExt as _;
use rmcp::transport::streamable_http_server::{
    session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
};
use tokio::sync::{Mutex, Semaphore};
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::timeout::TimeoutLayer;

use crate::config::{HttpPrincipal, HttpSessionAffinity};
use crate::runtime::{configured_shell_runtime, ShellRuntime};
use crate::server::{SessionIsolation, WinxService};

pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

const MAX_BODY_BYTES: usize = 64 * 1024 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
const RATE_WINDOW: Duration = Duration::from_secs(60);
const MAX_RATE_TRACKED_IPS: usize = 4_096;
const INVALID_AUTH_DELAY: Duration = Duration::from_millis(100);
pub const DEFAULT_MAX_CONCURRENCY: usize = 32;
pub const DEFAULT_REQUESTS_PER_MINUTE: u32 = 120;

/// Opaque per-request correlation shared by the HTTP middleware and MCP handler.
/// It contains no client input and is safe to include in metadata-only usage logs.
#[derive(Clone, Debug)]
pub(crate) struct RequestCorrelation(String);

impl RequestCorrelation {
    fn new() -> Self {
        Self(format!("r_{:032x}", rand::rng().random::<u128>()))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug)]
pub struct HttpServerOptions {
    pub bind: String,
    pub principals: Vec<HttpPrincipal>,
    pub extra_hosts: Vec<String>,
    pub allow_query_token: bool,
    pub allow_non_loopback: bool,
    pub session_affinity: HttpSessionAffinity,
    pub max_concurrency: usize,
    pub requests_per_minute: u32,
}

impl HttpServerOptions {
    pub fn single(
        bind: impl Into<String>,
        token: impl Into<String>,
        extra_hosts: Vec<String>,
        allow_query_token: bool,
    ) -> Result<Self, BoxError> {
        let token = token.into();
        let principal = HttpPrincipal::new("default", token.trim(), false)?;
        Ok(Self {
            bind: bind.into(),
            principals: vec![principal],
            extra_hosts,
            allow_query_token,
            allow_non_loopback: false,
            session_affinity: HttpSessionAffinity::Workspace,
            max_concurrency: DEFAULT_MAX_CONCURRENCY,
            requests_per_minute: DEFAULT_REQUESTS_PER_MINUTE,
        })
    }
}

pub async fn start_http_server(
    bind: &str,
    token: String,
    extra_hosts: Vec<String>,
    allow_query_token: bool,
) -> Result<(), BoxError> {
    let options = HttpServerOptions::single(bind, token, extra_hosts, allow_query_token)?;
    start_http_server_with_options(options).await
}

pub async fn start_http_server_with_options(options: HttpServerOptions) -> Result<(), BoxError> {
    let shell_runtime = configured_shell_runtime().await?;
    start_http_server_with_runtime(options, shell_runtime).await
}

pub async fn start_http_server_with_runtime(
    options: HttpServerOptions,
    shell_runtime: Arc<dyn ShellRuntime>,
) -> Result<(), BoxError> {
    let bind = validate_options(&options)?;
    let principal_names =
        options.principals.iter().map(|principal| principal.name().to_string()).collect::<Vec<_>>();
    let shared_concurrency = Arc::new(Semaphore::new(options.max_concurrency));
    let shared_rate_limiter = Arc::new(RateLimiter::new(options.requests_per_minute));

    let mut config = StreamableHttpServerConfig::default();
    config.legacy_session_mode = true;
    config.allowed_hosts.extend(options.extra_hosts);

    let shared = WinxService::with_runtime(SessionIsolation::Strict, shell_runtime);
    let mcp_service = StreamableHttpService::new(
        move || Ok(shared.clone()),
        Arc::new(LocalSessionManager::default()),
        config,
    );
    let security = Arc::new(RouteSecurity {
        principals: Arc::new(options.principals),
        allow_query: options.allow_query_token,
        session_affinity: options.session_affinity,
        concurrency: shared_concurrency,
        rate_limiter: shared_rate_limiter,
    });
    // The timeout sits INSIDE the security middleware: when it fires, the
    // middleware still receives the 408 response and records the usage event.
    // With the previous order the timed-out future was dropped before logging,
    // so 408s were invisible in the usage log.
    let protected_mcp = Router::new()
        .nest_service("/mcp", mcp_service)
        .layer(TimeoutLayer::with_status_code(StatusCode::REQUEST_TIMEOUT, REQUEST_TIMEOUT))
        .layer(middleware::from_fn_with_state(security, enforce_route_security))
        .layer(RequestBodyLimitLayer::new(MAX_BODY_BYTES));
    // Keep OAuth discovery probes and every unrelated path outside bearer auth.
    // Winx does not advertise OAuth metadata, so those routes should be ordinary
    // 404s instead of 401s that encourage clients to start a nonexistent flow.
    let app = Router::new().merge(protected_mcp);

    let listener = tokio::net::TcpListener::bind(bind).await?;
    tracing::warn!(
        bind = %bind,
        principals = ?principal_names,
        route = "/mcp",
        "Winx remote MCP is network-reachable; keep it behind authenticated HTTPS"
    );
    if options.allow_query_token {
        tracing::warn!(
            "query-token authentication is enabled; URLs may expose credentials in proxy logs/history"
        );
    }
    axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await?;
    Ok(())
}

fn validate_options(options: &HttpServerOptions) -> Result<SocketAddr, BoxError> {
    let bind: SocketAddr = options
        .bind
        .parse()
        .map_err(|error| format!("invalid --bind socket address {:?}: {error}", options.bind))?;
    if !bind.ip().is_loopback() && !options.allow_non_loopback {
        return Err(format!(
            "refusing non-loopback HTTP bind {bind}; pass --allow-non-loopback only behind a trusted firewall/tunnel"
        )
        .into());
    }
    if options.max_concurrency == 0 {
        return Err("max_concurrency must be at least 1".into());
    }
    if options.requests_per_minute == 0 {
        return Err("requests_per_minute must be at least 1".into());
    }
    if options.principals.is_empty() {
        return Err("at least one HTTP principal is required".into());
    }
    let mut names = HashSet::new();
    for principal in &options.principals {
        if !names.insert(principal.name()) {
            return Err(format!("duplicate HTTP principal: {}", principal.name()).into());
        }
    }
    Ok(bind)
}

#[derive(Clone)]
struct RouteSecurity {
    principals: Arc<Vec<HttpPrincipal>>,
    allow_query: bool,
    session_affinity: HttpSessionAffinity,
    concurrency: Arc<Semaphore>,
    rate_limiter: Arc<RateLimiter>,
}

async fn enforce_route_security(
    State(security): State<Arc<RouteSecurity>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    request: Request,
    next: Next,
) -> Response {
    let path = request.uri().path();
    if path != "/mcp" && !path.starts_with("/mcp/") {
        return next.run(request).await;
    }
    let started = Instant::now();
    if !security.rate_limiter.allow(peer.ip()).await {
        return retry_response(StatusCode::TOO_MANY_REQUESTS, "request rate limit exceeded\n");
    }
    let Ok(permit) = security.concurrency.clone().try_acquire_owned() else {
        return retry_response(StatusCode::SERVICE_UNAVAILABLE, "too many concurrent requests\n");
    };

    let Some(principal) =
        authenticate_request(&request, &security.principals, security.allow_query)
    else {
        tokio::time::sleep(INVALID_AUTH_DELAY).await;
        tracing::warn!(
            peer = %peer,
            path = %request.uri().path(),
            "rejected request with missing or invalid HTTP token"
        );
        return (StatusCode::UNAUTHORIZED, "missing or invalid token\n").into_response();
    };

    let principal_name = principal.name().to_string();
    let method = request.method().clone();
    let correlation = RequestCorrelation::new();
    let mut request = request;
    request.extensions_mut().insert(principal);
    request.extensions_mut().insert(security.session_affinity);
    request.extensions_mut().insert(correlation.clone());
    let response = next.run(request).await;
    drop(permit);
    // One structured `winx::usage` event per authenticated request. Metadata
    // only — the body (commands, file contents) is never logged.
    tracing::info!(
        target: "winx::usage",
        event = "http_request",
        principal = %principal_name,
        request_id = %correlation.as_str(),
        peer = %peer,
        method = %method,
        status = response.status().as_u16(),
        duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        "http request"
    );
    response
}

fn retry_response(status: StatusCode, body: &'static str) -> Response {
    let mut response = (status, body).into_response();
    response.headers_mut().insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
    response
}

#[derive(Debug)]
struct RateWindow {
    started_at: Instant,
    last_seen: Instant,
    count: u32,
}

#[derive(Debug)]
struct RateLimiter {
    requests_per_window: u32,
    windows: Mutex<HashMap<IpAddr, RateWindow>>,
}

impl RateLimiter {
    fn new(requests_per_window: u32) -> Self {
        Self { requests_per_window, windows: Mutex::new(HashMap::new()) }
    }

    async fn allow(&self, peer: IpAddr) -> bool {
        let now = Instant::now();
        let mut windows = self.windows.lock().await;
        windows.retain(|_, window| now.duration_since(window.last_seen) < RATE_WINDOW * 2);
        if !windows.contains_key(&peer) && windows.len() >= MAX_RATE_TRACKED_IPS {
            if let Some(oldest) =
                windows.iter().min_by_key(|(_, window)| window.last_seen).map(|(ip, _)| *ip)
            {
                windows.remove(&oldest);
            }
        }
        let window =
            windows.entry(peer).or_insert(RateWindow { started_at: now, last_seen: now, count: 0 });
        if now.duration_since(window.started_at) >= RATE_WINDOW {
            window.started_at = now;
            window.count = 0;
        }
        window.last_seen = now;
        if window.count >= self.requests_per_window {
            return false;
        }
        window.count = window.count.saturating_add(1);
        true
    }
}

fn authenticate_request(
    request: &Request,
    principals: &[HttpPrincipal],
    allow_query: bool,
) -> Option<HttpPrincipal> {
    let header = bearer_token(request);
    let query = allow_query.then(|| query_token(request)).flatten();
    [header, query].into_iter().flatten().find_map(|presented| {
        principals.iter().find(|principal| principal.matches_token(presented)).cloned()
    })
}

fn bearer_token(request: &Request) -> Option<&str> {
    let value = request.headers().get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = value.split_once(' ')?;
    scheme.eq_ignore_ascii_case("bearer").then_some(token.trim())
}

fn query_token(request: &Request) -> Option<&str> {
    request.uri().query()?.split('&').find_map(|pair| pair.strip_prefix("token="))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::net::{IpAddr, Ipv4Addr};

    use axum::body::Body;
    use axum::extract::Request;

    use super::{authenticate_request, validate_options, HttpServerOptions, RateLimiter};
    use crate::config::HttpPrincipal;

    fn request(uri: &str, authorization: Option<&str>) -> Request {
        let mut builder = Request::builder().uri(uri);
        if let Some(authorization) = authorization {
            builder = builder.header(axum::http::header::AUTHORIZATION, authorization);
        }
        builder.body(Body::empty()).expect("test request")
    }

    fn principal(name: &str, token_byte: char) -> HttpPrincipal {
        let token = token_byte.to_string().repeat(32);
        HttpPrincipal::new(name, &token, false).expect("valid principal")
    }

    fn strong_options(bind: &str) -> HttpServerOptions {
        HttpServerOptions {
            bind: bind.to_string(),
            principals: vec![principal("default", 'x')],
            extra_hosts: Vec::new(),
            allow_query_token: false,
            allow_non_loopback: false,
            session_affinity: crate::config::HttpSessionAffinity::Workspace,
            max_concurrency: super::DEFAULT_MAX_CONCURRENCY,
            requests_per_minute: super::DEFAULT_REQUESTS_PER_MINUTE,
        }
    }

    #[test]
    fn bearer_and_opt_in_query_tokens_select_the_matching_principal() {
        let principals = vec![principal("chatgpt", 'a'), principal("automation", 'b')];
        let header = request("/mcp", Some(&format!("Bearer {}", "a".repeat(32))));
        let matched = authenticate_request(&header, &principals, false).expect("header principal");
        assert_eq!(matched.name(), "chatgpt");

        let query_uri = format!("/mcp?token={}", "b".repeat(32));
        assert!(authenticate_request(&request(&query_uri, None), &principals, false).is_none());
        let matched = authenticate_request(&request(&query_uri, None), &principals, true)
            .expect("query principal");
        assert_eq!(matched.name(), "automation");
    }

    #[test]
    fn non_loopback_binding_requires_explicit_override() {
        assert!(validate_options(&strong_options("127.0.0.1:8000")).is_ok());
        assert!(validate_options(&strong_options("0.0.0.0:8000")).is_err());

        let mut public = strong_options("0.0.0.0:8000");
        public.allow_non_loopback = true;
        assert!(validate_options(&public).is_ok());
    }

    #[tokio::test]
    async fn rate_limiter_rejects_excess_requests() {
        let limiter = RateLimiter::new(2);
        let peer = IpAddr::V4(Ipv4Addr::LOCALHOST);
        assert!(limiter.allow(peer).await);
        assert!(limiter.allow(peer).await);
        assert!(!limiter.allow(peer).await);
    }

    #[test]
    fn duplicate_principal_names_are_rejected() {
        let mut options = strong_options("127.0.0.1:8000");
        options.principals = vec![principal("same", 'a'), principal("same", 'b')];
        assert!(validate_options(&options).is_err());
    }
}
