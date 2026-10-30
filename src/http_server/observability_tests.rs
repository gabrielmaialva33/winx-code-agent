//! Regression tests for the observable HTTP response lifecycle.

#![allow(clippy::expect_used)]

use std::collections::VecDeque;
use std::future::Future as _;
use std::net::{Ipv4Addr, SocketAddr};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

use axum::body::{Body, Bytes};
use axum::extract::{ConnectInfo, Request};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::middleware;
use axum::response::Response;
use axum::routing::any;
use axum::Router;
use http_body::{Body as _, Frame, SizeHint};
use serde_json::Value;
use tokio::sync::Semaphore;
use tower::Service as _;

use super::{
    enforce_route_security, HttpPrincipal, HttpSessionAffinity, LocalSessionManager, RateLimiter,
    RequestCorrelation, RouteSecurity, SessionOwners,
};

const TOKEN: &str = "SECRET-HTTP-TOKEN-0123456789abcdef";
const RESPONSE_SECRET: &str = "SECRET-RESPONSE-BODY";
const ERROR_SECRET: &str = "SECRET-BODY-ERROR";

#[derive(Clone, Default)]
struct UsageCapture(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for UsageCapture {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .map_err(|error| std::io::Error::other(error.to_string()))?
            .extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl UsageCapture {
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().expect("capture lock").clone()).expect("UTF-8 log")
    }

    fn events(&self) -> Vec<Value> {
        self.text()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).expect("JSON log event"))
            .filter(|event| event["target"] == "winx::usage")
            .map(|event| event["fields"].clone())
            .collect()
    }

    fn named(&self, name: &str) -> Vec<Value> {
        self.events().into_iter().filter(|event| event["event"] == name).collect()
    }
}

fn capture_usage() -> (UsageCapture, tracing::dispatcher::DefaultGuard) {
    let capture = UsageCapture::default();
    let writer = capture.clone();
    let subscriber = tracing_subscriber::fmt()
        .json()
        .without_time()
        .with_writer(move || writer.clone())
        .finish();
    (capture, tracing::subscriber::set_default(subscriber))
}

fn security() -> Arc<RouteSecurity> {
    Arc::new(RouteSecurity {
        principals: Arc::new(vec![
            HttpPrincipal::new("test-principal", TOKEN, false).expect("valid principal")
        ]),
        allow_query: false,
        session_affinity: HttpSessionAffinity::Workspace,
        concurrency: Arc::new(Semaphore::new(1)),
        auth_failure_limiter: Arc::new(RateLimiter::new(100)),
        principal_rate_limiter: Arc::new(RateLimiter::new(100)),
        session_owners: Arc::new(SessionOwners::default()),
        session_manager: Arc::new(LocalSessionManager::default()),
    })
}

fn request() -> Request {
    let mut request = Request::builder()
        .method("POST")
        .uri("/mcp?token=SECRET-QUERY-TOKEN")
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .header("mcp-session-id", "SECRET-SESSION-ID")
        .header("x-request-id", "UNTRUSTED-REQUEST-ID")
        .body(Body::from("SECRET-REQUEST-BODY"))
        .expect("test request");
    request.extensions_mut().insert(ConnectInfo(SocketAddr::from((Ipv4Addr::LOCALHOST, 12345))));
    request
}

fn router(security: Arc<RouteSecurity>, response: Response) -> Router {
    let response = Arc::new(Mutex::new(Some(response)));
    Router::new()
        .route(
            "/mcp",
            any(move |request: Request| {
                let response = response.clone();
                async move {
                    let mut response = response
                        .lock()
                        .expect("response lock")
                        .take()
                        .expect("one request per fixture");
                    let correlation = request
                        .extensions()
                        .get::<RequestCorrelation>()
                        .expect("middleware correlation reaches the handler");
                    response.headers_mut().insert(
                        "x-observed-request-id",
                        HeaderValue::from_str(correlation.as_str()).expect("opaque request id"),
                    );
                    response
                }
            }),
        )
        .layer(middleware::from_fn_with_state(security, enforce_route_security))
}

fn poll_body(body: &mut Body) -> Poll<Option<Result<Frame<Bytes>, axum::Error>>> {
    Pin::new(body).poll_frame(&mut Context::from_waker(Waker::noop()))
}

fn assert_terminal(capture: &UsageCapture, outcome: &str) {
    let events = capture.events();
    let names: Vec<_> = events.iter().filter_map(|event| event["event"].as_str()).collect();
    assert_eq!(names, ["http_request_received", "http_request", "http_response_body"]);
    assert_eq!(events[2]["outcome"], outcome);
    let request_id = events[0]["request_id"].as_str().expect("entry request id");
    assert_ne!(request_id, "");
    assert_ne!(request_id, "UNTRUSTED-REQUEST-ID");
    assert_eq!(events[1]["request_id"], request_id);
    assert_eq!(events[2]["request_id"], request_id);
    let ready_ms = events[1]["duration_ms"].as_u64().expect("legacy ready duration");
    let total_ms = events[2]["request_elapsed_ms"].as_u64().expect("server elapsed duration");
    let body_ms = events[2]["body_duration_ms"].as_u64().expect("body elapsed duration");
    assert!(total_ms >= ready_ms);
    assert!(total_ms >= body_ms);
    assert!(events[2].get("duration_ms").is_none(), "do not give the old field a new meaning");
    for secret in [
        TOKEN,
        RESPONSE_SECRET,
        ERROR_SECRET,
        "SECRET-QUERY-TOKEN",
        "SECRET-SESSION-ID",
        "SECRET-REQUEST-BODY",
        "SECRET-TRAILER-VALUE",
        "UNTRUSTED-REQUEST-ID",
    ] {
        assert!(!capture.text().contains(secret), "sensitive value logged: {secret}");
    }
}

/// Real `Body` fixtures for conservative EOF hints, trailers, errors, and a
/// stream that remains pending. No network or timer is involved.
enum BodyStep {
    Trailers(HeaderMap),
    Error,
    Pending,
}

struct ScriptedBody(VecDeque<BodyStep>);

impl http_body::Body for ScriptedBody {
    type Data = Bytes;
    type Error = std::io::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let this = self.get_mut();
        if matches!(this.0.front(), Some(BodyStep::Pending)) {
            return Poll::Pending;
        }
        match this.0.pop_front() {
            Some(BodyStep::Trailers(trailers)) => Poll::Ready(Some(Ok(Frame::trailers(trailers)))),
            Some(BodyStep::Error) => Poll::Ready(Some(Err(std::io::Error::other(ERROR_SECRET)))),
            Some(BodyStep::Pending) => unreachable!("pending steps are retained above"),
            None => Poll::Ready(None),
        }
    }

    fn size_hint(&self) -> SizeHint {
        // Zero data bytes does not imply that there are no trailer frames.
        SizeHint::with_exact(0)
    }
}

fn scripted(step: BodyStep) -> Body {
    Body::new(ScriptedBody(VecDeque::from([step])))
}

#[tokio::test(flavor = "current_thread")]
async fn received_is_logged_before_waiting_for_admission() {
    let (capture, _guard) = capture_usage();
    let security = security();
    let admission_lock = security.principal_rate_limiter.windows.lock().await;
    let mut router = router(security.clone(), Response::new(Body::empty()));
    let mut call = Box::pin(router.call(request()));
    assert!(call.as_mut().poll(&mut Context::from_waker(Waker::noop())).is_pending());
    let received = capture.named("http_request_received");
    assert_eq!(received.len(), 1, "entry must be visible while admission is waiting");
    assert!(capture.named("http_request").is_empty(), "no response is ready yet");
    drop(admission_lock);
    drop(call.await.expect("response"));
    assert_terminal(&capture, "complete");
}

#[tokio::test(flavor = "current_thread")]
async fn response_ready_precedes_stream_drop_and_keeps_its_duration() {
    let (capture, _guard) = capture_usage();
    let security = security();
    let mut response = Response::new(scripted(BodyStep::Pending));
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static("text/event-stream"));
    let mut router = router(security.clone(), response);
    let response = router.call(request()).await.expect("response");
    let ready = capture.named("http_request");
    assert_eq!(ready.len(), 1, "the legacy event remains a response-ready observation");
    assert_eq!(
        ready[0]["request_id"],
        response.headers()["x-observed-request-id"].to_str().expect("id")
    );
    assert!(capture.named("http_response_body").is_empty(), "an open stream has not finished");
    let mut body = response.into_body();
    for _ in 0..3 {
        assert!(poll_body(&mut body).is_pending());
    }
    assert!(capture.named("http_response_body").is_empty(), "pending polls must not log terminals");
    assert_eq!(security.concurrency.available_permits(), 0);
    drop(body);
    assert_eq!(security.concurrency.available_permits(), 1);
    assert_eq!(capture.named("http_request"), ready, "do not overwrite or repeat the ready event");
    assert_terminal(&capture, "dropped");
}

#[tokio::test(flavor = "current_thread")]
async fn empty_response_completes_without_any_body_poll() {
    let (capture, _guard) = capture_usage();
    let security = security();
    let mut router = router(security.clone(), Response::new(Body::empty()));
    let response = router.call(request()).await.expect("response");
    assert_terminal(&capture, "complete");
    assert_eq!(
        security.concurrency.available_permits(),
        0,
        "observation does not release the permit"
    );
    drop(response);
    assert_eq!(security.concurrency.available_permits(), 1);
    assert_terminal(&capture, "complete");
}

#[tokio::test(flavor = "current_thread")]
async fn last_data_frame_completes_without_a_followup_eof_poll() -> anyhow::Result<()> {
    let (capture, _guard) = capture_usage();
    let security = security();
    let mut router = router(security.clone(), Response::new(Body::from(RESPONSE_SECRET)));
    let response = router.call(request()).await.expect("response");
    assert_eq!(capture.named("http_response_body"), Vec::<Value>::new());
    let mut body = response.into_body();
    let Poll::Ready(Some(Ok(frame))) = poll_body(&mut body) else {
        anyhow::bail!("expected the original data frame");
    };
    assert_eq!(frame.data_ref().expect("data frame").as_ref(), RESPONSE_SECRET.as_bytes());
    assert_terminal(&capture, "complete");
    assert_eq!(security.concurrency.available_permits(), 0, "permit lives until body drop");
    drop(body);
    assert_eq!(security.concurrency.available_permits(), 1);
    assert_terminal(&capture, "complete");
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
async fn zero_byte_trailers_are_forwarded_before_explicit_eof() -> anyhow::Result<()> {
    let (capture, _guard) = capture_usage();
    let mut trailers = HeaderMap::new();
    trailers.insert("x-private-trailer", HeaderValue::from_static("SECRET-TRAILER-VALUE"));
    let mut router = router(security(), Response::new(scripted(BodyStep::Trailers(trailers))));
    let mut body = router.call(request()).await.expect("response").into_body();
    assert_eq!(body.size_hint().exact(), Some(0));
    assert!(capture.named("http_response_body").is_empty(), "zero bytes is not EOF");
    let Poll::Ready(Some(Ok(frame))) = poll_body(&mut body) else {
        anyhow::bail!("expected the original trailers");
    };
    assert_eq!(
        frame.trailers_ref().expect("trailers")["x-private-trailer"],
        "SECRET-TRAILER-VALUE"
    );
    assert!(
        capture.named("http_response_body").is_empty(),
        "the fixture's EOF hint is conservative"
    );
    assert!(matches!(poll_body(&mut body), Poll::Ready(None)));
    assert_terminal(&capture, "complete");
    assert!(matches!(poll_body(&mut body), Poll::Ready(None)));
    drop(body);
    assert_terminal(&capture, "complete");
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
async fn body_error_is_forwarded_once_without_logging_its_text() -> anyhow::Result<()> {
    let (capture, _guard) = capture_usage();
    let security = security();
    let mut router = router(security.clone(), Response::new(scripted(BodyStep::Error)));
    let mut body = router.call(request()).await.expect("response").into_body();
    let Poll::Ready(Some(Err(error))) = poll_body(&mut body) else {
        anyhow::bail!("expected the original body error");
    };
    assert_eq!(error.to_string(), ERROR_SECRET);
    assert_terminal(&capture, "error");
    assert_eq!(
        security.concurrency.available_permits(),
        0,
        "error observation does not release the permit"
    );
    drop(body);
    assert_eq!(security.concurrency.available_permits(), 1);
    assert_terminal(&capture, "error");
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
async fn admission_rejection_correlates_entry_without_a_ready_or_body_event() {
    let (capture, _guard) = capture_usage();
    let security = security();
    let held = security.concurrency.clone().try_acquire_owned().expect("occupy admission slot");
    let mut router = router(security.clone(), Response::new(Body::empty()));
    let response = router.call(request()).await.expect("rejection response");
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(response.headers()[header::RETRY_AFTER], "1");
    drop(response);
    let events = capture.events();
    let names: Vec<_> = events.iter().filter_map(|event| event["event"].as_str()).collect();
    assert_eq!(names, ["http_request_received", "http_rejected"]);
    let request_id = events[0]["request_id"].as_str().expect("entry request id");
    assert_ne!(request_id, "");
    assert_eq!(events[1]["request_id"], request_id);
    assert_eq!(events[1]["reason"], "concurrency");
    assert_eq!(events[1]["status"], 503);
    assert!(!capture.text().contains(TOKEN));
    assert_eq!(security.concurrency.available_permits(), 0);
    drop(held);
}
