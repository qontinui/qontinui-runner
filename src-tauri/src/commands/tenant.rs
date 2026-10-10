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
//! rows 2-4), so an unpinned -> pinned switch moves running sessions too. The
//! dual-write gate follows a switch within one flag-poll interval, because its
//! poll re-reads the pin each tick. Only the coord-mcp nonce restore and the
//! boot-time on-disk nonce adoption read the pin once at startup, so they see
//! a switch only at the next runner start. [`PIN_SURFACES`] is the
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
    /// The functions that read the pin DIRECTLY for this surface, as
    /// `<path under src-tauri/src>::<fn>`. Structured (one reader per entry)
    /// so the source-scan guard can check it: a direct read of one of the five
    /// pin tokens in a function named neither here nor in
    /// [`PIN_READER_EXCLUSIONS`] fails the tests. Callers of a WRAPPER are not
    /// checked; see [`PIN_SURFACES`] for that limit.
    #[serde(rename = "evidence")]
    pub readers: &'static [&'static str],
    pub detail: &'static str,
}

/// Every consumer of the machine pin, classified per-use vs startup.
///
/// Partly guarded by `commands::tenant::tests::every_pin_reader_is_classified`,
/// which scans `src-tauri/src` (inline test modules excluded) for each DIRECT
/// call or path reference to `resolve_tenant_pin`, `resolve_active_tenant_id`,
/// `active_tenant_uuid`, `active_tenant_id_str` and `machine_pin_tenant`, and
/// requires the enclosing function to be named here or in
/// [`PIN_READER_EXCLUSIONS`].
///
/// **What the guard does NOT catch.** It sees direct readers only. A new
/// caller of a WRAPPER — the per-module `resolve_tenant_id`s,
/// `read_active_tenant_id`, `current_machine_pin`, `resolve_new_session_tenant`
/// and the like — is not checked, so a new startup-time caller of a wrapper
/// would be misreported as live. Nor would it notice a wrapper that started
/// caching its read (a `LazyLock`/`OnceLock` behind it). The wrappers' current
/// callers were classified by reading them; the guard keeps the DIRECT set
/// complete, not the transitive one.
///
/// Sessions that ALREADY exist are not a surface here. Whether one moves
/// depends on how its tenant was held at creation, which is what the
/// `existing_sessions` block of the response reports
/// ([`crate::coord_mcp::device_session_pin_census`]).
pub(crate) const PIN_SURFACES: &[PinSurface] = &[
    PinSurface {
        surface: "new_session_tenant",
        timing: PinTiming::Live,
        readers: &["session/mod.rs::resolve_new_session_tenant"],
        detail: "the tenant a NEW session is recorded under, read at its creation \
                 (also reached through claude_session/coord_register.rs, which calls this \
                 resolver per registration)",
    },
    PinSurface {
        surface: "coord_mcp_mint_and_provisioning",
        timing: PinTiming::Live,
        readers: &[
            "coord_mcp.rs::mint_and_register_nonce_with",
            "coord_mcp.rs::reusable_in_cwd_device_nonce",
            "coord_mcp.rs::cwd_key_pinned_away_from_machine",
            "coord_mcp.rs::record_terminal_coord_mcp_delivery",
            "coord_mcp.rs::deliver_terminal_coord_mcp_unrecorded",
        ],
        detail: "a new coord-mcp key with no spawn-chosen tenant samples the pin at mint and, \
                 if pinned, freezes it for the key's life; provisioning decides key reuse and \
                 records the spawn-default pin per terminal",
    },
    PinSurface {
        surface: "coord_mcp_unpinned_session_requests",
        timing: PinTiming::Live,
        readers: &[
            "coord_mcp.rs::session_tenant_or_refuse",
            "coord_mcp.rs::session_tenant_decision",
        ],
        detail: "a key frozen unpinned (live or graced) re-reads the pin on every proxied \
                 request (decide_session_tenant rows 2-4); one frozen pinned (row 1) does not",
    },
    PinSurface {
        surface: "device_jwt_refresher",
        timing: PinTiming::Live,
        readers: &[
            "mcp/device_jwt_refresher.rs::refresher_loop",
            "mcp/device_jwt_refresher.rs::publish_coord_credential_status",
            "mcp/device_jwt_refresher.rs::read_sweep_inputs",
        ],
        detail: "re-read on each refresher tick (5 min) and each slot sweep",
    },
    PinSurface {
        surface: "device_level_publishers",
        timing: PinTiming::Live,
        readers: &[
            "agent_worktree/census.rs::resolve_tenant_id",
            "agent_worktree/fs_backstop.rs::resolve_tenant_id",
            "agent_worktree/maintenance_executor.rs::resolve_tenant_id",
            "fleet/resource_sample.rs::resolve_tenant_id",
        ],
        detail: "each module's resolver is called per publish/tick (census build_and_publish \
                 and resolve_volume_poster, which rebuilds the poster when the tenant in its \
                 key changes; fs_backstop tick_once; maintenance report_reset_git_op; \
                 resource_sample publish_once)",
    },
    PinSurface {
        surface: "register_heartbeat_slot_fallback_default",
        timing: PinTiming::Live,
        readers: &["fleet.rs::read_active_tenant"],
        detail: "the register heartbeat's DEFAULT tenant when paired_user.json and a usable \
                 legacy slot both miss (fleet.rs resolve_heartbeat_binding_set): re-read on \
                 every heartbeat (30 s) that reaches the per-tenant-slot fallback, and honoured \
                 only while it is among the usable slots coord's bound set lists \
                 (heartbeat_slot_fallback); no other resolver reads it",
    },
    PinSurface {
        surface: "session_outbox_tenant_backfill",
        timing: PinTiming::Live,
        readers: &[
            "session/coord_sync.rs::push_record",
            "session/coord_sync.rs::rebuild_create_body",
        ],
        detail: "fills the tenant only for records that carry none, read per record",
    },
    PinSurface {
        surface: "per_request_readouts",
        timing: PinTiming::Live,
        readers: &[
            "mcp_api.rs::health",
            "commands/session_info.rs::read_session_tenancy",
            "mcp/ui_bridge/gated_flow.rs::ui_bridge_session_handler",
            "repo_detection.rs::register_repo_with_coord",
            "coord_mcp.rs::report_for",
            "commands/tenant.rs::active_tenant_view",
            "commands/tenant.rs::read_active_tenant_id",
        ],
        detail: "read per request (/health, session info, the ui-bridge session route, repo \
                 registration, the coord-mcp doctor, GET /tenant/active)",
    },
    PinSurface {
        surface: "session_coordination_dual_write_gate",
        timing: PinTiming::Live,
        readers: &["session/coord_sync.rs::run_flag_poll_loop"],
        detail: "the flag poll re-reads the pin each tick, so a switch takes effect within about \
                 one QONTINUI_SESSION_FLAG_POLL_SECS interval (default 60s), plus at most one \
                 fetch already in flight for the previous tenant",
    },
    PinSurface {
        surface: "coord_mcp_nonce_restore",
        timing: PinTiming::NextStart,
        readers: &["coord_mcp.rs::restore_proxy_nonces_from"],
        detail: "persisted keys with no recorded tenant take the pin as sampled once at boot; \
                 keys already restored count under existing_sessions",
    },
    PinSurface {
        surface: "coord_mcp_on_disk_nonce_adoption",
        timing: PinTiming::NextStart,
        readers: &["coord_mcp.rs::adopt_on_disk_nonce"],
        detail: "the boot reconcile (reconcile_root_config_at, reconcile_session_configs, run \
                 from the mcp_api startup block) adopts on-disk .mcp.json keys with the pin as \
                 read then; adopted keys count under existing_sessions",
    },
];

/// Pin readers that are deliberately NOT a [`PinSurface`], with the reason.
/// Same `<path>::<fn>` form; the guard test accepts a function named here.
pub(crate) const PIN_READER_EXCLUSIONS: &[(&str, &str)] = &[
    (
        "ambient.rs::active_tenant_uuid",
        "the accessor itself; classified at its callers",
    ),
    (
        "session/dual_write.rs::resolve_active_tenant_id",
        "a delegate to resolve_tenant_pin; every caller is scanned and classified",
    ),
    (
        "coord_doctor.rs::read_active_tenant_id_from_machine_json",
        "diagnostic; reads per invocation and changes nothing",
    ),
    (
        "coord_doctor.rs::read_coord_tenant_bindings",
        "diagnostic; reads per invocation and changes nothing",
    ),
    (
        "coord_doctor.rs::resolve_coord_mcp_door",
        "diagnostic; reads per invocation and changes nothing",
    ),
    (
        "session_archive/mod.rs::machine_pin_tenant",
        "CLI only (qontinui-pr session-archive backfill), not the runner process",
    ),
    (
        "bin/qontinui_cli.rs::session_archive_backfill",
        "CLI only, not the runner process",
    ),
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
/// `existing_sessions` is measured, not asserted: a coord-mcp key (live or
/// graced) frozen `Pinned` at creation keeps its tenant, while one frozen
/// unpinned — the normal single-tenant shape, and a restored or adopted key
/// with no recorded tenant — follows the new pin on its next request. So an
/// unpinned → pinned switch, the likely first headless use, DOES move running
/// sessions. Reads both proxy registries, so call it off the async executor.
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
            "scope": "device coord-mcp keys in this runner process, live and graced \
                      (evicted keys keep serving for the grace TTL)",
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

    // ------------------------------------------------------------------
    // PIN_SURFACES is checked against the SOURCE, not against itself.
    // ------------------------------------------------------------------

    /// The pin-reading spellings the guard looks for.
    const PIN_READER_TOKENS: &[&str] = &[
        "resolve_tenant_pin",
        "resolve_active_tenant_id",
        "active_tenant_uuid",
        "active_tenant_id_str",
        "machine_pin_tenant",
    ];

    fn src_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src")
    }

    fn is_ident(c: char) -> bool {
        c.is_ascii_alphanumeric() || c == '_'
    }

    /// The name after the first `fn ` on `line`, if any.
    fn fn_name_on(line: &str) -> Option<String> {
        let mut rest = line;
        while let Some(i) = rest.find("fn ") {
            let before_ok = rest
                .get(..i)
                .and_then(|b| b.chars().last())
                .is_none_or(|c| !is_ident(c));
            let after = rest.get(i + 3..).unwrap_or("");
            if before_ok {
                let name: String = after
                    .trim_start()
                    .chars()
                    .take_while(|c| is_ident(*c))
                    .collect();
                if !name.is_empty() {
                    return Some(name);
                }
            }
            rest = after;
        }
        None
    }

    /// Does `line` CALL or PATH-REFERENCE `tok` (not merely name a field or
    /// define the fn)? A use is `tok(` or `::tok` / `.tok(`.
    fn uses_token(line: &str, tok: &str) -> bool {
        let mut from = 0;
        while let Some(off) = line.get(from..).and_then(|rest| rest.find(tok)) {
            let i = from + off;
            let end = i + tok.len();
            let before = line.get(..i).unwrap_or("");
            let after = line.get(end..).unwrap_or("");
            let bounded = before.chars().last().is_none_or(|c| !is_ident(c))
                && after.chars().next().is_none_or(|c| !is_ident(c));
            let is_def = before.trim_end().ends_with("fn");
            // Inside a string literal (an odd number of quotes before it) is a
            // mention, not a read — `PIN_READER_EXCLUSIONS` itself is one.
            let in_string = before.matches('"').count() % 2 == 1;
            let is_use = after.starts_with('(') || before.ends_with("::");
            if bounded && !is_def && is_use && !in_string {
                return true;
            }
            from = end;
        }
        false
    }

    /// If `lines[i]` opens an INLINE column-0 test module (`#[cfg(test)]`,
    /// further attributes, then `mod name {`), the index just past its
    /// column-0 closing `}`; otherwise `None`.
    ///
    /// An OUT-OF-LINE declaration (`mod name;`) has no body here — its code
    /// lives in another file, which the walk visits on its own — so it is not
    /// skipped. Treating it as a body swallowed the production code after it
    /// up to the next column-0 `}` (477 lines and 11 fns of `main.rs`).
    fn test_mod_skip_end(lines: &[&str], i: usize) -> Option<usize> {
        if lines.get(i).copied() != Some("#[cfg(test)]") {
            return None;
        }
        let mut j = i + 1;
        while lines.get(j).is_some_and(|l| l.starts_with("#[")) {
            j += 1;
        }
        let next = lines.get(j).copied().unwrap_or("");
        let is_mod = next.starts_with("mod ")
            || next.starts_with("pub mod ")
            || next.starts_with("pub(crate) mod ");
        // Judge the declaration on its code alone: `mod x; // why` is still
        // out-of-line.
        let code = next.split("//").next().unwrap_or("").trim_end();
        if !is_mod || code.ends_with(';') {
            return None;
        }
        // A one-line body (`mod x {}` / `mod x { ... }`) ends on this line.
        if code.contains('{') && code.matches('{').count() == code.matches('}').count() {
            return Some(j + 1);
        }
        let mut k = j + 1;
        while k < lines.len() && lines[k] != "}" {
            k += 1;
        }
        Some(k + 1)
    }

    /// Every `(path::fn)` whose body reads the pin, outside test modules.
    ///
    /// Heuristics, stated so a failure is readable: a column-0
    /// `#[cfg(test)]` followed by a column-0 `mod` WITH A BODY skips to the
    /// next column-0 `}` (or past its own line, for a one-line body); an
    /// out-of-line `mod name;` (trailing `//` comment allowed) is not skipped,
    /// because its code lives in another file the walk visits on its own
    /// ([`test_mod_skip_end`]); files named `tests.rs` or under a `tests/` dir are
    /// skipped; `//` lines are skipped; the enclosing function is the last
    /// `fn <name>` seen above the use.
    fn scan_pin_readers() -> std::collections::BTreeSet<String> {
        let root = src_root();
        let mut out = std::collections::BTreeSet::new();
        let mut stack = vec![root.clone()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    if path.file_name().is_some_and(|n| n != "tests") {
                        stack.push(path);
                    }
                    continue;
                }
                if path.extension().and_then(|e| e.to_str()) != Some("rs")
                    || path.file_name().is_some_and(|n| n == "tests.rs")
                {
                    continue;
                }
                let rel = path
                    .strip_prefix(&root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                let src = std::fs::read_to_string(&path).unwrap();
                let lines: Vec<&str> = src.lines().collect();
                let mut current_fn = String::from("<none>");
                let mut i = 0;
                while i < lines.len() {
                    let line = lines[i];
                    if let Some(resume) = test_mod_skip_end(&lines, i) {
                        i = resume;
                        continue;
                    }
                    if line.trim_start().starts_with("//") {
                        i += 1;
                        continue;
                    }
                    if let Some(name) = fn_name_on(line) {
                        current_fn = name;
                    }
                    if PIN_READER_TOKENS.iter().any(|t| uses_token(line, t)) {
                        out.insert(format!("{rel}::{current_fn}"));
                    }
                    i += 1;
                }
            }
        }
        out
    }

    #[test]
    fn every_pin_reader_is_classified() {
        let named: std::collections::BTreeSet<&str> = PIN_SURFACES
            .iter()
            .flat_map(|s| s.readers.iter().copied())
            .chain(PIN_READER_EXCLUSIONS.iter().map(|(r, _)| *r))
            .collect();
        let found = scan_pin_readers();
        assert!(
            found.len() >= 20,
            "the scan found only {} readers — it has stopped seeing the tree: {found:?}",
            found.len()
        );
        let unclassified: Vec<&String> = found
            .iter()
            .filter(|r| !named.contains(r.as_str()))
            .collect();
        assert!(
            unclassified.is_empty(),
            "pin readers named in neither PIN_SURFACES nor PIN_READER_EXCLUSIONS — classify \
             each as live or next_start (read the code), or exclude it with a reason: \
             {unclassified:?}"
        );
    }

    /// Every named reader names a function that EXISTS in that file, so an
    /// entry cannot rot into a pointer at nothing.
    #[test]
    fn every_named_reader_exists() {
        let root = src_root();
        for reader in PIN_SURFACES
            .iter()
            .flat_map(|s| s.readers.iter().copied())
            .chain(PIN_READER_EXCLUSIONS.iter().map(|(r, _)| *r))
        {
            let (file, func) = reader.rsplit_once("::").expect("<path>::<fn>");
            let src = std::fs::read_to_string(root.join(file))
                .unwrap_or_else(|e| panic!("{reader}: {file} unreadable: {e}"));
            assert!(
                src.lines().any(|l| fn_name_on(l).as_deref() == Some(func)),
                "{reader}: no `fn {func}` in {file}"
            );
        }
    }

    /// The startup readers, exactly — by function, not by surface label.
    #[test]
    fn next_start_readers_are_exactly_the_startup_reads() {
        let next_start: std::collections::BTreeSet<&str> = PIN_SURFACES
            .iter()
            .filter(|s| s.timing == PinTiming::NextStart)
            .flat_map(|s| s.readers.iter().copied())
            .collect();
        assert_eq!(
            next_start,
            [
                "coord_mcp.rs::adopt_on_disk_nonce",
                "coord_mcp.rs::restore_proxy_nonces_from",
            ]
            .into_iter()
            .collect()
        );
    }

    /// The scanner's own heuristics, on synthetic source.
    #[test]
    fn token_use_detection_ignores_fields_and_definitions() {
        assert!(uses_token(
            "let p = resolve_tenant_pin();",
            "resolve_tenant_pin"
        ));
        assert!(uses_token(
            ".or_else(crate::x::resolve_active_tenant_id)",
            "resolve_active_tenant_id"
        ));
        assert!(uses_token("doc.active_tenant_uuid()", "active_tenant_uuid"));
        assert!(!uses_token(
            "pub fn resolve_tenant_pin() -> T {",
            "resolve_tenant_pin"
        ));
        assert!(!uses_token(
            "    pub machine_pin_tenant: Option<Uuid>,",
            "machine_pin_tenant"
        ));
        assert!(!uses_token(
            "opts.machine_pin_tenant,",
            "machine_pin_tenant"
        ));
        assert!(!uses_token("resolve_tenant_pins()", "resolve_tenant_pin"));
        assert!(!uses_token(
            r#"        "session/dual_write.rs::resolve_active_tenant_id","#,
            "resolve_active_tenant_id"
        ));
        assert_eq!(
            fn_name_on("    pub(crate) async fn foo_bar(x: u8) {").as_deref(),
            Some("foo_bar")
        );
        assert_eq!(fn_name_on("let f = |x| x;"), None);

        // An inline test module is skipped to its column-0 `}`...
        let inline = [
            "#[cfg(test)]",
            "mod tests {",
            "    fn t() {}",
            "}",
            "fn after() {}",
        ];
        assert_eq!(test_mod_skip_end(&inline, 0), Some(4));
        // ...with stacked attributes too...
        let attrs = [
            "#[cfg(test)]",
            "#[allow(dead_code)]",
            "pub(crate) mod t {",
            "}",
        ];
        assert_eq!(test_mod_skip_end(&attrs, 0), Some(4));
        // ...but an OUT-OF-LINE declaration is not, so the production code
        // after it stays visible to the scan.
        let out_of_line = [
            "#[cfg(test)]",
            "mod runner_spawn_sites;",
            "fn boot() { let _ = resolve_tenant_pin(); }",
            "}",
        ];
        assert_eq!(test_mod_skip_end(&out_of_line, 0), None);
        // ...including one carrying a trailing comment, a style main.rs uses.
        let commented = [
            "#[cfg(test)]",
            "mod runner_spawn_sites; // spawn-site census",
            "fn boot() { let _ = resolve_tenant_pin(); }",
            "}",
        ];
        assert_eq!(test_mod_skip_end(&commented, 0), None);
        // A one-line body is skipped past its own line only.
        let one_line = ["#[cfg(test)]", "mod t {}", "fn after() {}", "}"];
        assert_eq!(test_mod_skip_end(&one_line, 0), Some(2));
        // And a non-module `#[cfg(test)]` item is not a module skip either.
        assert_eq!(
            test_mod_skip_end(&["#[cfg(test)]", "fn helper() {}"], 0),
            None
        );
    }
}
