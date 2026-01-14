//! Consciousness Loop.
//!
//! Orchestrates the Sense-Recall-Think-Act cycle.

use super::{behavior::BehaviorMod, pulse::WinxPulse};

/// Main consciousness controller.
pub struct Consciousness {
    pub pulse: WinxPulse,
    pub behavior: BehaviorMod,
}

impl Consciousness {
    pub fn new() -> Self {
        Self {
            pulse: WinxPulse::new(),
            behavior: BehaviorMod::new(),
        }
    }

    /// Runs a single cycle of consciousness.
    pub fn cycle(&mut self) {
        // TODO: Implement the full loop
        self.pulse.update();
        // self.behavior = self.pulse.behavior_modifier();
    }
}

impl Default for Consciousness {
    fn default() -> Self {
        Self::new()
    }
}
