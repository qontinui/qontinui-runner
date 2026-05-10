//! Cross-cutting utility helpers shared across modules.
//!
//! Modules under this namespace exist to host helpers that do not naturally
//! belong to any single feature crate. Keep them small, well-tested, and
//! free of dependencies on `commands::AppState` or other large state
//! aggregators — utilities should be invocable from anywhere.

pub mod path_extraction;
