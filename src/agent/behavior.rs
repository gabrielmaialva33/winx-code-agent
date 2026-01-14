//! Adaptive Behavior System.
//!
//! Modifies how the agent acts based on its Pulse and context.

use serde::{Deserialize, Serialize};

/// Behavior modifiers.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BehaviorMod {
    pub be_more_careful: bool,
    pub explain_more: bool,
    pub ask_confirmation: bool,
    pub suggest_break: bool,
}

impl BehaviorMod {
    pub fn new() -> Self {
        Self::default()
    }
}
