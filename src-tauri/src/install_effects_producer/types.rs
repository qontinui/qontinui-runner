//! Request/response types for the `POST /install-effects/run` route plus the
//! coord wire types this producer (de)serializes against.
//!
//! The coord-side definitions live in `qontinui-coord`'s
//! `src/install_effects.rs` and `src/policies/types.rs`. We mirror only the
//! subset this producer sends/reads, with the SAME serde casing, so the two
//! stay wire-compatible. Verified against coord `origin/main` 2026-06-06:
//!
//! - `PackageManager` is `#[serde(rename_all = "snake_case")]` →
//!   `npm | yarn | pnpm | cargo | pip | poetry`. There is NO `pipenv` variant
//!   (the route rejects pipenv explicitly — see [`super::pm`]).
//! - `PackageSpec { name, version_req?: Option<String> }`.
//! - `DeclareRequest` carries the dry-run/audit/importer/sibling payloads; in
//!   Phase 1 we send honest empty/absent defaults (the parsers are Phase 2).
//! - The predict-and-check response is `{ predicted_effect, resolution,
//!   risk_factors }` where `resolution` is a `PolicyResolution` internally
//!   tagged on `kind` (`decision` | `guidance` | `escalate`); the install gate
//!   only ever produces `kind: "decision"` whose inner `action` is tagged on
//!   `type` (`proceed` | `escalate`).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ===========================================================================
// Route request/response (runner-native)
// ===========================================================================

/// Body of `POST /install-effects/run`.
#[derive(Debug, Clone, Deserialize)]
pub struct RunRequest {
    /// Absolute path to the repo checkout the install runs in. Required — the
    /// `repo` slug coord keys on is its basename, and the subprocess `cwd`.
    pub repo_path: String,
    /// Package-manager token. Omitted ⇒ autodetected from `repo_path`
    /// (lockfile/manifest sniff, see [`super::pm`]). An unsupported token
    /// (e.g. `pipenv`) is a 400.
    #[serde(default)]
    pub package_manager: Option<String>,
    /// Explicitly requested specs (`name` + optional `version_req`). Empty ⇒ a
    /// lockfile-only refresh (e.g. `npm install` / `cargo update`).
    #[serde(default)]
    pub packages: Vec<PackageSpecInput>,
    /// Install dev/build deps (`npm install -D`, `cargo add --dev`,
    /// `pip` no-op). Default false.
    #[serde(default)]
    pub dev: bool,
    /// When the predict-and-check gate returns `Escalate`, proceed with the
    /// install anyway. Recorded in the run context so the Phase-3 verify push
    /// can mark provenance with an `+overridden` suffix. Default false.
    #[serde(default)]
    pub override_escalation: bool,
    /// Run declare + predict-and-check and STOP — the cheap "what would this
    /// do" probe. No install, no verify. Default false.
    #[serde(default)]
    pub dry_run_only: bool,
    /// Extra registry-auth env var names to inject from the keyring on top of
    /// the well-known set (e.g. a custom `CARGO_REGISTRIES_<NAME>_TOKEN`). Each
    /// must be `[A-Z0-9_]+`; invalid/absent names are silently skipped. The
    /// VALUES are never carried on the request — only the names; the producer
    /// reads the secret from its OS keyring. Default empty.
    #[serde(default)]
    pub registry_env: Vec<String>,
}

/// A requested spec on the route request (mirror of coord's [`PackageSpec`]
/// with a friendlier route field name).
#[derive(Debug, Clone, Deserialize)]
pub struct PackageSpecInput {
    pub name: String,
    #[serde(default)]
    pub version_req: Option<String>,
}

impl From<&PackageSpecInput> for PackageSpec {
    fn from(p: &PackageSpecInput) -> Self {
        PackageSpec {
            name: p.name.clone(),
            version_req: p.version_req.clone(),
        }
    }
}

/// Response of `POST /install-effects/run`.
#[derive(Debug, Clone, Serialize)]
pub struct RunResponse {
    /// The uuid minted for this run; echoed into every coord call so declare /
    /// predict-and-check / (Phase 3) verify correlate.
    pub correlation_id: Uuid,
    /// The resolved repo slug (basename of `repo_path`).
    pub repo: String,
    /// The package manager actually used (after autodetect).
    pub package_manager: String,
    /// The coord gate verdict: `proceed` | `escalate`. Derived from the
    /// predict-and-check `resolution`.
    pub gate: GateOutcome,
    /// The risk factors coord returned (empty ⇒ within autonomy thresholds).
    pub risk_factors: Vec<String>,
    /// True iff an `Escalate` was overridden via `override_escalation`. The
    /// Phase-3 verify push uses this to suffix provenance with `+overridden`.
    pub escalation_overridden: bool,
    /// The install subprocess outcome, when one ran. `None` for a blocked
    /// escalation (no override) or `dry_run_only`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub install: Option<InstallOutcome>,
    /// True when the route stopped after predict-and-check (`dry_run_only`).
    pub dry_run_only: bool,
    /// The raw predict-and-check resolution coord returned (passed through so
    /// the caller can branch on the typed `kind`/`action`).
    pub resolution: serde_json::Value,
    /// The composed verify outcome coord returned after the install (Phase 3).
    /// `None` when no verify ran (`dry_run_only` / blocked escalation).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verify: Option<VerifyOutcome>,
}

/// The slice of coord's `InstallVerification` the route surfaces to the caller
/// (Phase 3). Carries the composed D3 outcome + per-dimension verdicts so the
/// route's JSON now answers "did the install match its prediction".
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifyOutcome {
    /// The composed D3 outcome token (`confirmed | contradiction | surprise |
    /// partial`).
    pub composed_outcome: String,
    /// Coord's overall rationale for the composed outcome.
    pub rationale: String,
    /// Per-dimension verdicts (fs / dep_graph / security / types / build /
    /// coord). Passed through verbatim from coord's response.
    #[serde(default)]
    pub per_dimension: serde_json::Value,
    /// Fraction of the six dimensions that produced a real (non-gap) verdict.
    #[serde(default)]
    pub coverage: f64,
    /// Coord's verify provenance string.
    #[serde(default)]
    pub provenance: String,
    /// The correlation id coord echoed (must match this run's).
    #[serde(default)]
    pub correlation_id: Option<Uuid>,
}

/// The gate verdict the producer derives from coord's `PolicyResolution`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GateOutcome {
    /// `kind: decision` + `action.type: proceed` — within autonomy thresholds.
    Proceed,
    /// `kind: decision` + `action.type: escalate` (or a `kind: escalate`
    /// frame) — at least one autonomy threshold crossed.
    Escalate,
}

/// The captured outcome of the install subprocess.
#[derive(Debug, Clone, Serialize)]
pub struct InstallOutcome {
    /// The argv that ran (program first), for audit/debug.
    pub command: Vec<String>,
    /// Process exit code (`None` ⇒ killed by signal/timeout).
    pub exit_code: Option<i32>,
    pub success: bool,
    /// True iff the subprocess was killed for exceeding the bounded timeout.
    pub timed_out: bool,
    pub stdout: String,
    pub stderr: String,
}

// ===========================================================================
// coord wire types — mirror of qontinui-coord (snake_case where coord renames)
// ===========================================================================

/// Mirror of coord `install_effects::PackageManager`
/// (`#[serde(rename_all = "snake_case")]`). NO `pipenv` variant by design.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PackageManager {
    Npm,
    Yarn,
    Pnpm,
    Cargo,
    Pip,
    Poetry,
}

impl PackageManager {
    pub fn as_str(self) -> &'static str {
        match self {
            PackageManager::Npm => "npm",
            PackageManager::Yarn => "yarn",
            PackageManager::Pnpm => "pnpm",
            PackageManager::Cargo => "cargo",
            PackageManager::Pip => "pip",
            PackageManager::Poetry => "poetry",
        }
    }
}

/// Mirror of coord `install_effects::PackageSpec`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageSpec {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version_req: Option<String>,
}

/// Mirror of coord `install_effects::PackageId` (resolved `name`+`version`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageId {
    pub name: String,
    pub version: String,
}

/// Mirror of coord `install_effects::VersionBump`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionBump {
    pub name: String,
    pub from: String,
    pub to: String,
}

/// Mirror of coord `install_effects::Cve`. `severity` is the snake_case token
/// (`none | low | moderate | high | critical`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cve {
    pub id: String,
    #[serde(default)]
    pub severity: Option<String>,
    #[serde(default)]
    pub package: Option<String>,
}

/// Mirror of coord `install_effects::ImporterRecord` — the importer inventory
/// row that sharpens the `types` (import-break) dimension. **Phase 2 fills
/// these from the affected-source walk; Phase 1 always sends an empty list.**
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImporterRecord {
    pub package: String,
    #[serde(default)]
    pub importers: Vec<String>,
    #[serde(default)]
    pub from: Option<String>,
    #[serde(default)]
    pub to: Option<String>,
}

/// Mirror of coord `install_effects::DryRunData`. **Phase 2 builds this from
/// the parsed dry-run/lockfile output; Phase 1 sends `None` (honest unknown)
/// so coord's predictions degrade to honest `Unknown` rather than fabricated
/// data.** Every field defaults empty so a partial dry-run still sharpens what
/// it can.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DryRunData {
    #[serde(default)]
    pub direct_added: Vec<PackageId>,
    #[serde(default)]
    pub transitive_added: Vec<PackageId>,
    #[serde(default)]
    pub version_bumped: Vec<VersionBump>,
    #[serde(default)]
    pub predicted_pinned: BTreeMap<String, String>,
    #[serde(default)]
    pub manifest_paths: Vec<String>,
    #[serde(default)]
    pub lockfile_paths: Vec<String>,
    #[serde(default)]
    pub predicted_cves: Vec<Cve>,
}

/// Body of `POST /coord/installs/declare` and `…/predict-and-check`. Mirror of
/// coord `install_effects::DeclareRequest`. We send `dry_run`/importer/sibling
/// as honest empty/absent defaults in Phase 1.
#[derive(Debug, Clone, Serialize)]
pub struct DeclareRequest {
    pub package_manager: PackageManager,
    #[serde(default)]
    pub package_specs: Vec<PackageSpec>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    /// We intentionally do NOT send `repo_path` to coord: the coord-side
    /// dry-run invoker is gated OFF in prod, and this producer is the one that
    /// owns the worktree. Sending the path would only invite a coord-side
    /// shell-out if someone flipped the flag. Left as `None` always.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<Uuid>,
    /// Caller-supplied dry-run ground truth. Phase 1 ⇒ `None` (the Phase 2
    /// parser plugs in here — see [`super::DryRunProducer`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dry_run: Option<DryRunData>,
    /// Whether the caller ran a native audit over the dry-run plan. Phase 1 ⇒
    /// honest `false` (the audit parser is Phase 2).
    pub security_audited: bool,
    /// Importer inventory (§3.2). Phase 1 ⇒ empty (the importer walk is
    /// Phase 2).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub importer_inventory: Vec<ImporterRecord>,
    /// Sibling manifests for the cross-repo peer-dep walk (§3.4). Phase 1 ⇒
    /// empty.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub sibling_manifests: BTreeMap<String, String>,
}

/// Response of `POST /coord/installs/predict-and-check`. Mirror of coord
/// `install_effects::PredictAndCheckResponse`. We keep `predicted_effect` and
/// `resolution` as raw JSON (their full shapes are coord-internal and not
/// load-bearing for the gate decision — only the `resolution.kind`/`action`
/// discriminants are).
#[derive(Debug, Clone, Deserialize)]
pub struct PredictAndCheckResponse {
    #[serde(default)]
    pub predicted_effect: serde_json::Value,
    pub resolution: serde_json::Value,
    #[serde(default)]
    pub risk_factors: Vec<String>,
}

// ===========================================================================
// verify wire types (Phase 3) — mirror of qontinui-coord VerifyRequest
// ===========================================================================

/// Body of `POST /coord/installs/verify`. Mirror of coord
/// `install_effects::VerifyRequest` (verified against coord `origin/main`
/// 2026-06-06). We send the observed side (post-install lockfile pins, native
/// audit, per-file typecheck) plus the predicted echoes from the declare-time
/// signature, and the touched `paths` that scope coord's `coord.fs_observations`
/// lookup. We intentionally supply NO `observed_bundle_stats` (honest absent —
/// no bundler probe this round) and NO `predicted_post_blob_shas` (coord reads
/// the observed post-shas from the FS-observation push under the SAME
/// correlation_id).
#[derive(Debug, Clone, Serialize)]
pub struct VerifyRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,

    // ---- predicted side (echoed from declare's dry-run signature) -----------
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub predicted_pinned: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub predicted_cves: Vec<Cve>,
    /// FS paths the install touched (manifest + lockfile). Scopes the
    /// `coord.fs_observations` lookup alongside the correlation_id.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,

    // ---- observed side (post-install) ---------------------------------------
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub observed_pinned: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub observed_cves: Vec<Cve>,
    pub observed_audit_ran: bool,

    // ---- Ξ_Type — per-file typecheck results (runner-side loopback) ---------
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub observed_typecheck: Vec<TypecheckResult>,
    /// `true` ⇒ the per-file typecheck loop actually ran ≥1 file (so an EMPTY
    /// `observed_typecheck` reads as "no affected files ⇒ nothing to break" ⇒
    /// Confirmed, not "not run" ⇒ Partial).
    pub typecheck_ran: bool,
}

/// One per-file typecheck result, mirror of coord
/// `install_effects::TypecheckResult`. Built from the runner's own
/// `POST /code-semantics/typecheck` envelope: `pass` ⇐ `result.ok`,
/// `coverage`/`provenance` ⇐ the envelope, `diagnostics` ⇐ `result.errors`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TypecheckResult {
    pub file: String,
    pub pass: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provenance: Option<String>,
    pub coverage: f64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<String>,
}

// ===========================================================================
// FS-observation wire types (Phase 3) — mirror of coord edit_effects
// ===========================================================================
//
// We re-declare a thin install-effects-local copy rather than depend on the
// `agent_worktree::fs_observer` request types (those are tailored to the
// git-diff worktree push). Field names are load-bearing — coord deserializes
// exactly these keys (`FsObservationsRequest` / `FsObservationItem`).

/// One observed manifest/lockfile mutation pushed post-install. Mirror of coord
/// `edit_effects::FsObservationItem`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FsObservation {
    pub path: String,
    /// Pre-install blob sha. `None` for a file the install newly created.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pre_sha: Option<String>,
    pub post_sha: String,
    pub hunks: serde_json::Value,
}

/// Body of `POST /coord/fs/observations`. Mirror of coord
/// `edit_effects::FsObservationsRequest`. The `correlation_id` MUST match the
/// declare/verify correlation_id so coord's `load_observed_post_shas` joins the
/// FS rows to this install.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FsObservationsRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    pub observations: Vec<FsObservation>,
}

impl PredictAndCheckResponse {
    /// Derive the [`GateOutcome`] from coord's internally-tagged
    /// `PolicyResolution`. The install gate emits `kind: "decision"` with an
    /// inner `action` tagged on `type`; a `proceed` action ⇒ [`GateOutcome::Proceed`],
    /// anything else (an `escalate` action, or a top-level `kind: "escalate"`
    /// frame) ⇒ [`GateOutcome::Escalate`] — fail-safe: an unrecognized shape
    /// blocks rather than silently proceeds.
    pub fn gate(&self) -> GateOutcome {
        let r = &self.resolution;
        match r.get("kind").and_then(|k| k.as_str()) {
            Some("decision") => {
                let action_type = r
                    .get("action")
                    .and_then(|a| a.get("type"))
                    .and_then(|t| t.as_str());
                match action_type {
                    Some("proceed") => GateOutcome::Proceed,
                    _ => GateOutcome::Escalate,
                }
            }
            // `guidance` / `escalate` / unknown ⇒ do NOT auto-proceed.
            _ => GateOutcome::Escalate,
        }
    }
}
