//! UI Bridge Exploration Engine
//!
//! Drives automated exploration of SDK-connected apps, captures element
//! fingerprints across multiple pages, builds co-occurrence data, and
//! runs fingerprint-based state discovery — all in Rust.
//!
//! # Architecture
//! ```text
//! ExplorationEngine → SDK Proxy (HTTP) → Target App
//!        ↓
//!   Fingerprint Generation
//!        ↓
//!   Co-occurrence Builder
//!        ↓
//!   FingerprintStateDiscovery
//!        ↓
//!   Discovered States + Transitions
//! ```

pub mod cooccurrence;
pub mod discovery;
pub mod engine;
pub mod fingerprint;
pub mod types;

pub use discovery::FingerprintStateDiscovery;
pub use engine::ExplorationEngine;
pub use types::*;
