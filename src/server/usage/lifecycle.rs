//! Bounded, inclusive request phase observations. A missing terminal means that
//! completion was not observed; it is not evidence that an operation is still live.

use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::UsageEvent;

const MAX_TRACE_PAIRS: u64 = 16;
const PHASE_COUNT: usize = 10;

tokio::task_local! {
    static CURRENT: UsageTrace;
}

#[derive(Clone, Copy)]
#[repr(usize)]
pub(in crate::server) enum Phase {
    WorkspaceCheck,
    SessionAdmission,
    Catalog,
    RuntimeInspect,
    RuntimeCapture,
    PromptRpc,
    HerdrAdapter,
    WaitIdle,
    TempAudit,
    ResponseFinalize,
}

impl Phase {
    fn name(self) -> &'static str {
        match self {
            Self::WorkspaceCheck => "workspace_check",
            Self::SessionAdmission => "session_admission",
            Self::Catalog => "catalog",
            Self::RuntimeInspect => "runtime_inspect",
            Self::RuntimeCapture => "runtime_capture",
            Self::PromptRpc => "prompt_rpc",
            Self::HerdrAdapter => "herdr_adapter",
            Self::WaitIdle => "wait_idle",
            Self::TempAudit => "temp_audit",
            Self::ResponseFinalize => "response_finalize",
        }
    }
}

#[derive(Clone, Copy, Default)]
struct PhaseTotal {
    duration: Duration,
    calls: u64,
}

#[derive(Clone, Default)]
pub(super) struct TraceSummary {
    phases: [PhaseTotal; PHASE_COUNT],
    pub abandoned_count: u64,
    pub suppressed_count: u64,
}

impl TraceSummary {
    pub fn milliseconds(&self, phase: Phase) -> Option<u64> {
        let total = self.phases[phase as usize];
        (total.calls > 0).then(|| milliseconds(total.duration))
    }

    pub fn calls(&self, phase: Phase) -> Option<u64> {
        let calls = self.phases[phase as usize].calls;
        (calls > 0).then_some(calls)
    }
}

#[derive(Default)]
struct Stats {
    summary: TraceSummary,
    emitted_pairs: u64,
    next_phase_id: u64,
}

struct Context {
    call_id: String,
    request_id: String,
    tool: String,
    action: &'static str,
    backend: Option<&'static str>,
    principal: String,
    build: String,
    build_version: String,
    build_revision: String,
    build_dirty: bool,
}

struct Trace {
    context: Context,
    clock: Clock,
    stats: Mutex<Stats>,
}

#[derive(Clone)]
enum Clock {
    Monotonic(Instant),
    #[cfg(test)]
    Manual(Arc<std::sync::atomic::AtomicU64>),
}

impl Clock {
    fn now(&self) -> Duration {
        match self {
            Self::Monotonic(started) => started.elapsed(),
            #[cfg(test)]
            Self::Manual(ticks) => {
                Duration::from_nanos(ticks.load(std::sync::atomic::Ordering::SeqCst))
            }
        }
    }
}

#[derive(Clone, Default)]
pub(in crate::server) struct UsageTrace(Option<Arc<Trace>>);

impl UsageTrace {
    pub(super) fn start(usage: &UsageEvent) -> Self {
        let context = Context {
            call_id: new_call_id(),
            request_id: usage.request_id.clone(),
            tool: usage.tool.clone(),
            action: finite_action(&usage.action),
            backend: usage.workflow.map(|workflow| workflow.backend),
            principal: usage.principal.clone(),
            build: usage.build.display.clone(),
            build_version: usage.build.package_version.clone(),
            build_revision: usage.build.revision.clone(),
            build_dirty: usage.build.dirty,
        };
        Self::with_context(context, Clock::Monotonic(Instant::now()))
    }

    fn with_context(context: Context, clock: Clock) -> Self {
        let trace =
            Self(Some(Arc::new(Trace { context, clock, stats: Mutex::new(Stats::default()) })));
        if let Some(inner) = trace.0.as_ref() {
            inner.emit("tool_started", None, None, None, None, None);
        }
        trace
    }

    #[cfg(test)]
    pub(in crate::server) fn for_test(tool: &str, action: &str) -> Self {
        Self::with_context(
            Context {
                call_id: new_call_id(),
                request_id: "test-request".into(),
                tool: tool.into(),
                action: finite_action(action),
                backend: Some("native"),
                principal: "test".into(),
                build: "test".into(),
                build_version: "test".into(),
                build_revision: "test".into(),
                build_dirty: false,
            },
            Clock::Monotonic(Instant::now()),
        )
    }

    #[cfg(test)]
    pub(in crate::server) fn test_calls(&self, phase: Phase) -> Option<u64> {
        self.summary().calls(phase)
    }

    pub async fn scope<F: Future>(&self, operation: F) -> F::Output {
        CURRENT.scope(self.clone(), operation).await
    }

    pub fn phase(&self, phase: Phase) -> PhaseGuard {
        let Some(trace) = self.0.as_ref() else { return PhaseGuard::default() };
        let (phase_id, emitted, first_suppressed) = {
            let mut stats = trace.stats.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            stats.next_phase_id = stats.next_phase_id.saturating_add(1);
            let emitted = stats.emitted_pairs < MAX_TRACE_PAIRS;
            let mut first_suppressed = false;
            if emitted {
                stats.emitted_pairs += 1;
            } else {
                stats.summary.suppressed_count = stats.summary.suppressed_count.saturating_add(1);
                first_suppressed = stats.summary.suppressed_count == 1;
            }
            (stats.next_phase_id, emitted, first_suppressed)
        };
        let started = trace.clock.now();
        if emitted {
            trace.emit("tool_phase", Some(phase), Some("start"), Some(phase_id), None, None);
        } else if first_suppressed {
            trace.emit("tool_phase_trace_truncated", None, None, None, None, None);
        }
        PhaseGuard {
            trace: Some(trace.clone()),
            phase,
            phase_id,
            started,
            emitted,
            finished: false,
        }
    }

    pub(super) fn summary(&self) -> TraceSummary {
        self.0.as_ref().map_or_else(TraceSummary::default, |trace| {
            trace.stats.lock().unwrap_or_else(std::sync::PoisonError::into_inner).summary.clone()
        })
    }

    pub(super) fn call_id(&self) -> Option<&str> {
        self.0.as_ref().map(|trace| trace.context.call_id.as_str())
    }

    pub(super) fn schema(&self) -> Option<u64> {
        self.0.as_ref().map(|_| 1)
    }
}

impl Trace {
    fn emit(
        &self,
        event: &'static str,
        phase: Option<Phase>,
        edge: Option<&'static str>,
        phase_id: Option<u64>,
        duration: Option<u64>,
        outcome: Option<&'static str>,
    ) {
        let context = &self.context;
        tracing::info!(target:"winx::usage", event, usage_schema=1_u64, lifecycle_schema=1_u64,
            usage_call_id=%context.call_id, request_id=%context.request_id, tool=%context.tool,
            action=context.action, workflow_backend=context.backend, principal=%context.principal,
            build=%context.build, build_version=%context.build_version,
            build_revision=%context.build_revision, build_dirty=context.build_dirty,
            phase=phase.map(Phase::name), phase_edge=edge, phase_id,
            phase_duration_ms=duration, phase_outcome=outcome, "tool lifecycle");
    }
}

pub(in crate::server) struct PhaseGuard {
    trace: Option<Arc<Trace>>,
    phase: Phase,
    phase_id: u64,
    started: Duration,
    emitted: bool,
    finished: bool,
}

impl Default for PhaseGuard {
    fn default() -> Self {
        Self {
            trace: None,
            phase: Phase::Catalog,
            phase_id: 0,
            started: Duration::ZERO,
            emitted: false,
            finished: false,
        }
    }
}

impl PhaseGuard {
    pub fn finish(mut self, completed: bool) {
        self.record(if completed { "completed" } else { "error" });
    }

    fn record(&mut self, outcome: &'static str) {
        if self.finished {
            return;
        }
        self.finished = true;
        let Some(trace) = self.trace.as_ref() else { return };
        let duration = trace.clock.now().saturating_sub(self.started);
        {
            let mut stats = trace.stats.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let total = &mut stats.summary.phases[self.phase as usize];
            total.calls = total.calls.saturating_add(1);
            total.duration = total.duration.saturating_add(duration);
            if outcome == "abandoned" {
                stats.summary.abandoned_count = stats.summary.abandoned_count.saturating_add(1);
            }
        }
        if self.emitted {
            trace.emit(
                "tool_phase",
                Some(self.phase),
                Some("finish"),
                Some(self.phase_id),
                Some(milliseconds(duration)),
                Some(outcome),
            );
        }
    }
}

impl Drop for PhaseGuard {
    fn drop(&mut self) {
        self.record("abandoned");
    }
}

/// Background tasks do not inherit this request-local trace.
pub(in crate::server) fn phase(phase: Phase) -> PhaseGuard {
    CURRENT.try_with(|trace| trace.phase(phase)).unwrap_or_default()
}

fn new_call_id() -> String {
    format!("{:032x}", u128::from_ne_bytes(rand::random::<[u8; 16]>()))
}

fn milliseconds(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn finite_action(action: &str) -> &'static str {
    match action {
        "list" => "list",
        "get" => "get",
        "read" => "read",
        "explain" => "explain",
        "wait" => "wait",
        "prompt" => "prompt",
        "command" => "command",
        "status_check" => "status_check",
        "send_text" => "send_text",
        "send_specials" => "send_specials",
        "send_ascii" => "send_ascii",
        "screen" => "screen",
        "wait_for_turn" => "wait_for_turn",
        "inspect" => "inspect",
        "verify" => "verify",
        "verification" => "verification",
        _ => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use serde_json::Value;

    use super::*;

    #[derive(Clone)]
    struct Capture(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for Capture {
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

    fn capture<T>(operation: impl FnOnce() -> T) -> anyhow::Result<(T, Vec<Value>)> {
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let writer = bytes.clone();
        let subscriber = tracing_subscriber::fmt()
            .json()
            .without_time()
            .with_writer(move || Capture(writer.clone()))
            .finish();
        let value = tracing::subscriber::with_default(subscriber, operation);
        let bytes = bytes.lock().map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let events = bytes
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(serde_json::from_slice::<Value>)
            .collect::<Result<Vec<_>, _>>()?;
        Ok((value, events))
    }

    fn trace(ticks: &Arc<AtomicU64>) -> UsageTrace {
        UsageTrace::with_context(
            Context {
                call_id: new_call_id(),
                request_id: "reused-request-id".into(),
                tool: "Agent".into(),
                action: "wait",
                backend: Some("native"),
                principal: "test-principal".into(),
                build: "1+abc".into(),
                build_version: "1".into(),
                build_revision: "abc".into(),
                build_dirty: false,
            },
            Clock::Manual(ticks.clone()),
        )
    }

    #[test]
    fn lifecycle_guard_finish_and_drop_record_exactly_one_honest_outcome() -> anyhow::Result<()> {
        let ticks = Arc::new(AtomicU64::new(0));
        let (summary, events) = capture(|| {
            let trace = trace(&ticks);
            let completed = trace.phase(Phase::Catalog);
            ticks.fetch_add(600_000, Ordering::SeqCst);
            completed.finish(true);
            let failed = trace.phase(Phase::Catalog);
            ticks.fetch_add(600_000, Ordering::SeqCst);
            failed.finish(false);
            let abandoned = trace.phase(Phase::Catalog);
            ticks.fetch_add(600_000, Ordering::SeqCst);
            drop(abandoned);
            trace.summary()
        })?;
        assert_eq!(summary.calls(Phase::Catalog), Some(3));
        assert_eq!(summary.milliseconds(Phase::Catalog), Some(1));
        assert_eq!(summary.calls(Phase::WaitIdle), None);
        assert_eq!(summary.abandoned_count, 1);
        let outcomes = events
            .iter()
            .filter_map(|event| event["fields"]["phase_outcome"].as_str())
            .collect::<Vec<_>>();
        assert_eq!(outcomes, ["completed", "error", "abandoned"]);
        assert_eq!(
            events.iter().filter(|event| event["fields"]["phase_edge"] == "start").count(),
            3
        );
        assert_eq!(
            events.iter().filter(|event| event["fields"]["phase_edge"] == "finish").count(),
            3
        );
        Ok(())
    }

    #[test]
    fn lifecycle_cap_reserves_complete_pairs_and_summary_includes_suppressed_phases(
    ) -> anyhow::Result<()> {
        let ticks = Arc::new(AtomicU64::new(0));
        let (summary, events) = capture(|| {
            let trace = trace(&ticks);
            for _ in 0..1000 {
                let guard = trace.phase(Phase::Catalog);
                ticks.fetch_add(600_000, Ordering::SeqCst);
                guard.finish(true);
            }
            trace.summary()
        })?;
        assert_eq!(summary.calls(Phase::Catalog), Some(1000));
        assert_eq!(summary.milliseconds(Phase::Catalog), Some(600));
        assert_eq!(summary.suppressed_count, 984);
        assert_eq!(events.len(), 34);
        assert_eq!(
            events
                .iter()
                .filter(|event| event["fields"]["event"] == "tool_phase_trace_truncated")
                .count(),
            1
        );
        for id in 1..=16 {
            let edges = events
                .iter()
                .filter(|event| event["fields"]["phase_id"] == id)
                .map(|event| event["fields"]["phase_edge"].as_str())
                .collect::<Vec<_>>();
            assert_eq!(edges, [Some("start"), Some("finish")]);
        }
        Ok(())
    }

    #[test]
    fn lifecycle_call_ids_are_independent_of_reused_request_ids_and_actions_are_finite(
    ) -> anyhow::Result<()> {
        let ticks = Arc::new(AtomicU64::new(0));
        let ((first, second), events) = capture(|| {
            let first = trace(&ticks);
            let second = trace(&ticks);
            (first.call_id().map(str::to_string), second.call_id().map(str::to_string))
        })?;
        assert_ne!(first, second);
        for event in events {
            assert_eq!(event["fields"]["request_id"], "reused-request-id");
            assert_eq!(event["fields"]["usage_call_id"].as_str().map(str::len), Some(32));
            assert_eq!(event["fields"]["event"], "tool_started");
        }
        assert_eq!(finite_action("SECRET-ACTION"), "unknown");
        Ok(())
    }

    #[tokio::test]
    async fn lifecycle_task_local_does_not_escape_into_background_observers() -> anyhow::Result<()>
    {
        let ticks = Arc::new(AtomicU64::new(0));
        let trace = trace(&ticks);
        trace
            .scope(async {
                phase(Phase::Catalog).finish(true);
                tokio::spawn(async {
                    phase(Phase::Catalog).finish(true);
                })
                .await
            })
            .await?;
        assert_eq!(trace.summary().calls(Phase::Catalog), Some(1));
        phase(Phase::Catalog).finish(true);
        assert_eq!(trace.summary().calls(Phase::Catalog), Some(1));
        Ok(())
    }

    #[tokio::test]
    async fn lifecycle_cancelled_future_records_abandonment_without_a_terminal_claim(
    ) -> anyhow::Result<()> {
        let ticks = Arc::new(AtomicU64::new(0));
        let trace = trace(&ticks);
        let request_trace = trace.clone();
        let (started, observed) = tokio::sync::oneshot::channel();
        let request = tokio::spawn(async move {
            request_trace
                .scope(async {
                    let _guard = phase(Phase::Catalog);
                    let _ = started.send(());
                    std::future::pending::<()>().await;
                })
                .await;
        });
        observed.await?;
        ticks.store(1_500_000, Ordering::SeqCst);
        request.abort();
        assert!(request.await.is_err());
        assert_eq!(trace.summary().abandoned_count, 1);
        assert_eq!(trace.summary().calls(Phase::Catalog), Some(1));
        Ok(())
    }
}
