//! ONE durable session-identity read path — the projection behind the Terminal
//! zone-header session-info dropdown (plan
//! `2026-08-16-runner-session-info-dropdown-and-restore-verification`, D1/P3).
//!
//! ## Why one command instead of three
//!
//! The dropdown needs the durable lifecycle record, the process-lifetime live
//! registry overlay (account + the name the operator actually sees) and the
//! runner-local PR ledger. Assembling those from three separate frontend calls
//! produces a partially-loaded panel whose blank rows are indistinguishable
//! from genuinely-absent data — exactly the ambiguity G5 describes. So
//! [`session_info_get`] returns the WHOLE projection in one shot, and the
//! frontend renders it or renders its `reason`.
//!
//! ## Every unknown is spelled
//!
//! `prs.status: "unavailable"` WITH a `reason` is a different state from
//! `openCount: 0`, and a PR whose land verdict could not be evaluated
//! (`land-unknown`) is reported in its own `unknown` bucket with the recorded
//! reason — never folded into a confident "not landed". That is served policy
//! `verification-and-evidence` `silent-empty-is-unknown` applied to this
//! surface; it is the whole point of the surface.
//!
//! ## Pure core, thin shell
//!
//! Everything that decides a value lives in a pure function over plain data
//! ([`project_session_info`], [`project_prs`], [`prs_unavailable`]), so the
//! projection unit-tests without a Tauri handle, a live disk or PG — the idiom
//! [`crate::install_effects_producer::project_restore_health`] established. The
//! Tauri command and the HTTP twin (`GET /control/sessions/info`) are both thin
//! shells over the same functions, so the rendered and raw read paths cannot
//! drift (D5).

use std::sync::Arc;

use serde::Serialize;

use crate::database::pg::session_pr_ops::SessionPrRow;
use crate::session::claude_session_registry::LiveClaudeSession;
use crate::session::session_lifecycle_store::{
    SessionLifecycleStore, TerminalSessionRecord, NAME_SOURCE_DERIVED, NAME_SOURCE_OPERATOR,
};

/// `name.source` when nothing ever reported a name provenance for this session
/// — a record written before the D1 fields existed, or one whose process died
/// before the live registry was read. Deliberately NOT collapsed into
/// `"operator"`: "nobody told us" and "an operator chose it" are different
/// claims (R2 — never back-fill by guessing).
pub const NAME_SOURCE_UNKNOWN: &str = "unknown";

/// `prs.status` / envelope status: the ledger answered.
pub const STATUS_OK: &str = "ok";
/// `prs.status` / envelope status: the ledger could NOT answer, and `reason`
/// says why. Renders differently from an empty-but-known ledger.
pub const STATUS_UNAVAILABLE: &str = "unavailable";

/// The land signal the reconciler writes when it could not evaluate a land
/// verdict at all (see `session_pr_reconciler::land_verdict`). Such a row is
/// surfaced in [`SessionPrs::unknown`], never counted as not-landed.
const LAND_SIGNAL_UNKNOWN: &str = "land-unknown";

/// `identityEvidence`: a provider hook fired for this id — a REAL provider
/// started in this terminal.
pub const IDENTITY_CONFIRMED: &str = "confirmed";
/// `identityEvidence`: no hook, but a transcript for this id exists on disk —
/// the session demonstrably produced output.
pub const IDENTITY_TRANSCRIPT: &str = "transcript";
/// `identityEvidence`: NEITHER. The record is the spawn-time identity seam and
/// nothing has corroborated it, so the id and account are a PREDICTION about a
/// provider that may never start here.
pub const IDENTITY_PROVISIONAL: &str = "provisional";

/// Classify how much evidence backs this record's identity. Pure.
///
/// `apply_identity_seam` (`terminal/session.rs:803`) writes an
/// `origin: "authoritative"` record with a pinned session id and a
/// roster-selected account for **EVERY** terminal — including a plain shell
/// that never runs a provider, because at spawn time the runner cannot know
/// whether the operator will type `claude`, run `ls`, or sit at a prompt
/// (`session_lifecycle_store.rs:401-413`). So `origin` is NOT evidence: it
/// records who wrote the row, not whether a session exists.
///
/// Measured 2026-08-18: a bare `POST /terminals` alone yielded
/// `acct: tiohorst, origin: authoritative, confirmed: false,
/// transcriptExists: false` — a PowerShell shell advertising an account and a
/// Claude id. On a machine with a populated account roster that is EVERY
/// terminal, which is what this classification exists to keep the UI honest
/// about.
///
/// This is deliberately the SAME predicate the restore classifier already
/// gates auto-resume on (`confirmed_at` OR a real transcript) — one definition,
/// two consumers, so the panel can never disagree with what restore will do.
pub fn classify_identity_evidence(confirmed: bool, transcript_exists: bool) -> &'static str {
    if confirmed {
        IDENTITY_CONFIRMED
    } else if transcript_exists {
        IDENTITY_TRANSCRIPT
    } else {
        IDENTITY_PROVISIONAL
    }
}

/// Is `v` one of the three `identityEvidence` values? Pure; shared by the
/// debug seam's validation and its tests so the accepted set can never drift
/// from the classifier's output set.
pub fn is_identity_evidence(v: &str) -> bool {
    matches!(
        v,
        IDENTITY_CONFIRMED | IDENTITY_TRANSCRIPT | IDENTITY_PROVISIONAL
    )
}

/// Debug-only forced `identityEvidence`, driven by
/// `POST /ui-bridge/test/force-identity-evidence` (see `mcp::test_fixtures`).
///
/// The `provisional` treatment (the amber panel note plus the `— provisional`
/// row suffixes) was UNVERIFIABLE end-to-end before this seam existed: a bare
/// terminal carries no `claudeSessionId` yet, so no dropdown mounts at all, and
/// any session that DOES bind gets hook-confirmed within seconds — so the
/// rendering had never once been observed. Forcing the classification is the
/// only way to drive the treatment through the real projection, the real
/// command, and the real component.
///
/// Cfg-gated to exactly the same builds as `mcp::test_fixtures` itself, so a
/// release binary has no override slot, no setter and no read — the classifier
/// result is returned unconditionally there.
#[cfg(any(debug_assertions, feature = "test-fixtures"))]
mod evidence_override {
    use std::sync::{OnceLock, RwLock};

    fn slot() -> &'static RwLock<Option<String>> {
        static SLOT: OnceLock<RwLock<Option<String>>> = OnceLock::new();
        SLOT.get_or_init(|| RwLock::new(None))
    }

    /// Install (`Some`) or clear (`None`) the override; returns the PREVIOUS
    /// value so a caller can report what it replaced. A poisoned lock is
    /// treated as "no override" rather than panicking a request handler.
    pub fn set(next: Option<String>) -> Option<String> {
        match slot().write() {
            Ok(mut guard) => std::mem::replace(&mut *guard, next),
            Err(_) => None,
        }
    }

    /// The current override, if any.
    pub fn get() -> Option<String> {
        slot().read().ok().and_then(|g| g.clone())
    }
}

/// Install or clear the debug `identityEvidence` override. Returns the
/// previous value. See [`evidence_override`].
#[cfg(any(debug_assertions, feature = "test-fixtures"))]
pub fn set_forced_identity_evidence(next: Option<String>) -> Option<String> {
    evidence_override::set(next)
}

/// The debug `identityEvidence` override currently installed, if any.
#[cfg(any(debug_assertions, feature = "test-fixtures"))]
pub fn forced_identity_evidence() -> Option<String> {
    evidence_override::get()
}

/// The `identityEvidence` this projection reports: the classifier's verdict,
/// unless a debug seam has forced one.
///
/// Applied HERE (in the shared pure projection) rather than in the Tauri
/// command so the HTTP twin and the command cannot disagree about what the
/// panel is being shown.
fn resolve_identity_evidence(confirmed: bool, transcript_exists: bool) -> String {
    #[cfg(any(debug_assertions, feature = "test-fixtures"))]
    if let Some(forced) = forced_identity_evidence() {
        return forced;
    }
    classify_identity_evidence(confirmed, transcript_exists).to_string()
}

/// The four identifiers a session carries (D2). All four are shown rather than
/// guessing which two the operator meant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionIdentity {
    /// The `--resume` key and the registry's map key.
    pub claude_session_id: String,
    /// The runner PTY/terminal uuid currently hosting the session.
    pub terminal_id: String,
    /// Coord-minted fleet session handle (`fsh_…`). Already durable on the
    /// record as `handle` — read, never re-minted here.
    pub fleet_session_handle: Option<String>,
    pub tenant_id: Option<String>,
    /// Set only for worker (machine-spawned) sessions.
    pub task_run_id: Option<String>,
}

/// The name the operator sees, plus where it came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionName {
    /// `None` ⇒ no name was ever observed (NOT "the session is unnamed").
    pub value: Option<String>,
    /// `"operator"` | `"derived"` | `"unknown"`.
    pub source: String,
}

/// Which Claude account the session runs under — the G1 answer that used to die
/// with the process.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionAccount {
    pub label: Option<String>,
    /// CLI wrapper (`clp`, `clg`, …) — the half of the resume command that
    /// names the account.
    pub wrapper: Option<String>,
    pub config_dir: Option<String>,
}

/// Where the session's tile lives in the grid, and where the shell runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionPlacement {
    pub page_id: String,
    pub zone_index: i32,
    pub working_dir: Option<String>,
}

/// Lifecycle + restore state. `confirmed` / `transcriptExists` / `restorable`
/// are the SAME join `GET /control/sessions/restore-health` performs, so the
/// two surfaces can never disagree about whether a session is resumable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionLifecycleInfo {
    /// `"open"` | `"closed"`.
    pub state: String,
    pub provider: String,
    /// `"authoritative"` | `"observed"` | `"reconciled"`; `None` on a legacy
    /// row that asserted none.
    pub origin: Option<String>,
    pub opened_at: i64,
    pub last_seen_at: i64,
    pub closed_at: Option<i64>,
    pub close_reason: Option<String>,
    /// A provider hook (or a confirmed bind) proved a real session exists.
    pub confirmed: bool,
    /// A `*.jsonl` transcript for this id exists on disk.
    pub transcript_exists: bool,
    /// `confirmed && transcriptExists` — this id can actually be `--resume`d.
    pub restorable: bool,
    /// How much evidence backs this identity: `confirmed` | `transcript` |
    /// `provisional`. See [`classify_identity_evidence`]. A `provisional` row
    /// means the id and account are the spawn-time seam's PREDICTION, not an
    /// observation — the UI must not present them as fact.
    pub identity_evidence: String,
    /// Set iff a BOOT-RESTORE re-materialized this record (never for a freshly
    /// created session) — the restore census's only honest "it came back"
    /// signal.
    pub restored_from_boot_at: Option<i64>,
    /// `"resumed"` | `"terminal-only"` | `"failed"`; `None` = never restored,
    /// which is NOT the same claim as `"failed"`. RAW stored evidence — read
    /// `restore_status` for the verdict a human should be shown.
    pub restore_tier: Option<String>,
    /// Set while a boot-restore's `--resume` is in flight and unverified.
    /// Never projected before, which is why `restore_tier`'s deliberately
    /// pessimistic `failed` had to be read as terminal.
    pub restore_pending_at: Option<i64>,
    /// The RENDERED restore verdict — see
    /// [`crate::session::session_lifecycle_store::describe_restore_status`].
    /// A restore still in flight reads `pending (not yet confirmed)`; one whose
    /// marker outlived
    /// [`crate::session::session_lifecycle_store::RESTORE_PENDING_TTL_MS`]
    /// reads `failed (verification timed out)`.
    pub restore_status: String,
    /// `None` ⇒ never reported (unknown), not `false`.
    pub bypass_permissions: Option<bool>,
}

/// One PR this session authored (attributed by the `Session-Id:` git trailer).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionPrOpened {
    pub repo: String,
    pub pr_number: i64,
    pub branch: Option<String>,
    /// When the runner-local projection FIRST attributed this PR to the session
    /// (`project.session_prs.created_at`), RFC3339. The runner does not mirror
    /// GitHub's own `created_at`, so this is named for what it actually is.
    pub opened_at: Option<String>,
    /// `"open"` | `"closed"` | `"merged"` — the label the dropdown renders.
    pub pr_state: Option<String>,
}

/// One PR this session authored that some signal PROVED landed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionPrLanded {
    pub repo: String,
    pub pr_number: i64,
    /// RFC3339 land instant, by whichever signal proved it.
    pub landed_at: Option<String>,
    /// `"github-merge"` | `"ff-land"` | `"coord-label"` — WHY this counts as
    /// landed. Mirrored in `src/components/terminal/useSessionInfo.ts`
    /// (`SessionPrLanded.landSignal`); keep the two spellings in step.
    pub land_signal: Option<String>,
}

/// One PR whose land verdict could NOT be evaluated. Surfaced in its own bucket
/// because folding it into "not landed" asserts a negative the runner never
/// established — the R1 failure mode that would make the dropdown worse than no
/// dropdown.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionPrUnknown {
    pub repo: String,
    pub pr_number: i64,
    /// `"rebase_land_or_abandoned"` | `"coord_chip_on_open_pr"` |
    /// `"pr_state_unobserved"` | `"ref_stale"` | `"head_object_missing"` |
    /// `"no_base_ref"` |
    /// `"not_a_repo"` — or `"unspecified"` when a row somehow carries the
    /// unknown signal without its reason (still never a confident negative).
    pub reason: String,
}

/// The session's PR ledger, split opened / landed / unevaluable.
///
/// `opened` and `landed` are deliberately **NOT disjoint**: a PR opened and
/// landed by the same session appears in both, which is why the counts are
/// labelled `3 opened · 2 landed` rather than presented as a partition (D3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionPrs {
    /// [`STATUS_OK`] | [`STATUS_UNAVAILABLE`].
    pub status: String,
    /// REQUIRED whenever `status != "ok"`.
    pub reason: Option<String>,
    pub opened: Vec<SessionPrOpened>,
    pub landed: Vec<SessionPrLanded>,
    /// Rows whose land verdict could not be evaluated (`land-unknown`).
    pub unknown: Vec<SessionPrUnknown>,
    pub open_count: usize,
    pub landed_count: usize,
    pub unknown_count: usize,
    /// Has the reconciler EVER resolved a repo set for this session?
    ///
    /// `false` ⇒ nothing was ever looked at, so an empty ledger asserts
    /// NOTHING. This is the same G5 distinction `status: "unavailable"` draws
    /// for the store, applied one level down to the scan: before this field the
    /// dropdown rendered "no PRs attributed to this session" identically for a
    /// session that genuinely opened none and for one the reconciler silently
    /// dropped every tick because its cwd was the (non-repo) workspace parent.
    pub scanned: bool,
    /// The repo roots the reconciler last searched for this session. Empty WITH
    /// `scanned: true` ⇒ the cwd resolved to no git repositories at all.
    pub scanned_repos: Vec<String>,
}

/// `tenancy.credential.status` / `tenancy.credential.posture.status`: the value
/// was established.
pub const TENANCY_RESOLVED: &str = "resolved";
/// `tenancy.credential.posture.status`: a posture was published and it
/// describes this session's own credential slot.
pub const TENANCY_OBSERVED: &str = "observed";
/// Any tenancy value that could not be established — always with a `reason`.
pub const TENANCY_UNKNOWN: &str = "unknown";
/// `tenancy.row.spawnDeviceDefaultStatus`: the spawn-time default was recorded.
pub const TENANCY_RECORDED: &str = "recorded";
/// `tenancy.divergence`: the known tenants name more than one tenant.
pub const DIVERGENCE_DIVERGED: &str = "diverged";
/// `tenancy.divergence`: the credential tenant AND an expected tenant are both
/// known, and they are one tenant.
pub const DIVERGENCE_AGREE: &str = "agree";

/// What the lifecycle record says about the device default a session was
/// spawned under (W-B). `NotRecorded` is UNKNOWN — the comparison it feeds is
/// then unknown too, never agreement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SpawnDefaultRead {
    /// Recorded; `None` = the machine named no default at spawn.
    Recorded(Option<uuid::Uuid>),
    /// Not recorded, with the reason.
    NotRecorded(&'static str),
}

impl SpawnDefaultRead {
    /// Read the persisted value off a lifecycle record.
    pub(crate) fn from_record(rec: &TerminalSessionRecord) -> Self {
        match &rec.spawn_device_default {
            None => SpawnDefaultRead::NotRecorded("not_recorded"),
            Some(sd) => match sd.tenant_id.as_deref().map(str::trim) {
                None | Some("") => SpawnDefaultRead::Recorded(None),
                Some(t) => uuid::Uuid::parse_str(t)
                    .map(|u| SpawnDefaultRead::Recorded(Some(u)))
                    .unwrap_or(SpawnDefaultRead::NotRecorded("unparseable")),
            },
        }
    }
}

/// The coord row tenant: what the session was stamped with at spawn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TenancyRow {
    /// The tenant the spawn chose, as durably stamped on the lifecycle record —
    /// the same value `Intent.tenant_id` carried to coord. `None` ⇒ the spawn
    /// chose none, so coord's row carries whatever default the registry
    /// stamped; it does NOT mean the session has no tenant.
    pub tenant_id: Option<String>,
    /// The machine's default tenant for new sessions AS READ WHEN THIS SESSION
    /// WAS SPAWNED — the expected tenant of a spawn that chose none, frozen the
    /// way its key's pin is. Compared for divergence only when neither
    /// `tenantId` nor the data plane names a tenant, so a session presenting
    /// another tenant's key (a tenant-pinned cwd `.mcp.json`) is never silently
    /// reported as agreeing. `None` when this runner process did not record the
    /// spawn — see [`Self::spawn_device_default_status`].
    pub spawn_device_default_tenant_id: Option<String>,
    /// [`TENANCY_RECORDED`] (the spawn's default was recorded — `tenantId` may
    /// still be `None` when the machine named no default then) | [`TENANCY_UNKNOWN`]
    /// (NOT recorded: never compared, and never read as "no default").
    pub spawn_device_default_status: String,
    /// Set when the status is unknown: `not_recorded` (a record older than the
    /// field, or a spawn this runner never saw) | `unparseable`.
    pub spawn_device_default_reason: Option<String>,
    /// The machine's CURRENT default tenant. Context only, NEVER compared: after
    /// an operator switches the default, every running session legitimately
    /// differs from it (a switch re-points future sessions only).
    pub current_device_default_tenant_id: Option<String>,
}

/// The runner data-plane tenant: what the runner's own work-scoped coord writes
/// for this session present ([`crate::session::session_tenant_scope`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TenancyDataPlane {
    /// `"owned"` (a tenant is stamped on the registry session) | `"device"`
    /// (the default binding's credential; `tenantId` is that binding) |
    /// `"unresolved"` | `"unknown"` (`reason`: `no_coord_session`,
    /// `default_binding_unknown`).
    pub status: String,
    pub tenant_id: Option<String>,
    /// Why `tenantId` is absent: set for `"unknown"`, and for `"device"` on a
    /// device with no default binding (`default_binding_unbound`).
    pub reason: Option<String>,
}

/// The coord-credential posture of the slot the session's credential selects.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TenancyPosture {
    /// [`TENANCY_OBSERVED`] | [`TENANCY_UNKNOWN`].
    pub status: String,
    /// `live` | `expiring` | `expired` | `absent` | `unrefreshable` | `dark`.
    pub value: Option<String>,
    pub can_answer: Option<bool>,
    /// Set when `status` is `"unknown"`: `credential_tenant_unknown`,
    /// `no_posture_published`, or `posture_describes_another_slot` (the runner
    /// publishes ONE posture — the worst slot's — so another slot's reading is
    /// not a statement about this one).
    pub reason: Option<String>,
}

/// The coord-mcp credential tenant: what this session's coord-mcp writes (memory,
/// prompt documents, gates) are actually attributed to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TenancyCredential {
    /// [`TENANCY_RESOLVED`] | [`TENANCY_UNKNOWN`]. Never a default in place of
    /// an unknown — served policy `verification-and-evidence`
    /// `unknown-must-not-render-as-a-default`.
    pub status: String,
    /// The tenant whose slot the proxy selects. With `slot: "default"` this is
    /// the device's default binding when that is known, else `None`.
    pub tenant_id: Option<String>,
    /// `"tenant"` (a per-tenant slot) | `"default"` (the legacy default slot);
    /// `None` while unknown.
    pub slot: Option<String>,
    /// Set when `status` is `"unknown"`: `no_session_nonce`,
    /// `coord_mcp_delivery_unrecorded`, `nonce_not_live`,
    /// `default_slot_binding_unbound`, `default_slot_binding_unknown`, or
    /// `tenant_unresolvable: <the proxy's refusal>`.
    pub reason: Option<String>,
    pub posture: TenancyPosture,
}

/// Which tenant each half of this session acts as, and whether they disagree
/// (plan `2026-09-10-spawn-tenant-never-reaches-the-session-coord-credential`
/// P0).
///
/// Three values, not one, because three mechanisms decide them: the coord row
/// is stamped at spawn, the runner's data-plane writes follow the registry, and
/// the coord-mcp proxy resolves its own nonce. A session spawned for tenant B
/// used to be labelled B and have its runner writes go to B while its coord-mcp
/// writes went to the machine's tenant — so all three are reported, and the
/// disagreement is stated rather than resolved by picking one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionTenancy {
    pub row: TenancyRow,
    pub data_plane: TenancyDataPlane,
    pub credential: TenancyCredential,
    /// True iff the tenants that ARE known name more than one tenant. An
    /// unknown value never manufactures a divergence, and never hides one.
    /// `false` is NOT agreement — read [`Self::divergence`] for that.
    pub diverged: bool,
    /// [`DIVERGENCE_DIVERGED`] | [`DIVERGENCE_AGREE`] | [`TENANCY_UNKNOWN`].
    ///
    /// `agree` requires BOTH halves of the comparison to be known: the
    /// credential tenant, and an expected tenant (the row stamp, the data plane,
    /// or a RECORDED spawn-time default). A tenant-less session whose spawn
    /// default was not recorded, or a session whose credential is unknown, is
    /// `unknown` — the boolean alone used to render both as "not diverged".
    pub divergence: String,
}

/// Project the three tenancy reads into [`SessionTenancy`]. Pure.
///
/// - `row_tenant` — the lifecycle record's stamped tenant.
/// - `data_plane` — the registry session's scope, `None` when the terminal has
///   no registry session to ask.
/// - `credential` — what the session's coord-mcp nonce resolves to.
/// - `default_binding` — the device's default binding, which is the tenant
///   behind the legacy default slot. An `Unknown` or `Unbound` read makes every
///   default-slot answer UNKNOWN rather than a tenant-less "resolved".
/// - `spawn_default` — the machine's default tenant as recorded when the session
///   was spawned (compared); `current_default` — the default now (reported,
///   never compared — see [`TenancyRow`]).
/// - `posture` — the runner's published credential posture.
pub(crate) fn project_tenancy(
    row_tenant: Option<&str>,
    data_plane: Option<crate::auth::TenantScope>,
    credential: &crate::coord_mcp::CredentialTenantRead,
    default_binding: crate::auth::BindingTenantRead,
    spawn_default: SpawnDefaultRead,
    current_default: Option<uuid::Uuid>,
    posture: Option<&crate::mcp::device_jwt_refresher::CoordCredentialStatus>,
) -> SessionTenancy {
    use crate::auth::{BindingTenantRead, TenantScope};
    use crate::coord_mcp::CredentialTenantRead;

    let row_tenant = row_tenant.map(str::trim).filter(|t| !t.is_empty());
    let data_plane = match data_plane {
        Some(TenantScope::Owned(t)) => TenancyDataPlane {
            status: "owned".to_string(),
            tenant_id: Some(t.to_string()),
            reason: None,
        },
        // `Device` presents the default binding's credential, so it acts as
        // that binding's tenant — and takes part in `diverged` like any other.
        // Not reachable from `read_session_tenancy` today: the registry's
        // `tenant_scope_of` answers only `Owned` or `Unresolved` for a session.
        // The arm exists so a future scope source that does answer `Device` is
        // reported through the binding rather than as tenant-less.
        Some(TenantScope::Device) => match default_binding {
            BindingTenantRead::Bound(t) => TenancyDataPlane {
                status: "device".to_string(),
                tenant_id: Some(t.to_string()),
                reason: None,
            },
            BindingTenantRead::Unbound => TenancyDataPlane {
                status: "device".to_string(),
                tenant_id: None,
                reason: Some("default_binding_unbound".to_string()),
            },
            BindingTenantRead::Unknown => TenancyDataPlane {
                status: TENANCY_UNKNOWN.to_string(),
                tenant_id: None,
                reason: Some("default_binding_unknown".to_string()),
            },
        },
        Some(TenantScope::Unresolved) => TenancyDataPlane {
            status: "unresolved".to_string(),
            tenant_id: None,
            reason: None,
        },
        None => TenancyDataPlane {
            status: TENANCY_UNKNOWN.to_string(),
            tenant_id: None,
            reason: Some("no_coord_session".to_string()),
        },
    };

    // (the tenant the credential selects, the session tenant the posture gate
    // compares against) — the second is `None` for the default slot, which is
    // how `posture_describes_session_slot` spells it.
    let credential_slot: Result<(uuid::Uuid, Option<uuid::Uuid>), String> = match credential {
        CredentialTenantRead::Resolved(Some(t)) => Ok((*t, Some(*t))),
        CredentialTenantRead::Resolved(None) => match default_binding {
            BindingTenantRead::Bound(t) => Ok((t, None)),
            BindingTenantRead::Unbound => Err("default_slot_binding_unbound".to_string()),
            BindingTenantRead::Unknown => Err("default_slot_binding_unknown".to_string()),
        },
        CredentialTenantRead::NoNonce => Err("no_session_nonce".to_string()),
        CredentialTenantRead::DeliveryUnrecorded => {
            Err("coord_mcp_delivery_unrecorded".to_string())
        }
        CredentialTenantRead::NotLive => Err("nonce_not_live".to_string()),
        CredentialTenantRead::Refused(body) => Err(format!("tenant_unresolvable: {body}")),
    };
    let credential = match &credential_slot {
        Ok((tenant, session_tenant)) => {
            let posture = match posture {
                None => unknown_posture("no_posture_published"),
                Some(status)
                    if crate::coord_mcp::posture_describes_session_slot(
                        status,
                        *session_tenant,
                    ) =>
                {
                    TenancyPosture {
                        status: TENANCY_OBSERVED.to_string(),
                        value: Some(status.posture.as_str().to_string()),
                        can_answer: Some(status.posture.can_answer()),
                        reason: None,
                    }
                }
                Some(_) => unknown_posture("posture_describes_another_slot"),
            };
            TenancyCredential {
                status: TENANCY_RESOLVED.to_string(),
                tenant_id: Some(tenant.to_string()),
                slot: Some(
                    if session_tenant.is_some() {
                        "tenant"
                    } else {
                        "default"
                    }
                    .to_string(),
                ),
                reason: None,
                posture,
            }
        }
        Err(reason) => TenancyCredential {
            status: TENANCY_UNKNOWN.to_string(),
            tenant_id: None,
            slot: None,
            reason: Some(reason.clone()),
            posture: unknown_posture("credential_tenant_unknown"),
        },
    };

    // The expected tenant of a spawn that chose none is the device default it
    // was SPAWNED under — compared only when nothing better (the stamp, the
    // registry) names one. The live default is never compared: an operator's
    // switch would otherwise make every running tenant-less session read
    // diverged, a false signal rendered as a fact.
    let (spawn_default, spawn_default_status, spawn_default_reason) = match spawn_default {
        SpawnDefaultRead::Recorded(t) => (t.map(|t| t.to_string()), TENANCY_RECORDED, None),
        SpawnDefaultRead::NotRecorded(reason) => (None, TENANCY_UNKNOWN, Some(reason.to_string())),
    };
    let expected = row_tenant
        .map(String::from)
        .or_else(|| data_plane.tenant_id.clone())
        .or_else(|| spawn_default.clone());
    let mut known: Vec<String> = Vec::with_capacity(4);
    known.extend(expected.as_deref().map(str::to_ascii_lowercase));
    known.extend(row_tenant.map(str::to_ascii_lowercase));
    known.extend(data_plane.tenant_id.as_deref().map(str::to_ascii_lowercase));
    known.extend(credential.tenant_id.as_deref().map(str::to_ascii_lowercase));
    known.sort();
    known.dedup();
    let diverged = known.len() > 1;
    let divergence = if diverged {
        DIVERGENCE_DIVERGED
    } else if credential.tenant_id.is_some() && expected.is_some() {
        DIVERGENCE_AGREE
    } else {
        TENANCY_UNKNOWN
    };

    SessionTenancy {
        row: TenancyRow {
            tenant_id: row_tenant.map(String::from),
            spawn_device_default_tenant_id: spawn_default,
            spawn_device_default_status: spawn_default_status.to_string(),
            spawn_device_default_reason: spawn_default_reason,
            current_device_default_tenant_id: current_default.map(|t| t.to_string()),
        },
        data_plane,
        credential,
        diverged,
        divergence: divergence.to_string(),
    }
}

fn unknown_posture(reason: &str) -> TenancyPosture {
    TenancyPosture {
        status: TENANCY_UNKNOWN.to_string(),
        value: None,
        can_answer: None,
        reason: Some(reason.to_string()),
    }
}

/// Read the three tenancy values for `rec` from the live runner and project
/// them. The registry session is reached through the terminal that hosts the
/// session; the credential through that terminal's nonce, or its cwd's.
pub(crate) fn read_session_tenancy(rec: &TerminalSessionRecord) -> SessionTenancy {
    use tauri::Manager;
    let data_plane = crate::tauri_app_handle::current()
        .and_then(|app| {
            app.try_state::<Arc<crate::terminal::TerminalManager>>()
                .and_then(|tm| tm.get(&rec.terminal_id))
        })
        .and_then(|terminal| terminal.coord_session_id())
        .map(|id| crate::session::session_tenant_scope(Some(id)));
    let credential = crate::coord_mcp::session_credential_tenant(
        &rec.terminal_id,
        non_empty(rec.working_dir.as_ref()).as_deref(),
    );
    project_tenancy(
        rec.tenant_id.as_deref(),
        data_plane,
        &credential,
        crate::auth::default_binding_tenant_probe(),
        // W-B: the PERSISTED spawn-time default, never the process map — after
        // a restart that map holds the restore-time default for this terminal.
        SpawnDefaultRead::from_record(rec),
        crate::session::tenant_pin::resolve_tenant_pin().pinned(),
        crate::mcp::device_jwt_refresher::coord_credential_posture().as_ref(),
    )
}

/// The projected body — every field group of D1. Flattened into
/// [`SessionInfoEnvelope`] so the wire shape is exactly D1's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfoBody {
    pub identity: SessionIdentity,
    pub name: SessionName,
    pub account: SessionAccount,
    pub placement: SessionPlacement,
    pub lifecycle: SessionLifecycleInfo,
    pub prs: SessionPrs,
    /// Which tenant each half of the session acts as — see [`SessionTenancy`].
    pub tenancy: SessionTenancy,
}

/// The one-shot session-info envelope.
///
/// `available: false` ALWAYS carries a `reason` — there is no branch that
/// returns a bare empty projection, because "no such session" and "the store
/// isn't reachable" must not render identically (acceptance criterion 4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfoEnvelope {
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(flatten)]
    pub body: Option<SessionInfoBody>,
}

impl SessionInfoEnvelope {
    /// The degraded envelope: no projection, but always a stated reason.
    pub fn unavailable(reason: &str) -> Self {
        Self {
            available: false,
            reason: Some(reason.to_string()),
            body: None,
        }
    }
}

/// Response body of `GET /control/sessions/info` — every OPEN session's
/// projection.
///
/// Carries its own `status`/`reason` so a runner that cannot reach its
/// lifecycle store says so, instead of returning `sessions: []` (which reads
/// identically to "this runner has zero sessions" — the silent empty this whole
/// plan exists to remove).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionsInfoResponse {
    /// [`STATUS_OK`] | [`STATUS_UNAVAILABLE`].
    pub status: String,
    /// REQUIRED whenever `status != "ok"`.
    pub reason: Option<String>,
    pub sessions: Vec<SessionInfoEnvelope>,
}

impl SessionsInfoResponse {
    /// Degraded listing — empty WITH a reason, never a bare empty.
    pub fn unavailable(reason: &str) -> Self {
        Self {
            status: STATUS_UNAVAILABLE.to_string(),
            reason: Some(reason.to_string()),
            sessions: Vec::new(),
        }
    }
}

/// Normalize a stored/live `name_source` into the wire vocabulary.
///
/// The live registry spells "an operator renamed this" as the ABSENCE of the
/// key, which [`crate::session::session_lifecycle_store::durable_name_source`]
/// already resolves on the write side. Here we only interpret what is stored:
/// `"derived"` is Claude Code's own auto-name, any other present value is
/// operator-chosen, and ABSENT is honestly [`NAME_SOURCE_UNKNOWN`] — on the
/// durable record `None` means "never observed", and reporting that as
/// `"operator"` would invent a provenance (R2).
fn wire_name_source(stored: Option<&str>) -> &'static str {
    match stored.map(str::trim).filter(|s| !s.is_empty()) {
        Some(NAME_SOURCE_DERIVED) => NAME_SOURCE_DERIVED,
        Some(_) => NAME_SOURCE_OPERATOR,
        None => NAME_SOURCE_UNKNOWN,
    }
}

/// Trim `Some("")` to `None` — a blank stored string is "not reported", never a
/// value (the same normalization the store applies on write).
fn non_empty(v: Option<&String>) -> Option<String> {
    v.map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(String::from)
}

/// Project the runner-local PR ledger rows into the dropdown's opened/landed
/// split. Pure.
///
/// - `opened` — EVERY attributed row: the git trailer proves this session
///   authored the branch, regardless of wall-clock (assumption 9.2).
/// - `landed` — rows whose stored `merged` column (the LAND verdict, widened
///   when the land-signal cascade shipped) is true, each carrying the signal
///   that proved it.
/// - `unknown` — rows the cascade could not evaluate, with their reason.
pub fn project_prs(rows: &[SessionPrRow]) -> SessionPrs {
    project_prs_scanned(rows, None)
}

/// [`project_prs`] plus the scan provenance: `scanned_repos` is `None` when the
/// reconciler has never resolved a repo set for this session, `Some(&[])` when
/// it resolved one and found no repositories, `Some(&[..])` for the repos it
/// searched. Pure.
pub fn project_prs_scanned(rows: &[SessionPrRow], scanned_repos: Option<&[String]>) -> SessionPrs {
    let opened: Vec<SessionPrOpened> = rows
        .iter()
        .map(|r| SessionPrOpened {
            repo: r.repo.clone(),
            pr_number: r.pr_number,
            branch: r.head_branch.clone(),
            opened_at: r.created_at.map(|t| t.to_rfc3339()),
            pr_state: r.pr_state.clone(),
        })
        .collect();
    let landed: Vec<SessionPrLanded> = rows
        .iter()
        .filter(|r| r.merged)
        .map(|r| SessionPrLanded {
            repo: r.repo.clone(),
            pr_number: r.pr_number,
            landed_at: r.landed_at.or(r.merged_at).map(|t| t.to_rfc3339()),
            land_signal: r.land_signal.clone(),
        })
        .collect();
    let unknown: Vec<SessionPrUnknown> = rows
        .iter()
        .filter(|r| r.land_signal.as_deref() == Some(LAND_SIGNAL_UNKNOWN))
        .map(|r| SessionPrUnknown {
            repo: r.repo.clone(),
            pr_number: r.pr_number,
            reason: r
                .land_reason
                .clone()
                .unwrap_or_else(|| "unspecified".to_string()),
        })
        .collect();
    SessionPrs {
        status: STATUS_OK.to_string(),
        reason: None,
        open_count: opened.len(),
        landed_count: landed.len(),
        unknown_count: unknown.len(),
        opened,
        landed,
        unknown,
        scanned: scanned_repos.is_some(),
        scanned_repos: scanned_repos.unwrap_or(&[]).to_vec(),
    }
}

/// The degraded PR ledger: empty lists, `status: "unavailable"`, and a stated
/// reason. Pure.
///
/// This is NOT the same value as `project_prs(&[])`: that one asserts "this
/// session opened no PRs", this one asserts nothing at all. Rendering them
/// identically is the G5 defect.
pub fn prs_unavailable(reason: &str) -> SessionPrs {
    SessionPrs {
        status: STATUS_UNAVAILABLE.to_string(),
        reason: Some(reason.to_string()),
        opened: Vec::new(),
        landed: Vec::new(),
        unknown: Vec::new(),
        open_count: 0,
        landed_count: 0,
        unknown_count: 0,
        // The STORE could not answer, so the scan provenance is meaningless
        // here — `status: "unavailable"` already says "this asserts nothing".
        scanned: false,
        scanned_repos: Vec::new(),
    }
}

/// Project one durable record (⨝ live-registry overlay ⨝ PR ledger) into the
/// dropdown envelope. Pure — no disk, no PG, no Tauri handle.
///
/// The live overlay WINS on name and account when present (it is the fresher
/// copy of the same values the stickiness rule persisted), and is simply absent
/// once the process exits — at which point the durable copy is the only one
/// left, which is exactly why D1 stores it.
pub fn project_session_info(
    rec: &TerminalSessionRecord,
    live: Option<&LiveClaudeSession>,
    transcript_exists: bool,
    prs: SessionPrs,
    tenancy: SessionTenancy,
) -> SessionInfoEnvelope {
    let confirmed = rec.confirmed_at.is_some();
    let live_name = live
        .map(|l| l.name.trim())
        .filter(|n| !n.is_empty())
        .map(String::from);
    let (name_value, name_source) = match live_name {
        // A live row's name is ground truth, and its own `nameSource` (absent ⇒
        // operator-renamed) is the provenance that goes with it.
        Some(n) => (
            Some(n),
            crate::session::session_lifecycle_store::durable_name_source(
                live.and_then(|l| l.name_source.as_deref()),
            ),
        ),
        None => (
            non_empty(rec.session_name.as_ref()),
            wire_name_source(rec.name_source.as_deref()).to_string(),
        ),
    };
    // No name ⇒ no provenance to claim.
    let name_source = if name_value.is_none() {
        NAME_SOURCE_UNKNOWN.to_string()
    } else {
        name_source
    };

    SessionInfoEnvelope {
        available: true,
        reason: None,
        body: Some(SessionInfoBody {
            identity: SessionIdentity {
                claude_session_id: rec.claude_session_id.clone(),
                terminal_id: rec.terminal_id.clone(),
                fleet_session_handle: non_empty(rec.handle.as_ref()),
                tenant_id: non_empty(rec.tenant_id.as_ref()),
                task_run_id: non_empty(rec.task_run_id.as_ref()),
            },
            name: SessionName {
                value: name_value,
                source: name_source,
            },
            account: SessionAccount {
                label: live
                    .map(|l| l.account.label.clone())
                    .or_else(|| non_empty(rec.account_label.as_ref())),
                wrapper: live
                    .map(|l| l.account.wrapper.clone())
                    .or_else(|| non_empty(rec.account_wrapper.as_ref())),
                config_dir: non_empty(rec.config_dir.as_ref()),
            },
            placement: SessionPlacement {
                page_id: rec.page_id.clone(),
                zone_index: rec.zone_index,
                working_dir: non_empty(rec.working_dir.as_ref()),
            },
            lifecycle: SessionLifecycleInfo {
                state: rec.state.clone(),
                provider: rec.provider.clone(),
                origin: rec.origin.clone(),
                opened_at: rec.opened_at,
                last_seen_at: rec.last_seen_at,
                closed_at: rec.closed_at,
                close_reason: rec.close_reason.clone(),
                confirmed,
                transcript_exists,
                identity_evidence: resolve_identity_evidence(confirmed, transcript_exists),
                restorable: crate::session::snapshot_history::is_restorable_identity(
                    confirmed,
                    transcript_exists,
                ),
                restored_from_boot_at: rec.restored_from_boot_at,
                restore_status: crate::session::session_lifecycle_store::describe_restore_status(
                    rec.restore_tier.as_deref(),
                    rec.restore_pending_at,
                    chrono::Utc::now().timestamp_millis(),
                ),
                restore_tier: rec.restore_tier.clone(),
                restore_pending_at: rec.restore_pending_at,
                bypass_permissions: rec.bypass_permissions,
            },
            prs,
            tenancy,
        }),
    }
}

/// Read the runner-local PR ledger for one session, fail-soft.
///
/// Reaches PG through [`crate::database::pg::pg_available`] then
/// `PgDb::try_global()`, a process GLOBAL rather than Tauri state — which is
/// why the HTTP twin needs no `ApiState` plumbing for the join. The scan
/// provenance ([`crate::session_pr_reconciler::last_scanned_repos`]) is read
/// the same way, for the same reason.
///
/// Every degraded condition returns [`prs_unavailable`] with a reason:
/// `invalid_session_id` (the id is not a uuid, so it can never key the
/// projection), `db_unavailable`, `db_error`.
pub async fn load_prs(claude_session_id: &str) -> SessionPrs {
    let Ok(session_uuid) = uuid::Uuid::parse_str(claude_session_id.trim()) else {
        return prs_unavailable("invalid_session_id");
    };
    if !crate::database::pg::pg_available() {
        return prs_unavailable("db_unavailable");
    }
    let Some(pg_db) = crate::database::pg::PgDb::try_global() else {
        return prs_unavailable("db_unavailable");
    };
    // Which repos the reconciler last searched for this session. READ ONLY —
    // never re-run the resolver here: this is a passive read path polled per
    // zone, and resolving would put a filesystem scan on it.
    let scanned = crate::session_pr_reconciler::last_scanned_repos(session_uuid);
    match pg_db.list_session_prs(session_uuid).await {
        Ok(rows) => project_prs_scanned(&rows, scanned.as_deref()),
        Err(e) => {
            tracing::debug!("session_info: list_session_prs failed (fail-soft): {e}");
            prs_unavailable("db_error")
        }
    }
}

/// Build the full projection for `rec`, doing the impure reads (live registry,
/// transcript probe, PR ledger) around the pure core.
///
/// `config_dirs` and `probe` are hoisted by the caller so a LIST of sessions
/// discovers the Claude config dirs and the transcript index ONCE, not per row.
pub async fn build_session_info(
    rec: &TerminalSessionRecord,
    config_dirs: &[std::path::PathBuf],
    probe: &dyn crate::session::snapshot_history::TranscriptProbe,
) -> SessionInfoEnvelope {
    let live = crate::session::claude_session_registry::find_live_session_by_id(
        config_dirs,
        &rec.claude_session_id,
    );
    let transcript_exists =
        probe.transcript_exists(&rec.claude_session_id, rec.working_dir.as_deref());
    let prs = load_prs(&rec.claude_session_id).await;
    let tenancy = read_session_tenancy(rec);
    project_session_info(rec, live.as_ref(), transcript_exists, prs, tenancy)
}

/// `session_info_get` — the ONE read the session-info dropdown makes.
///
/// Returns the whole D1 projection for `claude_session_id`: durable record ⨝
/// live-registry overlay ⨝ PR ledger. Never `Err` for a data condition (the
/// dropdown must render an honest "unknown" rather than an error toast per
/// zone per poll); the degraded envelope always carries its `reason`:
///
/// | Branch | `reason` |
/// |---|---|
/// | blank id from the caller | `missing_session_id` |
/// | no registry record under that id | `session_not_found` |
///
/// PG degradation does NOT degrade the whole envelope — identity is still
/// answerable without the ledger — it degrades `prs.status` alone, with its own
/// reason.
#[tauri::command]
pub async fn session_info_get(
    store: tauri::State<'_, Arc<SessionLifecycleStore>>,
    claude_session_id: String,
) -> Result<SessionInfoEnvelope, String> {
    let id = claude_session_id.trim().to_string();
    if id.is_empty() {
        return Ok(SessionInfoEnvelope::unavailable("missing_session_id"));
    }
    let Some(rec) = store.get(&id) else {
        return Ok(SessionInfoEnvelope::unavailable("session_not_found"));
    };
    // Fire-and-forget: ask the reconciler to look at THIS session now rather
    // than up to 30s from now. This read does not wait on it and never can —
    // the reconciler ticks on its own task, debounced per session. Without it
    // a freshly opened PR took up to ~90s to surface (30s reconcile + 60s
    // poll); with it the NEXT poll after opening the dropdown carries it.
    if let Ok(uuid) = uuid::Uuid::parse_str(&id) {
        crate::session_pr_reconciler::nudge_session(uuid);
    }
    let config_dirs = crate::terminal::transcript::find_claude_config_dirs();
    let probe = crate::session::reconcile::DiskTranscriptIndex::discover();
    Ok(build_session_info(&rec, &config_dirs, &probe).await)
}

#[cfg(test)]
mod tests {
    /// The defect this classification exists for: a bare shell must NOT be
    /// presented as a real session. Measured live 2026-08-18 — `POST /terminals`
    /// alone produced `confirmed:false, transcriptExists:false` with a
    /// roster-selected account, so this is the common case, not an edge case.
    #[test]
    fn a_spawn_time_seam_record_with_no_corroboration_is_provisional() {
        assert_eq!(
            classify_identity_evidence(false, false),
            IDENTITY_PROVISIONAL
        );
    }

    /// A provider hook fired: this is the observable proof a REAL provider
    /// started, and it is what the restore classifier trusts.
    #[test]
    fn a_hook_confirmed_record_is_confirmed() {
        assert_eq!(classify_identity_evidence(true, false), IDENTITY_CONFIRMED);
        // Confirmation wins even with a transcript — it is the stronger signal.
        assert_eq!(classify_identity_evidence(true, true), IDENTITY_CONFIRMED);
    }

    /// No hook, but a transcript exists: the session demonstrably produced
    /// output. Weaker than a hook, but still evidence — and NOT provisional.
    #[test]
    fn a_transcript_without_a_hook_is_evidence_not_provisional() {
        assert_eq!(classify_identity_evidence(false, true), IDENTITY_TRANSCRIPT);
        assert_ne!(
            classify_identity_evidence(false, true),
            IDENTITY_PROVISIONAL
        );
    }

    /// The predicate must agree with the restore classifier, which gates
    /// auto-resume on `confirmed_at` OR a transcript. If these ever diverge the
    /// panel would claim an identity restore refuses to act on.
    #[test]
    fn evidence_agrees_with_the_restore_auto_resume_predicate() {
        for (confirmed, transcript) in [(true, true), (true, false), (false, true), (false, false)]
        {
            let restore_would_resume = confirmed || transcript;
            let ev = classify_identity_evidence(confirmed, transcript);
            assert_eq!(
                restore_would_resume,
                ev != IDENTITY_PROVISIONAL,
                "confirmed={confirmed} transcript={transcript} -> {ev}"
            );
        }
    }

    use super::*;
    use chrono::{TimeZone, Utc};

    fn record(id: &str) -> TerminalSessionRecord {
        TerminalSessionRecord {
            claude_session_id: id.to_string(),
            config_dir: Some("C:/Users/x/.claude-paktis".to_string()),
            working_dir: Some("C:/qontinui-root/qontinui-runner".to_string()),
            page_id: "default".to_string(),
            zone_index: 3,
            title: Some("runner".to_string()),
            terminal_id: "term-1".to_string(),
            opened_at: 1_000,
            last_seen_at: 2_000,
            state: "open".to_string(),
            closed_at: None,
            close_reason: None,
            provider: "claude".to_string(),
            origin: Some("authoritative".to_string()),
            restore_pending_at: None,
            confirmed_at: Some(1_500),
            handle: Some("fsh_abc".to_string()),
            account_label: Some("paktis".to_string()),
            account_wrapper: Some("clp".to_string()),
            session_name: Some("stored-name".to_string()),
            name_source: Some(NAME_SOURCE_DERIVED.to_string()),
            tenant_id: Some("tenant-1".to_string()),
            task_run_id: None,
            bypass_permissions: Some(false),
            restored_from_boot_at: None,
            restore_tier: None,
            finished_at: None,
            finish_reason: None,
            finish_synced: false,
            spawn_device_default: None,
        }
    }

    fn pr_row(pr_number: i64, merged: bool, land_signal: &str) -> SessionPrRow {
        SessionPrRow {
            claude_session_id: uuid::Uuid::nil(),
            repo: "qontinui/qontinui-runner".to_string(),
            pr_number,
            head_branch: Some("feat/x".to_string()),
            pr_state: Some(if merged { "merged" } else { "open" }.to_string()),
            merged,
            merged_at: None,
            land_signal: Some(land_signal.to_string()),
            land_reason: None,
            landed_at: merged.then(|| Utc.with_ymd_and_hms(2026, 8, 15, 10, 0, 0).unwrap()),
            created_at: Some(Utc.with_ymd_and_hms(2026, 8, 14, 9, 0, 0).unwrap()),
        }
    }

    /// A tenancy block that establishes nothing — for tests about other fields.
    fn unknown_tenancy() -> SessionTenancy {
        project_tenancy(
            None,
            None,
            &crate::coord_mcp::CredentialTenantRead::NoNonce,
            crate::auth::BindingTenantRead::Unknown,
            SpawnDefaultRead::NotRecorded("not_recorded"),
            None,
            None,
        )
    }

    fn tenant(n: u8) -> uuid::Uuid {
        uuid::Uuid::from_bytes([n; 16])
    }

    fn posture_on(
        posture: crate::mcp::device_jwt_refresher::CoordCredentialPosture,
        slot: Option<uuid::Uuid>,
    ) -> crate::mcp::device_jwt_refresher::CoordCredentialStatus {
        crate::mcp::device_jwt_refresher::CoordCredentialStatus {
            posture,
            tenant_id: slot.map(|t| t.to_string()),
            exp: None,
            last_ok_at: None,
            last_401_at: None,
            last_refresh_outcome: None,
            since: 0,
            observed_at_unix: 0,
            attributable: true,
        }
    }

    /// P0 (plan 2026-09-10-spawn-tenant-never-reaches-the-session-coord-credential):
    /// all three halves agreeing is NOT a divergence, and the posture of the
    /// credential's own slot is reported beside it.
    #[test]
    fn tenancy_agreeing_on_one_tenant_is_not_diverged_and_reports_that_slots_posture() {
        use crate::mcp::device_jwt_refresher::CoordCredentialPosture as P;
        let b = tenant(0xB2);
        let status = posture_on(P::Live, Some(b));
        let t = project_tenancy(
            Some(&b.to_string()),
            Some(crate::auth::TenantScope::Owned(b)),
            &crate::coord_mcp::CredentialTenantRead::Resolved(Some(b)),
            crate::auth::BindingTenantRead::Unknown,
            SpawnDefaultRead::NotRecorded("not_recorded"),
            None,
            Some(&status),
        );
        assert!(!t.diverged);
        assert_eq!(t.credential.status, TENANCY_RESOLVED);
        assert_eq!(t.credential.slot.as_deref(), Some("tenant"));
        assert_eq!(t.credential.posture.status, TENANCY_OBSERVED);
        assert_eq!(t.credential.posture.value.as_deref(), Some("live"));
    }

    /// The published posture is the WORST slot's. A reading about tenant A's
    /// slot says nothing about B's, so it must report UNKNOWN for a B credential
    /// rather than lend B the A reading.
    #[test]
    fn tenancy_never_reports_another_slots_posture_as_the_credentials_own() {
        use crate::mcp::device_jwt_refresher::CoordCredentialPosture as P;
        let (a, b) = (tenant(0xA1), tenant(0xB2));
        let status = posture_on(P::Expired, Some(a));
        let t = project_tenancy(
            Some(&b.to_string()),
            None,
            &crate::coord_mcp::CredentialTenantRead::Resolved(Some(b)),
            crate::auth::BindingTenantRead::Unknown,
            SpawnDefaultRead::NotRecorded("not_recorded"),
            None,
            Some(&status),
        );
        assert_eq!(t.credential.posture.status, TENANCY_UNKNOWN);
        assert_eq!(
            t.credential.posture.reason.as_deref(),
            Some("posture_describes_another_slot")
        );
        assert_eq!(t.data_plane.status, TENANCY_UNKNOWN);
        assert_eq!(t.data_plane.reason.as_deref(), Some("no_coord_session"));
    }

    /// The default slot IS the default binding's credential, so a row stamped B
    /// on a device whose default binding is A diverges even though the proxy
    /// names no tenant.
    #[test]
    fn tenancy_compares_a_default_slot_credential_as_the_default_binding() {
        let (a, b) = (tenant(0xA1), tenant(0xB2));
        let t = project_tenancy(
            Some(&b.to_string()),
            None,
            &crate::coord_mcp::CredentialTenantRead::Resolved(None),
            crate::auth::BindingTenantRead::Bound(a),
            SpawnDefaultRead::NotRecorded("not_recorded"),
            None,
            None,
        );
        assert_eq!(t.credential.slot.as_deref(), Some("default"));
        assert_eq!(t.credential.tenant_id, Some(a.to_string()));
        assert!(t.diverged);
    }

    /// An unknown credential never manufactures a divergence, and never renders
    /// as a tenant.
    #[test]
    fn tenancy_unknown_credential_is_spelled_unknown_not_defaulted() {
        let b = tenant(0xB2);
        let t = project_tenancy(
            Some(&b.to_string()),
            None,
            &crate::coord_mcp::CredentialTenantRead::NoNonce,
            crate::auth::BindingTenantRead::Bound(tenant(0xA1)),
            SpawnDefaultRead::NotRecorded("not_recorded"),
            None,
            None,
        );
        assert_eq!(t.credential.status, TENANCY_UNKNOWN);
        assert_eq!(t.credential.tenant_id, None);
        assert_eq!(t.credential.reason.as_deref(), Some("no_session_nonce"));
        assert!(!t.diverged);
    }

    /// S2 (review). A default-slot credential on a device whose default binding
    /// is unbound or unreadable names no tenant, so it is UNKNOWN — never
    /// `resolved` with a null tenant that a badge reads as "acting as B". A
    /// nonce the proxy no longer accepts is UNKNOWN too.
    #[test]
    fn tenancy_never_resolves_a_default_slot_whose_binding_is_not_known() {
        use crate::auth::BindingTenantRead;
        use crate::coord_mcp::CredentialTenantRead;
        let b = tenant(0xB2);
        for (binding, reason) in [
            (BindingTenantRead::Unbound, "default_slot_binding_unbound"),
            (BindingTenantRead::Unknown, "default_slot_binding_unknown"),
        ] {
            let t = project_tenancy(
                Some(&b.to_string()),
                None,
                &CredentialTenantRead::Resolved(None),
                binding,
                SpawnDefaultRead::NotRecorded("not_recorded"),
                None,
                None,
            );
            assert_eq!(t.credential.status, TENANCY_UNKNOWN, "{binding:?}");
            assert_eq!(t.credential.slot, None);
            assert_eq!(t.credential.reason.as_deref(), Some(reason));
        }
        let stale = project_tenancy(
            Some(&b.to_string()),
            None,
            &CredentialTenantRead::NotLive,
            BindingTenantRead::Bound(b),
            SpawnDefaultRead::NotRecorded("not_recorded"),
            None,
            None,
        );
        assert_eq!(stale.credential.status, TENANCY_UNKNOWN);
        assert_eq!(stale.credential.reason.as_deref(), Some("nonce_not_live"));
    }

    /// S3 (review). A `Device` data plane presents the default binding's
    /// credential, so it acts as that tenant and takes part in `diverged`; an
    /// unreadable binding makes it unknown rather than tenant-less.
    #[test]
    fn tenancy_maps_a_device_data_plane_through_the_default_binding() {
        use crate::auth::{BindingTenantRead, TenantScope};
        use crate::coord_mcp::CredentialTenantRead;
        let (a, b) = (tenant(0xA1), tenant(0xB2));
        let t = project_tenancy(
            None,
            Some(TenantScope::Device),
            &CredentialTenantRead::Resolved(Some(b)),
            BindingTenantRead::Bound(a),
            SpawnDefaultRead::NotRecorded("not_recorded"),
            None,
            None,
        );
        assert_eq!(t.data_plane.status, "device");
        assert_eq!(t.data_plane.tenant_id, Some(a.to_string()));
        assert!(
            t.diverged,
            "a Device data plane on A and a credential on B diverge"
        );

        let unknown = project_tenancy(
            None,
            Some(TenantScope::Device),
            &CredentialTenantRead::Resolved(Some(b)),
            BindingTenantRead::Unknown,
            SpawnDefaultRead::NotRecorded("not_recorded"),
            None,
            None,
        );
        assert_eq!(unknown.data_plane.status, TENANCY_UNKNOWN);
        assert_eq!(
            unknown.data_plane.reason.as_deref(),
            Some("default_binding_unknown")
        );
    }

    /// B1 (review). A tenant-less spawn whose credential is another tenant's
    /// (a tenant-pinned cwd key) diverges from the device default even when
    /// nothing else names a tenant — it is never reported as agreeing.
    #[test]
    fn tenancy_compares_a_tenantless_session_against_the_device_default() {
        use crate::auth::BindingTenantRead;
        use crate::coord_mcp::CredentialTenantRead;
        let (a, b) = (tenant(0xA1), tenant(0xB2));
        let t = project_tenancy(
            None,
            None,
            &CredentialTenantRead::Resolved(Some(b)),
            BindingTenantRead::Bound(a),
            SpawnDefaultRead::Recorded(Some(a)),
            None,
            None,
        );
        assert_eq!(t.row.spawn_device_default_tenant_id, Some(a.to_string()));
        assert!(t.diverged);
        let agreeing = project_tenancy(
            None,
            None,
            &CredentialTenantRead::Resolved(Some(a)),
            BindingTenantRead::Bound(a),
            SpawnDefaultRead::Recorded(Some(a)),
            None,
            None,
        );
        assert!(!agreeing.diverged);
    }

    /// W2 (re-review). The LIVE device default is context, never a comparison
    /// input: with no recorded spawn default, a session presenting A on a device
    /// now defaulting to B is not diverged.
    #[test]
    fn tenancy_never_compares_against_the_current_device_default() {
        use crate::auth::BindingTenantRead;
        use crate::coord_mcp::CredentialTenantRead;
        let (a, b) = (tenant(0xA1), tenant(0xB2));
        let t = project_tenancy(
            None,
            None,
            &CredentialTenantRead::Resolved(Some(a)),
            BindingTenantRead::Bound(a),
            SpawnDefaultRead::NotRecorded("not_recorded"),
            Some(b),
            None,
        );
        assert!(!t.diverged);
        assert_eq!(t.row.current_device_default_tenant_id, Some(b.to_string()));
        assert_eq!(t.row.spawn_device_default_tenant_id, None);
    }

    /// W-B. A tenant-less session whose spawn-time default was NOT recorded (a
    /// record from before the field, after a restart) cannot be compared: the
    /// status says so, and the comparison is `unknown` — not `diverged: false`
    /// read as agreement, which is what flattening "not recorded" used to do.
    #[test]
    fn tenancy_with_an_unrecorded_spawn_default_is_unknown_not_agreement() {
        use crate::auth::BindingTenantRead;
        use crate::coord_mcp::CredentialTenantRead;
        let (a, b) = (tenant(0xA1), tenant(0xB2));
        let t = project_tenancy(
            None,
            None,
            &CredentialTenantRead::Resolved(Some(b)),
            BindingTenantRead::Bound(a),
            SpawnDefaultRead::NotRecorded("not_recorded"),
            Some(a),
            None,
        );
        assert_eq!(t.row.spawn_device_default_status, TENANCY_UNKNOWN);
        assert_eq!(
            t.row.spawn_device_default_reason.as_deref(),
            Some("not_recorded")
        );
        assert!(!t.diverged);
        assert_eq!(t.divergence, TENANCY_UNKNOWN);

        // Recorded, the same credential is a real comparison either way.
        let agree = project_tenancy(
            None,
            None,
            &CredentialTenantRead::Resolved(Some(a)),
            BindingTenantRead::Bound(a),
            SpawnDefaultRead::Recorded(Some(a)),
            Some(b),
            None,
        );
        assert_eq!(agree.row.spawn_device_default_status, TENANCY_RECORDED);
        assert_eq!(agree.divergence, DIVERGENCE_AGREE);
        let diverged = project_tenancy(
            None,
            None,
            &CredentialTenantRead::Resolved(Some(b)),
            BindingTenantRead::Bound(a),
            SpawnDefaultRead::Recorded(Some(a)),
            None,
            None,
        );
        assert!(diverged.diverged);
        assert_eq!(diverged.divergence, DIVERGENCE_DIVERGED);
    }

    /// An unknown credential leaves the comparison unknown even when the row
    /// names a tenant — `agree` needs both halves.
    #[test]
    fn tenancy_divergence_is_unknown_while_the_credential_is() {
        let b = tenant(0xB2);
        let t = project_tenancy(
            Some(&b.to_string()),
            None,
            &crate::coord_mcp::CredentialTenantRead::NoNonce,
            crate::auth::BindingTenantRead::Unknown,
            SpawnDefaultRead::NotRecorded("not_recorded"),
            None,
            None,
        );
        assert!(!t.diverged);
        assert_eq!(t.divergence, TENANCY_UNKNOWN);
    }

    /// The persisted value is what the report reads: absent is not recorded,
    /// a recorded `None` is "no default then", and junk is not a tenant.
    #[test]
    fn the_spawn_default_is_read_off_the_record() {
        use crate::session::session_lifecycle_store::SpawnDeviceDefault;
        let mut rec = record("88888888-8888-4888-8888-888888888888");
        assert_eq!(
            SpawnDefaultRead::from_record(&rec),
            SpawnDefaultRead::NotRecorded("not_recorded")
        );
        rec.spawn_device_default = Some(SpawnDeviceDefault { tenant_id: None });
        assert_eq!(
            SpawnDefaultRead::from_record(&rec),
            SpawnDefaultRead::Recorded(None)
        );
        let a = tenant(0xA1);
        rec.spawn_device_default = Some(SpawnDeviceDefault {
            tenant_id: Some(a.to_string()),
        });
        assert_eq!(
            SpawnDefaultRead::from_record(&rec),
            SpawnDefaultRead::Recorded(Some(a))
        );
        rec.spawn_device_default = Some(SpawnDeviceDefault {
            tenant_id: Some("not-a-uuid".into()),
        });
        assert_eq!(
            SpawnDefaultRead::from_record(&rec),
            SpawnDefaultRead::NotRecorded("unparseable")
        );
        let v = serde_json::to_value(project_tenancy(
            None,
            None,
            &crate::coord_mcp::CredentialTenantRead::NoNonce,
            crate::auth::BindingTenantRead::Unknown,
            SpawnDefaultRead::from_record(&rec),
            None,
            None,
        ))
        .unwrap();
        assert_eq!(v["row"]["spawnDeviceDefaultStatus"], "unknown");
        assert_eq!(v["row"]["spawnDeviceDefaultReason"], "unparseable");
        assert_eq!(v["divergence"], "unknown");
    }

    #[test]
    fn session_info_projects_all_four_identifiers_and_the_durable_identity() {
        let rec = record("11111111-1111-4111-8111-111111111111");
        let env = project_session_info(&rec, None, true, project_prs(&[]), unknown_tenancy());
        let body = env
            .body
            .as_ref()
            .expect("available envelope carries a body");
        assert!(env.available);
        assert_eq!(
            body.identity.claude_session_id,
            "11111111-1111-4111-8111-111111111111"
        );
        assert_eq!(body.identity.terminal_id, "term-1");
        // The fleet handle is READ from the pre-existing `handle` field.
        assert_eq!(
            body.identity.fleet_session_handle.as_deref(),
            Some("fsh_abc")
        );
        assert_eq!(body.identity.tenant_id.as_deref(), Some("tenant-1"));
        assert_eq!(body.identity.task_run_id, None);
        // Durable account survives the process that produced it (G1).
        assert_eq!(body.account.label.as_deref(), Some("paktis"));
        assert_eq!(body.account.wrapper.as_deref(), Some("clp"));
        assert_eq!(body.placement.zone_index, 3);
        assert_eq!(body.placement.page_id, "default");
        // confirmed && transcriptExists — the restore-health join.
        assert!(body.lifecycle.confirmed);
        assert!(body.lifecycle.restorable);
    }

    #[test]
    fn transcriptless_confirmed_session_is_not_restorable() {
        let rec = record("22222222-2222-4222-8222-222222222222");
        let env = project_session_info(&rec, None, false, project_prs(&[]), unknown_tenancy());
        let body = env.body.unwrap();
        assert!(body.lifecycle.confirmed);
        assert!(!body.lifecycle.transcript_exists);
        assert!(
            !body.lifecycle.restorable,
            "a confirmed record with no transcript is a phantom — never restorable"
        );
    }

    #[test]
    fn name_source_absent_is_unknown_not_operator() {
        let mut rec = record("33333333-3333-4333-8333-333333333333");
        rec.name_source = None;
        let env = project_session_info(&rec, None, true, project_prs(&[]), unknown_tenancy());
        let body = env.body.unwrap();
        assert_eq!(body.name.value.as_deref(), Some("stored-name"));
        // R2: never back-fill a provenance nobody reported.
        assert_eq!(body.name.source, NAME_SOURCE_UNKNOWN);
    }

    #[test]
    fn nameless_record_reports_unknown_rather_than_a_blank_operator_name() {
        let mut rec = record("44444444-4444-4444-8444-444444444444");
        rec.session_name = None;
        rec.name_source = Some(NAME_SOURCE_OPERATOR.to_string());
        let env = project_session_info(&rec, None, true, project_prs(&[]), unknown_tenancy());
        let body = env.body.unwrap();
        assert_eq!(body.name.value, None);
        assert_eq!(body.name.source, NAME_SOURCE_UNKNOWN);
    }

    #[test]
    fn derived_name_source_is_carried_through_verbatim() {
        let rec = record("55555555-5555-4555-8555-555555555555");
        let env = project_session_info(&rec, None, true, project_prs(&[]), unknown_tenancy());
        assert_eq!(env.body.unwrap().name.source, NAME_SOURCE_DERIVED);
    }

    /// THE G5 FIX, pinned: an unavailable ledger and a genuinely empty one are
    /// different values on the wire.
    #[test]
    fn pr_ledger_unavailable_is_distinct_from_a_genuine_zero() {
        let rec = record("66666666-6666-4666-8666-666666666666");
        let degraded = project_session_info(
            &rec,
            None,
            true,
            prs_unavailable("db_unavailable"),
            unknown_tenancy(),
        )
        .body
        .unwrap()
        .prs;
        let empty = project_session_info(&rec, None, true, project_prs(&[]), unknown_tenancy())
            .body
            .unwrap()
            .prs;

        assert_eq!(degraded.status, STATUS_UNAVAILABLE);
        assert_eq!(degraded.reason.as_deref(), Some("db_unavailable"));
        assert_eq!(empty.status, STATUS_OK);
        assert_eq!(empty.reason, None);
        // Both have zero rows — the STATUS is the only thing that separates
        // "we could not ask" from "the answer is none".
        assert_eq!(degraded.open_count, 0);
        assert_eq!(empty.open_count, 0);
        assert_ne!(degraded, empty);
    }

    /// The SAME distinction one level down, on the SCAN rather than the store.
    /// An empty ledger the reconciler never looked at, an empty ledger whose
    /// working dir holds no repos, and an empty ledger from repos that WERE
    /// searched are three different claims, and the dropdown renders three
    /// different sentences for them. Before this, all three printed
    /// `no PRs attributed to this session`.
    #[test]
    fn never_scanned_is_distinct_from_scanned_with_no_prs() {
        let never = project_prs(&[]);
        let scanned_no_repos = project_prs_scanned(&[], Some(&[]));
        let repos = vec!["D:/qontinui-root/qontinui-runner".to_string()];
        let scanned = project_prs_scanned(&[], Some(&repos));

        // All three are a healthy, EMPTY ledger — the counts cannot separate
        // them, which is exactly why the scan provenance has to be on the wire.
        for prs in [&never, &scanned_no_repos, &scanned] {
            assert_eq!(prs.status, STATUS_OK);
            assert_eq!(prs.open_count, 0);
        }

        assert!(!never.scanned);
        assert!(never.scanned_repos.is_empty());

        assert!(scanned_no_repos.scanned);
        assert!(scanned_no_repos.scanned_repos.is_empty());

        assert!(scanned.scanned);
        assert_eq!(scanned.scanned_repos.len(), 1);

        assert_ne!(never, scanned_no_repos);
        assert_ne!(scanned_no_repos, scanned);
    }

    /// A degraded STORE says nothing about the scan either — it must not claim
    /// `scanned: true` off the back of a read it never made.
    #[test]
    fn an_unavailable_ledger_claims_no_scan_provenance() {
        let degraded = prs_unavailable("db_unavailable");
        assert_eq!(degraded.status, STATUS_UNAVAILABLE);
        assert!(!degraded.scanned);
        assert!(degraded.scanned_repos.is_empty());
    }

    #[test]
    fn opened_and_landed_overlap_rather_than_partition() {
        // 3 attributed rows, 2 of them landed — the `3 opened · 2 landed`
        // labelling D3 requires. A landed PR is STILL an opened PR.
        let rows = [
            pr_row(1, true, "github-merge"),
            pr_row(2, true, "ff-land"),
            pr_row(3, false, "not-landed"),
        ];
        let prs = project_prs(&rows);
        assert_eq!(prs.open_count, 3);
        assert_eq!(prs.landed_count, 2);
        assert_eq!(prs.opened.len(), 3);
        assert_eq!(prs.landed.len(), 2);
        // The ff-land is reported landed WITH its signal (G3, end to end).
        assert_eq!(prs.landed[1].land_signal.as_deref(), Some("ff-land"));
        assert!(prs.landed[1].landed_at.is_some());
        assert_eq!(prs.unknown_count, 0);
    }

    #[test]
    fn land_unknown_rows_are_surfaced_with_their_reason_not_counted_as_not_landed() {
        let mut unknown_row = pr_row(9, false, LAND_SIGNAL_UNKNOWN);
        unknown_row.land_reason = Some("ref_stale".to_string());
        let prs = project_prs(&[unknown_row]);
        // Not landed…
        assert_eq!(prs.landed_count, 0);
        // …but NOT a confident negative either: it has its own bucket + reason.
        assert_eq!(prs.unknown_count, 1);
        assert_eq!(prs.unknown[0].reason, "ref_stale");
        assert_eq!(prs.unknown[0].pr_number, 9);
        // Still an OPENED PR of this session.
        assert_eq!(prs.open_count, 1);
    }

    #[test]
    fn land_unknown_without_a_recorded_reason_still_never_reads_as_a_negative() {
        let prs = project_prs(&[pr_row(10, false, LAND_SIGNAL_UNKNOWN)]);
        assert_eq!(prs.unknown_count, 1);
        assert_eq!(prs.unknown[0].reason, "unspecified");
    }

    #[test]
    fn unavailable_envelope_always_carries_a_reason() {
        let env = SessionInfoEnvelope::unavailable("session_not_found");
        assert!(!env.available);
        assert_eq!(env.reason.as_deref(), Some("session_not_found"));
        assert!(env.body.is_none());
        let listing = SessionsInfoResponse::unavailable("lifecycle_store_unavailable");
        assert_eq!(listing.status, STATUS_UNAVAILABLE);
        assert_eq!(
            listing.reason.as_deref(),
            Some("lifecycle_store_unavailable")
        );
        assert!(listing.sessions.is_empty());
    }

    #[test]
    fn envelope_serializes_in_the_d1_camel_case_shape() {
        let rec = record("77777777-7777-4777-8777-777777777777");
        let env = project_session_info(
            &rec,
            None,
            true,
            project_prs(&[pr_row(1, true, "ff-land")]),
            unknown_tenancy(),
        );
        let v = serde_json::to_value(&env).unwrap();
        assert_eq!(v["available"], serde_json::json!(true));
        // Flattened groups sit at the top level, exactly as D1 spells them.
        assert_eq!(v["identity"]["terminalId"], serde_json::json!("term-1"));
        assert_eq!(
            v["identity"]["fleetSessionHandle"],
            serde_json::json!("fsh_abc")
        );
        assert_eq!(v["placement"]["zoneIndex"], serde_json::json!(3));
        assert_eq!(v["lifecycle"]["transcriptExists"], serde_json::json!(true));
        assert_eq!(v["lifecycle"]["restoreTier"], serde_json::Value::Null);
        // A record that was never restored says so — it does NOT borrow the
        // `failed` word, and it carries no pending marker.
        assert_eq!(
            v["lifecycle"]["restoreStatus"],
            serde_json::json!("not-restored")
        );
        assert_eq!(v["lifecycle"]["restorePendingAt"], serde_json::Value::Null);

        // M2: mid-restore, the stored tier is the pessimistic `failed` and the
        // RENDERED verdict must not read as terminal. The marker must be FRESH:
        // `describe_restore_status` ages one out after
        // `RESTORE_PENDING_TTL_MS`, and a fixed epoch-relative stamp would be
        // stale by decades.
        let fresh = chrono::Utc::now().timestamp_millis();
        let mut mid = rec.clone();
        mid.restore_tier = Some("failed".to_string());
        mid.restore_pending_at = Some(fresh);
        mid.restored_from_boot_at = Some(fresh);
        let mv = serde_json::to_value(project_session_info(
            &mid,
            None,
            true,
            project_prs(&[pr_row(1, true, "ff-land")]),
            unknown_tenancy(),
        ))
        .unwrap();
        assert_eq!(mv["lifecycle"]["restoreTier"], serde_json::json!("failed"));
        assert_eq!(
            mv["lifecycle"]["restoreStatus"],
            serde_json::json!("pending (not yet confirmed)")
        );
        assert_eq!(v["prs"]["status"], serde_json::json!("ok"));
        assert_eq!(v["prs"]["landedCount"], serde_json::json!(1));
        assert_eq!(
            v["prs"]["landed"][0]["landSignal"],
            serde_json::json!("ff-land")
        );
        // The scan provenance is camelCase on the wire and mirrored in
        // `src/components/terminal/useSessionInfo.ts` (`SessionPrs`).
        assert_eq!(v["prs"]["scanned"], serde_json::json!(false));
        assert_eq!(v["prs"]["scannedRepos"], serde_json::json!([]));
        // A degraded ledger spells its reason on the wire.
        let degraded = project_session_info(
            &rec,
            None,
            true,
            prs_unavailable("db_error"),
            unknown_tenancy(),
        );
        let dv = serde_json::to_value(&degraded).unwrap();
        assert_eq!(dv["prs"]["status"], serde_json::json!("unavailable"));
        assert_eq!(dv["prs"]["reason"], serde_json::json!("db_error"));
    }
}
