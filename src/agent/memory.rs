//! Winx Memory System
//!
//! Manages short-term (session) and long-term (semantic) memory.
//! Integrates with Qdrant for semantic retrieval.

use serde::{Deserialize, Serialize};

/// Agent memory state.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AgentMemory {
    // TODO: Implement session history and Qdrant integration
}

impl AgentMemory {
    pub fn new() -> Self {
        Self::default()
    }
}
