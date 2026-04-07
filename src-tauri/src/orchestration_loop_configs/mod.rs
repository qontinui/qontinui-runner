//! Orchestration Loop Config storage module.
//!
//! Provides types and CRUD operations for persisting orchestration loop
//! configurations (saved presets with name, favorite, and full config JSON)
//! in the runner's PostgreSQL database.

pub mod storage;
pub mod types;

pub use types::*;
