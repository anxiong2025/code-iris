//! `iris-core` — the engine behind code-iris.
//!
//! This crate is UI-agnostic and can be driven from the CLI (`iris-cli`),
//! the TUI (`iris-tui`), or any future frontend.

pub mod agent;
pub mod agent_def;
pub mod config;
pub mod hooks;
pub mod instructions;
pub mod memory;
pub mod context;
pub mod coordinator;
pub mod models;
pub mod permissions;
pub mod reporter;
pub mod scanner;
pub mod storage;
pub mod tools;

pub use models::*;
