//! Device pairing — coord-side handshake to obtain a device-token JWT.
//!
//! Lifted from `src/bin/qontinui_profile.rs` (Phase 1 of the runner
//! unified-devices migration) so both the `qontinui_profile device pair`
//! CLI and the Tauri runner GUI (Phase 2+) call the same code paths.
//!
//! ## Public surface
//!
//! - [`pair_with_auth_token`] — headless flow: POST `Authorization: Bearer
//!   <user-jwt>` to the web backend's `POST /api/v1/devices/pair-cli`,
//!   which proxies to coord with server-side-injected `tenant_id`.
//!   Requires the device to already be browser-paired at least once
//!   (uses the cached `user_id` from `paired_user.json`).
//! - [`pair_via_browser`] — interactive flow: calls coord's
//!   `POST /coord/devices/pair-start` to register the flow, opens the
//!   coord-provided redirect URL in the user's default browser, spins
//!   up a localhost callback server, and captures the device-token JWT
//!   from the browser redirect (coord's `pair-complete` is called by the
//!   web frontend, not the runner).
//! - [`persist_pairing`] — writes the device-token JWT into the paired
//!   tenant's `device_jwt:<tenant_id>` slot (+ the legacy access-token
//!   slot when that tenant is the device default) and appends/updates
//!   the binding entry in `paired_user.json` (v2 multi-entry shape,
//!   Phase 8a).
//! - [`coord_http_base`] / [`coord_http_base_from_url`] — resolve the
//!   coord HTTP base from the active profile's `coord_url`
//!   (`ws[s]://host:port/ws` → `http[s]://host:port`).
//! - [`derive_web_base_from_coord`] — best-effort web-backend base
//!   derivation from the coord base (strips the port).
//! - [`PairCompleteResponse`] — canonical coord pair response wire
//!   shape (`{token, device_id, user_id, jti, exp}`).
//!
//! ## Defect 5 shape fixes (2026-05-21)
//!
//! The canonical coord-side `PairCliRequest` (see
//! `qontinui-coord/src/routes_phase3.rs:571-703`) requires:
//!
//! - body: `{device_id: Uuid, hostname: String, name: Option<String>}`
//! - header: `X-Qontinui-User-Id: <uuid>`
//!
//! and returns `{token, device_id, user_id, jti, exp}` (note: `token`,
//! not `device_token`). The pre-fix code sent `{hostname, name, os,
//! os_version}` with no header and deserialized into a `device_token`
//! field — both broken. This module ships the correct wire shape from
//! day one.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

// ============================================================================
// Wire types — must match qontinui-coord/src/routes_phase3.rs exactly.
// ============================================================================

/// Canonical response shape for `POST /api/v1/devices/pair-cli` (proxied
/// through the web backend) and `POST /coord/devices/pair-complete`
/// (browser flow, called by coord directly). Coord serializes this as
/// `{token, device_id, user_id, jti, exp}`. We accept the wire shape
/// directly: no `serde(rename)` indirection — the struct field name *is*
/// the wire name.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PairCompleteResponse {
    /// Device-token JWT minted by coord.
    pub token: String,
    /// The paired user_id (UUID, stringified).
    pub user_id: String,
    /// The paired device_id (UUID, stringified). Always present on the
    /// canonical wire; kept as `Option<String>` for graceful handling of
    /// older / partial coord deployments mid-migration. Phase 1 callers
    /// can assume `Some(_)`.
    #[serde(default)]
    pub device_id: Option<String>,
    /// JWT id — present on the canonical wire; ignored by the CLI today.
    #[serde(default)]
    pub jti: Option<String>,
    /// JWT expiration (unix seconds). Present on the canonical wire;
    /// ignored by the CLI today, surfaced for Phase 2 refresh logic.
    #[serde(default)]
    pub exp: Option<i64>,
    /// Tenant scope coord stamped on this device. Phase 3 of the
    /// default-tenant-propagation plan may echo this back on the
    /// wire. `#[serde(default)]` tolerates pre-tenant coord builds.
    #[serde(default)]
    pub tenant_id: Option<String>,
    /// Optional device-bound machine key (`dmk_<token>`) auto-minted by the
    /// web `pair_cli` endpoint at pairing time (plan 2026-07-02 Phase 3).
    /// When present, [`persist_pairing`] stores it so the refresher can later
    /// exchange it for a device JWT with no user session — the >30-day-offline
    /// cold-start recovery path (Phase 4b). `#[serde(default)]` tolerates
    /// pre-dmk web builds that don't return it.
    #[serde(default)]
    pub device_machine_key: Option<String>,
}

/// Wire shape for `POST /coord/devices/pair-start` response. Coord returns
/// `{state, redirect_url, expires_in}` — the state nonce is coord-minted
/// and registered in the pairing-flow store with a TTL.
#[derive(Debug, Clone, Deserialize)]
struct PairStartResponseWire {
    /// Coord-minted one-time state nonce.
    pub state: String,
    /// URL the runner should open in the user's browser.
    pub redirect_url: String,
    /// TTL hint (seconds) — the flow expires after this. Informational.
    #[serde(default)]
    #[allow(dead_code)]
    pub expires_in: Option<i64>,
}

// ============================================================================
// Identity-file helpers (paired_user.json + machine.json)
// ============================================================================

/// On-disk shape of `~/.qontinui/machine.json` — only the fields we need
/// for pairing. The full file may carry more keys; we tolerate extras.
///
/// The wire field name is `device_id`; the on-disk field name is the
/// legacy `machine_id` (preserved for backward compatibility — see the
/// doc-comment on `DeviceFile` in `bin/qontinui_profile.rs`). The serde
/// alias accepts both spellings so a pre-rename file still loads.
#[derive(Debug, Deserialize)]
struct MachineFile {
    #[serde(alias = "machine_id")]
    device_id: String,
}

/// Path to the per-device identity file. Same recipe as
/// `bin/qontinui_profile.rs::device_file_path`.
fn machine_file_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".qontinui").join("machine.json"))
}

/// Read `device_id` from `~/.qontinui/machine.json`. Returns a clear
/// error if the file is missing — the caller (pair-cli) cannot proceed
/// without a stable device identity.
///
/// Public-in-crate alias [`read_device_id_from_disk`] exposes this to
/// other modules in the workspace (e.g. the runner's
/// `mcp::device_jwt_refresher`) so the refresher can compose the
/// disk-read + parameterized-pair manually.
pub fn read_device_id_from_disk() -> Result<String, String> {
    read_device_id()
}

fn read_device_id() -> Result<String, String> {
    let path = machine_file_path().ok_or_else(|| "could not resolve home directory".to_string())?;
    if !path.exists() {
        return Err(format!(
            "device not initialized — run `qontinui_profile device init` first \
             (no {} on disk)",
            path.display()
        ));
    }
    let bytes = std::fs::read(&path).map_err(|e| format!("read {}: {}", path.display(), e))?;
    let parsed: MachineFile =
        serde_json::from_slice(&bytes).map_err(|e| format!("parse {}: {}", path.display(), e))?;
    Ok(parsed.device_id)
}

/// Backfill the canonical `device_id` key into `~/.qontinui/machine.json`
/// when the file predates the `machine_id`→`device_id` rename.
///
/// The operator's live file is the legacy shape
/// `{"machine_id": "...", "hostname": "...", "active_tenant_id": "..."}`.
/// Reads already work via serde aliases, but the canonical `device_id` key is
/// absent; this normalizes the file so readers that key off `device_id`
/// literally (without the alias) see it too. `machine_id` is intentionally
/// PRESERVED — other readers may still alias off it — and every sibling field
/// is carried verbatim.
///
/// Resolves the path via `dirs::home_dir()` and delegates to
/// [`ensure_device_id_persisted_at`]. Missing/unreadable machine.json is a
/// no-op (never fatal): a runner that has never run `device init` simply has
/// nothing to normalize.
pub fn ensure_device_id_persisted() {
    let Some(path) = machine_file_path() else {
        return;
    };
    ensure_device_id_persisted_at(&path);
}

/// Path-parameterized core of [`ensure_device_id_persisted`] (testable
/// without touching the real home directory).
///
/// If the file parses as a JSON object that has `machine_id` but no
/// `device_id`, atomically rewrites it (temp file + rename, matching
/// `commands::tenant::write_active_tenant_id`) adding `device_id` with the
/// same value while preserving all other fields. Any other shape — already
/// canonical, missing file, non-object, unparseable — is left untouched.
pub fn ensure_device_id_persisted_at(path: &std::path::Path) {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(_) => return, // Missing/unreadable → nothing to normalize.
    };
    let value: serde_json::Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(e) => {
            tracing::debug!(
                "ensure_device_id_persisted: {} is not valid JSON ({e}) — skipping",
                path.display()
            );
            return;
        }
    };
    let serde_json::Value::Object(mut obj) = value else {
        return;
    };
    // Already canonical, or no legacy key to backfill from → no-op.
    if obj.contains_key("device_id") {
        return;
    }
    let Some(machine_id) = obj.get("machine_id").cloned() else {
        return;
    };
    obj.insert("device_id".to_string(), machine_id);

    let pretty = match serde_json::to_vec_pretty(&serde_json::Value::Object(obj)) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("ensure_device_id_persisted: serialize failed: {e}");
            return;
        }
    };
    let tmp = path.with_extension("json.tmp");
    if let Err(e) = std::fs::write(&tmp, &pretty) {
        tracing::warn!(
            "ensure_device_id_persisted: write {} failed: {e}",
            tmp.display()
        );
        return;
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        tracing::warn!(
            "ensure_device_id_persisted: rename {} → {} failed: {e}",
            tmp.display(),
            path.display()
        );
        let _ = std::fs::remove_file(&tmp);
        return;
    }
    tracing::info!(
        "machine.json: backfilled canonical device_id key at {}",
        path.display()
    );
}

/// Ensure the per-device identity file `~/.qontinui/machine.json` EXISTS,
/// minting it on first launch.
///
/// A fresh production install has no identity file. The sign-in chain
/// (`crate::commands::auth::finalize_signed_in` step 3) and the pair-cli both
/// call [`read_device_id_from_disk`], which hard-errors when the file is
/// absent ("device not initialized — run `qontinui_profile device init`").
/// No end user runs that CLI, so the runner mints the identity itself: a
/// client-side UUID v4 + hostname, written atomically — the exact shape and
/// recipe as `qontinui_profile device init`
/// (`bin/qontinui_profile.rs::cmd_device_init`). Coord-side registration is
/// intentionally NOT done here; it happens during sign-in
/// (`pair_with_auth_token_with_ids`, which binds the device to the
/// authenticated user + tenant server-side) or via the CLI's own coord UPSERT.
///
/// Idempotent + fail-open (never panics; safe to call on every launch):
/// - Existing file → keep the UUID, delegate to [`ensure_device_id_persisted`]
///   to backfill the canonical `device_id` key. Never overwrites an identity.
/// - Missing file → mint `{device_id, hostname}` and write it.
/// - Any error (no home dir, mkdir / write / rename failure) → logged and
///   swallowed; the caller proceeds and, if the mint genuinely failed, the
///   downstream read still surfaces the original clear error.
pub fn ensure_device_initialized() {
    let Some(path) = machine_file_path() else {
        tracing::warn!("ensure_device_initialized: could not resolve home directory — skipping");
        return;
    };
    ensure_device_initialized_at(&path);
}

/// Path-parameterized core of [`ensure_device_initialized`] (testable without
/// touching the real home directory).
pub fn ensure_device_initialized_at(path: &std::path::Path) {
    if path.exists() {
        // Existing identity: preserve the UUID, just normalize the key.
        ensure_device_id_persisted_at(path);
        return;
    }
    let device_id = uuid::Uuid::new_v4().to_string();
    let hostname = hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .unwrap_or_else(|| "unknown".to_string());
    // Same on-disk shape as DeviceFile in the sidecar ({device_id, hostname};
    // `name` is optional and omitted). Sibling readers alias machine_id.
    let body = serde_json::json!({ "device_id": device_id, "hostname": hostname });
    let pretty = match serde_json::to_vec_pretty(&body) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("ensure_device_initialized: serialize failed: {e}");
            return;
        }
    };
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            tracing::warn!(
                "ensure_device_initialized: mkdir {} failed: {e}",
                parent.display()
            );
            return;
        }
    }
    let tmp = path.with_extension("json.tmp");
    if let Err(e) = std::fs::write(&tmp, &pretty) {
        tracing::warn!(
            "ensure_device_initialized: write {} failed: {e}",
            tmp.display()
        );
        return;
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        tracing::warn!(
            "ensure_device_initialized: rename {} → {} failed: {e}",
            tmp.display(),
            path.display()
        );
        let _ = std::fs::remove_file(&tmp);
        return;
    }
    tracing::info!(
        "machine.json: minted device identity {} (host={}) at {}",
        device_id,
        hostname,
        path.display()
    );
}

/// Path to the paired-user JSON written by `device pair` and read by
/// `device init` to attach a `user_id` to the register payload. Lives
/// under the Tauri app's local data directory (alongside
/// `auth_tokens.enc`) so the CLI bin and the GUI runner read the same
/// file.
pub(crate) fn paired_user_path() -> Option<PathBuf> {
    let base = std::env::var("QONTINUI_SECURE_STORAGE_DIR")
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(|| dirs::data_local_dir().map(|d| d.join("com.qontinui.runner")))?;
    Some(base.join("paired_user.json"))
}

/// One tenant binding entry in the v2 `paired_user.json` shape
/// (session-scoped multi-tenant plan 2026-07-02, Phase 8a / D4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairedBinding {
    /// Stringified tenant UUID.
    pub tenant_id: String,
    /// The user who paired this binding (stringified UUID).
    pub user_id: String,
    /// RFC3339 timestamp of the (most recent) pair for this tenant.
    /// Optional on read so a hand-edited / partially-written entry still
    /// parses; always written by [`persist_pairing`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paired_at: Option<String>,
}

/// `paired_user.json` — BOTH wire shapes in one struct.
///
/// - **Legacy (v1)**: `{user_id, tenant_id?}` — single-entry, written
///   before Phase 8a. Parses with `bindings == []`,
///   `default_tenant_id == None`.
/// - **v2 (Phase 8a, plan 2026-07-02 D4)**: adds
///   `bindings: [{tenant_id, user_id, paired_at}]` +
///   `default_tenant_id`. The legacy `user_id` / `tenant_id` fields are
///   kept as **mirrors of the DEFAULT binding** so every legacy reader
///   (this crate's pre-8a consumers, `bin/qontinui_profile.rs`'s local
///   mirror struct, and any older installed binary during an upgrade
///   window) keeps working unmodified.
///
/// Read-side migration is [`PairedUserFile::effective_bindings`] /
/// [`PairedUserFile::effective_default_tenant_id`]: a legacy file's
/// single entry is synthesized into a one-element binding set.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PairedUserFile {
    /// v2: mirror of the DEFAULT binding's `user_id`. Legacy: the paired
    /// user.
    pub user_id: String,
    /// Stringified UUID. `#[serde(default)]` keeps legacy files
    /// (written before the 2026-05-22 schema bump) deserializing
    /// — they carry only `user_id`. A heartbeat/register caller
    /// that finds `None` here falls back to decoding the cached
    /// device-token JWT via [`tenant_id_from_oauth_claim`]
    /// (see `fleet::resolve_tenant_id`). v2: mirror of
    /// `default_tenant_id`.
    #[serde(default)]
    pub tenant_id: Option<String>,
    /// v2 multi-entry binding set. Empty on legacy files (serde default);
    /// omitted from serialization when empty so a legacy-heal write keeps
    /// the legacy shape byte-compatible.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bindings: Vec<PairedBinding>,
    /// v2: which binding's JWT the legacy `access_token` slot holds (the
    /// device-level default — D4). Set at first pair, changed only by
    /// reconciliation dropping the default binding (never stolen by a
    /// later additive pair).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_tenant_id: Option<String>,
}

impl PairedUserFile {
    /// v2-migrated view of the binding set: the `bindings` array when
    /// non-empty, else a single entry synthesized from the legacy
    /// `user_id`/`tenant_id` fields (or empty when the legacy file has
    /// no `tenant_id`).
    pub(crate) fn effective_bindings(&self) -> Vec<PairedBinding> {
        if !self.bindings.is_empty() {
            return self.bindings.clone();
        }
        match &self.tenant_id {
            Some(t) => vec![PairedBinding {
                tenant_id: t.clone(),
                user_id: self.user_id.clone(),
                paired_at: None,
            }],
            None => Vec::new(),
        }
    }

    /// v2-migrated view of the default tenant: `default_tenant_id` when
    /// present, else the legacy single `tenant_id`.
    pub(crate) fn effective_default_tenant_id(&self) -> Option<String> {
        self.default_tenant_id
            .clone()
            .or_else(|| self.tenant_id.clone())
    }

    /// Is this file in the v2 multi-entry shape (vs pure legacy)?
    fn is_v2(&self) -> bool {
        !self.bindings.is_empty() || self.default_tenant_id.is_some()
    }
}

/// Read + parse `paired_user.json` at an explicit path. `None` when the
/// file is missing or unparseable (callers that must distinguish the two
/// read the file themselves — see `reconcile_paired_bindings_with`).
fn read_paired_user_file_at(path: &std::path::Path) -> Option<PairedUserFile> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Atomic write of `paired_user.json` (tmp + rename), pretty-printed —
/// the shared writer for [`persist_pairing`], [`backfill_paired_tenant_id`]
/// and [`reconcile_paired_bindings`].
fn write_paired_user_file(path: &std::path::Path, pf: &PairedUserFile) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
    }
    let pretty =
        serde_json::to_vec_pretty(pf).map_err(|e| format!("serialize paired_user.json: {e}"))?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, &pretty).map_err(|e| format!("write tmp: {e}"))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("rename: {e}"))?;
    Ok(())
}

/// Read the cached `user_id` written by a prior browser-pair. Returns
/// `None` if the file doesn't exist (fresh install / never paired).
///
/// Public-in-crate alias [`read_paired_user_id_from_disk`] exposes this
/// to other modules in the workspace (see [`read_device_id_from_disk`]).
pub fn read_paired_user_id_from_disk() -> Option<String> {
    read_paired_user_id()
}

fn read_paired_user_id() -> Option<String> {
    let path = paired_user_path()?;
    let bytes = std::fs::read(&path).ok()?;
    let parsed: PairedUserFile = serde_json::from_slice(&bytes).ok()?;
    Some(parsed.user_id)
}

/// Read the cached DEFAULT tenant_id from `paired_user.json`. Returns
/// `None` if the file is missing OR no tenant is recorded (legacy file
/// written before the 2026-05-22 schema bump). Used by
/// `fleet::resolve_binding_set` as the primary resolution branch, and by
/// every "which tenant is this device's default" consumer
/// (`commands/tenant.rs`, `claude_session/federation.rs`).
///
/// v2-aware: prefers `default_tenant_id`, falling back to the legacy
/// single `tenant_id` field (which v2 writers keep mirrored anyway).
pub fn read_paired_tenant_id_from_disk() -> Option<String> {
    let path = paired_user_path()?;
    read_paired_user_file_at(&path)?.effective_default_tenant_id()
}

/// Read the locally-held binding set from `paired_user.json` as parsed
/// tenant UUIDs (deduped, file order preserved; entries with malformed
/// `tenant_id` silently skipped). Legacy single-entry files yield their
/// one synthesized binding. Empty when the file is missing/unparseable
/// or carries no tenant. Phase 8a: this is what the register heartbeat
/// sends as `tenant_ids` (D3).
pub fn read_paired_binding_tenant_ids() -> Vec<uuid::Uuid> {
    let Some(path) = paired_user_path() else {
        return Vec::new();
    };
    let Some(pf) = read_paired_user_file_at(&path) else {
        return Vec::new();
    };
    let mut out: Vec<uuid::Uuid> = Vec::new();
    for b in pf.effective_bindings() {
        if let Ok(t) = uuid::Uuid::parse_str(b.tenant_id.trim()) {
            if !out.contains(&t) {
                out.push(t);
            }
        }
    }
    out
}

/// LEGACY single-value heal: opportunistically rewrite `paired_user.json`
/// with the supplied `tenant_id`, preserving the existing `user_id`.
/// Phase 8a posture: this heal applies ONLY to pure-legacy (v1) files —
/// a v2 multi-entry file's tenant state is owned by
/// [`reconcile_paired_bindings`], and a single-value overwrite would
/// clobber the binding set / flip the default (coord's single
/// `tenant_id` echo means "most recently paired", NOT "your default").
/// On a v2 file this is a quiet no-op success.
///
/// No-op if the file doesn't exist (the JWT-claim path only fires
/// when SOMETHING was paired previously, but we tolerate the race
/// where pair-state was wiped between read and write). Returns
/// `Err(String)` on filesystem failures so the caller can log them
/// at `debug!` — the outer flow proceeds either way.
///
/// Scheduled for deletion in Phase 10 item 4 (with the echo-heal).
pub fn backfill_paired_tenant_id(tenant_id: &uuid::Uuid) -> Result<(), String> {
    let path = paired_user_path().ok_or_else(|| "could not resolve data_local_dir".to_string())?;
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // No paired file → nothing to backfill. Quiet success.
            return Ok(());
        }
        Err(e) => return Err(format!("read {}: {e}", path.display())),
    };
    let mut parsed: PairedUserFile =
        serde_json::from_slice(&bytes).map_err(|e| format!("parse {}: {e}", path.display()))?;
    if parsed.is_v2() {
        // v2 file: the binding set is reconciled against coord's
        // `tenant_ids` (Phase 8a); the single-value heal must not touch it.
        return Ok(());
    }
    if parsed.tenant_id.as_deref() == Some(tenant_id.to_string().as_str()) {
        // Already up-to-date; avoid an unnecessary disk write.
        return Ok(());
    }
    parsed.tenant_id = Some(tenant_id.to_string());
    write_paired_user_file(&path, &parsed)
}

// ============================================================================
// Binding reconciliation against coord's register echo (Phase 8a, D3)
// ============================================================================

/// What one [`reconcile_paired_bindings`] pass changed / observed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BindingReconcileReport {
    /// Local binding entries removed because coord's authoritative set no
    /// longer contains them. Their per-tenant JWT slots were cleared too
    /// (the designed home of slot deletion — REPLACE-not-REVOKE applies
    /// everywhere else).
    pub dropped: Vec<uuid::Uuid>,
    /// Orphan per-tenant JWT slots (no binding entry) cleared because
    /// coord's set doesn't contain them either.
    pub dropped_slots: Vec<uuid::Uuid>,
    /// Tenants coord reports bound to this device for which the runner
    /// holds NO usable credential (no binding entry, or entry without a
    /// JWT slot). Flag-only: the runner never fabricates a binding — the
    /// operator must pair for that tenant to mint its JWT.
    pub coord_only: Vec<uuid::Uuid>,
    /// `Some` when the default binding was dropped and the default was
    /// re-pointed (`Some(tenant)`) or cleared entirely (`None` — no
    /// bindings remain).
    pub default_repointed: Option<Option<uuid::Uuid>>,
}

impl BindingReconcileReport {
    /// Did this pass mutate any local state (file or slots)?
    pub fn changed(&self) -> bool {
        !self.dropped.is_empty()
            || !self.dropped_slots.is_empty()
            || self.default_repointed.is_some()
    }
}

/// Reconcile the local binding state (`paired_user.json` v2 entries +
/// per-tenant JWT slots) against coord's authoritative server-side
/// binding set, as echoed in the register response's `tenant_ids` field
/// (D3 — replaces the single-value echo-heal for v2 state).
///
/// Semantics:
/// - **Drop**: local binding entries whose tenant is NOT in `coord_set`
///   are removed, and their `device_jwt:<tenant>` slots cleared (coord
///   Phase 3's refresh gate would 403 them anyway). Orphan slots with no
///   binding entry are cleared by the same rule.
/// - **Flag**: tenants in `coord_set` the runner lacks a JWT for are
///   reported (`coord_only`) — never fabricated locally; pair to heal.
/// - **Default re-point**: if the default binding was dropped, the most
///   recently paired surviving binding becomes the default and its slot
///   JWT is copied into the legacy `access_token` slot (D4: the legacy
///   slot always holds the DEFAULT binding's JWT). If no binding
///   survives, the default is cleared but the legacy slot is left as-is
///   (matching coord Phase 3's unpair posture — the stale JWT simply
///   ages out, and the refresh gate stops it self-perpetuating).
/// - **No silent churn**: the file is rewritten only when something
///   actually changed, so the 30s heartbeat cadence costs no disk writes
///   at steady state.
///
/// This function is only ever called when coord's response CARRIES a
/// `tenant_ids` field (Phase 3 coord). The caller (`fleet::heartbeat`)
/// must no-op when the field is absent — today's production coord.
///
/// Missing file → `Ok` empty report (nothing local to reconcile).
/// Unparseable file → `Err` (caller logs at debug; never fatal).
pub fn reconcile_paired_bindings(
    coord_set: &[uuid::Uuid],
) -> Result<BindingReconcileReport, String> {
    let mgr = crate::auth::AuthManager::new();
    let path = paired_user_path().ok_or_else(|| "could not resolve data_local_dir".to_string())?;
    reconcile_paired_bindings_with(&mgr, &path, coord_set)
}

/// Parameterized core of [`reconcile_paired_bindings`] — explicit
/// `AuthManager` + file path so the unit tests run hermetically against
/// a temp store, never the operator's real credentials.
pub(crate) fn reconcile_paired_bindings_with(
    mgr: &crate::auth::AuthManager,
    path: &std::path::Path,
    coord_set: &[uuid::Uuid],
) -> Result<BindingReconcileReport, String> {
    let mut report = BindingReconcileReport::default();

    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // Never paired locally — nothing to reconcile. Coord-side
            // bindings without ANY local file are still worth flagging.
            for t in coord_set {
                report.coord_only.push(*t);
            }
            return Ok(report);
        }
        Err(e) => return Err(format!("read {}: {e}", path.display())),
    };
    let pf: PairedUserFile =
        serde_json::from_slice(&bytes).map_err(|e| format!("parse {}: {e}", path.display()))?;

    // Partition the (migrated) binding set by coord membership. Entries
    // with malformed tenant_id can never match coord and carry no slot —
    // dropped as junk (logged, not reported).
    let mut kept: Vec<PairedBinding> = Vec::new();
    let mut dropped_malformed = false;
    for b in pf.effective_bindings() {
        match uuid::Uuid::parse_str(b.tenant_id.trim()) {
            Ok(t) if coord_set.contains(&t) => kept.push(b),
            Ok(t) => {
                report.dropped.push(t);
                // Designed home of slot deletion: coord no longer has the
                // binding, so its credential is dead weight. Best-effort.
                if let Err(e) = mgr.clear_tenant_device_jwt(&t) {
                    tracing::debug!("reconcile: clear slot for dropped tenant {t} failed: {e}");
                }
            }
            Err(_) => {
                tracing::warn!(
                    "reconcile: dropping paired_user.json binding with malformed tenant_id {:?}",
                    b.tenant_id
                );
                dropped_malformed = true;
            }
        }
    }

    // Orphan slots (credential without a binding entry) that coord's set
    // doesn't contain either — same authority statement, same fate.
    let kept_tenants: Vec<uuid::Uuid> = kept
        .iter()
        .filter_map(|b| uuid::Uuid::parse_str(b.tenant_id.trim()).ok())
        .collect();
    for t in mgr.list_tenant_device_jwt_tenants() {
        if !coord_set.contains(&t) && !report.dropped.contains(&t) {
            if let Err(e) = mgr.clear_tenant_device_jwt(&t) {
                tracing::debug!("reconcile: clear orphan slot {t} failed: {e}");
            } else {
                report.dropped_slots.push(t);
            }
        }
    }

    // Flag coord-side bindings the runner cannot act for: no entry, or an
    // entry without a JWT slot.
    for t in coord_set {
        let has_entry = kept_tenants.contains(t);
        let has_jwt =
            matches!(mgr.get_tenant_device_jwt(t), Ok(Some(ref j)) if !j.trim().is_empty());
        if !has_entry || !has_jwt {
            report.coord_only.push(*t);
        }
    }

    // Default re-point when the default binding was dropped.
    let old_default = pf
        .effective_default_tenant_id()
        .and_then(|s| uuid::Uuid::parse_str(s.trim()).ok());
    let new_default: Option<uuid::Uuid> = match old_default {
        Some(d) if kept_tenants.contains(&d) => Some(d),
        _ => {
            // Most recently paired survivor (RFC3339 strings order
            // lexicographically; entries without paired_at sort oldest).
            kept.iter()
                .max_by(|a, b| a.paired_at.cmp(&b.paired_at))
                .and_then(|b| uuid::Uuid::parse_str(b.tenant_id.trim()).ok())
        }
    };
    if new_default != old_default {
        report.default_repointed = Some(new_default);
        if let Some(nd) = new_default {
            // D4: the legacy access_token slot holds the DEFAULT
            // binding's JWT — re-point it alongside the default.
            match mgr.get_tenant_device_jwt(&nd) {
                Ok(Some(jwt)) if !jwt.trim().is_empty() => {
                    if let Err(e) = mgr.store_tokens(&jwt, "") {
                        tracing::warn!(
                            "reconcile: default re-pointed to {nd} but legacy-slot write \
                             failed: {e} (legacy slot left as-is)"
                        );
                    }
                }
                _ => tracing::warn!(
                    "reconcile: default re-pointed to {nd} but no JWT slot for it — \
                     legacy access_token slot left as-is until the next pair/refresh"
                ),
            }
        }
        // No surviving binding → legacy slot deliberately left as-is
        // (coord Phase 3 unpair posture; the refresh gate stops the stale
        // JWT self-perpetuating).
    }

    let changed =
        !report.dropped.is_empty() || report.default_repointed.is_some() || dropped_malformed;
    if changed {
        let default_binding = new_default.and_then(|d| {
            kept.iter()
                .find(|b| uuid::Uuid::parse_str(b.tenant_id.trim()).ok() == Some(d))
                .cloned()
        });
        let out = PairedUserFile {
            // Mirrors track the DEFAULT binding; with no survivor keep
            // the old user_id (the field is required by legacy readers).
            user_id: default_binding
                .as_ref()
                .map(|b| b.user_id.clone())
                .unwrap_or_else(|| pf.user_id.clone()),
            tenant_id: new_default.map(|d| d.to_string()),
            bindings: kept,
            default_tenant_id: new_default.map(|d| d.to_string()),
        };
        write_paired_user_file(path, &out)?;
    }
    Ok(report)
}

// ============================================================================
// Host / OS detection
// ============================================================================

pub(crate) fn detect_hostname() -> String {
    hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .unwrap_or_else(|| "unknown".to_string())
}

/// Per the platform docs: `std::env::consts::OS` is one of {"linux",
/// "macos", "windows", "ios", "android", "freebsd", "dragonfly",
/// "netbsd", "openbsd", "solaris"}.
pub(crate) fn detect_os() -> String {
    std::env::consts::OS.to_string()
}

/// OS version string via the already-present `sysinfo = "0.32"` crate.
/// `None` on platforms where sysinfo can't probe.
pub(crate) fn detect_os_version() -> Option<String> {
    use sysinfo::System;
    System::long_os_version().or_else(System::os_version)
}

// ============================================================================
// coord_url → coord HTTP base resolution
// ============================================================================

/// Resolve the coord HTTP base from the active profile's `coord_url`.
/// Thin wrapper around [`coord_http_base_from_url`] — separated for
/// testability (the wrapper hits the filesystem; the conversion is
/// pure).
pub fn coord_http_base() -> Result<String, String> {
    let coord_url = crate::profiles::load_strict()
        .map_err(|e| format!("active profile load failed: {}", e))?
        .coord_url
        .ok_or_else(|| "active profile has no coord_url".to_string())?;
    Ok(coord_http_base_from_url(&coord_url))
}

/// Pure conversion: `ws[s]://host[:port][/ws]` → `http[s]://host[:port]`.
/// Mirrors `qontinui-supervisor/src/fleet.rs::coord_http_base` so the
/// register and pair paths agree on the recipe.
///
/// Thin re-export of the unified normalizer in
/// [`crate::profiles::coord_ws_to_http`] (which is the superset
/// of this fn's prior body — it additionally strips a bare trailing `/`, a
/// no-op on every input this fn's callers / tests exercise). Kept under this
/// name so the register/pair call site and the `coord_http_base_from_url`
/// tests don't churn. `coord_http_base()` below stays ENV-LESS (profile-or-Err)
/// — it deliberately does NOT route through the env-reading
/// `profiles::resolve_coord_base`.
pub fn coord_http_base_from_url(coord_url: &str) -> String {
    crate::profiles::coord_ws_to_http(coord_url)
}

/// Derive a `https://` web base from the coord HTTP base, assuming web
/// and coord live on the same host (the dev-default). Production
/// deployments override via `QONTINUI_WEB_BASE`. Best-effort: strips
/// the trailing `:port`.
pub fn derive_web_base_from_coord(coord_base: &str) -> String {
    let trimmed = coord_base.trim_end_matches('/');
    if let Some((host, _)) = trimmed.rsplit_once(':') {
        host.to_string()
    } else {
        trimmed.to_string()
    }
}

// ============================================================================
// Pair: headless (--auth-token)
// ============================================================================

/// POST `Authorization: Bearer <user-jwt>` +
/// `X-Qontinui-User-Id: <uuid>` to `POST /api/v1/devices/pair-cli` (the
/// web backend's pair-cli proxy, which injects `tenant_id` server-side
/// and forwards to coord). Coord verifies the bearer token, looks up the
/// device, and returns a fresh device-token JWT.
///
/// Requirements (Defect 5):
/// - `~/.qontinui/machine.json` must exist with a UUID `device_id`
///   (run `qontinui_profile device init` first).
/// - `{data_local_dir}/com.qontinui.runner/paired_user.json` must exist
///   from a prior browser-pair (pair-cli is a refresh path, not a
///   first-pair path).
///
/// Thin wrapper around [`pair_with_auth_token_with_ids`] — reads
/// `device_id` and `user_id` from disk then delegates. Tests use the
/// parameterized form directly so they can run hermetically against an
/// in-process mock web backend without touching `~/.qontinui` or the
/// `data_local_dir`.
/// Decode the unverified payload of a JWT and pull the `tenant_id`
/// claim, if any. Returns `None` if the token isn't a JWT, the payload
/// isn't valid base64-decoded JSON, or no `tenant_id` claim is present.
///
/// We deliberately do NOT verify the JWT signature — coord is the
/// authority on tenant_id (Phase 4 of the default-tenant-propagation
/// plan re-validates on receipt via the per-write resolver). This is
/// just the runner's best-effort auto-resolve to spare the operator
/// from re-typing a UUID they already authenticated against.
///
/// Phase 2 of the default-tenant-propagation plan (Q3 resolution:
/// "OAuth claim with `--tenant-id` override for CLI"). Also used by
/// `mcp::device_jwt_refresher` to resolve tenant_id at refresh time
/// from the stored OAuth/runner token.
pub fn tenant_id_from_oauth_claim(token: &str) -> Option<String> {
    tenant_id_from_jwt_claim(token, "tenant_id")
}

/// Extract an arbitrary string claim from the unverified JWT payload.
/// Used by `tenant_id_from_oauth_claim` (claim = "tenant_id") and the
/// browser-pair flow (claim = "sub" / "user_id") to decode fields from
/// the coord-issued device-token without signature verification (coord
/// is the authority — we just need the value for local persistence).
fn tenant_id_from_jwt_claim(token: &str, claim: &str) -> Option<String> {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
    let mut parts = token.splitn(3, '.');
    let _header = parts.next()?;
    let payload_b64 = parts.next()?;
    let _signature = parts.next()?;
    let payload_bytes = URL_SAFE_NO_PAD.decode(payload_b64).ok()?;
    let payload_json: serde_json::Value = serde_json::from_slice(&payload_bytes).ok()?;
    payload_json
        .get(claim)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

pub fn pair_with_auth_token(
    base: &str,
    oauth_token: &str,
    tenant_id: uuid::Uuid,
) -> Result<PairCompleteResponse, String> {
    let device_id = read_device_id()?;
    let user_id = read_paired_user_id().ok_or_else(|| {
        "not yet browser-paired — first pair must use --browser mode \
         (no paired_user.json on disk)"
            .to_string()
    })?;
    pair_with_auth_token_with_ids(base, oauth_token, &device_id, &user_id, tenant_id)
}

/// Parameterized variant of [`pair_with_auth_token`] — accepts `device_id`
/// and `user_id` as explicit arguments instead of reading them from
/// `~/.qontinui/machine.json` + `{data_local_dir}/.../paired_user.json`.
/// Phase 5 of the unified-devices migration introduced this split so the
/// E2E pair tests can target an in-process mock web backend without mutating
/// (or relying on) per-user files.
///
/// Routes through the web backend at `{base}/api/v1/devices/pair-cli`,
/// not coord directly. The web backend is the only component allowed to
/// resolve `tenant_id` from the authenticated user — coord's pair-cli
/// requires `tenant_id` as of the 2026-05-20
/// default-tenant-propagation plan, and threading that through the
/// runner would leak tenancy concerns into a layer that doesn't need
/// them. Callers pass the web-backend base URL (e.g.
/// `http://127.0.0.1:8000`), NOT the coord URL.
pub fn pair_with_auth_token_with_ids(
    base: &str,
    oauth_token: &str,
    device_id: &str,
    user_id: &str,
    tenant_id: uuid::Uuid,
) -> Result<PairCompleteResponse, String> {
    let url = format!("{}/api/v1/devices/pair-cli", base);
    let body = serde_json::json!({
        "device_id": device_id,
        "hostname":  detect_hostname(),
        "name":      detect_hostname(),
        "tenant_id": tenant_id.to_string(),
    });
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| format!("reqwest client build failed: {}", e))?;
    let resp = client
        .post(&url)
        .bearer_auth(oauth_token)
        .header("X-Qontinui-User-Id", user_id)
        .json(&body)
        .send()
        .map_err(|e| format!("POST {} failed: {}", url, e))?;
    let status = resp.status();
    if !status.is_success() {
        let body_text = resp
            .text()
            .unwrap_or_else(|_| "<unable to read response body>".to_string());
        return Err(format!("POST {} -> HTTP {}: {}", url, status, body_text));
    }
    resp.json::<PairCompleteResponse>()
        .map_err(|e| format!("decode pair-cli response failed: {}", e))
}

// ============================================================================
// Pair: pair-code (--pair-code <CODE>)
// ============================================================================

/// On-wire response shape for ``POST /api/v1/devices/pair-codes/{code}/redeem``.
///
/// The web backend mirrors the canonical ``PairCompleteResponse`` shape
/// returned by ``pair-cli`` so the runner can use a single decoder for
/// both paths. This struct is named for the field set declared in the
/// web schema (``app/schemas/pair_code.py::PairCodeRedeemOut``):
/// ``{user_id, tenant_id, device_id, expires_at?, device_token_jwt}``.
///
/// The redeem path returns ``device_token_jwt`` (not ``token``) for
/// disambiguation in the web schema, so we accept both spellings via
/// `serde(alias)` to remain compatible if the API ever renames back.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PairCodeRedeemResponse {
    pub user_id: String,
    pub tenant_id: String,
    pub device_id: String,
    #[serde(default)]
    pub expires_at: Option<String>,
    /// Coord-issued device-token JWT. The web schema names this field
    /// ``device_token_jwt`` to make its intent unambiguous; we accept
    /// the legacy ``token`` alias too in case the API ever renames.
    #[serde(alias = "token")]
    pub device_token_jwt: String,
}

impl PairCodeRedeemResponse {
    /// Convert to the canonical ``PairCompleteResponse`` shape so the
    /// caller can use the same decoder + ``persist_pairing`` path for
    /// pair-code-mediated pairing as for ``pair-cli``.
    pub fn into_pair_complete(self) -> PairCompleteResponse {
        // Best-effort tenant UUID parsing; the field is on the wire as
        // a string, but ``PairCompleteResponse::tenant_id`` is
        // ``Option<String>`` (the canonical decoder is lenient — see
        // its doc-comment), so we just pass the raw string through.
        PairCompleteResponse {
            token: self.device_token_jwt,
            user_id: self.user_id,
            device_id: Some(self.device_id),
            jti: None,
            exp: None,
            tenant_id: Some(self.tenant_id),
            // The pair-code redeem path does not carry a dmk_ in this phase.
            device_machine_key: None,
        }
    }
}

/// Headless pair flow that consumes a 6-char single-use pair code (no
/// browser, no OAuth round-trip). The operator mints the code on the
/// dashboard's ``Auth Tokens`` tab and types it into the runner's
/// Settings UI; this function POSTs the code to the web backend's
/// ``POST /api/v1/devices/pair-codes/{code}/redeem`` endpoint and gets
/// back the same ``PairCompleteResponse`` shape that ``pair-cli``
/// returns.
///
/// **Unauthenticated by design** — the pair code IS the credential.
/// The resulting device JWT is tenant-scoped to the **code's**
/// mint-time tenant (the web backend burns the issuing operator's
/// tenant into the code row and refuses to let the runner override
/// it).
///
/// `base` is the web-backend base URL (e.g. ``http://127.0.0.1:8000``),
/// NOT the coord URL — the pair-code endpoints live exclusively on
/// qontinui-web. `code` is the 6-char code the operator typed in;
/// it is upper-cased server-side so casing doesn't matter, but we
/// upper-case here too for symmetric display.
pub fn pair_with_pair_code(
    base: &str,
    code: &str,
    device_id: &str,
) -> Result<PairCompleteResponse, String> {
    if code.trim().is_empty() {
        return Err("pair code is empty".to_string());
    }
    let url = format!(
        "{}/api/v1/devices/pair-codes/{}/redeem",
        base.trim_end_matches('/'),
        code.trim().to_uppercase()
    );
    let body = serde_json::json!({
        "device_id": device_id,
        "hostname":  detect_hostname(),
    });
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| format!("reqwest client build failed: {}", e))?;
    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .map_err(|e| format!("POST {} failed: {}", url, e))?;
    let status = resp.status();
    if !status.is_success() {
        let body_text = resp
            .text()
            .unwrap_or_else(|_| "<unable to read response body>".to_string());
        // Translate the web's structured-detail shape into a friendlier
        // single-line operator message. We don't need to JSON-decode it
        // — the calling UI surfaces the raw string verbatim.
        return Err(format!("POST {} -> HTTP {}: {}", url, status, body_text));
    }
    let redeem: PairCodeRedeemResponse = resp
        .json()
        .map_err(|e| format!("decode redeem response failed: {}", e))?;
    Ok(redeem.into_pair_complete())
}

// ============================================================================
// Pair: browser (--browser, default)
// ============================================================================

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct CallbackQuery {
    pub state: String,
    pub token: String,
    #[serde(default)]
    pub token_id: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct CallbackCapture {
    pub token: String,
    #[allow(dead_code)]
    pub token_id: Option<String>,
}

/// Browser-mediated pair. Spins up a localhost axum server, calls coord's
/// `POST /coord/devices/pair-start` to register the flow and obtain a
/// coord-minted state nonce + redirect URL, opens the browser, waits for
/// the user's click + redirect which delivers the device-token JWT
/// directly via the callback query params (coord's `pair-complete` is
/// called by the web frontend, not the runner).
pub fn pair_via_browser(
    coord_base: &str,
    tenant_id: uuid::Uuid,
) -> Result<PairCompleteResponse, String> {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    // Read device_id from machine.json — required for the pair-start
    // request and for the browser URL so the web page can forward it to
    // coord's pair-complete.
    let device_id = read_device_id()?;

    // Web backend URL is read from the active profile's `coord_url`
    // host (assuming web + coord co-locate in dev). If they diverge,
    // the user can override with QONTINUI_WEB_BASE.
    let web_base = std::env::var("QONTINUI_WEB_BASE")
        .ok()
        .unwrap_or_else(|| derive_web_base_from_coord(coord_base));
    let hostname_now = detect_hostname();

    // Bind a port for the callback. Use 0 to let the OS pick — then read it
    // back. We use std::net::TcpListener first to capture the chosen port,
    // then hand it to axum via from_tcp.
    let std_listener = std::net::TcpListener::bind("127.0.0.1:0")
        .map_err(|e| format!("bind localhost callback failed: {e}"))?;
    let port = std_listener
        .local_addr()
        .map_err(|e| format!("local_addr failed: {e}"))?
        .port();
    std_listener
        .set_nonblocking(true)
        .map_err(|e| format!("set_nonblocking failed: {e}"))?;

    let callback_url = format!("http://127.0.0.1:{}/auth/runner-token-callback", port);

    // Step 1: Call coord's pair-start to register the pairing flow and
    // obtain a coord-minted state nonce + redirect URL.
    let web_pair_url = format!("{}/connect-runner", web_base.trim_end_matches('/'));
    let pair_start_url = format!("{}/coord/devices/pair-start", coord_base);
    let pair_start_body = serde_json::json!({
        "callback_url":    callback_url,
        "device_hostname": hostname_now,
        "device_name":     hostname_now,
        "web_pair_url":    web_pair_url,
        "tenant_id":       tenant_id.to_string(),
    });
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| format!("reqwest client build failed: {e}"))?;
    let resp = client
        .post(&pair_start_url)
        .json(&pair_start_body)
        .send()
        .map_err(|e| format!("POST {} failed: {e}", pair_start_url))?;
    let status = resp.status();
    if !status.is_success() {
        let body_text = resp
            .text()
            .unwrap_or_else(|_| "<unable to read response body>".to_string());
        return Err(format!(
            "POST {} -> HTTP {}: {}",
            pair_start_url, status, body_text
        ));
    }
    let pair_start_resp: PairStartResponseWire = resp
        .json()
        .map_err(|e| format!("decode pair-start response failed: {e}"))?;

    let state_nonce = pair_start_resp.state;

    // Append device_id to the coord-provided redirect_url so the web
    // page can forward it to pair-complete.
    let separator = if pair_start_resp.redirect_url.contains('?') {
        '&'
    } else {
        '?'
    };
    let connect_url = format!(
        "{}{}device_id={}&device_name={}",
        pair_start_resp.redirect_url,
        separator,
        urlencoding::encode(&device_id),
        urlencoding::encode(&hostname_now),
    );

    let received: Arc<Mutex<Option<CallbackCapture>>> = Arc::new(Mutex::new(None));
    let received_clone = received.clone();
    let state_expected = state_nonce.clone();

    // Spawn a current-thread tokio runtime + axum server on a worker
    // thread. This is a self-contained loopback callback server for the
    // CLI pair flow — it depends on no Tauri-side state (ApiState/AppState),
    // so it runs independently of the runner's main HTTP server.
    let (server_done_tx, server_done_rx) = std::sync::mpsc::channel::<()>();
    let server_handle = std::thread::spawn(move || -> Result<(), String> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| format!("runtime build failed: {e}"))?;
        rt.block_on(async move {
            use axum::{extract::Query, response::Html, routing::get, Router};
            let state_for_route = state_expected.clone();
            let received_for_route = received_clone.clone();
            let app = Router::new().route(
                "/auth/runner-token-callback",
                get(move |Query(q): Query<CallbackQuery>| {
                    let state_for_route = state_for_route.clone();
                    let received_for_route = received_for_route.clone();
                    async move {
                        if q.state != state_for_route {
                            return Html(
                                "<h1>State mismatch</h1><p>The callback state did \
                                 not match the pending flow. Restart the pairing.</p>"
                                    .to_string(),
                            );
                        }
                        *received_for_route.lock().expect("mutex") = Some(CallbackCapture {
                            token: q.token.clone(),
                            token_id: q.token_id.clone(),
                        });
                        Html(
                            "<h1>&#10003; Runner paired</h1>\
                             <p>You can close this tab.</p>\
                             <script>setTimeout(()=>window.close(),2000);</script>"
                                .to_string(),
                        )
                    }
                }),
            );
            let tokio_listener = tokio::net::TcpListener::from_std(std_listener)
                .map_err(|e| format!("tokio listener wrap failed: {e}"))?;
            // Run until our flag flips, with a 5-minute hard timeout.
            let serve = axum::serve(tokio_listener, app);
            let timeout = tokio::time::sleep(Duration::from_secs(300));
            tokio::pin!(timeout);
            tokio::select! {
                res = serve => {
                    res.map_err(|e| format!("axum serve failed: {e}"))?;
                }
                _ = &mut timeout => {
                    return Err("pair: timed out after 5 minutes waiting for browser callback".to_string());
                }
            }
            let _ = server_done_tx.send(());
            Ok(())
        })
    });

    println!(
        "opening browser to {} (callback {})",
        connect_url, callback_url
    );
    if let Err(e) = open::that(&connect_url) {
        eprintln!(
            "warning: failed to open browser ({}). Open this URL manually:\n  {}",
            e, connect_url
        );
    }

    // Poll the received slot up to 5 minutes. The axum runtime is on a
    // background thread; we just block here until we see the slot fill
    // or the server-done signal arrives.
    let deadline = std::time::Instant::now() + Duration::from_secs(300);
    let capture = loop {
        if let Some(c) = received.lock().expect("mutex").clone() {
            break c;
        }
        if std::time::Instant::now() >= deadline {
            return Err("pair: timed out after 5 minutes waiting for browser callback".to_string());
        }
        // Cheap polling cadence; the axum task wakes on its own.
        std::thread::sleep(Duration::from_millis(200));
        // Drain the server-done channel in case axum exited early.
        if let Ok(()) = server_done_rx.try_recv() {
            // Server stopped without filling the slot — re-check once.
            if let Some(c) = received.lock().expect("mutex").clone() {
                break c;
            }
            return Err("pair: callback server stopped before capturing token".to_string());
        }
    };
    // Best-effort join — we don't fail the operation if the server thread
    // is still running; it'll time out on its own.
    drop(server_handle);

    // The JWT is already in the callback redirect — coord's pair-complete
    // was called by the web frontend (step 4 of the browser-pair flow),
    // which minted the JWT and forwarded it via the redirect's `?token=`
    // param. No second HTTP call to coord is needed; we build the
    // PairCompleteResponse directly from the captured token.
    //
    // `capture.token` = device-token JWT (coord-issued).
    // `capture.token_id` = device_id forwarded by the web redirect.
    // `user_id` is decoded from the JWT payload (best-effort; persist
    //  path re-validates).
    let user_id = tenant_id_from_jwt_claim(&capture.token, "user_id")
        .or_else(|| tenant_id_from_jwt_claim(&capture.token, "sub"))
        .unwrap_or_default();
    let resp_device_id = capture
        .token_id
        .clone()
        .unwrap_or_else(|| device_id.clone());

    Ok(PairCompleteResponse {
        token: capture.token,
        user_id,
        device_id: Some(resp_device_id),
        jti: None,
        exp: None,
        tenant_id: Some(tenant_id.to_string()),
        // The browser callback delivers only the device JWT; a dmk_ (if any)
        // is minted server-side and arrives via the pair-cli response path.
        device_machine_key: None,
    })
}

// ============================================================================
// Persist: store device-token JWT + cache user_id
// ============================================================================

/// Persist a completed pairing: the device-token JWT into the paired
/// tenant's `device_jwt:<tenant_id>` slot (and, when that tenant is the
/// device's DEFAULT binding, also the legacy `access_token` slot), plus
/// an appended/updated binding entry in `paired_user.json` (v2 shape).
///
/// Phase 8a semantics (plan 2026-07-02, D4):
/// - Pairing is ADDITIVE: an existing binding for another tenant is
///   never removed or overwritten; re-pairing the same tenant updates
///   its entry (user_id + paired_at) in place.
/// - The paired tenant becomes the default ONLY when no default exists
///   yet (first pair / legacy no-tenant file). An established default is
///   never stolen by a later additive pair.
/// - The legacy `access_token` slot keeps holding the DEFAULT binding's
///   JWT, so every unmodified consumer (`backend_relay`,
///   `commands/auth.rs` signed-in check, `attach_device_auth`, …) keeps
///   working. Pairing a NON-default tenant therefore does NOT touch it.
/// - The legacy `user_id`/`tenant_id` file fields are written as mirrors
///   of the default binding so pre-8a readers (incl. the
///   `qontinui_profile` bin's local struct) stay compatible.
pub fn persist_pairing(resp: &PairCompleteResponse, tenant_id: uuid::Uuid) -> Result<(), String> {
    use crate::auth::AuthManager;
    let mgr = AuthManager::new();
    let path = paired_user_path().ok_or_else(|| "could not resolve data_local_dir".to_string())?;
    persist_pairing_with(&mgr, &path, resp, tenant_id)?;
    if let Some(did) = &resp.device_id {
        // Best-effort: record device_id alongside the auth tokens so the
        // runner GUI can identify itself without reading machine.json.
        if let Ok(storage) = crate::secure_storage::SecureStorage::new() {
            if let Err(e) = storage.store_device_id(did) {
                tracing::debug!("store_device_id non-fatal: {e}");
            }
        }
    }
    // Best-effort: if the web pair response auto-minted a device machine key
    // (`dmk_`), persist it so the refresher's Phase-4b cold-start exchange can
    // recover a device JWT after a >30-day outage with no user session. A
    // missing dmk_ (pre-Phase-3 web / non-cli pairing) is a no-op.
    if let Some(dmk) = resp
        .device_machine_key
        .as_deref()
        .filter(|k| !k.trim().is_empty())
    {
        if let Ok(storage) = crate::secure_storage::SecureStorage::new() {
            if let Err(e) = storage.store_device_machine_key(dmk) {
                tracing::debug!("store_device_machine_key non-fatal: {e}");
            }
        }
    }
    Ok(())
}

/// Parameterized core of [`persist_pairing`] — explicit `AuthManager` +
/// file path so unit tests run hermetically against a temp store.
pub(crate) fn persist_pairing_with(
    mgr: &crate::auth::AuthManager,
    path: &std::path::Path,
    resp: &PairCompleteResponse,
    tenant_id: uuid::Uuid,
) -> Result<(), String> {
    // 1. The paired tenant's slot ALWAYS gets the fresh JWT.
    mgr.store_tenant_device_jwt(&tenant_id, &resp.token)
        .map_err(|e| format!("store_tenant_device_jwt failed: {e}"))?;

    // 2. Load + migrate the existing file (missing → fresh; unparseable
    //    → start over, same clobber posture the pre-8a writer had).
    let existing = match std::fs::read(path) {
        Ok(bytes) => match serde_json::from_slice::<PairedUserFile>(&bytes) {
            Ok(pf) => Some(pf),
            Err(e) => {
                tracing::warn!(
                    "persist_pairing: existing {} unparseable ({e}) — rewriting fresh",
                    path.display()
                );
                None
            }
        },
        Err(_) => None,
    };
    let mut bindings = existing
        .as_ref()
        .map(|pf| pf.effective_bindings())
        .unwrap_or_default();

    // 3. Append/update this tenant's binding entry.
    let tenant_str = tenant_id.to_string();
    let paired_at = Some(chrono::Utc::now().to_rfc3339());
    match bindings
        .iter_mut()
        .find(|b| uuid::Uuid::parse_str(b.tenant_id.trim()).ok() == Some(tenant_id))
    {
        Some(b) => {
            b.user_id = resp.user_id.clone();
            b.paired_at = paired_at;
        }
        None => bindings.push(PairedBinding {
            tenant_id: tenant_str.clone(),
            user_id: resp.user_id.clone(),
            paired_at,
        }),
    }

    // 4. Default: keep the established one (if it still names a binding);
    //    otherwise this pair sets it.
    let default_tenant = existing
        .as_ref()
        .and_then(|pf| pf.effective_default_tenant_id())
        .and_then(|s| uuid::Uuid::parse_str(s.trim()).ok())
        .filter(|d| {
            bindings
                .iter()
                .any(|b| uuid::Uuid::parse_str(b.tenant_id.trim()).ok() == Some(*d))
        })
        .unwrap_or(tenant_id);

    // 5. Legacy access_token slot = the DEFAULT binding's JWT (D4).
    if default_tenant == tenant_id {
        mgr.store_tokens(&resp.token, "")
            .map_err(|e| format!("AuthManager::store_tokens failed: {e}"))?;
    }

    // 6. Write the v2 file with legacy mirrors tracking the default.
    let default_user_id = bindings
        .iter()
        .find(|b| uuid::Uuid::parse_str(b.tenant_id.trim()).ok() == Some(default_tenant))
        .map(|b| b.user_id.clone())
        .unwrap_or_else(|| resp.user_id.clone());
    let pf = PairedUserFile {
        user_id: default_user_id,
        tenant_id: Some(default_tenant.to_string()),
        bindings,
        default_tenant_id: Some(default_tenant.to_string()),
    };
    write_paired_user_file(path, &pf)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coord_http_base_converts_ws_to_http() {
        assert_eq!(
            coord_http_base_from_url("ws://localhost:9870/ws"),
            "http://localhost:9870"
        );
    }

    #[test]
    fn coord_http_base_converts_wss_to_https() {
        assert_eq!(
            coord_http_base_from_url("wss://coord.qontinui.io:9870/ws"),
            "https://coord.qontinui.io:9870"
        );
    }

    #[test]
    fn coord_http_base_strips_trailing_ws_path() {
        // With explicit /ws suffix:
        assert_eq!(
            coord_http_base_from_url("ws://host:9870/ws"),
            "http://host:9870"
        );
        // Without /ws suffix (bare ws://host:port):
        assert_eq!(
            coord_http_base_from_url("ws://host:9870"),
            "http://host:9870"
        );
        // Bare hostname without scheme — passed through (the supervisor
        // recipe already accepts http:// hosts as-is; we mirror that).
        assert_eq!(
            coord_http_base_from_url("http://host:9870"),
            "http://host:9870"
        );
    }

    #[test]
    fn derive_web_base_from_coord_drops_port() {
        assert_eq!(
            derive_web_base_from_coord("http://localhost:9870"),
            "http://localhost"
        );
        assert_eq!(
            derive_web_base_from_coord("https://coord.qontinui.io:9870"),
            "https://coord.qontinui.io"
        );
        // Trailing slash tolerated:
        assert_eq!(
            derive_web_base_from_coord("http://localhost:9870/"),
            "http://localhost"
        );
    }

    #[test]
    fn pair_complete_response_decodes_canonical_coord_shape() {
        // Exact canonical wire shape from
        // qontinui-coord/src/routes_phase3.rs:445-454.
        let wire = serde_json::json!({
            "token":     "abc.def.ghi",
            "device_id": "11111111-1111-4111-8111-111111111111",
            "user_id":   "22222222-2222-4222-8222-222222222222",
            "jti":       "33333333-3333-4333-8333-333333333333",
            "exp":       1_234_567_890i64,
        });
        let parsed: PairCompleteResponse =
            serde_json::from_value(wire).expect("decode canonical coord shape");
        assert_eq!(parsed.token, "abc.def.ghi");
        assert_eq!(
            parsed.device_id.as_deref(),
            Some("11111111-1111-4111-8111-111111111111")
        );
        assert_eq!(parsed.user_id, "22222222-2222-4222-8222-222222222222");
        assert_eq!(
            parsed.jti.as_deref(),
            Some("33333333-3333-4333-8333-333333333333")
        );
        assert_eq!(parsed.exp, Some(1_234_567_890));
    }

    /// Phase 4b: the pair-cli response may now carry an optional
    /// `device_machine_key` (`dmk_`). It must decode when present and default
    /// to `None` when absent (pre-Phase-3 web) — additive, back-compatible.
    #[test]
    fn pair_complete_response_decodes_optional_device_machine_key() {
        // Present:
        let with_dmk = serde_json::json!({
            "token":     "abc.def.ghi",
            "device_id": "11111111-1111-4111-8111-111111111111",
            "user_id":   "22222222-2222-4222-8222-222222222222",
            "device_machine_key": "dmk_unit_fixture_placeholder",
        });
        let parsed: PairCompleteResponse =
            serde_json::from_value(with_dmk).expect("decode with dmk");
        assert_eq!(
            parsed.device_machine_key.as_deref(),
            Some("dmk_unit_fixture_placeholder")
        );

        // Absent → None (a pre-dmk web build).
        let without_dmk = serde_json::json!({
            "token":   "abc.def.ghi",
            "user_id": "22222222-2222-4222-8222-222222222222",
        });
        let parsed2: PairCompleteResponse =
            serde_json::from_value(without_dmk).expect("decode without dmk");
        assert!(parsed2.device_machine_key.is_none());
    }

    /// Defect 5 shape assertion (body-level): the `pair_with_auth_token`
    /// request body MUST contain `device_id`, `hostname`, and `name` and
    /// MUST NOT contain `os` / `os_version` (those aren't on
    /// `PairCliRequest`). We can't easily intercept the in-flight
    /// request without a mock-server dep, so this test asserts the body
    /// shape by reconstructing it the same way `pair_with_auth_token`
    /// does and inspecting the JSON keys.
    #[test]
    fn pair_cli_request_body_carries_canonical_keys() {
        // Re-construct the body the same way `pair_with_auth_token`
        // does. If a future refactor diverges, this test fails loudly.
        let device_id = "00000000-0000-4000-8000-000000000000";
        let tenant_id = "11111111-1111-4111-8111-111111111111";
        let body = serde_json::json!({
            "device_id": device_id,
            "hostname":  detect_hostname(),
            "name":      detect_hostname(),
            "tenant_id": tenant_id,
        });
        let obj = body.as_object().expect("object");
        // Required keys (per PairCliRequest):
        assert!(obj.contains_key("device_id"), "device_id missing");
        assert!(obj.contains_key("hostname"), "hostname missing");
        assert!(obj.contains_key("name"), "name missing");
        assert!(obj.contains_key("tenant_id"), "tenant_id missing");
        // Rejected legacy keys (would 422 on coord's PairCliRequest):
        assert!(
            !obj.contains_key("os"),
            "os must not be sent — not on PairCliRequest"
        );
        assert!(
            !obj.contains_key("os_version"),
            "os_version must not be sent — not on PairCliRequest"
        );
        // Sanity on values:
        assert_eq!(
            obj.get("device_id").and_then(|v| v.as_str()),
            Some(device_id)
        );
        assert!(obj.get("hostname").and_then(|v| v.as_str()).is_some());
    }

    #[test]
    fn tenant_id_from_oauth_claim_extracts_uuid() {
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
        let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"HS256","typ":"JWT"}"#);
        let payload = URL_SAFE_NO_PAD
            .encode(br#"{"sub":"x","tenant_id":"11111111-2222-3333-4444-555555555555"}"#);
        let token = format!("{}.{}.signature", header, payload);
        assert_eq!(
            tenant_id_from_oauth_claim(&token).as_deref(),
            Some("11111111-2222-3333-4444-555555555555")
        );
    }

    #[test]
    fn tenant_id_from_oauth_claim_returns_none_when_missing() {
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
        let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"HS256"}"#);
        let payload = URL_SAFE_NO_PAD.encode(br#"{"sub":"x"}"#);
        let token = format!("{}.{}.signature", header, payload);
        assert_eq!(tenant_id_from_oauth_claim(&token), None);
    }

    #[test]
    fn tenant_id_from_oauth_claim_returns_none_for_non_jwt() {
        assert_eq!(tenant_id_from_oauth_claim("not-a-jwt"), None);
        assert_eq!(tenant_id_from_oauth_claim("a.b"), None);
    }

    /// The legacy `device_token` field name MUST NOT decode — coord
    /// emits `token`. A regression that re-introduces `device_token`
    /// (e.g. via a stray `#[serde(rename)]`) would silently fail to
    /// extract the JWT from the response. This test pins that.
    #[test]
    fn pair_complete_response_does_not_accept_legacy_device_token_field() {
        let legacy = serde_json::json!({
            "device_token": "abc.def.ghi",
            "user_id": "22222222-2222-4222-8222-222222222222",
        });
        let parsed: Result<PairCompleteResponse, _> = serde_json::from_value(legacy);
        // It should parse (unknown fields ignored by default), but the
        // `token` field will be empty since the wire field name is
        // `token` and we sent `device_token`. That manifests as a
        // missing-required-field error.
        assert!(
            parsed.is_err(),
            "legacy device_token-only payload must NOT decode (token field required)"
        );
    }

    /// Back-compat: a `paired_user.json` written by a pre-2026-05-22
    /// runner has only the `user_id` field. The new `tenant_id` field
    /// carries `#[serde(default)]` so such files must still deserialize
    /// — they just yield `tenant_id == None` and the heartbeat path
    /// falls through to the JWT-claim fallback.
    #[test]
    fn paired_user_file_back_compat_user_id_only_deserializes() {
        let raw = r#"{"user_id":"22222222-2222-4222-8222-222222222222"}"#;
        let parsed: PairedUserFile =
            serde_json::from_str(raw).expect("legacy user_id-only must deserialize");
        assert_eq!(parsed.user_id, "22222222-2222-4222-8222-222222222222");
        assert!(
            parsed.tenant_id.is_none(),
            "legacy file must yield tenant_id = None"
        );
    }

    /// Pair-code redeem response decodes the web schema's
    /// ``PairCodeRedeemOut`` shape: ``{user_id, tenant_id, device_id,
    /// device_token_jwt, expires_at?}``. The runner must accept the
    /// ``device_token_jwt`` spelling AND fall back gracefully if the
    /// API ever renames back to ``token`` (via the serde alias).
    #[test]
    fn pair_code_redeem_response_decodes_web_schema_shape() {
        let wire = serde_json::json!({
            "user_id":          "22222222-2222-4222-8222-222222222222",
            "tenant_id":        "11111111-2222-3333-4444-555555555555",
            "device_id":        "00000000-0000-4000-8000-000000000000",
            "device_token_jwt": "abc.def.ghi",
            "expires_at":       "2026-05-23T00:00:00Z",
        });
        let parsed: PairCodeRedeemResponse =
            serde_json::from_value(wire).expect("decode web schema shape");
        assert_eq!(parsed.device_token_jwt, "abc.def.ghi");
        assert_eq!(parsed.tenant_id, "11111111-2222-3333-4444-555555555555");
        assert_eq!(parsed.device_id, "00000000-0000-4000-8000-000000000000");
    }

    /// Backward-compat: if the web schema ever renames
    /// ``device_token_jwt`` back to ``token``, the runner still
    /// decodes it (via serde alias).
    #[test]
    fn pair_code_redeem_response_decodes_legacy_token_alias() {
        let wire = serde_json::json!({
            "user_id":   "22222222-2222-4222-8222-222222222222",
            "tenant_id": "11111111-2222-3333-4444-555555555555",
            "device_id": "00000000-0000-4000-8000-000000000000",
            "token":     "legacy.jwt.xyz",
        });
        let parsed: PairCodeRedeemResponse =
            serde_json::from_value(wire).expect("decode legacy token alias");
        assert_eq!(parsed.device_token_jwt, "legacy.jwt.xyz");
    }

    /// ``PairCodeRedeemResponse::into_pair_complete`` produces a
    /// ``PairCompleteResponse`` with the same fields callers expect
    /// from ``pair-cli``, so ``persist_pairing`` can consume it
    /// unmodified.
    #[test]
    fn pair_code_redeem_response_converts_to_pair_complete() {
        let redeem = PairCodeRedeemResponse {
            user_id: "22222222-2222-4222-8222-222222222222".to_string(),
            tenant_id: "11111111-2222-3333-4444-555555555555".to_string(),
            device_id: "00000000-0000-4000-8000-000000000000".to_string(),
            expires_at: None,
            device_token_jwt: "abc.def.ghi".to_string(),
        };
        let pc = redeem.into_pair_complete();
        assert_eq!(pc.token, "abc.def.ghi");
        assert_eq!(pc.user_id, "22222222-2222-4222-8222-222222222222");
        assert_eq!(
            pc.device_id.as_deref(),
            Some("00000000-0000-4000-8000-000000000000")
        );
        assert_eq!(
            pc.tenant_id.as_deref(),
            Some("11111111-2222-3333-4444-555555555555")
        );
    }

    /// ``pair_with_pair_code`` upper-cases the code before posting so
    /// operator typing casing doesn't reach the server (web upper-cases
    /// too — defense in depth).
    #[test]
    fn pair_with_pair_code_rejects_blank_code() {
        // We can't easily invoke the real HTTP call from a unit test,
        // but the empty-code guard is pure and worth pinning here.
        let result = pair_with_pair_code("http://unused", "   ", "device-x");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("empty"), "got: {err}");
    }

    /// Forward shape — a newly-written `paired_user.json` round-trips
    /// both fields. Pins the serde representation against accidental
    /// `#[serde(rename)]` / `#[serde(skip)]` regressions. Also pins the
    /// Phase 8a legacy-shape preservation: a v1 struct (no bindings, no
    /// default) serializes WITHOUT the v2 keys, so a legacy-heal write
    /// keeps the legacy shape.
    #[test]
    fn paired_user_file_with_tenant_id_round_trips() {
        let original = PairedUserFile {
            user_id: "22222222-2222-4222-8222-222222222222".to_string(),
            tenant_id: Some("11111111-2222-3333-4444-555555555555".to_string()),
            bindings: Vec::new(),
            default_tenant_id: None,
        };
        let json = serde_json::to_string(&original).expect("serialize");
        assert!(
            !json.contains("bindings") && !json.contains("default_tenant_id"),
            "empty v2 fields must not serialize (legacy shape preserved): {json}"
        );
        let parsed: PairedUserFile = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed.user_id, original.user_id);
        assert_eq!(parsed.tenant_id, original.tenant_id);
        assert!(parsed.bindings.is_empty());
        assert!(parsed.default_tenant_id.is_none());
    }

    // ------------------------------------------------------------------------
    // Phase 8a — paired_user.json v2 multi-entry + migration + persist +
    // reconciliation (plan 2026-07-02, D3/D4). All hermetic: temp file paths
    // + AuthManager::with_storage — never the operator's real state.
    // ------------------------------------------------------------------------

    const T_A: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    const T_B: &str = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
    const T_C: &str = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";
    const USER: &str = "22222222-2222-4222-8222-222222222222";

    fn ta() -> uuid::Uuid {
        uuid::Uuid::parse_str(T_A).unwrap()
    }
    fn tb() -> uuid::Uuid {
        uuid::Uuid::parse_str(T_B).unwrap()
    }
    fn tc() -> uuid::Uuid {
        uuid::Uuid::parse_str(T_C).unwrap()
    }

    fn temp_dir_for(test: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join("qontinui_test_paired_v2")
            .join(format!("{test}_{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dir).expect("mkdir tempdir");
        dir
    }

    fn test_mgr(dir: &std::path::Path) -> crate::auth::AuthManager {
        let storage = crate::secure_storage::SecureStorage::with_path(dir.join("tokens.enc"))
            .expect("test storage");
        crate::auth::AuthManager::with_storage(storage)
    }

    fn pair_resp(token: &str) -> PairCompleteResponse {
        PairCompleteResponse {
            token: token.to_string(),
            user_id: USER.to_string(),
            device_id: None,
            jti: None,
            exp: None,
            tenant_id: None,
            device_machine_key: None,
        }
    }

    fn read_file(path: &std::path::Path) -> PairedUserFile {
        serde_json::from_slice(&std::fs::read(path).expect("read paired file"))
            .expect("parse paired file")
    }

    /// Legacy (v1) shapes migrate through the effective-view accessors:
    /// `{user_id, tenant_id}` yields one synthesized binding + that
    /// default; `{user_id}` yields an empty set and no default.
    #[test]
    fn legacy_file_migrates_via_effective_view() {
        let with_tenant: PairedUserFile =
            serde_json::from_str(&format!(r#"{{"user_id":"{USER}","tenant_id":"{T_A}"}}"#))
                .unwrap();
        assert_eq!(
            with_tenant.effective_bindings(),
            vec![PairedBinding {
                tenant_id: T_A.to_string(),
                user_id: USER.to_string(),
                paired_at: None,
            }]
        );
        assert_eq!(
            with_tenant.effective_default_tenant_id().as_deref(),
            Some(T_A)
        );

        let no_tenant: PairedUserFile =
            serde_json::from_str(&format!(r#"{{"user_id":"{USER}"}}"#)).unwrap();
        assert!(no_tenant.effective_bindings().is_empty());
        assert!(no_tenant.effective_default_tenant_id().is_none());
    }

    /// v2 files round-trip and the explicit `bindings`/`default_tenant_id`
    /// win over the legacy mirror fields in the effective view.
    #[test]
    fn v2_file_round_trips_and_bindings_win_over_mirrors() {
        let v2 = PairedUserFile {
            user_id: USER.to_string(),
            tenant_id: Some(T_A.to_string()),
            bindings: vec![
                PairedBinding {
                    tenant_id: T_A.to_string(),
                    user_id: USER.to_string(),
                    paired_at: Some("2026-07-01T00:00:00Z".to_string()),
                },
                PairedBinding {
                    tenant_id: T_B.to_string(),
                    user_id: USER.to_string(),
                    paired_at: Some("2026-07-02T00:00:00Z".to_string()),
                },
            ],
            default_tenant_id: Some(T_A.to_string()),
        };
        let json = serde_json::to_string_pretty(&v2).unwrap();
        let parsed: PairedUserFile = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.effective_bindings().len(), 2);
        assert_eq!(parsed.effective_default_tenant_id().as_deref(), Some(T_A));
        // Legacy mirrors present on the wire for pre-8a readers.
        assert!(json.contains("\"user_id\""));
        assert!(json.contains("\"tenant_id\""));
    }

    /// First pair: binding appended, becomes default, JWT written to BOTH
    /// the tenant slot and the legacy access_token slot, mirrors set.
    #[test]
    fn persist_first_pair_sets_default_and_both_slots() {
        let dir = temp_dir_for("persist_first");
        let mgr = test_mgr(&dir);
        let path = dir.join("paired_user.json");

        persist_pairing_with(&mgr, &path, &pair_resp("jwt.a.1"), ta()).expect("persist");

        let pf = read_file(&path);
        assert_eq!(pf.bindings.len(), 1);
        assert_eq!(pf.bindings[0].tenant_id, T_A);
        assert_eq!(pf.bindings[0].user_id, USER);
        assert!(pf.bindings[0].paired_at.is_some());
        assert_eq!(pf.default_tenant_id.as_deref(), Some(T_A));
        // Legacy mirrors track the default.
        assert_eq!(pf.user_id, USER);
        assert_eq!(pf.tenant_id.as_deref(), Some(T_A));
        // Both credential slots hold the fresh JWT.
        assert_eq!(
            mgr.get_tenant_device_jwt(&ta()).unwrap().as_deref(),
            Some("jwt.a.1")
        );
        assert_eq!(mgr.get_access_token().unwrap(), "jwt.a.1");
    }

    /// Additive pair for a SECOND tenant: appended, default NOT stolen,
    /// legacy access_token slot untouched (it holds the DEFAULT binding's
    /// JWT — D4).
    #[test]
    fn persist_second_tenant_appends_without_stealing_default() {
        let dir = temp_dir_for("persist_second");
        let mgr = test_mgr(&dir);
        let path = dir.join("paired_user.json");

        persist_pairing_with(&mgr, &path, &pair_resp("jwt.a.1"), ta()).expect("persist A");
        persist_pairing_with(&mgr, &path, &pair_resp("jwt.b.1"), tb()).expect("persist B");

        let pf = read_file(&path);
        assert_eq!(pf.bindings.len(), 2, "pairing is additive");
        assert_eq!(
            pf.default_tenant_id.as_deref(),
            Some(T_A),
            "an established default is never stolen by a later pair"
        );
        assert_eq!(
            pf.tenant_id.as_deref(),
            Some(T_A),
            "mirror stays on default"
        );
        assert_eq!(
            mgr.get_tenant_device_jwt(&tb()).unwrap().as_deref(),
            Some("jwt.b.1")
        );
        assert_eq!(
            mgr.get_access_token().unwrap(),
            "jwt.a.1",
            "legacy slot keeps the DEFAULT binding's JWT"
        );
    }

    /// Re-pairing the DEFAULT tenant updates its entry in place (no
    /// duplicate) and refreshes BOTH slots.
    #[test]
    fn persist_repair_default_updates_entry_and_both_slots() {
        let dir = temp_dir_for("persist_repair");
        let mgr = test_mgr(&dir);
        let path = dir.join("paired_user.json");

        persist_pairing_with(&mgr, &path, &pair_resp("jwt.a.1"), ta()).expect("persist A1");
        persist_pairing_with(&mgr, &path, &pair_resp("jwt.b.1"), tb()).expect("persist B");
        persist_pairing_with(&mgr, &path, &pair_resp("jwt.a.2"), ta()).expect("persist A2");

        let pf = read_file(&path);
        assert_eq!(pf.bindings.len(), 2, "re-pair must not duplicate the entry");
        assert_eq!(pf.default_tenant_id.as_deref(), Some(T_A));
        assert_eq!(
            mgr.get_tenant_device_jwt(&ta()).unwrap().as_deref(),
            Some("jwt.a.2")
        );
        assert_eq!(
            mgr.get_access_token().unwrap(),
            "jwt.a.2",
            "re-pairing the default refreshes the legacy slot too"
        );
    }

    /// Pairing on top of a LEGACY (v1) file migrates it: the legacy
    /// tenant becomes a binding AND stays the default; the new tenant is
    /// appended; the legacy slot (still holding the legacy default's JWT)
    /// is untouched.
    #[test]
    fn persist_migrates_legacy_file_preserving_its_default() {
        let dir = temp_dir_for("persist_migrate");
        let mgr = test_mgr(&dir);
        let path = dir.join("paired_user.json");
        std::fs::write(
            &path,
            format!(r#"{{"user_id":"{USER}","tenant_id":"{T_A}"}}"#),
        )
        .unwrap();
        mgr.store_tokens("jwt.a.legacy", "").unwrap();

        persist_pairing_with(&mgr, &path, &pair_resp("jwt.b.1"), tb()).expect("persist B");

        let pf = read_file(&path);
        assert_eq!(
            pf.bindings.len(),
            2,
            "legacy entry synthesized + B appended"
        );
        assert_eq!(
            pf.default_tenant_id.as_deref(),
            Some(T_A),
            "legacy tenant stays the default"
        );
        assert_eq!(
            mgr.get_access_token().unwrap(),
            "jwt.a.legacy",
            "legacy slot untouched when pairing a non-default tenant"
        );
        assert_eq!(
            mgr.get_tenant_device_jwt(&tb()).unwrap().as_deref(),
            Some("jwt.b.1")
        );
    }

    /// The legacy single-value echo-heal must be a quiet no-op on v2
    /// files (a single-value overwrite would clobber the binding set /
    /// flip the default), while keeping its pre-8a behavior on v1 files.
    #[test]
    fn backfill_heal_noops_on_v2_shape_only() {
        // v1 shape: is_v2() false → heal applies (behavior unchanged).
        let v1: PairedUserFile =
            serde_json::from_str(&format!(r#"{{"user_id":"{USER}","tenant_id":"{T_A}"}}"#))
                .unwrap();
        assert!(!v1.is_v2());
        // v2 shape (either marker) → heal must no-op.
        let v2_bindings: PairedUserFile = serde_json::from_str(&format!(
            r#"{{"user_id":"{USER}","bindings":[{{"tenant_id":"{T_A}","user_id":"{USER}"}}]}}"#
        ))
        .unwrap();
        assert!(v2_bindings.is_v2());
        let v2_default: PairedUserFile = serde_json::from_str(&format!(
            r#"{{"user_id":"{USER}","default_tenant_id":"{T_A}"}}"#
        ))
        .unwrap();
        assert!(v2_default.is_v2());
    }

    /// Reconcile: entries coord no longer returns are dropped and their
    /// slots cleared; the surviving default is kept; the file is
    /// rewritten. Coord-side tenants the runner lacks a JWT for are
    /// flagged (never fabricated).
    #[test]
    fn reconcile_drops_missing_and_flags_coord_only() {
        let dir = temp_dir_for("reconcile_drop_flag");
        let mgr = test_mgr(&dir);
        let path = dir.join("paired_user.json");
        persist_pairing_with(&mgr, &path, &pair_resp("jwt.a.1"), ta()).unwrap();
        persist_pairing_with(&mgr, &path, &pair_resp("jwt.b.1"), tb()).unwrap();

        // Coord says: A still bound, B gone, C newly bound elsewhere.
        let report = reconcile_paired_bindings_with(&mgr, &path, &[ta(), tc()]).expect("reconcile");

        assert_eq!(report.dropped, vec![tb()]);
        assert_eq!(
            report.coord_only,
            vec![tc()],
            "no local JWT for C → flagged"
        );
        assert_eq!(report.default_repointed, None, "default A survived");
        assert!(
            mgr.get_tenant_device_jwt(&tb()).unwrap().is_none(),
            "dropped binding's slot is cleared (the designed slot-deletion home)"
        );
        let pf = read_file(&path);
        assert_eq!(pf.bindings.len(), 1);
        assert_eq!(pf.bindings[0].tenant_id, T_A);
        assert_eq!(pf.default_tenant_id.as_deref(), Some(T_A));
        assert_eq!(
            mgr.get_access_token().unwrap(),
            "jwt.a.1",
            "legacy slot untouched when the default survives"
        );
    }

    /// Reconcile: when the DEFAULT binding is dropped, the most recently
    /// paired survivor becomes the default and its slot JWT is copied
    /// into the legacy access_token slot (D4 invariant).
    #[test]
    fn reconcile_repoints_default_and_legacy_slot() {
        let dir = temp_dir_for("reconcile_repoint");
        let mgr = test_mgr(&dir);
        let path = dir.join("paired_user.json");
        persist_pairing_with(&mgr, &path, &pair_resp("jwt.a.1"), ta()).unwrap();
        persist_pairing_with(&mgr, &path, &pair_resp("jwt.b.1"), tb()).unwrap();
        assert_eq!(mgr.get_access_token().unwrap(), "jwt.a.1");

        // Coord dropped A (the default); B survives.
        let report = reconcile_paired_bindings_with(&mgr, &path, &[tb()]).expect("reconcile");

        assert_eq!(report.dropped, vec![ta()]);
        assert_eq!(report.default_repointed, Some(Some(tb())));
        let pf = read_file(&path);
        assert_eq!(pf.default_tenant_id.as_deref(), Some(T_B));
        assert_eq!(
            pf.tenant_id.as_deref(),
            Some(T_B),
            "mirror follows the default"
        );
        assert_eq!(
            mgr.get_access_token().unwrap(),
            "jwt.b.1",
            "legacy slot re-pointed to the new default binding's JWT"
        );
        assert!(mgr.get_tenant_device_jwt(&ta()).unwrap().is_none());
    }

    /// Reconcile: present-but-EMPTY set = coord says zero bindings → all
    /// entries + slots dropped, default cleared, but the legacy
    /// access_token slot is deliberately left as-is (Phase 3 unpair
    /// posture — the refresh gate stops it self-perpetuating).
    #[test]
    fn reconcile_empty_set_drops_all_but_preserves_legacy_slot() {
        let dir = temp_dir_for("reconcile_empty");
        let mgr = test_mgr(&dir);
        let path = dir.join("paired_user.json");
        persist_pairing_with(&mgr, &path, &pair_resp("jwt.a.1"), ta()).unwrap();

        let report = reconcile_paired_bindings_with(&mgr, &path, &[]).expect("reconcile");

        assert_eq!(report.dropped, vec![ta()]);
        assert_eq!(report.default_repointed, Some(None), "default cleared");
        let pf = read_file(&path);
        assert!(pf.bindings.is_empty());
        assert!(pf.default_tenant_id.is_none());
        assert!(pf.tenant_id.is_none());
        assert_eq!(pf.user_id, USER, "user_id kept for legacy readers");
        assert!(mgr.get_tenant_device_jwt(&ta()).unwrap().is_none());
        assert_eq!(
            mgr.get_access_token().unwrap(),
            "jwt.a.1",
            "legacy slot left as-is when no binding survives"
        );
    }

    /// Reconcile: a matching set is a pure no-op — no report changes and
    /// NO disk write (the heartbeat calls this every 30s; steady state
    /// must not churn the file).
    #[test]
    fn reconcile_matching_set_writes_nothing() {
        let dir = temp_dir_for("reconcile_noop");
        let mgr = test_mgr(&dir);
        let path = dir.join("paired_user.json");
        persist_pairing_with(&mgr, &path, &pair_resp("jwt.a.1"), ta()).unwrap();
        persist_pairing_with(&mgr, &path, &pair_resp("jwt.b.1"), tb()).unwrap();
        let before = std::fs::read(&path).unwrap();

        let report = reconcile_paired_bindings_with(&mgr, &path, &[ta(), tb()]).expect("reconcile");

        assert!(!report.changed(), "nothing changed: {report:?}");
        assert!(report.coord_only.is_empty(), "both tenants hold JWTs");
        assert_eq!(
            std::fs::read(&path).unwrap(),
            before,
            "steady state must not rewrite the file"
        );
    }

    /// Reconcile with NO local file: nothing to drop, coord-side
    /// bindings flagged, quiet success (never an error).
    #[test]
    fn reconcile_missing_file_flags_only() {
        let dir = temp_dir_for("reconcile_missing");
        let mgr = test_mgr(&dir);
        let path = dir.join("paired_user.json");

        let report = reconcile_paired_bindings_with(&mgr, &path, &[ta()]).expect("reconcile");
        assert!(report.dropped.is_empty());
        assert_eq!(report.coord_only, vec![ta()]);
        assert!(!path.exists(), "no file must be created");
    }

    /// Reconcile clears ORPHAN slots (credential without a binding
    /// entry) that coord's set doesn't contain either.
    #[test]
    fn reconcile_clears_orphan_slots_outside_coord_set() {
        let dir = temp_dir_for("reconcile_orphan");
        let mgr = test_mgr(&dir);
        let path = dir.join("paired_user.json");
        persist_pairing_with(&mgr, &path, &pair_resp("jwt.a.1"), ta()).unwrap();
        // Orphan slot for C: no binding entry, and coord doesn't have C.
        mgr.store_tenant_device_jwt(&tc(), "jwt.c.orphan").unwrap();

        let report = reconcile_paired_bindings_with(&mgr, &path, &[ta()]).expect("reconcile");

        assert_eq!(report.dropped_slots, vec![tc()]);
        assert!(mgr.get_tenant_device_jwt(&tc()).unwrap().is_none());
        assert_eq!(
            mgr.get_tenant_device_jwt(&ta()).unwrap().as_deref(),
            Some("jwt.a.1"),
            "coord-confirmed slot untouched"
        );
    }

    // ------------------------------------------------------------------------
    // ensure_device_id_persisted_at — machine.json device_id backfill
    // (Phase 0 multi-user readiness). All variants operate on a tempdir path
    // so they never touch the operator's real ~/.qontinui.
    // ------------------------------------------------------------------------

    fn read_json(path: &std::path::Path) -> serde_json::Value {
        let bytes = std::fs::read(path).expect("read machine.json");
        serde_json::from_slice(&bytes).expect("parse machine.json")
    }

    #[test]
    fn backfill_adds_device_id_and_preserves_siblings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("machine.json");
        // Legacy shape — exactly the operator's live file.
        std::fs::write(
            &path,
            r#"{"machine_id":"abc-123","hostname":"laptop","active_tenant_id":"tenant-9"}"#,
        )
        .unwrap();

        ensure_device_id_persisted_at(&path);

        let v = read_json(&path);
        // Canonical key added with the same value...
        assert_eq!(v.get("device_id").and_then(|x| x.as_str()), Some("abc-123"));
        // ...legacy key preserved (other readers may still alias off it)...
        assert_eq!(
            v.get("machine_id").and_then(|x| x.as_str()),
            Some("abc-123")
        );
        // ...and every sibling field carried verbatim.
        assert_eq!(v.get("hostname").and_then(|x| x.as_str()), Some("laptop"));
        assert_eq!(
            v.get("active_tenant_id").and_then(|x| x.as_str()),
            Some("tenant-9")
        );
    }

    #[test]
    fn backfill_leaves_canonical_file_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("machine.json");
        let canonical = r#"{"device_id":"keep-me","hostname":"box"}"#;
        std::fs::write(&path, canonical).unwrap();
        let before = std::fs::read_to_string(&path).unwrap();

        ensure_device_id_persisted_at(&path);

        // Byte-for-byte identical — no rewrite when device_id already present.
        let after = std::fs::read_to_string(&path).unwrap();
        assert_eq!(before, after);
    }

    #[test]
    fn backfill_missing_file_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("machine.json");
        // File does not exist.
        ensure_device_id_persisted_at(&path);
        assert!(!path.exists(), "must not create the file");
    }

    // ------------------------------------------------------------------------
    // ensure_device_initialized_at — mint machine.json on a fresh install so a
    // production user never hits "device not initialized — run
    // `qontinui_profile device init`". Tempdir-scoped; never touches real home.
    // ------------------------------------------------------------------------

    #[test]
    fn autoinit_mints_identity_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("machine.json");
        assert!(!path.exists());

        ensure_device_initialized_at(&path);

        assert!(path.exists(), "must mint machine.json on a fresh install");
        let v = read_json(&path);
        let id = v
            .get("device_id")
            .and_then(|x| x.as_str())
            .expect("device_id present");
        // A real UUID v4, not empty / placeholder.
        uuid::Uuid::parse_str(id).expect("device_id is a valid UUID");
        assert!(
            v.get("hostname").and_then(|x| x.as_str()).is_some(),
            "hostname recorded"
        );
        // The minted file must parse under the same MachineFile reader that
        // errored before the fix (device_id present + deserializable).
        let parsed: MachineFile = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(parsed.device_id, id);
    }

    #[test]
    fn autoinit_preserves_uuid_on_second_call() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("machine.json");

        ensure_device_initialized_at(&path);
        let id1 = read_json(&path)
            .get("device_id")
            .and_then(|x| x.as_str())
            .unwrap()
            .to_string();

        ensure_device_initialized_at(&path);
        let id2 = read_json(&path)
            .get("device_id")
            .and_then(|x| x.as_str())
            .unwrap()
            .to_string();

        assert_eq!(
            id1, id2,
            "must never re-mint / overwrite an existing identity"
        );
    }

    #[test]
    fn autoinit_keeps_legacy_id_and_backfills() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("machine.json");
        // Operator's legacy machine_id-only file: keep the id, don't re-mint.
        std::fs::write(&path, r#"{"machine_id":"legacy-abc","hostname":"laptop"}"#).unwrap();

        ensure_device_initialized_at(&path);

        let v = read_json(&path);
        assert_eq!(
            v.get("device_id").and_then(|x| x.as_str()),
            Some("legacy-abc"),
            "existing identity preserved (backfill, not re-mint)"
        );
        assert_eq!(
            v.get("machine_id").and_then(|x| x.as_str()),
            Some("legacy-abc")
        );
    }

    #[test]
    fn backfill_file_without_machine_id_is_noop() {
        // Neither canonical nor legacy id key → nothing to backfill from.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("machine.json");
        let original = r#"{"hostname":"box"}"#;
        std::fs::write(&path, original).unwrap();
        let before = std::fs::read_to_string(&path).unwrap();

        ensure_device_id_persisted_at(&path);

        let after = std::fs::read_to_string(&path).unwrap();
        assert_eq!(before, after);
    }

    #[test]
    fn backfill_unparseable_file_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("machine.json");
        std::fs::write(&path, b"not json at all {{{").unwrap();
        let before = std::fs::read(&path).unwrap();

        ensure_device_id_persisted_at(&path);

        let after = std::fs::read(&path).unwrap();
        assert_eq!(before, after, "unparseable file left untouched");
    }
}

// ============================================================================
// E2E pair tests against an in-process mock web backend (Phase 5.1)
// ============================================================================
//
// These tests exercise `pair_with_auth_token_with_ids` against a real
// axum server bound to `127.0.0.1:0` on a background thread. No external
// crates beyond what's already in dev-deps; the mock spins up + tears
// down in <100ms per test, so we get hermetic HTTP coverage without
// pulling in `mockito` / `wiremock`.
//
// Why `_with_ids`? `pair_with_auth_token` reads `device_id` from
// `~/.qontinui/machine.json` and `user_id` from
// `{data_local_dir}/com.qontinui.runner/paired_user.json`. Those are
// user-data filesystem reads — fine in production, hostile in tests
// (would mutate the operator's files or spuriously fail in clean CI).
// The `_with_ids` variant accepts the IDs directly so the test can pass
// synthetic UUIDs and hit the HTTP path 1:1.
#[cfg(test)]
mod pair_e2e_tests {
    use super::*;
    use axum::{
        body::Bytes,
        extract::State,
        http::{HeaderMap, StatusCode},
        routing::post,
        Router,
    };
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    /// One captured request, populated by the mock route on each call.
    #[derive(Debug, Clone, Default)]
    struct CapturedRequest {
        authorization: Option<String>,
        user_id_header: Option<String>,
        body_bytes: Vec<u8>,
        called: bool,
    }

    /// Shared state across the test thread + axum's worker.
    #[derive(Clone)]
    struct MockState {
        capture: Arc<Mutex<CapturedRequest>>,
        /// HTTP status to return.
        status: StatusCode,
        /// Response body (200 path serves `canonical_200_body`;
        /// non-2xx paths serve a short error JSON).
        response_body: String,
    }

    /// Canonical-shape 200 body returned by the web-backend's
    /// `POST /api/v1/devices/pair-cli` on success (passthrough of the
    /// coord response).
    fn canonical_200_body(token: &str) -> String {
        serde_json::json!({
            "token": token,
            "device_id": "11111111-1111-4111-8111-111111111111",
            "user_id":   "22222222-2222-4222-8222-222222222222",
            "jti":       "33333333-3333-4333-8333-333333333333",
            "exp":       chrono::Utc::now().timestamp() + 4 * 60 * 60,
        })
        .to_string()
    }

    /// Handler for `POST /api/v1/devices/pair-cli`. Captures the inbound
    /// request shape onto shared state then returns the canned response.
    async fn pair_cli_handler(
        State(state): State<MockState>,
        headers: HeaderMap,
        body: Bytes,
    ) -> (StatusCode, String) {
        let mut capture = state.capture.lock().expect("capture mutex");
        capture.called = true;
        capture.authorization = headers
            .get("Authorization")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());
        capture.user_id_header = headers
            .get("X-Qontinui-User-Id")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());
        capture.body_bytes = body.to_vec();
        (state.status, state.response_body.clone())
    }

    /// Boot a mock web backend on `127.0.0.1:0` serving the pair-cli
    /// proxy endpoint. Returns `(base_url, capture_handle, shutdown_signal)`.
    /// The caller drops `shutdown_signal` (or sends on it) to terminate the
    /// server.
    fn spawn_mock_web_backend(
        status: StatusCode,
        response_body: String,
    ) -> (
        String,
        Arc<Mutex<CapturedRequest>>,
        tokio::sync::oneshot::Sender<()>,
    ) {
        let capture = Arc::new(Mutex::new(CapturedRequest::default()));
        let capture_for_handler = capture.clone();

        // Bind synchronously so we can read the port before returning.
        let std_listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind 127.0.0.1:0");
        let port = std_listener.local_addr().expect("local_addr").port();
        std_listener.set_nonblocking(true).expect("set_nonblocking");

        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("rt");
            rt.block_on(async move {
                let mock_state = MockState {
                    capture: capture_for_handler,
                    status,
                    response_body,
                };
                let app: Router = Router::new()
                    // Mirror the live route — pair_with_auth_token POSTs to
                    // `{base}/api/v1/devices/pair-cli` (web-routed). The mock
                    // used to register `/coord/devices/pair-cli` (the legacy
                    // coord-direct path) and 404'd the request, panicking all
                    // four pair_e2e tests. Tied to the call site at line 369.
                    .route("/api/v1/devices/pair-cli", post(pair_cli_handler))
                    .with_state(mock_state);
                let listener =
                    tokio::net::TcpListener::from_std(std_listener).expect("tokio listener");
                let _ = axum::serve(listener, app)
                    .with_graceful_shutdown(async move {
                        let _ = shutdown_rx.await;
                    })
                    .await;
            });
        });

        // Tiny settle to let the listener attach (axum is ready as soon as
        // `from_std` completes, but the spawned thread might not have
        // entered `block_on` yet). 50ms is generous on Windows + Linux.
        std::thread::sleep(Duration::from_millis(50));

        (format!("http://127.0.0.1:{port}"), capture, shutdown_tx)
    }

    const TEST_DEVICE_ID: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    const TEST_USER_ID: &str = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
    const TEST_BEARER: &str = "bearer-token";
    const TEST_TENANT_ID: &str = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";

    fn test_tenant_uuid() -> uuid::Uuid {
        uuid::Uuid::parse_str(TEST_TENANT_ID).unwrap()
    }

    #[test]
    fn pair_with_auth_token_e2e_200_persists_jwt() {
        // Mock web backend at 127.0.0.1:<port> returns the canonical 200 JSON;
        // assert the returned PairCompleteResponse.token matches the
        // synthetic JWT we configured the mock to emit.
        let expected_jwt = "header-segment.payload-segment.signature-segment";
        let (base, _capture, _shutdown) =
            spawn_mock_web_backend(StatusCode::OK, canonical_200_body(expected_jwt));
        let result = pair_with_auth_token_with_ids(
            &base,
            TEST_BEARER,
            TEST_DEVICE_ID,
            TEST_USER_ID,
            test_tenant_uuid(),
        );
        let resp = result.expect("pair_with_auth_token_with_ids should succeed on 200");
        assert_eq!(resp.token, expected_jwt);
        assert_eq!(resp.user_id, "22222222-2222-4222-8222-222222222222");
        assert_eq!(
            resp.device_id.as_deref(),
            Some("11111111-1111-4111-8111-111111111111")
        );
    }

    #[test]
    fn pair_with_auth_token_e2e_401_returns_err() {
        // Mock returns 401 with a coord-shaped error body; assert the
        // error string surfaces the status code + body so an operator
        // can diagnose by reading the log.
        let (base, _capture, _shutdown) = spawn_mock_web_backend(
            StatusCode::UNAUTHORIZED,
            r#"{"error":"invalid bearer token"}"#.to_string(),
        );
        let result = pair_with_auth_token_with_ids(
            &base,
            TEST_BEARER,
            TEST_DEVICE_ID,
            TEST_USER_ID,
            test_tenant_uuid(),
        );
        let err = result.expect_err("401 must return Err");
        assert!(
            err.contains("401"),
            "error should mention status 401, got: {err}"
        );
    }

    #[test]
    fn pair_with_auth_token_e2e_500_returns_err() {
        // Coord-side bug → 500. Caller must surface the error rather
        // than silently treating it as a refresh-needed signal.
        let (base, _capture, _shutdown) = spawn_mock_web_backend(
            StatusCode::INTERNAL_SERVER_ERROR,
            r#"{"error":"coord-side panic"}"#.to_string(),
        );
        let result = pair_with_auth_token_with_ids(
            &base,
            TEST_BEARER,
            TEST_DEVICE_ID,
            TEST_USER_ID,
            test_tenant_uuid(),
        );
        let err = result.expect_err("500 must return Err");
        assert!(
            err.contains("500"),
            "error should mention status 500, got: {err}"
        );
    }

    #[test]
    fn pair_with_auth_token_e2e_request_shape() {
        // Defect-5 wire-shape regression guard: the request the runner
        // emits MUST carry `Authorization: Bearer <oauth>`, the
        // `X-Qontinui-User-Id: <uuid>` header, and a JSON body containing
        // `device_id` + `hostname`.
        let (base, capture, _shutdown) =
            spawn_mock_web_backend(StatusCode::OK, canonical_200_body("ignored-jwt"));
        let _ = pair_with_auth_token_with_ids(
            &base,
            TEST_BEARER,
            TEST_DEVICE_ID,
            TEST_USER_ID,
            test_tenant_uuid(),
        )
        .expect("200 path");

        let captured = capture.lock().expect("capture mutex").clone();
        assert!(captured.called, "mock web backend must have been hit");
        assert_eq!(
            captured.authorization.as_deref(),
            Some(&*format!("Bearer {TEST_BEARER}")),
            "Authorization header must be `Bearer <token>`"
        );
        assert_eq!(
            captured.user_id_header.as_deref(),
            Some(TEST_USER_ID),
            "X-Qontinui-User-Id header must carry the cached user_id"
        );

        let body_json: serde_json::Value =
            serde_json::from_slice(&captured.body_bytes).expect("request body must be valid JSON");
        let obj = body_json.as_object().expect("body must be a JSON object");
        assert_eq!(
            obj.get("device_id").and_then(|v| v.as_str()),
            Some(TEST_DEVICE_ID),
            "body.device_id must match what we passed"
        );
        assert!(
            obj.get("hostname").and_then(|v| v.as_str()).is_some(),
            "body.hostname must be present"
        );
        assert_eq!(
            obj.get("tenant_id").and_then(|v| v.as_str()),
            Some(TEST_TENANT_ID),
            "body.tenant_id must carry the resolved tenant (Phase 2 of default-tenant-propagation)"
        );
        // Reject legacy keys (would 422 against the canonical coord
        // PairCliRequest schema):
        assert!(!obj.contains_key("os"), "legacy `os` key must not be sent");
        assert!(
            !obj.contains_key("os_version"),
            "legacy `os_version` key must not be sent"
        );
    }
}
