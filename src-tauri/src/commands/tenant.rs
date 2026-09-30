//! Tenant resolution Tauri commands — plan
//! [`2026-05-22-coord-native-session-coordination`] §D12 + §Phase 4.
//!
//! ## `active_tenant_id` semantics (Phase 8b, plan
//! `2026-07-02-session-scoped-multi-tenant-device-binding` §D4)
//!
//! `machine.json::active_tenant_id` is the **default for NEW sessions and
//! for device-level surfaces** (heartbeat, census, backstop, maintenance,
//! doctor, flag-poll) — NOT "the only tenant this device serves". A device
//! holds N concurrent tenant bindings (`paired_user.json` v2 +
//! `coord.tenant_devices`); each session records its own tenant at
//! creation (spawn input, else this default).
//!
//! A switch is NOT limited to future sessions. A coord-mcp binding frozen
//! PINNED at creation keeps its tenant, but one frozen UNPINNED (the normal
//! single-tenant shape, and a restored/adopted nonce with no recorded tenant)
//! re-reads the machine pin on every request (`coord_mcp::decide_session_tenant`
//! rows 2-4), so an unpinned -> pinned switch moves running sessions too. And
//! the dual-write gate and the nonce restore read the pin once at startup, so
//! they see a switch only at the next runner start. [`PIN_SURFACES`] is the
//! per-consumer table; `PUT /tenant/active` reports it with a live count.
//!
//! The frontend [`TenantContext`] reads the active tenant id (per machine)
//! and offers a switcher when the operator belongs to >1 tenant. The
//! source of truth for the default is `~/.qontinui/machine.json`'s
//! `active_tenant_id` field (D12 explicitly nominates this file as the
//! per-machine pin). At first launch on a multi-tenant operator account
//! the file may not yet hold the field; in that case we fall back to the
//! DEFAULT binding available via the existing
//! [`qontinui_runner_lib::pair::read_paired_tenant_id_from_disk`] reader
//! (v2-aware: `default_tenant_id`) so the runner UI can render before the
//! operator has explicitly chosen.
//!
//! Note: a richer "list of tenants the operator belongs to" requires a
//! coord round-trip (Phase 5 dashboard). Phase 4 only needs the active
//! pin + persistence — the runner header switcher renders nothing when
//! the operator is in exactly one tenant, which is the common case.
//!
//! ## Two doors, one path
//!
//! The read and the write are plain functions ([`active_tenant_view`],
//! [`apply_active_tenant`]) that BOTH the Tauri commands here and the
//! headless `GET`/`PUT /tenant/active` routes (`mcp::tenant`) call — plan
//! `2026-09-23-remote-create-residuals-after-coord-registration-confirm`
//! Phase 4. A write refuses any tenant outside this device's local binding
//! set, and reports which surfaces see a switch live and which at the next
//! start ([`PIN_SURFACES`]), and how many running sessions it moves.

use std::path::{Path, PathBuf};

use crate::commands::CommandResponse;

/// Path of `~/.qontinui/machine.json`, through the ambient seam.
fn machine_file_path() -> Option<PathBuf> {
    qontinui_runner_lib::ambient::machine_json_path()
}

/// The raw JSON object — the WRITER's shape, so sibling fields (`device_id`,
/// `hostname`, `name`) round-trip verbatim through [`write_active_tenant_id`].
fn read_machine_file(path: &Path) -> Option<serde_json::Value> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice::<serde_json::Value>(&bytes).ok()
}

fn read_active_tenant_id(path: &Path) -> Option<String> {
    qontinui_runner_lib::ambient::read_machine_json_at(path)
        .ok()?
        .active_tenant_id_str()
        .map(str::to_string)
}

/// Atomic rewrite: read → patch `active_tenant_id` → unique-temp write →
/// rename. Other top-level fields (`device_id`, `hostname`, `name`) are
/// preserved verbatim. Returns an error string fit for the frontend.
///
/// **FAILS CLOSED on a machine.json that has no device identity.** This used
/// to fall back to an EMPTY `serde_json::Map` whenever the file was missing,
/// unparseable, or not a JSON object — so a tenant switch wrote
/// `{"active_tenant_id": …}` with no `device_id`. That file is worse than no
/// file: the minter (`pair::ensure_device_initialized_at`) sees
/// `path.exists()` and delegates to the preserving backfill, which finds no
/// key to preserve, so the identity is permanently wedged; and the only
/// documented recovery — `rm machine.json` + `device init` — MINTS A FRESH
/// UUID, i.e. a brand-new `coord.devices` row for the same physical machine.
/// Refusing to write is strictly better: the existing identity file (if any)
/// is left intact and the operator gets guidance that does not re-mint.
///
/// The refusal messages distinguish **recoverable** from **unrecoverable**.
/// An ABSENT file has nothing to lose, so `device init` is the right advice.
/// A file that is present but corrupt / non-object may still hold this
/// machine's real UUID, so the advice is "inspect and repair by hand" and
/// explicitly NOT `rm` — `rm` + `device init` mints a fresh id, which is the
/// very outcome this function exists to prevent.
/// Plan `2026-08-06-device-identity-is-per-profile-not-per-machine` §0.3 H4.
///
/// The error is typed so the HTTP door can tell a REFUSAL (the file on disk
/// does not carry an identity this writer may preserve — the caller's device
/// state, a 409) from an I/O failure of a write it was entitled to make (a
/// 500). The message text is unchanged either way.
fn write_active_tenant_id(path: &Path, tenant_id: &str) -> Result<(), MachineJsonWriteError> {
    // Read the existing JSON object so we preserve sibling fields — and
    // REFUSE outright if there is no device identity to preserve.
    let mut obj = match read_machine_file(path) {
        Some(serde_json::Value::Object(map)) => map,
        Some(_) => {
            return Err(MachineJsonWriteError::Refused(format!(
                "tenant: refusing to write {} — it is not a JSON object, so it holds no \
                 device_id to preserve. Inspect it by hand; do NOT `rm` it (that mints a \
                 NEW device identity and a new coord.devices row).",
                path.display()
            )))
        }
        // `read_machine_file` collapses "absent" and "present but unreadable"
        // into one `None`, and the two need OPPOSITE guidance: `rm` is harmless
        // on an absent file and DESTRUCTIVE on a corrupt one (which may still
        // carry the machine's real UUID). Split on `path.exists()` so the
        // operator is not funnelled into a re-mint.
        None if path.exists() => {
            return Err(MachineJsonWriteError::Refused(format!(
                "tenant: refusing to write {} — it EXISTS but is unreadable (I/O error or \
                 invalid JSON), so a write here would replace it with a machine.json that \
                 has no device_id and wedge this machine's identity. The file may still \
                 contain this machine's real device_id: inspect and repair it by hand; do \
                 NOT `rm` it (that mints a NEW device identity and a new coord.devices \
                 row). Then switch tenant again.",
                path.display()
            )))
        }
        None => {
            return Err(MachineJsonWriteError::Refused(format!(
                "tenant: refusing to write {} — it does not exist, so a write here would \
                 produce a machine.json with no device_id and wedge this machine's \
                 identity. Nothing is at risk: run `qontinui_profile device init` (it \
                 mints only when there is genuinely no file, and re-uses any existing \
                 device_id), then switch tenant again.",
                path.display()
            )))
        }
    };
    // `machine_id` is the pre-rename spelling every reader still aliases, so a
    // legacy-shaped file counts as having an identity.
    let identity_key = ["device_id", "machine_id"].into_iter().find(|k| {
        obj.get(*k)
            .and_then(|v| v.as_str())
            .is_some_and(|s| !s.trim().is_empty())
    });
    let Some(identity_key) = identity_key else {
        return Err(MachineJsonWriteError::Refused(format!(
            "tenant: refusing to write {} — it carries no device_id. Writing would \
             permanently wedge this machine's identity (the minter skips an existing \
             file). Restore the file or run `qontinui_profile device init`, then switch \
             tenant again.",
            path.display()
        )));
    };
    // Normalize surrounding whitespace on the identity we are about to
    // re-serialize. Every READER trims (`machine_identity::read_device_id_at`),
    // so an on-disk `" abc "` and the id presented to coord would otherwise
    // differ by exactly the padding — two `coord.devices` rows for one machine.
    // Writing the trimmed form makes disk and wire agree.
    let trimmed_identity = obj
        .get(identity_key)
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string());
    if let Some(trimmed) = trimmed_identity {
        obj.insert(identity_key.to_string(), serde_json::Value::String(trimmed));
    }
    obj.insert(
        "active_tenant_id".to_string(),
        serde_json::Value::String(tenant_id.to_string()),
    );
    let pretty = serde_json::to_vec_pretty(&serde_json::Value::Object(obj)).map_err(|e| {
        MachineJsonWriteError::Io(format!("tenant: serialize machine.json failed: {e}"))
    })?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            MachineJsonWriteError::Io(format!("tenant: create ~/.qontinui dir failed: {e}"))
        })?;
    }
    // Unique-temp atomic write. The fixed `machine.json.tmp` this used to
    // share with the three other writers is raced by every runner instance.
    qontinui_runner_lib::fs_atomic::atomic_write(path, &pretty).map_err(|e| {
        MachineJsonWriteError::Io(format!("tenant: atomic write machine.json failed: {e}"))
    })?;
    Ok(())
}

/// Why [`write_active_tenant_id`] did not write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MachineJsonWriteError {
    /// The file on disk carries no device identity this writer may preserve
    /// (absent, unreadable, not an object, no `device_id`). Nothing was
    /// written; the message says how to recover without re-minting.
    Refused(String),
    /// The write itself failed (serialize, create dir, atomic rename).
    Io(String),
}

impl std::fmt::Display for MachineJsonWriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(m) | Self::Io(m) => f.write_str(m),
        }
    }
}

// ============================================================================
// Shared read / write paths — the Tauri commands below AND the headless
// `GET`/`PUT /tenant/active` routes (`mcp::tenant`) both call these, so the
// two doors cannot drift. Plan
// `2026-09-23-remote-create-residuals-after-coord-registration-confirm` Phase 4.
// ============================================================================

/// When a tenant switch takes effect, overall: some surfaces switch at once,
/// some only at the next runner start. The per-surface answer is
/// [`PIN_SURFACES`]; this is its one-word summary.
pub(crate) const TAKES_EFFECT: &str = "mixed";

/// When one pin consumer sees a switch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PinTiming {
    /// Re-reads `machine.json` on each use (`ambient::read_machine_json` is a
    /// fresh `std::fs::read`, no cache), so the next use sees the new pin.
    Live,
    /// Reads the pin once when the runner starts; this process keeps what it
    /// read, and the new pin applies from the next runner start.
    NextStart,
}

/// One consumer of the pin, and when it sees a switch.
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub(crate) struct PinSurface {
    pub surface: &'static str,
    pub timing: PinTiming,
    /// Where the read is: `file::fn`. Named by function rather than line so it
    /// does not go stale on the next unrelated edit.
    pub evidence: &'static str,
    pub detail: &'static str,
}

/// Every consumer of the machine pin, classified per-use vs startup. Built
/// from a grep of every `resolve_tenant_pin` / `resolve_active_tenant_id` /
/// `active_tenant_uuid` / `active_tenant_id_str` reader, excluding tests, this
/// module's own readout, and one-shot diagnostics (`coord_doctor`), which read
/// per invocation.
///
/// Sessions that ALREADY exist are not a surface here. Whether one moves
/// depends on how its tenant was held at creation, which is what the
/// `existing_sessions` block of the response reports
/// ([`crate::coord_mcp::device_session_pin_census`]).
pub(crate) const PIN_SURFACES: &[PinSurface] = &[
    PinSurface {
        surface: "new_session_tenant",
        timing: PinTiming::Live,
        evidence: "session/mod.rs::resolve_new_session_tenant \
                   (also claude_session/coord_register.rs::AiCoordRegistrar::new, \
                   ::federation_session_tenant)",
        detail: "the tenant a NEW session is recorded under, read at its creation",
    },
    PinSurface {
        surface: "coord_mcp_new_nonce_mint",
        timing: PinTiming::Live,
        evidence: "coord_mcp.rs::mint_and_register_nonce_with (MintPin::MachineNow)",
        detail: "a new coord-mcp binding with no spawn-chosen tenant samples the pin at mint \
                 and, if pinned, freezes it for the binding's life",
    },
    PinSurface {
        surface: "coord_mcp_unpinned_session_requests",
        timing: PinTiming::Live,
        evidence: "coord_mcp.rs::decide_session_tenant rows 2-4 \
                   (via session_tenant_or_refuse / session_tenant_decision)",
        detail: "a binding frozen unpinned re-reads the pin on every proxied request; one \
                 frozen pinned (row 1) does not",
    },
    PinSurface {
        surface: "device_jwt_refresher",
        timing: PinTiming::Live,
        evidence: "mcp/device_jwt_refresher.rs::refresher_loop, \
                   ::publish_coord_credential_status, ::read_sweep_inputs",
        detail: "re-read on each refresher tick (5 min) and each slot sweep",
    },
    PinSurface {
        surface: "device_level_publishers",
        timing: PinTiming::Live,
        evidence: "agent_worktree/census.rs::build_and_publish, ::resolve_volume_poster; \
                   agent_worktree/fs_backstop.rs::tick_once; \
                   agent_worktree/maintenance_executor.rs::report_reset_git_op; \
                   fleet/resource_sample.rs::publish_once",
        detail: "re-read on each publish/tick; the volume poster is rebuilt when the tenant \
                 in its key changes",
    },
    PinSurface {
        surface: "session_outbox_tenant_backfill",
        timing: PinTiming::Live,
        evidence: "session/coord_sync.rs::push_record, ::rebuild_create_body",
        detail: "fills the tenant only for records that carry none, read per record",
    },
    PinSurface {
        surface: "per_request_readouts",
        timing: PinTiming::Live,
        evidence: "mcp_api.rs::health; commands/session_info.rs::read_session_tenancy; \
                   mcp/ui_bridge/gated_flow.rs::ui_bridge_session_handler; \
                   repo_detection.rs::register_repo_with_coord",
        detail: "read per request",
    },
    PinSurface {
        surface: "session_coordination_dual_write_gate",
        timing: PinTiming::NextStart,
        evidence: "session/dual_write.rs::DualWriteGate::new; \
                   session/coord_sync.rs::CoordSync::start_flag_poll_task",
        detail: "the gate resolves its tenant once at construction and the flag poll is \
                 started only for that tenant; this process keeps both",
    },
    PinSurface {
        surface: "coord_mcp_nonce_restore",
        timing: PinTiming::NextStart,
        evidence: "coord_mcp.rs::restore_proxy_nonces_from (restore_time_pin)",
        detail: "persisted bindings with no recorded tenant take the pin as sampled once at \
                 boot; bindings already restored count under existing_sessions",
    },
];

/// The pin as it stands, plus the bound tenants a switch may choose from.
#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct ActiveTenantView {
    /// The effective default: the `machine.json` pin, else the paired default
    /// binding (`source` says which), else `null`.
    pub active_tenant_id: Option<String>,
    /// `"machine.json"` | `"paired_user.json"` | `null`.
    pub source: Option<&'static str>,
    /// How `machine.json` itself classifies (`tenant_pin::TenantPin`):
    /// `"pinned"`, `"unpinned"` (readable, no field — the paired default
    /// applies) or `"unresolvable"` (unreadable, or a malformed value).
    pub pin: &'static str,
    /// The tenants this device is LOCALLY bound to (`paired_user.json` v2),
    /// the set [`apply_active_tenant`] validates against. Local file read, no
    /// coord round-trip.
    pub candidates: Vec<String>,
}

fn pin_label(pin: qontinui_runner_lib::tenant_pin::TenantPin) -> &'static str {
    use qontinui_runner_lib::tenant_pin::TenantPin;
    match pin {
        TenantPin::Pinned(_) => "pinned",
        TenantPin::Unpinned => "unpinned",
        TenantPin::Unresolvable => "unresolvable",
    }
}

/// Read the active tenant. Resolution order:
///
/// 1. `~/.qontinui/machine.json` → `active_tenant_id` field (D12 pin)
/// 2. Cached pair file (`paired_user.json`) → default binding (fallback so
///    the UI has something to render before the operator pins)
pub(crate) fn active_tenant_view() -> Result<ActiveTenantView, String> {
    let machine_path = machine_file_path()
        .ok_or_else(|| "tenant: no home directory; cannot read machine.json".to_string())?;

    let (tenant, source) = if let Some(t) = read_active_tenant_id(&machine_path) {
        (Some(t), Some("machine.json"))
    } else if let Some(t) = qontinui_runner_lib::pair::read_paired_tenant_id_from_disk() {
        (Some(t), Some("paired_user.json"))
    } else {
        (None, None)
    };

    // The device's bound tenants live locally in `paired_user.json` v2 — the
    // switcher must render OFFLINE, so this is a local read, not a coord query.
    let candidates: Vec<String> = qontinui_runner_lib::pair::read_paired_binding_tenant_ids()
        .into_iter()
        .map(|t| t.to_string())
        .collect();

    Ok(ActiveTenantView {
        active_tenant_id: tenant,
        source,
        pin: pin_label(qontinui_runner_lib::tenant_pin::resolve_tenant_pin()),
        candidates,
    })
}

/// Why [`apply_active_tenant`] refused or failed. Every refusal leaves
/// `machine.json` untouched: the checks all run before the write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SetActiveTenantError {
    /// Blank `tenant_id`.
    Empty,
    /// Not a UUID. Writing it would make the pin `Unresolvable`, which the
    /// coord-mcp proxy REFUSES on — so it is refused here instead.
    Malformed(String),
    /// `paired_user.json` names no binding at all (unpaired, missing or
    /// unreadable), so no tenant can be proven bound. Fail closed.
    NoBindings,
    /// A well-formed tenant this device holds no binding for.
    NotBound { tenant: String, bound: Vec<String> },
    /// No home directory, so no `machine.json` path.
    NoHome,
    /// The write was refused ([`MachineJsonWriteError::Refused`]).
    WriteRefused(String),
    /// The write failed ([`MachineJsonWriteError::Io`]).
    WriteFailed(String),
}

impl SetActiveTenantError {
    /// SCREAMING_SNAKE_CASE discriminator for the HTTP envelope's `code`.
    pub(crate) fn code(&self) -> &'static str {
        match self {
            Self::Empty | Self::Malformed(_) => "INVALID_TENANT_ID",
            Self::NoBindings => "NO_TENANT_BINDINGS",
            Self::NotBound { .. } => "TENANT_NOT_BOUND",
            Self::NoHome => "NO_HOME_DIR",
            Self::WriteRefused(_) => "MACHINE_JSON_WRITE_REFUSED",
            Self::WriteFailed(_) => "MACHINE_JSON_WRITE_FAILED",
        }
    }
}

impl std::fmt::Display for SetActiveTenantError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => f.write_str("tenant: tenant_id cannot be empty"),
            Self::Malformed(raw) => write!(f, "tenant: tenant_id {raw:?} is not a UUID"),
            Self::NoBindings => f.write_str(
                "tenant: this device holds no tenant binding (paired_user.json is missing, \
                 unreadable or names no tenant), so no tenant can be proven bound; pair the \
                 device first. machine.json was not changed.",
            ),
            Self::NotBound { tenant, bound } => write!(
                f,
                "tenant: this device is not bound to tenant {tenant}; bound tenants: [{}]. \
                 machine.json was not changed.",
                bound.join(", ")
            ),
            Self::NoHome => f.write_str("tenant: no home directory; cannot write machine.json"),
            Self::WriteRefused(m) | Self::WriteFailed(m) => f.write_str(m),
        }
    }
}

/// Persist the operator's tenant choice to
/// `~/.qontinui/machine.json::active_tenant_id` — the ONE write path, shared
/// by the `set_active_tenant` Tauri command and `PUT /tenant/active`.
///
/// Refuses (with `machine.json` byte-identical) a blank or non-UUID id, and
/// any tenant not in this device's local binding set (`paired_user.json` v2,
/// the same set `get_active_tenant` offers as `candidates`). Returns the
/// canonical (lowercase, hyphenated) id written.
///
/// Phase 8b semantics: this sets the DEFAULT for NEW sessions and device-level
/// surfaces on a device that may hold N concurrent bindings. It does NOT
/// migrate already-running sessions and does NOT unpair any other binding.
pub(crate) fn apply_active_tenant(tenant_id: &str) -> Result<String, SetActiveTenantError> {
    let trimmed = tenant_id.trim();
    if trimmed.is_empty() {
        return Err(SetActiveTenantError::Empty);
    }
    let tenant = uuid::Uuid::parse_str(trimmed)
        .map_err(|_| SetActiveTenantError::Malformed(trimmed.to_string()))?;

    let bound = qontinui_runner_lib::pair::read_paired_binding_tenant_ids();
    if bound.is_empty() {
        return Err(SetActiveTenantError::NoBindings);
    }
    if !bound.contains(&tenant) {
        return Err(SetActiveTenantError::NotBound {
            tenant: tenant.to_string(),
            bound: bound.iter().map(|t| t.to_string()).collect(),
        });
    }

    let machine_path = machine_file_path().ok_or(SetActiveTenantError::NoHome)?;
    let canonical = tenant.to_string();
    write_active_tenant_id(&machine_path, &canonical).map_err(|e| match e {
        MachineJsonWriteError::Refused(m) => SetActiveTenantError::WriteRefused(m),
        MachineJsonWriteError::Io(m) => SetActiveTenantError::WriteFailed(m),
    })?;
    Ok(canonical)
}

/// The success payload both doors return after [`apply_active_tenant`].
///
/// `existing_sessions` is measured, not asserted: a coord-mcp binding frozen
/// `Pinned` at creation keeps its tenant, while one frozen unpinned — the
/// normal single-tenant shape, and a restored or adopted nonce with no recorded
/// tenant — follows the new pin on its next request. So an unpinned → pinned
/// switch, the likely first headless use, DOES move running sessions.
pub(crate) fn applied_payload(
    active_tenant_id: &str,
    previous: Option<String>,
) -> serde_json::Value {
    let census = crate::coord_mcp::device_session_pin_census();
    serde_json::json!({
        "active_tenant_id": active_tenant_id,
        "previous_active_tenant_id": previous,
        "source": "machine.json",
        "takes_effect": TAKES_EFFECT,
        "surfaces": PIN_SURFACES,
        "existing_sessions": {
            "pinned_at_creation": {
                "count": census.pinned_at_creation,
                "effect": "keep the tenant they were created with",
            },
            "follows_machine_pin": {
                "count": census.follows_machine_pin,
                "effect": "use the new pin on their next coord-mcp request, unless their \
                           workspace declares a tenant (so the count is an upper bound)",
            },
            "scope": "live device coord-mcp bindings in this runner process",
        },
    })
}

/// The `machine.json` pin before a write, for the response's
/// `previous_active_tenant_id`. `None` when unpinned or unreadable.
pub(crate) fn current_machine_pin() -> Option<String> {
    machine_file_path().and_then(|p| read_active_tenant_id(&p))
}

/// Return the active tenant id for this machine — the DEFAULT binding for
/// new sessions and device-level surfaces (Phase 8b semantics; existing
/// sessions keep their own recorded tenant). See [`active_tenant_view`].
///
/// Returns `{ active_tenant_id, source, pin, candidates }`. The frontend
/// treats `candidates.length <= 1` as "operator is in exactly one tenant" and
/// elides the switcher accordingly.
#[tauri::command]
pub fn get_active_tenant() -> Result<CommandResponse, String> {
    let view = active_tenant_view()?;
    Ok(CommandResponse {
        success: true,
        message: None,
        data: Some(serde_json::to_value(view).map_err(|e| e.to_string())?),
    })
}

/// Persist the operator's tenant choice. See [`apply_active_tenant`] — the
/// same path `PUT /tenant/active` takes.
#[tauri::command]
pub fn set_active_tenant(tenant_id: String) -> Result<CommandResponse, String> {
    let previous = current_machine_pin();
    let written = apply_active_tenant(&tenant_id).map_err(|e| e.to_string())?;
    Ok(CommandResponse {
        success: true,
        message: None,
        data: Some(applied_payload(&written, previous)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn write_then_read_round_trips() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("machine.json");
        // Seed with sibling fields to confirm preservation.
        std::fs::write(&path, br#"{"device_id":"abc","hostname":"x"}"#).unwrap();
        write_active_tenant_id(&path, "11111111-1111-1111-1111-111111111111").unwrap();
        let v = read_machine_file(&path).unwrap();
        // Sibling preserved.
        assert_eq!(v.get("device_id").and_then(|x| x.as_str()), Some("abc"));
        // New field present.
        let got = read_active_tenant_id(&path).unwrap();
        assert_eq!(got, "11111111-1111-1111-1111-111111111111");
    }

    #[test]
    fn read_missing_returns_none() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nope.json");
        assert!(read_active_tenant_id(&path).is_none());
    }

    // ------------------------------------------------------------------
    // Identity preservation + fail-closed — plan
    // `2026-08-06-device-identity-is-per-profile-not-per-machine` Phase 2.
    // ------------------------------------------------------------------

    /// A tenant switch preserves `device_id`, `hostname` and `name` verbatim.
    /// This is the invariant that keeps one machine to one `coord.devices`
    /// row: coord UPSERTs `ON CONFLICT (device_id)`, so losing the id here
    /// means the next `device init` mints a new one and a new row.
    #[test]
    fn write_preserves_device_id_hostname_and_name() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("machine.json");
        std::fs::write(
            &path,
            br#"{"device_id":"c79a07d5-0000-4000-8000-000000000001","hostname":"spaceship","name":"primary","extra":7}"#,
        )
        .unwrap();

        write_active_tenant_id(&path, "c231d9da-0000-4000-8000-000000000002").unwrap();

        let v = read_machine_file(&path).unwrap();
        assert_eq!(
            v.get("device_id").and_then(|x| x.as_str()),
            Some("c79a07d5-0000-4000-8000-000000000001"),
            "device_id must survive a tenant switch"
        );
        assert_eq!(
            v.get("hostname").and_then(|x| x.as_str()),
            Some("spaceship")
        );
        assert_eq!(v.get("name").and_then(|x| x.as_str()), Some("primary"));
        assert_eq!(v.get("extra").and_then(|x| x.as_i64()), Some(7));
        assert_eq!(
            read_active_tenant_id(&path).as_deref(),
            Some("c231d9da-0000-4000-8000-000000000002")
        );
    }

    /// FAIL CLOSED: a missing machine.json must NOT be created here. The old
    /// empty-map fallback wrote a `device_id`-less file, which permanently
    /// blocks the minter (`ensure_device_initialized_at` sees `path.exists()`
    /// and delegates to the preserving backfill, which finds nothing to
    /// preserve) — and the only documented recovery mints a fresh UUID, i.e.
    /// a new `coord.devices` row for the same machine.
    #[test]
    fn write_refuses_when_machine_json_is_missing() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("machine.json");
        let err = write_active_tenant_id(&path, "tenant-uuid")
            .expect_err("a missing machine.json must be refused");
        assert!(
            matches!(err, MachineJsonWriteError::Refused(_)),
            "an absent file is a refusal, not an I/O failure: {err:?}"
        );
        let err = err.to_string();
        assert!(err.contains("refusing to write"), "got: {err}");
        // An ABSENT file is the one case where `device init` is right and `rm`
        // is harmless — the guidance must say exactly that.
        assert!(
            err.contains("does not exist"),
            "the absent case must name itself as absent, got: {err}"
        );
        assert!(
            err.contains("qontinui_profile device init"),
            "the absent case must point at `device init`, got: {err}"
        );
        assert!(
            !path.exists(),
            "the refusal must not leave a device_id-less machine.json behind"
        );
    }

    /// FAIL CLOSED on an unparseable, non-object, or `device_id`-less file —
    /// leave it byte-identical for inspection, AND give guidance specific to
    /// that case.
    ///
    /// Asserting only `contains("refusing to write")` is what let the original
    /// defect through: `read_machine_file` returns `None` for BOTH an absent
    /// and a corrupt file, and the single shared message told the operator to
    /// run `device init` — which itself refuses on a corrupt file and used to
    /// say `rm`. `rm` + `device init` mints a fresh UUID, i.e. the second
    /// `coord.devices` row this whole change exists to prevent. So the pin is
    /// per-case: does the message name THIS file's class, and does it give the
    /// right (non-re-minting) recovery for it?
    #[test]
    fn write_refuses_with_guidance_specific_to_each_bad_file() {
        let dir = tempdir().unwrap();
        for (label, contents, must_contain, must_not_contain) in [
            // Present but unreadable: the bytes may still hold the real UUID,
            // so the advice is hand-repair and an explicit "do NOT rm".
            (
                "corrupt",
                &b"{ not json"[..],
                &["EXISTS but is unreadable", "do NOT `rm`"][..],
                &["device init"][..],
            ),
            (
                "not an object",
                &b"[1,2,3]"[..],
                &["not a JSON object", "do NOT `rm`"][..],
                &["device init"][..],
            ),
            // Parsed fine and provably carries no identity: nothing to lose.
            (
                "no device_id",
                &br#"{"hostname":"spaceship"}"#[..],
                &["carries no device_id"][..],
                &[][..],
            ),
            (
                "blank device_id",
                &br#"{"device_id":"   "}"#[..],
                &["carries no device_id"][..],
                &[][..],
            ),
        ] {
            let path = dir.path().join(format!("{}.json", label.replace(' ', "_")));
            std::fs::write(&path, contents).unwrap();
            let err = match write_active_tenant_id(&path, "tenant-uuid") {
                Ok(()) => panic!("{label} must be refused, not written"),
                Err(e @ MachineJsonWriteError::Refused(_)) => e.to_string(),
                Err(e) => panic!("{label} must be a refusal, not an I/O failure: {e:?}"),
            };
            assert!(
                err.contains("refusing to write"),
                "{label}: expected a refusal, got: {err}"
            );
            for needle in must_contain {
                assert!(
                    err.contains(needle),
                    "{label}: refusal must mention {needle:?}, got: {err}"
                );
            }
            for needle in must_not_contain {
                assert!(
                    !err.contains(needle),
                    "{label}: refusal must NOT mention {needle:?} (that path re-mints), got: {err}"
                );
            }
            assert_eq!(
                std::fs::read(&path).unwrap(),
                contents,
                "{label}: the file must be left byte-identical for inspection"
            );
        }
    }

    /// The absent and the present-but-corrupt refusals must not be the SAME
    /// message. They came from one `None` arm and therefore were, which is how
    /// a corrupt file's operator got told to run `device init` → which refuses
    /// → whose message said `rm` → which mints a new identity.
    #[test]
    fn absent_and_corrupt_refusals_are_not_the_same_message() {
        let dir = tempdir().unwrap();
        let absent = dir.path().join("absent.json");
        let corrupt = dir.path().join("corrupt.json");
        std::fs::write(&corrupt, b"{ not json").unwrap();

        let absent_err = write_active_tenant_id(&absent, "t")
            .expect_err("absent must refuse")
            .to_string();
        let corrupt_err = write_active_tenant_id(&corrupt, "t")
            .expect_err("corrupt must refuse")
            .to_string();
        assert_ne!(
            absent_err.replace("absent.json", "X").as_str(),
            corrupt_err.replace("corrupt.json", "X").as_str(),
            "absent and corrupt need OPPOSITE guidance, so they cannot share a message"
        );
    }

    /// A padded on-disk `device_id` is normalized on write. Every READER trims
    /// (`machine_identity::read_device_id_at`), so leaving `" abc "` on disk
    /// means disk and the coord-facing value disagree by exactly the padding —
    /// and coord UPSERTs `ON CONFLICT (device_id)`, so that is two rows.
    #[test]
    fn write_trims_a_padded_device_id() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("machine.json");
        std::fs::write(&path, br#"{"device_id":"  padded-id  ","hostname":"x"}"#).unwrap();

        write_active_tenant_id(&path, "tenant-uuid").unwrap();

        let v = read_machine_file(&path).unwrap();
        assert_eq!(
            v.get("device_id").and_then(|x| x.as_str()),
            Some("padded-id"),
            "the identity must be written back TRIMMED, matching what readers see"
        );
    }

    /// Same normalization for the legacy `machine_id` spelling.
    #[test]
    fn write_trims_a_padded_legacy_machine_id() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("machine.json");
        std::fs::write(&path, br#"{"machine_id":"  legacy-id  "}"#).unwrap();

        write_active_tenant_id(&path, "tenant-uuid").unwrap();

        let v = read_machine_file(&path).unwrap();
        assert_eq!(
            v.get("machine_id").and_then(|x| x.as_str()),
            Some("legacy-id")
        );
    }

    /// A legacy `machine_id`-spelled file still HAS an identity, so a tenant
    /// switch on it is allowed (every reader aliases the old spelling).
    #[test]
    fn write_accepts_the_legacy_machine_id_spelling() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("machine.json");
        std::fs::write(&path, br#"{"machine_id":"legacy-id","hostname":"x"}"#).unwrap();
        write_active_tenant_id(&path, "tenant-uuid").unwrap();
        let v = read_machine_file(&path).unwrap();
        assert_eq!(
            v.get("machine_id").and_then(|x| x.as_str()),
            Some("legacy-id")
        );
        assert_eq!(read_active_tenant_id(&path).as_deref(), Some("tenant-uuid"));
    }

    /// Defect 4: this writer must not use the single shared
    /// `machine.json.tmp` path, which every runner instance races.
    #[test]
    fn write_does_not_use_the_shared_fixed_temp_path() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("machine.json");
        std::fs::write(&path, br#"{"device_id":"abc","hostname":"x"}"#).unwrap();
        let squatted = path.with_extension("json.tmp");
        std::fs::create_dir(&squatted).unwrap();

        write_active_tenant_id(&path, "tenant-uuid")
            .expect("write must succeed despite the squatted legacy tmp path");
        assert_eq!(read_active_tenant_id(&path).as_deref(), Some("tenant-uuid"));
        assert!(squatted.is_dir(), "the squatted path must be untouched");
    }

    // ------------------------------------------------------------------
    // The shared path — plan
    // `2026-09-23-remote-create-residuals-after-coord-registration-confirm`
    // Phase 4. The Tauri command and `PUT /tenant/active` both go through
    // `apply_active_tenant`; these pin the command side of that sharing.
    // ------------------------------------------------------------------

    const BOUND: &str = "aaaaaaaa-0000-4000-8000-00000000000a";
    const UNBOUND: &str = "cccccccc-0000-4000-8000-00000000000c";

    fn bound_fixture() -> (
        qontinui_runner_lib::ambient::test_support::IsolatedAmbient,
        PathBuf,
    ) {
        let amb = qontinui_runner_lib::ambient::test_support::IsolatedAmbient::new();
        let machine = amb.write_machine_json(r#"{"device_id":"dev-1","hostname":"box"}"#);
        std::fs::write(
            amb.dir().join("paired_user.json"),
            format!(r#"{{"user_id":"u","tenant_id":"{BOUND}"}}"#),
        )
        .unwrap();
        (amb, machine)
    }

    #[test]
    fn set_active_tenant_command_uses_the_shared_path_and_reports_effect_timing() {
        let (_amb, machine) = bound_fixture();
        let resp = set_active_tenant(BOUND.to_string()).expect("a bound tenant is accepted");
        let data = resp.data.expect("payload");
        assert_eq!(data["active_tenant_id"], BOUND);
        assert_eq!(data["takes_effect"], TAKES_EFFECT);
        assert!(data["existing_sessions"]["follows_machine_pin"]["count"].is_u64());
        assert_eq!(read_active_tenant_id(&machine).as_deref(), Some(BOUND));
    }

    #[test]
    fn set_active_tenant_command_refuses_an_unbound_tenant_without_writing() {
        let (_amb, machine) = bound_fixture();
        let before = std::fs::read(&machine).unwrap();
        let err = set_active_tenant(UNBOUND.to_string()).expect_err("unbound must be refused");
        assert!(err.contains("not bound"), "got: {err}");
        assert_eq!(std::fs::read(&machine).unwrap(), before);
    }

    #[test]
    fn apply_refuses_before_reading_bindings_on_a_malformed_id() {
        let (_amb, _machine) = bound_fixture();
        assert_eq!(
            apply_active_tenant("nope"),
            Err(SetActiveTenantError::Malformed("nope".into()))
        );
        assert_eq!(apply_active_tenant("  "), Err(SetActiveTenantError::Empty));
    }
}
