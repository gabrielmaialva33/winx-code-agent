# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build/Run/Test Commands
- Build: `cargo build` (development), `cargo build --release` (production)
- Run: `cargo run` or `./target/release/winx-code-agent`
- Test: `cargo test` (all tests), `cargo test <test_name>` (single test)
- Lint: `cargo clippy` (static analysis), `cargo fmt` (code formatting)

## Code Style Guidelines
- Follow Rust 2021 edition idioms
- Use `thiserror` for error types with descriptive messages
- Implement custom error types in `errors.rs` using `#[derive(Error, Debug)]`
- Use type aliases like `type Result<T> = std::result::Result<T, WinxError>`
- Document public functions with `///` doc comments
- Module documentation with `//!` comments at the top of files
- Use Tokio for async operations
- Leverage `tracing` for structured logging
- Implement `Clone` for types when needed
- Use descriptive enum variants and struct field names
- Prefer `match` expressions for error handling with detailed patterns
- Keep functions focused on a single responsibility