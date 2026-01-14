pub mod behavior;
pub mod consciousness;
pub mod identity;
pub mod memory;
pub mod pulse;
pub mod sense;

pub use behavior::BehaviorMod;
pub use consciousness::Consciousness;
pub use identity::WinxIdentity;
pub use memory::AgentMemory;
pub use pulse::WinxPulse;
pub use sense::SenseSystem;

/// Global agent state.
pub struct AgentState {
    pub identity: WinxIdentity,
    pub sense: SenseSystem,
    pub memory: AgentMemory,
    pub consciousness: Consciousness,
}

impl AgentState {
    pub fn new() -> Self {
        let mut sense = SenseSystem::new();
        sense.scan_all();

        Self {
            identity: WinxIdentity::new(),
            sense,
            memory: AgentMemory::new(),
            consciousness: Consciousness::new(),
        }
    }
}

impl Default for AgentState {
    fn default() -> Self {
        Self::new()
    }
}