//! Metadata-only observations at the HTTP middleware and response-body boundaries.
//!
//! Completion means the body yielded EOF or reported its end-of-stream hint.
//! The observation ends at this interface; network delivery is outside it.

use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Instant;

use http_body::{Body, Frame, SizeHint};
use tokio::sync::OwnedSemaphorePermit;

use super::RequestCorrelation;

/// Observe MCP middleware entry before admission, using only bounded metadata.
pub(super) fn observe_request_entry(
    peer: SocketAddr,
    request_method: &axum::http::Method,
) -> (Instant, axum::http::Method, RequestCorrelation) {
    let started = Instant::now();
    let method = request_method.clone();
    let correlation = RequestCorrelation::new();
    let logged_method = match method.as_str() {
        "GET" | "HEAD" | "POST" | "PUT" | "DELETE" | "CONNECT" | "OPTIONS" | "TRACE" | "PATCH" => {
            method.as_str()
        }
        _ => "OTHER",
    };
    tracing::info!(
        target: "winx::usage",
        event = "http_request_received",
        request_id = %correlation.as_str(),
        peer = %peer,
        method = logged_method,
        route = "/mcp",
        "http request received"
    );
    (started, method, correlation)
}

/// Keeps the admission permit until this wrapper is dropped, including while
/// a streaming response is open. Observation never releases the permit early.
pub(super) struct PermitBody<B: Body> {
    inner: Pin<Box<B>>,
    _permit: OwnedSemaphorePermit,
    observation: Option<BodyObservation>,
}

impl<B: Body> PermitBody<B> {
    pub(super) fn new(inner: B, permit: OwnedSemaphorePermit) -> Self {
        Self { inner: Box::pin(inner), _permit: permit, observation: None }
    }

    /// Attach after the response-ready event so even an already-empty body
    /// produces observations in entry, response-ready, body-terminal order.
    pub(super) fn observe(
        mut self,
        correlation: RequestCorrelation,
        request_started: Instant,
        response_ready: Instant,
    ) -> Self {
        self.observation =
            Some(BodyObservation { correlation, request_started, response_ready, finished: false });
        if self.inner.is_end_stream() {
            self.finish(BodyOutcome::Complete);
        }
        self
    }

    fn finish(&mut self, outcome: BodyOutcome) {
        if let Some(observation) = self.observation.as_mut() {
            observation.finish(outcome);
        }
    }
}

impl<B: Body> Body for PermitBody<B> {
    type Data = B::Data;
    type Error = B::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        // The box keeps B pinned; the wrapper's own fields are all Unpin.
        let this = self.get_mut();
        let frame = this.inner.as_mut().poll_frame(cx);
        match &frame {
            Poll::Ready(None) => this.finish(BodyOutcome::Complete),
            Poll::Ready(Some(Err(_))) => this.finish(BodyOutcome::Error),
            // Consumers may stop polling after the final frame when this hint
            // is true, so observing only an explicit None misses valid EOFs.
            Poll::Ready(Some(Ok(_))) if this.inner.is_end_stream() => {
                this.finish(BodyOutcome::Complete);
            }
            _ => {}
        }
        frame
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

impl<B: Body> Drop for PermitBody<B> {
    fn drop(&mut self) {
        if self.observation.as_ref().is_none_or(|observation| observation.finished) {
            return;
        }
        let outcome = if self.inner.is_end_stream() {
            BodyOutcome::Complete
        } else {
            // A drop records an unobserved EOF. It does not identify why the
            // consumer discarded the body; open SSE streams also take this path.
            BodyOutcome::Dropped
        };
        self.finish(outcome);
    }
}

#[derive(Clone, Copy)]
enum BodyOutcome {
    Complete,
    Error,
    Dropped,
}

impl BodyOutcome {
    fn as_str(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Error => "error",
            Self::Dropped => "dropped",
        }
    }
}

struct BodyObservation {
    correlation: RequestCorrelation,
    request_started: Instant,
    response_ready: Instant,
    finished: bool,
}

impl BodyObservation {
    fn finish(&mut self, outcome: BodyOutcome) {
        if self.finished {
            return;
        }
        self.finished = true;
        let ended = Instant::now();
        tracing::info!(
            target: "winx::usage",
            event = "http_response_body",
            request_id = %self.correlation.as_str(),
            outcome = outcome.as_str(),
            request_elapsed_ms = elapsed_ms(self.request_started, ended),
            body_duration_ms = elapsed_ms(self.response_ready, ended),
            "http response body observation"
        );
    }
}

fn elapsed_ms(started: Instant, ended: Instant) -> u64 {
    u64::try_from(ended.saturating_duration_since(started).as_millis()).unwrap_or(u64::MAX)
}
