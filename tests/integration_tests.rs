// Main integration test entry point
// This file is discovered by cargo test and runs all integration tests

mod integration;

// Re-export integration test modules for easy access
pub use integration::*;