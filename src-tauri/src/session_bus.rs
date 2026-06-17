//! Session Bus delivery executor (plan `2026-06-15-inter-session-session-bus.md`
//! Phase 3b). Polls coord for UNDELIVERED directed messages addressed to Claude
//! sessions THIS device hosts, injects each into its target's live terminal via
//! the runner's own `submit-prompt`, then marks it delivered so it is not
//! re-injected. Mirrors `fleet.rs::spawn_tree_publisher`.
//!
//! **Gated OFF by default** (`COORD_SESSION_BUS_ENABLED`): the executor is not
//! even spawned unless explicitly enabled, so this has ZERO live effect until an
//! operator opts a machine in. The coord side (the durable mailbox + the
//! device-authed pull/mark routes) is independently usable via MCP
//! (`coord_send_message` / `coord_inbox`); this executor is the *push*
//! automation on top of it.
//!
//! Delivery posture (first cut): inject-once per message (coord only returns
//! `delivered_at IS NULL` rows, and we mark delivered on success). A target
//! session that isn't live on this device is skipped and retried next tick — so
//! a closed target's mail simply waits (Phase 4 adds the resume/spawn wake).
//! Idle-only / re-surface-until-read refinement (plan items 4/10) is a
//! follow-up: today the message lands in the PTY input buffer and Claude reads
//! it on its next turn.

use std::time::Duration;

use serde::Deserialize;
use tracing::{debug, info, warn};

/// Default poll interval. Floored at 5s.
const DEFAULT_INTERVAL_SECS: u64 = 20;

/// The inter-session coordination preamble (Session Bus plan Phase 1), injected
/// into the initial prompt of every runner-SPAWNED session.
///
/// This is the **fleet-portable** home for the overlap/inbox/findings nudge: the
/// runner is the only component on every fleet host, so a preamble it splices
/// into the spawn prompt runs everywhere, unlike a `qontinui-claude-config`
/// SessionStart hook which fires ONLY on the operator's machine (memory
/// `feedback_fleet_wide_capabilities_belong_in_runner_not_claude_config`; same
/// non-portability that bit `wip_attribution`, re-homed in runner #589). The
/// claude-config hook remains a best-effort cover for operator-local *manual*
/// terminals; this preamble covers the fleet-spawned sessions it can't reach.
///
/// IMPORTANT — keep this preamble in lockstep with what is actually LIVE on
/// coord's deployed main. Because the runner can deploy to the fleet ahead of
/// coord (separate repos, separate deploy timing), the preamble must only name
/// coord MCP tools that exist on coord origin/main — otherwise every spawned
/// session is nudged to call tools that 404. As of 2026-06-17 the only
/// Session-Bus tool merged + deployed is `coord_declare_intent` (its coord-main
/// contract: derive likely globs server-side, record your scope so peers see
/// you, return any peers whose declared intent OVERLAPS yours). The richer
/// surface (`coord_who_is_working_on`, `coord_inbox`/`coord_ack_message`,
/// `coord_recent_findings`, `coord_post_finding`, `coord_send_message`) lives
/// on this same branch's unmerged coord counterpart (`feat/session-bus-phase01`,
/// Phases 1–4a). As each phase lands + deploys to coord main, fold its tool
/// into the preamble below — verify with `git grep '"coord_<tool>"'` against
/// coord origin/main `src/mcp/tools.rs` BEFORE adding it.
pub const COORDINATION_PREAMBLE: &str = "<system-reminder>Inter-session coordination (Session Bus): \
other Claude sessions may be working this repo concurrently. BEFORE you start editing files or \
writing a plan, call coord_declare_intent with a short free-text description of what you're about \
to work on. Coord derives the likely file globs server-side, records your scope so concurrent peers \
see you, and returns any peers whose declared intent overlaps yours — so you can coordinate or \
sequence around them instead of colliding at claim/merge time. This is advisory; the merge train \
remains the authoritative conflict gate.</system-reminder>";

// FULL-SURFACE preamble — DO NOT enable wholesale. As each Session-Bus phase
// lands + deploys to coord main, fold the matching step into COORDINATION_PREAMBLE
// above (and retire this note once the surface is fully shipped):
//   - coord_who_is_working_on(paths?/intent?)  [Phase 1] read-only "who else is on these files", {session_id, session_label}
//   - coord_inbox / coord_ack_message          [Phase 3a] pick up + ack directed peer messages
//   - coord_recent_findings(paths)             [Phase 2] learn from a peer's recent investigation before redoing it
//   - coord_post_finding(...)                  [Phase 2] publish an investigation worth sharing
//   - coord_send_message(session_id, ...)      [Phase 3a] reach a specific peer
// (The push-executor below still references coord_inbox/coord_ack_message in its
// own delivery-message strings; those are self-gating — they only fire when a
// message actually exists in the bus, i.e. once coord supports messaging.)

/// Prepend [`COORDINATION_PREAMBLE`] to a spawn prompt (blank line between), so
/// the spawned session reads the coordination nudge first, then its task.
pub fn prepend_coordination_preamble(prompt: &str) -> String {
    format!("{COORDINATION_PREAMBLE}\n\n{prompt}")
}

/// One pending message as returned by `GET /coord/session-messages/pending`.
/// Only the fields the executor needs are deserialized.
#[derive(Debug, Deserialize)]
struct PendingMessage {
    message_id: String,
    to_session: Option<String>,
    from_session: Option<String>,
    kind: String,
    priority: String,
    body: String,
}

#[derive(Debug, Deserialize)]
struct PendingResponse {
    messages: Vec<PendingMessage>,
}

/// Is the executor enabled? OFF unless `COORD_SESSION_BUS_ENABLED` is an
/// explicit truthy value — opt-in, the opposite default of the repo-pull
/// executor, because injecting into a live operator terminal is higher-touch.
fn enabled() -> bool {
    std::env::var("COORD_SESSION_BUS_ENABLED")
        .ok()
        .map(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

/// Resolve the coord HTTP base, or `None` when nothing is configured (skip the
/// tick cleanly rather than spam connection errors) — same posture as
/// `fleet::coord_http_base`.
fn coord_base() -> Option<String> {
    match qontinui_runner_lib::profiles::resolve_coord_base() {
        qontinui_runner_lib::profiles::CoordBase::Configured(base) => Some(base),
        _ => None,
    }
}

/// The runner's lifecycle store path (`~/.qontinui/runner/terminal-sessions.json`)
/// — the `claude_session_id -> terminal_id` map. Mirrors the boot path in
/// `main.rs`. (Instance-runner suffixed files are a follow-up; the primary
/// covers the common case.)
fn lifecycle_store_path() -> std::path::PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".qontinui")
        .join("runner")
        .join("terminal-sessions.json")
}

/// Spawn the periodic delivery executor on the ambient tokio runtime. A no-op
/// (logs + returns) when disabled.
pub fn spawn_session_bus_executor() {
    if !enabled() {
        debug!("session_bus: disabled (set COORD_SESSION_BUS_ENABLED=1 to enable)");
        return;
    }
    let secs: u64 = std::env::var("COORD_SESSION_BUS_INTERVAL_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_INTERVAL_SECS)
        .max(5);
    info!("session_bus: starting delivery executor, interval={secs}s");
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(secs));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tick.tick().await;
            if let Err(e) = deliver_once().await {
                warn!("session_bus: delivery tick failed: {e}");
            }
        }
    });
}

/// One delivery pass: pull pending → inject into live targets → mark delivered.
async fn deliver_once() -> anyhow::Result<()> {
    let Some(base) = coord_base() else {
        debug!("session_bus: no coord base configured — skipping tick");
        return Ok(());
    };
    let base = base.trim_end_matches('/').to_string();

    let token = crate::auth::AuthManager::new()
        .get_access_token()
        .map_err(|e| anyhow::anyhow!("no device JWT available: {e}"))?;

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()?;

    // 1. Pull undelivered messages for this device's sessions (device from JWT).
    let pending_url = format!("{base}/coord/session-messages/pending");
    let resp = client.get(&pending_url).bearer_auth(&token).send().await?;
    if !resp.status().is_success() {
        anyhow::bail!("pull {pending_url} -> HTTP {}", resp.status());
    }
    let pending: PendingResponse = resp.json().await?;
    if pending.messages.is_empty() {
        return Ok(());
    }

    // 2. Map target sessions to their live terminals.
    let store = crate::session::session_lifecycle_store::SessionLifecycleStore::open(
        lifecycle_store_path(),
    )?;
    let open = store.open_records();
    let self_base = crate::api_config::get_runner_api_url();
    let self_base = self_base.trim_end_matches('/');

    let mut delivered = 0usize;
    for msg in &pending.messages {
        let Some(to_session) = msg.to_session.as_deref() else {
            continue; // unresolved address (Phase 4) — not this executor's job
        };
        let Some(rec) = open.iter().find(|r| r.claude_session_id == to_session) else {
            // Target not live on this device. PASSIVE closed-session delivery
            // (plan Phase 4): leave the message pending — when that session is
            // next spawned/resumed, the runner's Session Bus spawn preamble
            // drives it to call coord_inbox and it pulls this message itself. We
            // deliberately do NOT proactively spawn/resume here: a fleet event
            // (e.g. a merge wave red-ing many PRs) could otherwise spawn a fleet,
            // so the PROACTIVE wake is deferred until it carries the fleet-wide
            // spawn-rate-cap from plan review items 6/19. A blocking message is
            // logged at info so the wait is observable meanwhile.
            if msg.priority == "blocking" {
                info!(
                    "session_bus: BLOCKING msg {} for session {to_session} which is not live on \
                     this device — pending until it next opens (preamble→coord_inbox delivers it)",
                    msg.message_id
                );
            } else {
                debug!(
                    "session_bus: msg {} target session {to_session} not live — pending \
                     (delivered on its next open)",
                    msg.message_id
                );
            }
            continue;
        };

        // 3. Inject into the target PTY via the runner's own submit-prompt. The
        // body is framed as a system-reminder so the recipient knows it's an
        // out-of-band inter-session message, not operator input.
        let from = msg
            .from_session
            .as_deref()
            .map(|f| format!(", from session {f}"))
            .unwrap_or_default();
        let framed = format!(
            "<system-reminder>Session Bus {} message ({} priority{from}). \
             Act on it, then coord_ack_message message_id={}. Message: {}</system-reminder>",
            msg.kind, msg.priority, msg.message_id, msg.body
        );
        let inject_url = format!("{self_base}/terminals/{}/submit-prompt", rec.terminal_id);
        match client
            .post(&inject_url)
            .json(&serde_json::json!({ "message": framed }))
            .send()
            .await
        {
            Ok(r) if r.status().is_success() => {
                // 4. Mark delivered so we don't re-inject.
                let mark_url = format!("{base}/coord/session-messages/mark-delivered");
                if let Err(e) = client
                    .post(&mark_url)
                    .bearer_auth(&token)
                    .json(&serde_json::json!({ "message_id": msg.message_id }))
                    .send()
                    .await
                {
                    warn!(
                        "session_bus: injected msg {} but mark-delivered failed: {e} \
                         (may re-inject next tick)",
                        msg.message_id
                    );
                } else {
                    delivered += 1;
                    info!(
                        "session_bus: delivered msg {} to session {to_session} (terminal {})",
                        msg.message_id, rec.terminal_id
                    );
                }
            }
            Ok(r) => warn!(
                "session_bus: submit-prompt HTTP {} for terminal {} (msg {})",
                r.status(),
                rec.terminal_id,
                msg.message_id
            ),
            Err(e) => warn!(
                "session_bus: submit-prompt failed for terminal {} (msg {}): {e}",
                rec.terminal_id, msg.message_id
            ),
        }
    }

    if delivered > 0 {
        debug!("session_bus: delivered {delivered} message(s) this tick");
    }
    Ok(())
}
