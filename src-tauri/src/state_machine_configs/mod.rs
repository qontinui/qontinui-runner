//! State Machine Config storage module.
//!
//! Provides types and CRUD operations for persisting state machine
//! configurations (configs, states, transitions) in the runner's SQLite database.
//!
//! This enables the runner to build and edit state machines locally,
//! without requiring the web frontend.

pub mod storage;
pub mod types;

pub use types::*;
