//! Cross-cutting utility helpers shared across modules.
//!
//! Modules under this namespace exist to host helpers that do not naturally
//! belong to any single feature crate. Keep them small, well-tested, and
//! free of dependencies on `commands::AppState` or other large state
//! aggregators — utilities should be invocable from anywhere.

/// THE `source()`-chain renderer — one implementation, replacing the three
/// private copies `fleet`, `agent_worktree::reclaim` and `env_agent` each grew
/// for the same defect. Also declared from `lib.rs` (inline `pub mod util`) so
/// the lib crate reaches this same FILE under the same spelling.
/// The process-level context every coord-egress failure carries (uptime, open
/// handle/socket counts, per-client in-flight + failure totals), plus the
/// periodic INFO baseline that gives those numbers something to be compared
/// against. Bin-only: every coord egress site lives in the bin crate.
pub mod egress_context;
pub mod error_chain;
pub mod path_extraction;
