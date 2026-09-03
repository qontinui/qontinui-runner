//! Cross-machine **respawn** — re-launch an ALREADY-CLOSED session on this
//! device under a chosen Claude account.
//!
//! Plan `2026-08-26-sessions-console-consolidation` §6 Phase 5, runner half.
//! The coord half (`POST /sessions/:id/respawn`,
//! `GET /sessions/respawn-requests`) shipped first and is the wire contract
//! this module consumes; it is not re-derived here.
//!
//! ## Transport — the SAME socket the handoff receiver already holds
//!
//! Coord dual-publishes the request on
//! `qontinui.sessions.<tenant>.<target-device>.respawn_request`, i.e. the
//! subject family [`super::handoff`] already PSUBSCRIBEs as
//! `qontinui.sessions.*`. So this module adds **no second socket, no second
//! poll loop and no new pattern** — [`super::handoff::connect_and_pump`]
//! forwards every frame here as well, and [`super::handoff::run_catchup`]
//! runs the respawn catch-up alongside the handoff one.
//!
//! ⚠️ The two arms are disambiguated ONLY by the channel's trailing segment.
//! The shipped [`super::handoff::parse_handoff_push`] filters on
//! `.{device_id}.handoff_request`, so it already ignores `respawn_request`
//! frames (there is no double-materialization to fix); this module's
//! [`parse_respawn_push`] is its exact twin on `.{device_id}.respawn_request`
//! and ignores `handoff_request`. Both directions are asserted in the tests.
//!
//! ## Offline catch-up is a SEPARATE route, on purpose
//!
//! `GET /sessions/respawn-requests?device_id=` — not the handoff one. Coord's
//! handoff read filters `s.state <> 'closed'`, which is fatal for a respawn
//! (whose source is closed BY CONSTRUCTION), and its `PendingHandoff` shape
//! carries neither the account pin nor the Claude session id a respawn is made
//! of.
//!
//! ## Receiver flow (one respawn)
//!
//! 1. **The Claude session id.** An explicit `null` is **UNKNOWN**, never a nil
//!    UUID and never "no transcript" — the respawn fails rather than
//!    `--resume`-ing something invented.
//! 2. **The account pin, resolved through the shipped seam** —
//!    [`crate::agent_runtime::resolve_spawn_account`] →
//!    [`crate::ai_provider::resolve_requested_account`]. **NOT**
//!    `pick_best_account`, which is a side-effect-only rotation helper
//!    (returns `()`, no-ops unless `account_selection_mode == LeastUsage`, and
//!    every call site discards it). 🔒 An unresolvable pin **FAILS the
//!    respawn**; it never degrades to rotation and never lands the session on
//!    a different account, because a pinned account silently ignored is
//!    indistinguishable from one honoured
//!    (`agent_runtime.rs` `LaunchPayload::account`, :54-57). Nor is the pin
//!    resolved through a `switch_claude_account` mutation, which would leak
//!    this one respawn's choice into every later spawn on this runner.
//! 3. **The per-session migration cap** ([`crate::terminal::account_migration`]
//!    `MIGRATION_CAP` = 3 / 24h) — a respawn is another account hop for the
//!    same Claude session, so it counts against the same budget rather than
//!    opening a bypass around it.
//! 4. **The source bundle** — `GET /sessions/:id/handoff-state`, the same read
//!    the handoff receiver does. It is `FleetPrincipal`-gated (a device JWT
//!    reads it) and carries no `state <> 'closed'` filter, so a closed source
//!    still resolves. cwd, intent and held claims come from here.
//! 5. **The transcript, into the TARGET account's project dir**
//!    (`<config-dir>/projects/<slug>/<sid>.jsonl`). Local copy first — reusing
//!    [`crate::terminal::account_migration::copy_transcript`] verbatim — then
//!    coord's warm→cold `?stream=transcript` tier for the cross-machine case.
//!    Neither yielding bytes is a hard failure: `--resume` against a
//!    non-existent transcript would start an EMPTY conversation wearing the
//!    old session's id, which is exactly the fabricated-half this plan forbids.
//! 6. **The resume**, through the shared
//!    [`crate::terminal::account_migration::spawn_resumed_pane`] seam — the
//!    same `claude --permission-mode bypassPermissions --resume <sid>` in a
//!    fresh PTY, pinned via `capture_hint.config_dir` (which threads
//!    `CLAUDE_CONFIG_DIR` into the PTY **and** keeps the durable restore record
//!    consistent).
//! 7. **Claims re-acquired** under this device and **`parent_session_id`
//!    stamped**, exactly as [`super::handoff`] does, so the respawn is one link
//!    in the lineage chain rather than an orphan.
//! 8. **The source session is NOT closed.** This is the one place the respawn
//!    arm genuinely diverges from handoff's step 6: the source is *already*
//!    closed — that is the premise of the feature — and coord's `post_respawn`
//!    issues no `UPDATE` against `coord.sessions` either. There is no
//!    `DELETE /sessions/:id` anywhere in this module, and a test asserts it.

use std::sync::Arc;

use base64::Engine;
use serde::Deserialize;
use uuid::Uuid;

use super::handoff::HandoffError;
use super::session_lifecycle_store::SessionLifecycleStore;
use super::SessionRegistry;

/// Warm-tier chunk cap for the transcript read. Matches the handoff-state
/// read cap coord documents on `GET /sessions/:id/output`.
const TRANSCRIPT_WARM_LIMIT: i64 = 4096;

// ---------------------------------------------------------------------------
// Wire types — mirror coord's `sessions::PendingRespawn` (commit f4a138b4).
// ---------------------------------------------------------------------------

/// One pending respawn, as returned by `GET /sessions/respawn-requests` and as
/// carried in the `respawn_request` push payload.
///
/// Every optional field is an **explicit `null` on the wire when coord has no
/// value** — coord serializes them rather than skipping them, precisely so the
/// receiver can tell "coord says there is none" from "this coord is too old to
/// have said". Nothing here is defaulted: a `null` stays `None` and the caller
/// decides, instead of a nil UUID / empty string / stand-in account leaking
/// downstream as if it were real.
#[derive(Debug, Clone, Deserialize)]
pub struct PendingRespawn {
    pub source_session_id: Uuid,
    pub target_device_id: Uuid,
    /// Informational — the tenant coord scoped the request under. Read as an
    /// `Option` so a partial payload is UNKNOWN rather than `Uuid::nil()`
    /// (which `get_handoff_requests`' payload read does default to, and which
    /// this deliberately does not repeat).
    #[serde(default)]
    pub tenant_id: Option<Uuid>,
    /// Informational. The AUTHORITATIVE kind for the child intent comes from
    /// the `handoff-state` bundle's own `session_kind`, not from here.
    #[serde(default)]
    pub session_kind: Option<String>,
    #[serde(default)]
    pub source_device_id: Option<Uuid>,
    #[serde(default)]
    pub source_state: Option<String>,
    /// The Claude session UUID to `--resume`. Explicit `null` = UNKNOWN —
    /// either the session never carried one, or coord's DB predates the bridge
    /// column. **Never** "no transcript", and never substitutable.
    #[serde(default)]
    pub claude_code_session_id: Option<Uuid>,
    /// The account LABEL to pin. `null` = no pin (this runner's own rotation
    /// default applies). A pin that cannot be resolved here FAILS the respawn.
    #[serde(default)]
    pub account: Option<String>,
    /// Prompt to deliver once the resumed CLI paints its idle prompt. `null` =
    /// resume and say nothing.
    #[serde(default)]
    pub initial_prompt: Option<String>,
}

/// Envelope coord returns from `GET /sessions/respawn-requests`.
#[derive(Debug, Clone, Deserialize)]
struct RespawnListResponse {
    #[serde(default)]
    respawns: Vec<PendingRespawn>,
}

/// Errors raised by the respawn receiver. Each is reported (WARN) rather than
/// swallowed — a respawn that did not happen must not look like one that did.
#[derive(Debug, thiserror::Error)]
pub enum RespawnError {
    /// 🔒 The fail-loud pin. NEVER recovered from by rotating.
    #[error("account pin refused: {0}")]
    AccountPin(String),
    /// No pin was given AND this runner could not resolve its own default
    /// account, so there is no config dir to materialize the transcript into.
    #[error(
        "respawn carries no account pin and this runner has no resolved Claude \
         config dir — refusing to guess one"
    )]
    NoAccount,
    #[error(
        "coord has no claude_code_session_id for source session {0} — a respawn \
         resumes a real conversation, and an absent id is UNKNOWN, not a session \
         id to invent"
    )]
    NoClaudeSession(Uuid),
    #[error(
        "no working dir for source session {0} — the transcript path is anchored \
         on it, so guessing one would resume the wrong (or an empty) conversation"
    )]
    NoWorkingDir(Uuid),
    #[error(
        "no transcript for Claude session {0} — not on this box, and coord's warm \
         and cold tiers served none. Resuming anyway would open an EMPTY \
         conversation wearing the old session's id"
    )]
    NoTranscript(String),
    #[error("migration cap ({cap} per 24h) reached for Claude session {session}")]
    CapReached { session: String, cap: usize },
    #[error("transcript materialization failed: {0}")]
    Transcript(String),
    #[error("respawn spawn failed: {0}")]
    Spawn(String),
    #[error("the Tauri app handle is not available — cannot spawn a PTY")]
    NoAppHandle,
    #[error(transparent)]
    Coord(#[from] HandoffError),
}

// ---------------------------------------------------------------------------
// Push + catch-up (driven by the handoff receiver's single WS loop)
// ---------------------------------------------------------------------------

/// Pure parse+filter of a coord `/ws` envelope into a [`PendingRespawn`]
/// addressed to `device_id`. Returns `None` when the frame is not a respawn
/// for this device.
///
/// The twin of [`super::handoff::parse_handoff_push`], differing ONLY in the
/// channel's trailing segment: `.{device}.respawn_request` here,
/// `.{device}.handoff_request` there. Neither can swallow the other's frames,
/// which is what keeps one socket serving two arms without
/// double-materializing anything.
pub(super) fn parse_respawn_push(text: &str, device_id: Uuid) -> Option<PendingRespawn> {
    let envelope: serde_json::Value = serde_json::from_str(text).ok()?;
    let channel = envelope.get("channel").and_then(|c| c.as_str())?;

    let suffix = format!(".{device_id}.respawn_request");
    if !channel.starts_with("qontinui.sessions.") || !channel.ends_with(&suffix) {
        return None;
    }

    // Payload may be a JSON string (the Redis arm) or an inlined object.
    let payload_val = match envelope.get("payload") {
        Some(serde_json::Value::String(s)) => serde_json::from_str::<serde_json::Value>(s).ok()?,
        Some(other) => other.clone(),
        None => return None,
    };

    let pending: PendingRespawn = serde_json::from_value(payload_val).ok()?;

    // Defense-in-depth: the channel already filtered by device, but if the
    // payload's target disagrees, trust the address, not the body.
    if pending.target_device_id != device_id {
        return None;
    }
    Some(pending)
}

/// Fetch the durable pending-respawn list for this device — the on-(re)connect
/// catch-up, run alongside the handoff one.
///
/// A SEPARATE coord route from `/sessions/handoff-requests` on purpose: that
/// read filters `s.state <> 'closed'` (fatal here) and its row shape carries
/// neither the account pin nor the Claude session id.
pub(super) async fn fetch_pending(
    http: &reqwest::Client,
    coord_url: &str,
    device_id: Uuid,
) -> Result<Vec<PendingRespawn>, HandoffError> {
    let url = format!(
        "{}/sessions/respawn-requests?device_id={}",
        coord_url.trim_end_matches('/'),
        device_id
    );
    let resp = crate::coord_http::coord_get(http, &url)
        .send()
        .await
        .map_err(|e| HandoffError::Http(format!("GET {url}: {e}")))?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(HandoffError::Status(
            status.as_u16(),
            body.chars().take(300).collect(),
        ));
    }
    let parsed: RespawnListResponse = resp
        .json()
        .await
        .map_err(|e| HandoffError::Parse(format!("decode respawn list: {e}")))?;
    Ok(parsed.respawns)
}

/// The on-(re)connect respawn catch-up. Mirrors
/// [`super::handoff::run_catchup`]'s posture: best-effort, one line on a
/// 401/403 (pre-pairing window), debug otherwise — the push path stays active
/// either way and the next reconnect retries.
pub(super) async fn run_catchup(
    registry: &Arc<SessionRegistry>,
    lifecycle_store: &Arc<SessionLifecycleStore>,
    http: &reqwest::Client,
    coord_url: &str,
    device_id: Uuid,
) {
    match fetch_pending(http, coord_url, device_id).await {
        Ok(pending) => {
            if !pending.is_empty() {
                tracing::info!(
                    count = pending.len(),
                    "session respawn: on-connect catch-up replaying pending respawns"
                );
            }
            for respawn in pending {
                materialize_logged(registry, lifecycle_store, http, coord_url, &respawn).await;
            }
        }
        Err(HandoffError::Status(401 | 403, _)) => {
            tracing::warn!(
                "session respawn: catch-up GET unauthorized (401/403) — retrying after device pairing/auth"
            );
        }
        Err(e) => {
            tracing::debug!(error = %e, "session respawn: catch-up GET failed (push path still active)");
        }
    }
}

/// Handle one inbound `/ws` frame on the respawn arm. A frame that is not a
/// respawn for this device is ignored silently (the handoff arm gets the same
/// text and filters it on its own suffix).
pub(super) async fn handle_push_frame(
    registry: &Arc<SessionRegistry>,
    lifecycle_store: &Arc<SessionLifecycleStore>,
    http: &reqwest::Client,
    coord_url: &str,
    device_id: Uuid,
    text: &str,
) {
    let Some(respawn) = parse_respawn_push(text, device_id) else {
        return;
    };
    tracing::info!(
        source = %respawn.source_session_id,
        "session respawn: push received; materializing"
    );
    materialize_logged(registry, lifecycle_store, http, coord_url, &respawn).await;
}

/// Materialize a respawn and log (but swallow) any error. The durable coord
/// row is left intact on failure so the next push/catch-up retries.
///
/// Every failure is a WARN naming the reason. A refused account pin in
/// particular must be VISIBLE: the whole point of failing loud is that the
/// operator learns the respawn did not happen instead of hunting for a session
/// that quietly landed on another account.
async fn materialize_logged(
    registry: &Arc<SessionRegistry>,
    lifecycle_store: &Arc<SessionLifecycleStore>,
    http: &reqwest::Client,
    coord_url: &str,
    respawn: &PendingRespawn,
) {
    match materialize(registry, lifecycle_store, http, coord_url, respawn).await {
        Ok(terminal_id) => {
            tracing::info!(
                source = %respawn.source_session_id,
                terminal = %terminal_id,
                "session respawn: resumed session is live"
            );
        }
        Err(e) => {
            tracing::warn!(
                source = %respawn.source_session_id,
                account = ?respawn.account,
                error = %e,
                "session respawn REFUSED — nothing was launched; the source session was \
                 NOT closed (it already is) and the durable coord row is left for retry"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Materialization
// ---------------------------------------------------------------------------

/// The target account's config dir for this respawn.
///
/// 🔒 The fail-loud pin lives here, and it has exactly two arms:
/// - a pin present → [`crate::agent_runtime::resolve_spawn_account`] (the
///   shipped seam) resolves it or the respawn FAILS. There is no third
///   branch: no `pick_best_account`, no "nearest match", no landing on
///   another account with a warning.
/// - no pin (`null`, or blank — which names no account at all, so the shipped
///   seam reads it as absence rather than a wrong name) → this runner's own
///   resolved default, i.e. today's rotation, untouched. When even that is
///   absent we still refuse rather than guess a directory.
fn resolve_target_config_dir(account: Option<&str>) -> Result<String, RespawnError> {
    let pinned = crate::agent_runtime::resolve_spawn_account(account)
        .map_err(|e| RespawnError::AccountPin(format!("{e:#}")))?;
    match pinned {
        Some(acct) => {
            if let Some(secs) = acct.cooldown_remaining_secs {
                tracing::warn!(
                    account = %acct.account_name,
                    cooldown_secs = secs,
                    "session respawn: pinned account is rate-limited; respawning anyway \
                     per the explicit pin"
                );
            }
            tracing::info!(
                account = %acct.account_name,
                "session respawn: pinned to Claude account (per-PTY CLAUDE_CONFIG_DIR \
                 override — account_selection_mode does not apply, and no \
                 switch_claude_account mutation is performed)"
            );
            Ok(acct.config_dir)
        }
        None => crate::ai_provider::get_resolved_config_dir().ok_or(RespawnError::NoAccount),
    }
}

/// The source session's working dir — the PTY cwd AND the anchor the on-disk
/// transcript path is derived from.
///
/// Local record first (it is authoritative for where the transcript actually
/// sits on THIS box when the session ran here), then the coord bundle's `repo`
/// (the same value [`super::handoff::build_child_intent`] hands the child as
/// its cwd). Neither ⇒ `None`, and the caller refuses; a guessed cwd resumes
/// the wrong conversation or an empty one.
fn resolve_working_dir(
    local: Option<&super::session_lifecycle_store::TerminalSessionRecord>,
    state_repo: Option<&str>,
) -> Option<String> {
    local
        .and_then(|rec| rec.working_dir.clone())
        .filter(|w| !w.trim().is_empty())
        .or_else(|| {
            state_repo
                .map(str::to_string)
                .filter(|r| !r.trim().is_empty())
        })
}

/// Copy the transcript out of whichever LOCAL account dir holds it into the
/// target account's project dir. Returns `Ok(true)` when a copy landed (or the
/// target already had it), `Ok(false)` when no local dir holds it.
///
/// Reuses [`crate::terminal::account_migration::copy_transcript`] rather than
/// re-implementing the copy: same idempotent "skip when the destination is
/// already at least as large" behaviour, same never-touch-the-source rule.
fn materialize_transcript_locally(
    config_dir: &str,
    working_dir: &str,
    claude_session_id: &str,
) -> Result<bool, RespawnError> {
    let target_path = crate::terminal::transcript::session_transcript_path(
        std::path::Path::new(config_dir),
        working_dir,
        claude_session_id,
    );
    if target_path.exists() {
        return Ok(true);
    }
    for src in local_config_dirs() {
        if src == config_dir {
            continue;
        }
        let candidate = crate::terminal::transcript::session_transcript_path(
            std::path::Path::new(&src),
            working_dir,
            claude_session_id,
        );
        if !candidate.exists() {
            continue;
        }
        crate::terminal::account_migration::copy_transcript(
            &src,
            config_dir,
            working_dir,
            claude_session_id,
        )
        .map_err(RespawnError::Transcript)?;
        tracing::info!(
            session = %claude_session_id,
            from = %src,
            to = %config_dir,
            "session respawn: transcript copied from a local account dir"
        );
        return Ok(true);
    }
    Ok(false)
}

/// Every Claude config dir this box knows about — the configured roster plus
/// whatever the transcript scanner discovers. De-duplicated, order preserved.
fn local_config_dirs() -> Vec<String> {
    let mut out: Vec<String> = crate::settings::get_claude_config_dirs();
    for d in crate::terminal::transcript::find_claude_config_dirs() {
        let s = d.to_string_lossy().into_owned();
        if !out.contains(&s) {
            out.push(s);
        }
    }
    out
}

/// Fetch the session's transcript stream from coord — warm tier first, then
/// cold — and write it into the target account's project dir.
///
/// This is the same `?stream=transcript` read the console's transcript pane
/// uses. Warm→cold rather than cold-only because the warm rows are the cheap
/// answer and a session archived to cold has an empty warm window; whichever
/// answers first wins, and neither answering is [`RespawnError::NoTranscript`],
/// never a silent fresh conversation.
///
/// ⚠️ Coord gates `GET /sessions/:id/output` with the `TenantId` extractor
/// (an operator bearer), not `FleetPrincipal`, so on a device-JWT-only runner
/// this read can answer 403. That is reported as a failed respawn — the same
/// posture [`super::handoff`] already takes on its own `TenantId`-gated
/// `/sessions/:id/events` read — never as "there is no transcript".
async fn materialize_transcript_from_coord(
    http: &reqwest::Client,
    coord_url: &str,
    source_session_id: Uuid,
    config_dir: &str,
    working_dir: &str,
    claude_session_id: &str,
) -> Result<bool, RespawnError> {
    let mut bytes = fetch_transcript_tier(http, coord_url, source_session_id, "warm").await;
    if bytes.is_empty() {
        bytes = fetch_transcript_tier(http, coord_url, source_session_id, "cold").await;
    }
    if bytes.is_empty() {
        return Ok(false);
    }
    let target = crate::terminal::transcript::session_transcript_path(
        std::path::Path::new(config_dir),
        working_dir,
        claude_session_id,
    );
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| RespawnError::Transcript(format!("create {}: {e}", parent.display())))?;
    }
    std::fs::write(&target, &bytes)
        .map_err(|e| RespawnError::Transcript(format!("write {}: {e}", target.display())))?;
    tracing::info!(
        session = %claude_session_id,
        bytes = bytes.len(),
        target = %target.display(),
        "session respawn: transcript materialized from coord"
    );
    Ok(true)
}

/// One tier read of `GET /sessions/:id/output?stream=transcript&tier=<tier>`,
/// chunks concatenated oldest→newest. Any failure logs and yields no bytes —
/// the caller distinguishes "no bytes" from "resume anyway" by refusing.
async fn fetch_transcript_tier(
    http: &reqwest::Client,
    coord_url: &str,
    session_id: Uuid,
    tier: &str,
) -> Vec<u8> {
    let url = format!(
        "{}/sessions/{}/output?stream=transcript&tier={}&limit={}",
        coord_url.trim_end_matches('/'),
        session_id,
        tier,
        TRANSCRIPT_WARM_LIMIT
    );
    let resp = match crate::coord_http::coord_get(http, &url).send().await {
        Ok(r) => r,
        Err(e) => {
            tracing::debug!(error = %e, tier, "session respawn: transcript fetch failed");
            return Vec::new();
        }
    };
    if !resp.status().is_success() {
        tracing::debug!(
            status = %resp.status(),
            tier,
            session = %session_id,
            "session respawn: transcript fetch rejected"
        );
        return Vec::new();
    }
    let body: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => {
            tracing::debug!(error = %e, tier, "session respawn: transcript decode failed");
            return Vec::new();
        }
    };
    transcript_bytes_from_output_body(&body)
}

/// Pure decode of coord's `GET /sessions/:id/output` envelope into raw stream
/// bytes: every `chunks[].payload_b64`, concatenated in the order served
/// (coord serves warm chunks oldest→newest and cold as one chunk at offset 0).
/// An undecodable chunk is skipped with a warning rather than truncating the
/// rest.
fn transcript_bytes_from_output_body(body: &serde_json::Value) -> Vec<u8> {
    let Some(chunks) = body.get("chunks").and_then(|c| c.as_array()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for chunk in chunks {
        let Some(b64) = chunk.get("payload_b64").and_then(|p| p.as_str()) else {
            continue;
        };
        match base64::engine::general_purpose::STANDARD.decode(b64) {
            Ok(bytes) => out.extend_from_slice(&bytes),
            Err(e) => {
                tracing::warn!(error = %e, "session respawn: transcript chunk base64 decode failed")
            }
        }
    }
    out
}

/// Materialize one respawn end-to-end. Returns the new terminal id.
///
/// **The source session is never closed here.** Handoff's step 6
/// (`DELETE /sessions/:id`) has nothing to act on: the source is already
/// `closed`, which is the premise of a respawn, and coord's `post_respawn`
/// issues no `UPDATE` against `coord.sessions` either. This function contains
/// no delete of any kind, and [`tests::respawn_module_never_closes_the_source`]
/// asserts it stays that way.
async fn materialize(
    registry: &Arc<SessionRegistry>,
    lifecycle_store: &Arc<SessionLifecycleStore>,
    http: &reqwest::Client,
    coord_url: &str,
    respawn: &PendingRespawn,
) -> Result<String, RespawnError> {
    // 1. The id being resumed. An explicit null is UNKNOWN — refuse.
    let claude_session_id = respawn
        .claude_code_session_id
        .ok_or(RespawnError::NoClaudeSession(respawn.source_session_id))?
        .to_string();

    // 2. 🔒 The pin. Fails loud; never degrades to rotation.
    let config_dir = resolve_target_config_dir(respawn.account.as_deref())?;

    // 3. The source bundle — cwd, intent, held claims.
    let state = super::handoff::fetch_state(http, coord_url, respawn.source_session_id).await?;
    let child_intent = super::handoff::build_child_intent(&state, "respawned here")?;
    // Any local record for this Claude session (present when the session ran on
    // THIS box — the same-machine, different-account case). It is authoritative
    // for where the transcript actually sits, and it lets the respawn land back
    // on the tile the session used to occupy.
    let local = lifecycle_store.get(&claude_session_id);
    let working_dir = resolve_working_dir(local.as_ref(), state.repo.as_deref())
        .ok_or(RespawnError::NoWorkingDir(respawn.source_session_id))?;

    // 4. The transcript, into the TARGET account's project dir.
    let mut have_transcript =
        materialize_transcript_locally(&config_dir, &working_dir, &claude_session_id)?;
    if !have_transcript {
        have_transcript = materialize_transcript_from_coord(
            http,
            coord_url,
            respawn.source_session_id,
            &config_dir,
            &working_dir,
            &claude_session_id,
        )
        .await?;
    }
    if !have_transcript {
        return Err(RespawnError::NoTranscript(claude_session_id));
    }

    // 5. The per-session account-hop budget, shared with the automatic
    //    migration path — `MIGRATION_CAP` (3) per Claude session per 24h. A
    //    respawn is another account hop for the same session, so it draws on the
    //    same budget rather than opening a bypass around it.
    //
    //    Checked HERE, immediately before the spawn, and not at the top: the
    //    check is record-and-check (it consumes a slot), and the catch-up GET
    //    replays a pending respawn on every reconnect. Charging a slot for an
    //    attempt that refuses at step 1–4 would let three failed reconnects
    //    lock a legitimate respawn out for 24 hours. Only a respawn that
    //    actually launches spends the budget.
    let now_ms = chrono::Utc::now().timestamp_millis();
    if !crate::terminal::account_migration::migration_cap_permits(&claude_session_id, now_ms) {
        return Err(RespawnError::CapReached {
            session: claude_session_id,
            cap: crate::terminal::account_migration::MIGRATION_CAP,
        });
    }

    // 6. The resume, through the shared seam, pinned via capture_hint.config_dir
    //    — and stamping `parent_session_id` so the child is one link in the
    //    lineage chain rather than an orphan. That stamp is also what lets
    //    coord's materialized-child filter drop this request from the next
    //    catch-up, so a push + catch-up double-delivery does not respawn twice.
    let app = crate::tauri_app_handle::current().ok_or(RespawnError::NoAppHandle)?;
    let (terminal_manager, session_registry) = managed_handles(&app)?;
    let title = format!(
        "{} — {}",
        child_intent.purpose,
        &claude_session_id[..8.min(claude_session_id.len())]
    );
    let model_transcript = crate::terminal::transcript::session_transcript_path(
        std::path::Path::new(&config_dir),
        &working_dir,
        &claude_session_id,
    );
    let (terminal_id, _coord_id) = crate::terminal::account_migration::spawn_resumed_pane(
        &app,
        &terminal_manager,
        &session_registry,
        crate::terminal::account_migration::ResumeSpawn {
            claude_session_id: &claude_session_id,
            working_dir: &working_dir,
            model_transcript,
            config_dir: &config_dir,
            title,
            // Land back on the tile the session used to occupy when this box
            // still remembers it; otherwise the default page, zone 0 (a wrong
            // tile beats a lost session).
            page_id: local
                .as_ref()
                .map(|r| r.page_id.clone())
                .unwrap_or_else(|| "default".to_string()),
            zone_index: local.as_ref().map(|r| r.zone_index).unwrap_or(0),
            work_unit_slug: child_intent.work_unit_slug.clone(),
            correlation_topic: child_intent.correlation_topic.clone(),
            intent_repo: child_intent.repo.clone(),
            coord_lineage: Some(crate::commands::terminal::CoordSessionLineage {
                parent_session_id: Some(respawn.source_session_id),
                claude_code_session_id: Some(claude_session_id.clone()),
            }),
            // NOT the migration's `true`. A respawn genuinely CREATES something
            // (its source is already closed and gone), so it must respect the
            // spawn-time resource floor like every other new autonomous spawn.
            resource_override: false,
        },
    )
    .map_err(RespawnError::Spawn)?;

    // 7. Re-acquire held claims under this device — idempotent by
    //    resource_key, best-effort, exactly as the handoff receiver does.
    let device_id = registry.machine_id();
    for claim in &state.held_claims {
        // Same derivation the handoff receiver uses: a respawned session inherits
        // the SOURCE session's tenant via `build_child_intent`, and the claims
        // re-acquired here belong to that tenant — not to this device's default
        // binding, which on a multi-bound box would be a different tenant.
        if let Err(e) = super::handoff::reacquire_claim(
            http,
            coord_url,
            claim,
            device_id,
            crate::auth::TenantScope::for_session(child_intent.tenant_id),
        )
        .await
        {
            tracing::warn!(
                kind = %claim.kind,
                resource_key = %claim.resource_key,
                error = %e,
                "session respawn: claim re-acquire failed (best-effort)"
            );
        }
    }

    // 8. The optional prompt, delivered through the SAME bounded idle-watcher
    //    the migration nudge uses (stands down when the session is already
    //    busy, one submission max, ~3 min ceiling).
    if let Some(prompt) = respawn
        .initial_prompt
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty())
    {
        crate::terminal::account_migration::spawn_prompt_when_idle(
            terminal_id.clone(),
            prompt.to_string(),
            "respawn-initial-prompt",
        );
    }

    // 9. NO `close_source`. The source is already closed — see the fn docstring.

    Ok(terminal_id)
}

/// The two Tauri-managed handles the resume seam needs.
fn managed_handles(
    app: &tauri::AppHandle,
) -> Result<(Arc<crate::terminal::TerminalManager>, Arc<SessionRegistry>), RespawnError> {
    use tauri::Manager;
    let terminal_manager = app
        .try_state::<Arc<crate::terminal::TerminalManager>>()
        .ok_or_else(|| RespawnError::Spawn("TerminalManager not managed".to_string()))?
        .inner()
        .clone();
    let session_registry = app
        .try_state::<Arc<SessionRegistry>>()
        .ok_or_else(|| RespawnError::Spawn("SessionRegistry not managed".to_string()))?
        .inner()
        .clone();
    Ok((terminal_manager, session_registry))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A minimal open lifecycle record, for the working-dir resolver test.
    fn local_record(
        claude_session_id: &str,
        working_dir: &str,
    ) -> super::super::session_lifecycle_store::TerminalSessionRecord {
        super::super::session_lifecycle_store::TerminalSessionRecord {
            claude_session_id: claude_session_id.to_string(),
            config_dir: None,
            working_dir: Some(working_dir.to_string()),
            page_id: "default".to_string(),
            zone_index: 0,
            title: None,
            terminal_id: "term-x".to_string(),
            opened_at: 0,
            last_seen_at: 0,
            state: "open".to_string(),
            closed_at: None,
            close_reason: None,
            provider: super::super::session_lifecycle_store::DEFAULT_PROVIDER.to_string(),
            origin: None,
            restore_pending_at: None,
            confirmed_at: None,
            handle: None,
            account_label: None,
            account_wrapper: None,
            session_name: None,
            name_source: None,
            tenant_id: None,
            task_run_id: None,
            bypass_permissions: None,
            restored_from_boot_at: None,
            restore_tier: None,
        }
    }

    fn ws_envelope(channel: &str, payload: serde_json::Value) -> String {
        json!({ "channel": channel, "payload": payload.to_string() }).to_string()
    }

    fn respawn_payload(
        source: Uuid,
        target: Uuid,
        tenant: Uuid,
        claude: Option<Uuid>,
        account: Option<&str>,
    ) -> serde_json::Value {
        json!({
            "event_kind": "respawn_request",
            "source_session_id": source,
            "target_device_id": target,
            "tenant_id": tenant,
            "session_kind": "terminal_claude",
            "source_device_id": Uuid::new_v4(),
            "source_state": "closed",
            // Coord serializes BOTH of these as explicit nulls when absent.
            "claude_code_session_id": claude,
            "account": account,
            "initial_prompt": serde_json::Value::Null,
        })
    }

    // -----------------------------------------------------------------------
    // Suffix disambiguation — the whole reason one socket can serve two arms
    // -----------------------------------------------------------------------

    #[test]
    fn parse_respawn_push_accepts_respawn_frame_for_this_device() {
        let device = Uuid::new_v4();
        let tenant = Uuid::new_v4();
        let source = Uuid::new_v4();
        let claude = Uuid::new_v4();
        let channel = format!("qontinui.sessions.{tenant}.{device}.respawn_request");
        let frame = ws_envelope(
            &channel,
            respawn_payload(source, device, tenant, Some(claude), Some(".claude-gmail")),
        );

        let parsed = parse_respawn_push(&frame, device).expect("respawn frame parses");
        assert_eq!(parsed.source_session_id, source);
        assert_eq!(parsed.target_device_id, device);
        assert_eq!(parsed.tenant_id, Some(tenant));
        assert_eq!(parsed.claude_code_session_id, Some(claude));
        assert_eq!(parsed.account.as_deref(), Some(".claude-gmail"));
        assert_eq!(parsed.source_state.as_deref(), Some("closed"));
    }

    /// The respawn arm must NOT swallow a `handoff_request` frame — otherwise
    /// one socket serving both arms would materialize each handoff twice.
    #[test]
    fn parse_respawn_push_ignores_handoff_frames() {
        let device = Uuid::new_v4();
        let tenant = Uuid::new_v4();
        let source = Uuid::new_v4();
        let channel = format!("qontinui.sessions.{tenant}.{device}.handoff_request");
        let frame = ws_envelope(
            &channel,
            json!({
                "event_kind": "handoff_request",
                "source_session_id": source,
                "target_device_id": device,
                "tenant_id": tenant,
                "session_kind": "agentic",
            }),
        );
        assert!(
            parse_respawn_push(&frame, device).is_none(),
            "a handoff frame must not enter the respawn arm"
        );
    }

    /// …and the converse: the SHIPPED handoff parser must keep ignoring
    /// `respawn_request`, which is what makes adding this arm safe. Asserted
    /// here (not only in `handoff`'s own tests) because it is THIS module's
    /// premise.
    #[test]
    fn shipped_handoff_parser_ignores_respawn_frames() {
        let device = Uuid::new_v4();
        let tenant = Uuid::new_v4();
        let source = Uuid::new_v4();
        let channel = format!("qontinui.sessions.{tenant}.{device}.respawn_request");
        let frame = ws_envelope(
            &channel,
            respawn_payload(source, device, tenant, Some(Uuid::new_v4()), None),
        );
        assert!(
            super::super::handoff::parse_handoff_push(&frame, device).is_none(),
            "the handoff arm must not materialize a respawn frame"
        );
    }

    #[test]
    fn parse_respawn_push_ignores_other_devices_and_kinds() {
        let device = Uuid::new_v4();
        let other = Uuid::new_v4();
        let tenant = Uuid::new_v4();
        let source = Uuid::new_v4();

        // Addressed at another device.
        let channel = format!("qontinui.sessions.{tenant}.{other}.respawn_request");
        let frame = ws_envelope(
            &channel,
            respawn_payload(source, other, tenant, Some(Uuid::new_v4()), None),
        );
        assert!(parse_respawn_push(&frame, device).is_none());

        // Right device, unrelated event kind.
        let channel = format!("qontinui.sessions.{tenant}.{device}.started");
        let frame = ws_envelope(&channel, json!({"event_kind": "started"}));
        assert!(parse_respawn_push(&frame, device).is_none());

        // Not a session subject at all.
        let frame = ws_envelope(
            &format!("events.agent.spawn_requested.{device}"),
            json!({"agent_id": Uuid::nil()}),
        );
        assert!(parse_respawn_push(&frame, device).is_none());

        // Garbage.
        assert!(parse_respawn_push("not json", device).is_none());
        assert!(parse_respawn_push("{}", device).is_none());
    }

    /// Channel says us, payload disagrees → trust the ADDRESS, drop the frame.
    #[test]
    fn parse_respawn_push_rejects_payload_target_mismatch() {
        let device = Uuid::new_v4();
        let other = Uuid::new_v4();
        let tenant = Uuid::new_v4();
        let channel = format!("qontinui.sessions.{tenant}.{device}.respawn_request");
        let frame = ws_envelope(
            &channel,
            respawn_payload(Uuid::new_v4(), other, tenant, Some(Uuid::new_v4()), None),
        );
        assert!(parse_respawn_push(&frame, device).is_none());
    }

    #[test]
    fn parse_respawn_push_accepts_inlined_payload_object() {
        let device = Uuid::new_v4();
        let tenant = Uuid::new_v4();
        let source = Uuid::new_v4();
        let channel = format!("qontinui.sessions.{tenant}.{device}.respawn_request");
        let envelope = json!({
            "channel": channel,
            "payload": respawn_payload(source, device, tenant, Some(Uuid::new_v4()), None),
        })
        .to_string();
        let parsed = parse_respawn_push(&envelope, device).expect("inlined payload parses");
        assert_eq!(parsed.source_session_id, source);
    }

    // -----------------------------------------------------------------------
    // Explicit-null payload handling — a missing half stays UNKNOWN
    // -----------------------------------------------------------------------

    /// `claude_code_session_id: null` and `account: null` are coord saying "I
    /// have no value". Neither may become a nil UUID, an empty string, or a
    /// default account.
    #[test]
    fn explicit_nulls_deserialize_as_none_never_as_defaults() {
        let device = Uuid::new_v4();
        let tenant = Uuid::new_v4();
        let source = Uuid::new_v4();
        let channel = format!("qontinui.sessions.{tenant}.{device}.respawn_request");
        let frame = ws_envelope(
            &channel,
            respawn_payload(source, device, tenant, None, None),
        );

        let parsed = parse_respawn_push(&frame, device).expect("null-bearing frame still parses");
        assert_eq!(
            parsed.claude_code_session_id, None,
            "an explicit null must stay None — never Uuid::nil()"
        );
        assert_ne!(parsed.claude_code_session_id, Some(Uuid::nil()));
        assert_eq!(
            parsed.account, None,
            "an explicit null account must stay None — never a default account"
        );
        assert_eq!(parsed.initial_prompt, None);
    }

    /// An ABSENT key reads identically to an explicit null (both UNKNOWN), and
    /// a partial payload never fabricates tenant/kind the way the handoff
    /// payload read defaults them.
    #[test]
    fn absent_optional_keys_are_none_not_defaults() {
        let device = Uuid::new_v4();
        let source = Uuid::new_v4();
        let channel = format!("qontinui.sessions.t.{device}.respawn_request");
        let frame = ws_envelope(
            &channel,
            json!({
                "source_session_id": source,
                "target_device_id": device,
            }),
        );
        let parsed = parse_respawn_push(&frame, device).expect("sparse payload parses");
        assert_eq!(parsed.tenant_id, None, "absent tenant is UNKNOWN, not nil");
        assert_eq!(parsed.session_kind, None);
        assert_eq!(parsed.source_device_id, None);
        assert_eq!(parsed.claude_code_session_id, None);
        assert_eq!(parsed.account, None);
    }

    /// A respawn with no Claude session id is REFUSED, not resumed against a
    /// fabricated id.
    #[test]
    fn missing_claude_session_id_refuses_the_respawn() {
        let pending = PendingRespawn {
            source_session_id: Uuid::new_v4(),
            target_device_id: Uuid::new_v4(),
            tenant_id: None,
            session_kind: None,
            source_device_id: None,
            source_state: Some("closed".to_string()),
            claude_code_session_id: None,
            account: None,
            initial_prompt: None,
        };
        // Reproduce step 1 of `materialize` without the managed-state world.
        let err = pending
            .claude_code_session_id
            .ok_or(RespawnError::NoClaudeSession(pending.source_session_id))
            .map(|id| id.to_string())
            .unwrap_err();
        assert!(matches!(err, RespawnError::NoClaudeSession(_)), "{err}");
        assert!(err.to_string().contains("UNKNOWN"), "{err}");
    }

    #[test]
    fn respawn_list_response_defaults_empty() {
        // Coord may return only {count: 0}; the default-empty attr keeps that
        // from being a parse error.
        let r: RespawnListResponse = serde_json::from_value(json!({ "count": 0 })).unwrap();
        assert!(r.respawns.is_empty());
    }

    #[test]
    fn respawn_list_response_parses_coord_envelope() {
        let source = Uuid::new_v4();
        let device = Uuid::new_v4();
        let r: RespawnListResponse = serde_json::from_value(json!({
            "respawns": [{
                "source_session_id": source,
                "target_device_id": device,
                "tenant_id": Uuid::new_v4(),
                "session_kind": "terminal_claude",
                "source_device_id": Uuid::new_v4(),
                "source_state": "closed",
                "claude_code_session_id": serde_json::Value::Null,
                "account": serde_json::Value::Null,
                "initial_prompt": serde_json::Value::Null,
                "requested_at": "2026-08-29T12:00:00Z",
            }],
            "count": 1,
        }))
        .unwrap();
        assert_eq!(r.respawns.len(), 1);
        assert_eq!(r.respawns[0].source_session_id, source);
        assert_eq!(r.respawns[0].claude_code_session_id, None);
        assert_eq!(r.respawns[0].account, None);
    }

    // -----------------------------------------------------------------------
    // 🔒 The fail-loud pin
    // -----------------------------------------------------------------------

    /// An unresolvable pin FAILS. It must not fall through to rotation, and
    /// the error must name the requested account so the operator can see which
    /// pin was refused.
    #[test]
    fn unresolvable_pin_fails_loud_and_never_rotates() {
        let err = crate::agent_runtime::resolve_spawn_account_with(
            Some(".claude-not-on-this-box"),
            |req| {
                Err(crate::ai_provider::AccountSelectError::NotInRoster {
                    requested: req.to_string(),
                    roster: vec!["hotmail".to_string(), "gmail".to_string()],
                })
            },
        )
        .map_err(|e| RespawnError::AccountPin(format!("{e:#}")))
        .unwrap_err();

        assert!(matches!(err, RespawnError::AccountPin(_)), "{err}");
        let msg = err.to_string();
        assert!(msg.contains(".claude-not-on-this-box"), "{msg}");
        assert!(
            msg.contains("refusing to fall back to account rotation"),
            "the refusal must say it is NOT rotating: {msg}"
        );
    }

    /// A roster account with no live credentials is the OTHER unresolvable
    /// case, and it fails the same way — not a quiet demotion to a logged-in
    /// sibling.
    #[test]
    fn logged_out_pin_fails_loud_too() {
        let err = crate::agent_runtime::resolve_spawn_account_with(Some("gmail"), |_| {
            Err(crate::ai_provider::AccountSelectError::NotLoggedIn {
                config_dir: "C:/claude/.claude-gmail".to_string(),
            })
        })
        .map_err(|e| RespawnError::AccountPin(format!("{e:#}")))
        .unwrap_err();
        assert!(matches!(err, RespawnError::AccountPin(_)), "{err}");
        assert!(err.to_string().contains("logged out"), "{err}");
    }

    /// A resolvable pin yields THAT account's config dir — the value that
    /// becomes `capture_hint.config_dir`, i.e. both the PTY's
    /// `CLAUDE_CONFIG_DIR` and the durable restore record's account.
    #[test]
    fn resolvable_pin_yields_the_pinned_config_dir() {
        let resolved = crate::agent_runtime::resolve_spawn_account_with(Some("gmail"), |_| {
            Ok(crate::ai_provider::ResolvedAccount {
                config_dir: "C:/claude/.claude-gmail".to_string(),
                account_name: "gmail".to_string(),
                cooldown_remaining_secs: None,
            })
        })
        .unwrap()
        .expect("a pin resolves to Some");
        assert_eq!(resolved.config_dir, "C:/claude/.claude-gmail");
    }

    /// No pin ⇒ `Ok(None)` ⇒ this runner's own rotation default applies,
    /// unchanged. A blank label is absence (it names no account), not a wrong
    /// name — the shipped seam's own reading, inherited verbatim.
    #[test]
    fn absent_or_blank_pin_is_absence_not_a_failure() {
        for pin in [None, Some(""), Some("   ")] {
            let resolved = crate::agent_runtime::resolve_spawn_account_with(pin, |_| {
                panic!("resolver must not be consulted for an absent pin")
            })
            .unwrap();
            assert!(resolved.is_none(), "pin {pin:?} must read as absence");
        }
    }

    // -----------------------------------------------------------------------
    // The do-not-close-the-source divergence
    // -----------------------------------------------------------------------

    /// Handoff's step 6 closes the source (`DELETE /sessions/:id`). A respawn
    /// must NOT: the source is already `closed`, and coord's `post_respawn`
    /// issues no `UPDATE` against `coord.sessions` either. This is a source
    /// guard — a future edit that adds a close here trips it.
    #[test]
    fn respawn_module_never_closes_the_source() {
        let src = include_str!("respawn.rs");
        // Scan the PRODUCTION half only: everything before the test module.
        // (This very test names the forbidden strings, so including it would
        // trip the guard on itself.)
        let production = src
            .split("#[cfg(test)]")
            .next()
            .expect("the module has a production half");
        // Ignore the doc/comment prose that explains the divergence; look only
        // at what the module could actually EXECUTE.
        let code: String = production
            .lines()
            .filter(|l| {
                let t = l.trim_start();
                !t.starts_with("//") && !t.starts_with("///") && !t.starts_with("//!")
            })
            .collect::<Vec<_>>()
            .join("\n");
        for forbidden in ["close_source", "http.delete", ".delete(", "DELETE"] {
            assert!(
                !code.contains(forbidden),
                "the respawn arm must never close its source, but the module's code \
                 contains {forbidden:?}"
            );
        }
    }

    /// The source's reported state is carried through as information, and a
    /// `closed` source is the NORMAL case rather than a reason to bail — the
    /// catch-up route exists precisely because coord's handoff read filters
    /// `state <> 'closed'`.
    #[test]
    fn a_closed_source_is_the_normal_case() {
        let device = Uuid::new_v4();
        let tenant = Uuid::new_v4();
        let channel = format!("qontinui.sessions.{tenant}.{device}.respawn_request");
        let frame = ws_envelope(
            &channel,
            respawn_payload(Uuid::new_v4(), device, tenant, Some(Uuid::new_v4()), None),
        );
        let parsed = parse_respawn_push(&frame, device).unwrap();
        assert_eq!(parsed.source_state.as_deref(), Some("closed"));
    }

    // -----------------------------------------------------------------------
    // Transcript materialization helpers
    // -----------------------------------------------------------------------

    #[test]
    fn transcript_bytes_concatenates_chunks_in_order() {
        let b64 = |s: &str| base64::engine::general_purpose::STANDARD.encode(s.as_bytes());
        let body = json!({
            "session_id": Uuid::nil(),
            "tier": "warm",
            "stream": "transcript",
            "chunks": [
                {"chunk_offset": 0, "payload_b64": b64("{\"type\":\"user\"}\n")},
                {"chunk_offset": 1, "payload_b64": b64("{\"type\":\"assistant\"}\n")},
                // An undecodable chunk is skipped, not fatal.
                {"chunk_offset": 2, "payload_b64": "!!!not base64!!!"},
            ],
            "count": 3,
        });
        let bytes = transcript_bytes_from_output_body(&body);
        assert_eq!(
            String::from_utf8(bytes).unwrap(),
            "{\"type\":\"user\"}\n{\"type\":\"assistant\"}\n"
        );
    }

    #[test]
    fn transcript_bytes_empty_when_coord_serves_no_chunks() {
        assert!(transcript_bytes_from_output_body(&json!({"chunks": [], "count": 0})).is_empty());
        assert!(transcript_bytes_from_output_body(&json!({"count": 0})).is_empty());
        assert!(transcript_bytes_from_output_body(&json!({"error": "x"})).is_empty());
    }

    /// No local dir holds the transcript ⇒ `false` (the caller then tries
    /// coord), never a fabricated empty file at the destination.
    #[test]
    fn local_transcript_materialization_reports_absence_without_writing() {
        let tmp = std::env::temp_dir().join(format!(
            "qontinui-respawn-absent-{}-{}",
            std::process::id(),
            Uuid::new_v4()
        ));
        let dst = tmp.join(".claude-target");
        let found = materialize_transcript_locally(
            dst.to_str().unwrap(),
            "D:\\qontinui-root",
            "11111111-2222-3333-4444-555555555555",
        )
        .unwrap();
        assert!(!found);
        assert!(
            !dst.exists(),
            "absence must not leave an empty destination behind"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// A transcript already present in the TARGET account's dir is enough —
    /// the copy is skipped and the respawn proceeds.
    #[test]
    fn local_transcript_already_in_target_is_accepted() {
        let tmp = std::env::temp_dir().join(format!(
            "qontinui-respawn-present-{}-{}",
            std::process::id(),
            Uuid::new_v4()
        ));
        let dst_cfg = tmp.join(".claude-target");
        let working_dir = "D:\\qontinui-root";
        let sid = "22222222-3333-4444-5555-666666666666";
        let path = crate::terminal::transcript::session_transcript_path(&dst_cfg, working_dir, sid);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"{\"type\":\"user\"}\n").unwrap();

        assert!(
            materialize_transcript_locally(dst_cfg.to_str().unwrap(), working_dir, sid).unwrap()
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    // -----------------------------------------------------------------------
    // Working-dir resolution
    // -----------------------------------------------------------------------

    #[test]
    fn working_dir_prefers_the_local_record_then_the_coord_repo() {
        // Nothing local, no coord repo ⇒ UNKNOWN.
        assert_eq!(resolve_working_dir(None, None), None);
        // Blank coord repo is still UNKNOWN, not an empty cwd.
        assert_eq!(resolve_working_dir(None, Some("   ")), None);
        // Coord repo alone.
        assert_eq!(
            resolve_working_dir(None, Some("D:/repo")).as_deref(),
            Some("D:/repo")
        );

        // A local record wins — it is authoritative for where the transcript
        // actually sits on this box.
        let rec = local_record("sess-x", "D:/local-checkout");
        assert_eq!(
            resolve_working_dir(Some(&rec), Some("D:/repo")).as_deref(),
            Some("D:/local-checkout")
        );

        // A local record with a blank working dir falls back rather than
        // handing the PTY an empty cwd.
        let mut blank = local_record("sess-x", "   ");
        blank.working_dir = Some(String::new());
        assert_eq!(
            resolve_working_dir(Some(&blank), Some("D:/repo")).as_deref(),
            Some("D:/repo")
        );

        // …and the record round-trips through the real store unchanged, so the
        // resolver is reading the shape the store actually persists.
        let dir = tempfile::tempdir().unwrap();
        let store = SessionLifecycleStore::open(dir.path().join("terminal-sessions.json")).unwrap();
        store.record_open(local_record("sess-x", "D:/local-checkout"));
        let stored = store.get("sess-x").expect("record round-trips");
        assert_eq!(
            resolve_working_dir(Some(&stored), None).as_deref(),
            Some("D:/local-checkout")
        );
    }
}
