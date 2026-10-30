//! Cross-cutting utility helpers shared across modules.
//!
//! Modules under this namespace exist to host helpers that do not naturally
//! belong to any single feature crate. Keep them small, well-tested, and
//! free of dependencies on `commands::AppState` or other large state
//! aggregators — utilities should be invocable from anywhere.

/// The process-level context every coord-egress failure carries (uptime, open
/// handle/socket counts, per-client in-flight + failure totals), plus the
/// periodic INFO baseline that gives those numbers something to be compared
/// against. Bin-only: every coord egress site lives in the bin crate.
pub mod egress_context;
// THE `source()`-chain renderer — one implementation, replacing the three
// private copies `fleet`, `agent_worktree::reclaim` and `env_agent` each grew
// for the same defect. OWNED by the lib (`lib.rs`'s inline `pub mod util`) and
// re-exported here, so both crates reach one compiled copy under the same
// spelling. Declaring the file here as well would compile it twice
// (`crate_roots_ratchet` fails on that).
pub use qontinui_runner_lib::util::error_chain;
pub mod path_extraction;
