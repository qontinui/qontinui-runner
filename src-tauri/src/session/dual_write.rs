//! Phase 10 cutover scaffolding — flag-gated dual-write of legacy
//! sessions into the coord-native [`crate::session`] primitive.
//!
//! Plan: `D:/qontinui-root/qontinui-dev-notes/plans/
//! 2026-05-23-coord-native-sessions-phase-7-10.md` §Phase 10 (stacked on
//! the coord `/tenant-policy` read endpoint added in the companion
//! coord PR).
//!
//! ## What this is
//!
//! Today the runner has two parallel session surfaces:
//!
//! - **Legacy** — `commands/terminal.rs::terminal_create` (+ siblings)
//!   and `claude_session/` spawn PTYs / Claude CLI subprocesses with no
//!   coord integration. This is what the runner UI actually drives.
//! - **Coord-native** — the [`crate::session`] module: `Session::start`
//!   → local outbox → [`super::coord_sync`] drain loop → `coord.sessions`.
//!   Wired through the new `session_*` Tauri commands, which the frontend
//!   has NOT cut over to yet (plan §Phase 4 frontend cutover is deferred).
//!
//! Phase 10's job is to ship the **dual-write window**: when a tenant
//! flips its `coord.tenant_policies.session_coordination_enabled` flag,
//! the legacy path *also* materializes a coord-native session so the
//! dashboard renders identically from the new schema — without removing
//! the legacy path (that is Phase 9). The actual flag flip is a
//! multi-release operator decision; this code ships dormant.
//!
//! ## Dormant by construction
//!
//! The gate is closed unless ALL of these hold:
//!
//! 1. The runner resolved an `active_tenant_id` from
//!    `~/.qontinui/machine.json`. MSI's `machine.json` has none today, so
//!    the gate never even queries coord — it stays a hard no-op.
//! 2. Coord's `/tenant-policy?tenant_id=<id>` returns
//!    `session_coordination_enabled = true` for that tenant. The DB column
//!    defaults `false` and the coord-side missing-row fallback is also
//!    `false` (companion coord PR), so an un-flipped tenant reads closed.
//!
//! When the gate is closed, the `mirror_legacy_session` entry point on
//! [`super::coord_sync::CoordSync`] returns immediately without touching
//! the registry, the outbox, or coord. There is **zero** production
//! behavior change with the flag off — the property the plan's §Phase 10
//! requires ("ship code but DO NOT flip the flag").
//!
//! ## Why poll instead of read-per-session?
//!
//! Session creation is latency-sensitive (the operator is staring at a
//! terminal that hasn't opened yet). A synchronous coord round-trip on
//! every `terminal_create` would add a network hop to the hot path even
//! when the flag is off. Instead a background task refreshes the flag
//! into an [`AtomicBool`] on a slow cadence; the hot path reads the atom
//! with `Relaxed` ordering (sub-nanosecond). Staleness is bounded by the
//! poll interval (default 60s) — acceptable for a rollout knob that flips
//! at most a handful of times per tenant per release.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use uuid::Uuid;

/// Default cadence for refreshing the per-tenant flag from coord. Slow —
/// the flag flips rarely and the hot path reads a cached atom, so there's
/// no benefit to polling fast (and a tight poll would add coord load
/// across the whole fleet for no reason).
const DEFAULT_FLAG_POLL_SECS: u64 = 60;

/// Holds the resolved dual-write state for one runner process. Lives
/// inside [`super::coord_sync::CoordSync`]; cheap to read on the hot path.
#[derive(Debug)]
pub struct DualWriteGate {
    /// Cached `session_coordination_enabled` for this runner's tenant.
    /// Default `false` (dormant). The poll task is the only writer.
    enabled: AtomicBool,
    /// The tenant whose policy gates this runner, resolved once from
    /// `machine.json` at construction. `None` → the gate never opens and
    /// the poll task short-circuits (no tenant → no coord-native mirror).
    tenant_id: Mutex<Option<Uuid>>,
    /// Poll cadence, env-tunable via `QONTINUI_SESSION_FLAG_POLL_SECS`.
    poll_interval: Duration,
}

impl DualWriteGate {
    /// Construct a gate, resolving the active tenant from
    /// `~/.qontinui/machine.json`'s `active_tenant_id` field (plan §D12 —
    /// written on the first multi-tenant prompt). Single-tenant operators
    /// have no such field, so the gate stays permanently dormant for them
    /// regardless of any tenant's flag.
    pub fn new() -> Self {
        let tenant_id = resolve_active_tenant_id();
        let poll_interval = Duration::from_secs(env_u64(
            "QONTINUI_SESSION_FLAG_POLL_SECS",
            DEFAULT_FLAG_POLL_SECS,
        ));
        Self {
            enabled: AtomicBool::new(false),
            tenant_id: Mutex::new(tenant_id),
            poll_interval,
        }
    }

    /// Test-only constructor that pins the tenant + poll cadence without
    /// touching `machine.json` or env vars.
    #[cfg(test)]
    pub fn new_for_test(tenant_id: Option<Uuid>, poll_interval: Duration) -> Self {
        Self {
            enabled: AtomicBool::new(false),
            tenant_id: Mutex::new(tenant_id),
            poll_interval,
        }
    }

    /// Hot-path read. `true` only when the resolved tenant has flipped
    /// `session_coordination_enabled`. Default `false`.
    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    /// The tenant this gate is bound to, if any.
    pub fn tenant_id(&self) -> Option<Uuid> {
        *self
            .tenant_id
            .lock()
            .expect("dual_write tenant slot poisoned")
    }

    /// Poll cadence — surfaced for the poll task + tests.
    pub fn poll_interval(&self) -> Duration {
        self.poll_interval
    }

    /// Apply a freshly-fetched flag value. Logs only on a genuine
    /// transition so an un-flipped fleet stays quiet.
    pub fn apply(&self, value: bool) {
        let prev = self.enabled.swap(value, Ordering::Relaxed);
        if prev != value {
            tracing::info!(
                tenant = ?self.tenant_id(),
                session_coordination_enabled = value,
                "dual_write: tenant cutover flag changed"
            );
        }
    }
}

impl Default for DualWriteGate {
    fn default() -> Self {
        Self::new()
    }
}

/// Read `active_tenant_id` from `~/.qontinui/machine.json`. Returns `None`
/// (gate stays dormant) on any failure — missing file, missing field,
/// unparseable UUID. Single-tenant operators legitimately have no field.
///
/// `pub(crate)` so the session-sync POST path can use the same resolved
/// tenant to attribute sessions (otherwise they register under the nil
/// tenant and are invisible to the operator's tenant-scoped dashboard).
pub(crate) fn resolve_active_tenant_id() -> Option<Uuid> {
    let path = dirs::home_dir()?.join(".qontinui").join("machine.json");
    let bytes = std::fs::read(path).ok()?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let raw = value.get("active_tenant_id").and_then(|v| v.as_str())?;
    Uuid::parse_str(raw).ok()
}

/// Read a `u64` env var with a sane default. (Duplicated from
/// `coord_sync` to keep this module self-contained for Phase 9 deletion.)
fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_dormant() {
        let gate = DualWriteGate::new_for_test(Some(Uuid::new_v4()), Duration::from_secs(1));
        assert!(
            !gate.enabled(),
            "dual-write must default off — the flag flip is an operator decision"
        );
    }

    #[test]
    fn apply_toggles_the_atom() {
        let gate = DualWriteGate::new_for_test(Some(Uuid::new_v4()), Duration::from_secs(1));
        gate.apply(true);
        assert!(gate.enabled());
        gate.apply(false);
        assert!(!gate.enabled());
    }

    #[test]
    fn no_tenant_means_permanently_dormant() {
        // No active tenant resolved (single-tenant operator). Even an
        // explicit apply(true) would enable the atom, but the poll task
        // (see coord_sync) never calls apply for a None tenant — it
        // short-circuits. This test pins the precondition the poll task
        // relies on.
        let gate = DualWriteGate::new_for_test(None, Duration::from_secs(1));
        assert!(gate.tenant_id().is_none());
        assert!(!gate.enabled());
    }
}
