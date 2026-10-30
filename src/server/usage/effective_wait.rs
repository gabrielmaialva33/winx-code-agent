//! The handler's selected inline allowance, not elapsed wait or process completion.

use rmcp::model::JsonObject;
use serde_json::Value;

use crate::types::BashWaitPolicy;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::server) enum EffectiveWaitMode {
    Synchronous,
    TaskInline,
    Task,
}

#[derive(Clone, Copy)]
pub(in crate::server) struct EffectiveWait {
    pub policy: &'static str,
    pub milliseconds: u64,
    pub mode: &'static str,
}

impl EffectiveWait {
    pub fn from_normalized(
        arguments: Option<&JsonObject>,
        policy: BashWaitPolicy,
        mode: EffectiveWaitMode,
    ) -> Option<Self> {
        let (mode, milliseconds) = match mode {
            EffectiveWaitMode::Task if policy == BashWaitPolicy::UntilComplete => ("task", 0),
            EffectiveWaitMode::Task => return None,
            EffectiveWaitMode::TaskInline if policy != BashWaitPolicy::Adaptive => return None,
            mode => {
                // Read the value already normalized by the handler; no policy
                // defaults or caps are duplicated in telemetry.
                let seconds = arguments?.get("wait_for_seconds").and_then(Value::as_f64)?;
                let duration = std::time::Duration::try_from_secs_f64(seconds).ok()?;
                let milliseconds = u64::try_from(duration.as_millis()).ok()?;
                let label = match mode {
                    EffectiveWaitMode::TaskInline => "task_inline",
                    EffectiveWaitMode::Synchronous => "synchronous",
                    EffectiveWaitMode::Task => return None,
                };
                (label, milliseconds)
            }
        };
        let policy = match policy {
            BashWaitPolicy::Adaptive => "adaptive",
            BashWaitPolicy::ReturnEarly => "return_early",
            BashWaitPolicy::UntilComplete => "until_complete",
        };
        Some(Self { policy, milliseconds, mode })
    }

    /// An `Execution` wait's `timeout_seconds`, defaulting like the validator.
    pub fn execution_wait(arguments: Option<&JsonObject>) -> Option<Self> {
        let seconds = match arguments?.get("action")?.get("timeout_seconds") {
            None | Some(Value::Null) => 5.0,
            Some(value) => value.as_f64()?,
        };
        let milliseconds =
            u64::try_from(std::time::Duration::try_from_secs_f64(seconds).ok()?.as_millis())
                .ok()?;
        Some(Self { policy: "adaptive", milliseconds, mode: "synchronous" })
    }
}
