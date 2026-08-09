//! Federated observable categories — runner-mediated coord bridges.
//!
//! Plan: `2026-05-22-memories-on-coord-cross-machine.md`, generalized by
//! `2026-05-24-federation-verify-and-gitop.md`. The original memory
//! category was retired by
//! `2026-07-26-claude-session-memory-cutover-to-coord` Phase 3a — coord
//! is now the single writer for Claude-session memory and the local
//! memory dir is a one-way coord→file cache rendered outside the runner.
//!
//! ## Pattern
//!
//! Every federated agent observable follows the same shape:
//!
//!   Agent (Claude session, ...) writes/reads via local convention
//!     → Runner bridges on session boundary + during session
//!     → Coord HTTP API (per-category routes)
//!     → Single tenant-scoped store, fleet-wide
//!
//! The [`RunnerObservableBridge`] trait abstracts this lifecycle.
//! `GitOpBridge` is the sole implementation today. The trait is
//! category-generic: how a bridge detects local changes and what a
//! change looks like stay bridge-internal.

use anyhow::Result;
use async_trait::async_trait;
use once_cell::sync::OnceCell;
use std::path::PathBuf;
use std::sync::Arc;
use uuid::Uuid;

pub mod git_ops;
pub mod git_ops_client;

/// Process-wide registry of every enabled observable bridge, set once at
/// runner init by `main.rs::setup`. Spawn-site call paths
/// (`claude_session::runner`, `claude_session::session`) reach it via
/// [`global_registry`] because they run in the binary tree without
/// access to Tauri `State`, and iterate every bridge for pull / watch /
/// reconcile.
///
/// Empty (or unset) if init failed (e.g., no profile, no reqwest
/// builder) — every caller iterates zero bridges and the spawn proceeds
/// without federation.
static GLOBAL_REGISTRY: OnceCell<Vec<Arc<dyn RunnerObservableBridge>>> = OnceCell::new();

/// Install the process-wide observable-bridge registry. Idempotent —
/// subsequent calls are silently ignored so accidental double-init
/// (e.g., from two Tauri `.setup` closures during testing) does not
/// panic.
pub fn init_registry(bridges: Vec<Arc<dyn RunnerObservableBridge>>) {
    let _ = GLOBAL_REGISTRY.set(bridges);
}

/// Read the process-wide observable-bridge registry. Returns an empty
/// slice until [`init_registry`] runs successfully — federation-aware
/// call sites iterate it and a zero-length registry is the
/// "feature disabled this session" signal.
pub fn global_registry() -> &'static [Arc<dyn RunnerObservableBridge>] {
    GLOBAL_REGISTRY.get().map(Vec::as_slice).unwrap_or(&[])
}

/// Resolves the `git_op` category's kill switch, per call.
///
/// This indirection exists for a crate-boundary reason, not a stylistic
/// one: `GitOpBridge` lives in the **lib** crate (`qontinui_runner_lib`)
/// while `settings` lives in the **bin** crate, so the bridge cannot call
/// `settings::load_settings()` directly. The bin installs a closure here at
/// startup; the lib invokes it at dispatch.
///
/// Storing a **function** rather than a `bool` is the whole point. The
/// closure re-reads settings on every invocation, so the flag stays
/// hot-swappable — edit `settings.json` and the next session honours it,
/// with no runner restart. A cached `bool` would silently turn this back
/// into the startup-read control the plan rejected, and this fleet never
/// restarts runners, so that control could never be exercised.
///
/// Plan: `2026-07-28-git-op-federation-lost-its-kill-switch`.
static GIT_OP_ENABLED_PROVIDER: OnceCell<Box<dyn Fn() -> bool + Send + Sync>> = OnceCell::new();

/// Install the `git_op` kill-switch resolver. Called once from the bin's
/// Tauri `.setup`, alongside [`init_registry`]. Idempotent — a second call
/// is silently ignored, matching [`init_registry`]'s contract.
pub fn init_git_op_enabled_provider(resolver: Box<dyn Fn() -> bool + Send + Sync>) {
    let _ = GIT_OP_ENABLED_PROVIDER.set(resolver);
}

/// Is `git_op` federation currently enabled?
///
/// **Defaults to `true` when no provider is installed** — matching the
/// setting's own `default ON` contract. The uninstalled case is the lib
/// used without the bin (unit tests, the `wrappers_mcp` helper binary),
/// never a signed-out or misconfigured runner, so defaulting off here
/// would disable federation in exactly the situations where nobody asked
/// for it to be off.
pub fn git_op_federation_enabled() -> bool {
    GIT_OP_ENABLED_PROVIDER.get().map(|f| f()).unwrap_or(true)
}

/// Per-session context every bridge receives.
///
/// `tenant_id` is carried for local bookkeeping + UI surfacing only —
/// the coord-side `TenantId` extractor resolves it from the device-token
/// JWT, so HTTP request bodies do not include it.
#[derive(Debug, Clone)]
pub struct SessionContext {
    pub tenant_id: Uuid,
    pub device_id: Uuid,
    pub account_name: String,
    /// The working directory the session spawned in. `GitOpBridge`
    /// reads it to locate `<working_dir>/.git/`.
    pub working_dir: PathBuf,
    pub session_id: Uuid,
}

/// Aggregate counts from a `reconcile` pass.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ReconcileReport {
    pub pushed: u32,
    pub pulled: u32,
    pub unchanged: u32,
    pub failed: u32,
    /// Names of files that failed during reconcile (for debugging).
    #[serde(default)]
    pub failed_names: Vec<String>,
}

/// The trait every federated observable category implements.
///
/// The interface is category-generic. How a bridge detects local
/// changes during a session (a file watcher, a git hook, …) and how it
/// represents those changes are bridge-internal — they never cross this
/// trait.
#[async_trait]
pub trait RunnerObservableBridge: Send + Sync {
    /// Stable category name — used as the `coord.<category>` table name
    /// and the dashboard route segment. Git-ops: `"git_op"`.
    fn category(&self) -> &'static str;

    /// Spawn-time: pull the tenant pool from coord, materialize locally.
    async fn pull(&self, ctx: &mut SessionContext) -> Result<()>;

    /// During-session: start watching for local changes and push them to
    /// coord best-effort.
    ///
    /// The receiver is `self: Arc<Self>` (NOT `&self`): an
    /// implementation may `Arc::clone(&self)` into a detached watch task
    /// so the watcher can keep calling its own internal push after this
    /// returns. `&self` cannot produce that owned `Arc`, and `&Arc<Self>`
    /// is not object-safe. `Arc<Self>` IS object-safe — callable on
    /// `Arc<dyn RunnerObservableBridge>` — so the dispatch site passes
    /// `Arc::clone(b)`. `ctx` is borrowed; the impl clones it internally
    /// before moving the clone into any spawned task.
    async fn start_watching(self: Arc<Self>, ctx: &SessionContext) -> Result<()>;

    /// Stop the watcher for `session_id` if one is running. Idempotent —
    /// a no-op when no watcher is registered for the id. Safe to call
    /// before `reconcile` (which may also stop internally).
    async fn stop_watching(&self, session_id: Uuid);

    /// Session-end: stop watching, diff final state against the post-pull
    /// baseline, push anything the watcher missed, return aggregate
    /// counts.
    async fn reconcile(&self, ctx: &SessionContext) -> Result<ReconcileReport>;
}
