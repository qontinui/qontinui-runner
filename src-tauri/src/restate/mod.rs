//! Restate durable execution integration.
//!
//! Provides journal-based replay, exactly-once semantics, saga compensation,
//! durable timers, and awakeables for workflow crash recovery.
//!
//! The Restate server runs as a managed subprocess alongside the runner.
//! When enabled, workflows execute through Restate's durable execution engine
//! instead of the legacy checkpoint-based system. The existing checkpoint
//! infrastructure is preserved as a fallback when Restate is unavailable.

pub mod compensation;
pub mod config;
pub mod durable_executor;
pub mod launch;
pub mod lifecycle;
pub mod types;

// These modules depend on the restate-sdk crate (proc macros, Endpoint, HttpServer)
// and are only compiled when the `restate` feature flag is enabled.
#[cfg(feature = "restate")]
pub mod service;
#[cfg(feature = "restate")]
pub mod http_endpoint;
