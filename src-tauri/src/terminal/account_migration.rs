//! Automatic cross-account migration of token-exhausted Claude terminal
//! sessions.
//!
//! ## Problem
//!
//! A long-running interactive Claude session pins the account it was launched
//! under (`CLAUDE_CONFIG_DIR`). When that account runs out of tokens the CLI
//! prints a usage-limit message and the session is stranded — the operator
//! has to manually re-open it under another account (the `/resume-foreign`
//! slash-command flow) even when a sibling account has plenty of headroom.
//!
//! ## Flow
//!
//! 1. **Hint** — [`super::usage_limit::UsageLimitWatchHook`] sees a
//!    usage-limit message in the PTY stream and calls
//!    [`handle_usage_limit_hint`] (debounced per terminal).
//! 2. **Confirm** — re-probe every configured account
//!    (`refresh_account_usage_snapshot`) and require the *probe* to agree the
//!    session's account is exhausted. Conversation text that merely quotes a
//!    limit message never migrates anything.
//! 3. **Pick target** — the non-exhausted, credential-valid, non-cooled
//!    account with the best weekly-usage headroom
//!    (`pick_migration_target` — same `(exhausted, headroom)` ranking the
//!    spawn-time picker uses, i.e. lowest used-vs-expected ratio wins).
//! 4. **Migrate** — copy the session transcript
//!    (`<src>/projects/<slug>/<sid>.jsonl` → same slug under the target dir;
//!    the source file is never touched), close the old pane
//!    (`close_reason: "account-migrated"`), and respawn
//!    `claude --permission-mode bypassPermissions --resume <sid>` in a fresh
//!    PTY pinned to the target account, on the same grid page/zone.
//!
//! Guards: per-session migration cap (no ping-pong when every account is
//! dry), settings kill-switch
//! (`claude_cli.auto_migrate_on_token_exhaustion`), and a no-op with fewer
//! than two configured accounts. The manual Tauri command
//! (`terminal_migrate_session_account`) reuses step 4 directly — an operator
//! click is its own confirmation.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

use serde_json::json;
use tauri::Emitter;
use tracing::{info, warn};

use crate::session::session_lifecycle_store::TerminalSessionRecord;

/// Close reason recorded on the old pane's lifecycle row. Deliberately NOT
/// `pty-exit`/`poll-dead`: the boot-restore path must not resurrect the old
/// pane — its replacement is already running under the new account.
pub const CLOSE_REASON_MIGRATED: &str = "account-migrated";

/// Max migrations per Claude session within [`MIGRATION_CAP_WINDOW_MS`].
/// A second hop is legitimate (the target can run dry too); an unbounded
/// chain means every account is exhausted and migrating is pure churn.
pub(crate) const MIGRATION_CAP: usize = 3;
const MIGRATION_CAP_WINDOW_MS: i64 = 24 * 60 * 60 * 1000;

/// Cooldown stamped on the exhausted source account so spawn-time selection
/// (`pick_best_account` / `rotate_account_on_rate_limit`) stays off it. One
/// hour — matches `EXHAUSTION_STALE_TTL`'s trust window for the probe's
/// `exhausted` flag; a weekly/5-hour cap doesn't clear in minutes.
const EXHAUSTED_ACCOUNT_COOLDOWN: std::time::Duration = std::time::Duration::from_secs(3600);

/// Per-session migration timestamps (unix millis), for the cap.
static MIGRATION_HISTORY: Mutex<Option<HashMap<String, Vec<i64>>>> = Mutex::new(None);

/// Record-and-check the migration cap for a session. Returns `false` when
/// the cap is exhausted (caller must not migrate).
pub(crate) fn migration_cap_permits(claude_session_id: &str, now_ms: i64) -> bool {
    let Ok(mut hist) = MIGRATION_HISTORY.lock() else {
        return false;
    };
    let map = hist.get_or_insert_with(HashMap::new);
    let entry = map.entry(claude_session_id.to_string()).or_default();
    entry.retain(|t| now_ms - *t <= MIGRATION_CAP_WINDOW_MS);
    if entry.len() >= MIGRATION_CAP {
        return false;
    }
    entry.push(now_ms);
    true
}

/// Outcome payload emitted on `session-account-migrated`.
#[derive(Debug)]
pub struct MigrationOutcome {
    pub new_terminal_id: String,
    pub from_config_dir: String,
    pub to_config_dir: String,
}

/// Entry point for a PTY usage-limit hint (spawned async off the reader
/// thread). Resolves everything it needs from the global app handle, mirrors
/// `agent_runtime`'s state-access pattern.
pub async fn handle_usage_limit_hint(terminal_id: String, matched_pattern: &'static str) {
    use tauri::Manager;

    let Some(app) = crate::tauri_app_handle::current() else {
        return;
    };
    let Some(store) = app.try_state::<std::sync::Arc<crate::session::session_lifecycle_store::SessionLifecycleStore>>() else {
        return;
    };

    // Only sessions the lifecycle registry knows about can migrate — a plain
    // shell pane (or an unregistered session) has no transcript binding.
    let Some(record) = store.find_open_by_terminal(&terminal_id) else {
        info!(
            terminal_id,
            matched_pattern,
            "usage-limit hint on a terminal with no registered Claude session — ignoring"
        );
        return;
    };

    // Source account: the dir the session launched under, else the runner's
    // currently-resolved global dir (pre-existing sessions registered before
    // config_dir capture).
    let Some(src) = record
        .config_dir
        .clone()
        .or_else(crate::ai_provider::get_resolved_config_dir)
    else {
        warn!(
            terminal_id,
            session = %record.claude_session_id,
            "usage-limit hint but the session's account is unknown — cannot migrate"
        );
        return;
    };

    let ai_settings = crate::settings::get_ai_settings();
    if !ai_settings.claude_cli.auto_migrate_on_token_exhaustion {
        info!(
            session = %record.claude_session_id,
            "usage-limit confirmed-candidate but auto-migration is disabled in settings"
        );
        return;
    }
    if crate::settings::get_claude_config_dirs().len() < 2 {
        return; // nothing to migrate to
    }

    // CONFIRM: a usage-limit *message* is only a hint (conversation text can
    // quote one; a resume repaint can re-render a historical one). Re-probe
    // and require the account to actually be exhausted before acting.
    let usage = crate::commands::ai_settings::refresh_account_usage_snapshot().await;
    if !crate::ai_provider::account_known_exhausted(&src) {
        info!(
            terminal_id,
            session = %record.claude_session_id,
            account = %src,
            matched_pattern,
            "usage-limit message NOT confirmed by probe — skipping (likely echoed text)"
        );
        return;
    }

    // Keep spawn-time selection off the dead account, and repoint the global
    // resolved dir so unrelated new sessions stop landing on it. When the
    // usage stats carry the 5-hour window's actual reset time, cool down
    // until then (clamped) instead of the blanket hour — the account comes
    // back into rotation the moment its session window rolls over.
    let cooldown = usage
        .iter()
        .find(|i| i.config_dir == src)
        .and_then(|i| i.session_resets_at)
        .and_then(|reset_ts| {
            let now = chrono::Utc::now().timestamp();
            let secs = (reset_ts as i64) - now;
            (secs > 0).then(|| std::time::Duration::from_secs((secs as u64).clamp(300, 6 * 3600)))
        })
        .unwrap_or(EXHAUSTED_ACCOUNT_COOLDOWN);
    crate::ai_provider::mark_account_rate_limited_with_duration(&src, cooldown);
    crate::ai_provider::pick_best_account();

    let now_ms = chrono::Utc::now().timestamp_millis();
    if !migration_cap_permits(&record.claude_session_id, now_ms) {
        warn!(
            session = %record.claude_session_id,
            cap = MIGRATION_CAP,
            "migration cap reached for this session — not migrating again"
        );
        emit_skipped(&app, &record, &src, "migration-cap-reached");
        return;
    }

    let Some(dst) = crate::ai_provider::pick_migration_target(&src) else {
        warn!(
            session = %record.claude_session_id,
            from = %src,
            "no usable migration target (every other account exhausted/cooled/unauthenticated)"
        );
        emit_skipped(&app, &record, &src, "no-usable-target");
        return;
    };

    info!(
        session = %record.claude_session_id,
        from = %src,
        to = %dst,
        matched_pattern,
        "token exhaustion confirmed — migrating session to a fresh account"
    );

    match migrate_session(&app, &record, &src, &dst) {
        Ok(outcome) => {
            info!(
                session = %record.claude_session_id,
                new_terminal = %outcome.new_terminal_id,
                from = %outcome.from_config_dir,
                to = %outcome.to_config_dir,
                "session migrated"
            );
        }
        Err(e) => {
            warn!(
                session = %record.claude_session_id,
                error = %e,
                "session migration failed"
            );
            emit_skipped(&app, &record, &src, &format!("migration-failed: {e}"));
        }
    }
}

/// The model the session was last actually running on, from the transcript's
/// most recent assistant turn (`.message.model`). `None` when the transcript
/// is unreadable or records no real model (synthetic error turns are
/// skipped) — the respawn then omits `--model` and the CLI default applies.
fn transcript_last_model(path: &Path) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    for line in content.lines().rev() {
        // Cheap pre-filter before parsing multi-KB JSONL lines.
        if !line.contains("\"assistant\"") || !line.contains("\"model\"") {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if v.get("type").and_then(|t| t.as_str()) != Some("assistant") {
            continue;
        }
        if let Some(m) = v
            .get("message")
            .and_then(|m| m.get("model"))
            .and_then(|m| m.as_str())
        {
            if m.starts_with("claude-") {
                return Some(m.to_string());
            }
        }
    }
    None
}

/// Text typed into the migrated session once the resumed CLI is idle.
const CONTINUE_NUDGE: &str = "This session was automatically migrated to a fresh Claude account \
after the previous account hit a usage limit. Continue the task you were working on from where \
it left off; if you were idle awaiting operator input, say so and wait.";

/// How the nudge watcher decides the resumed CLI is ready for input: the Ink
/// footer hint that is painted exactly when the input box is idle. Matched
/// against the normalized (lowercased, whitespace-collapsed) grid.
const READY_MARKER: &str = "? for shortcuts";
/// Painted while Claude is mid-turn — if visible, the session already picked
/// itself up (or the operator beat us to it) and no nudge is needed.
const BUSY_MARKER: &str = "esc to interrupt";

/// Watch the freshly-respawned terminal until the resumed CLI paints its idle
/// prompt, then submit [`CONTINUE_NUDGE`]. Detached and strictly best-effort:
/// bounded (~3 min), one submission max, and it stands down when the session
/// is already busy or the terminal goes away. Deliberately does NOT stand
/// down on a visible usage-limit message: a `--resume` repaint can re-render
/// the OLD account's historical banner (see the `usage_limit` module doc),
/// and a genuinely-dry target is caught by the grid scanner firing a fresh
/// migration hint on this terminal anyway.
fn spawn_continue_nudge(terminal_id: String) {
    spawn_prompt_when_idle(terminal_id, CONTINUE_NUDGE.to_string(), "continue-nudge");
}

/// The generalized idle-watcher behind [`spawn_continue_nudge`]: wait for the
/// resumed CLI to paint its idle prompt, then submit `prompt` once.
///
/// Extracted so the respawn receiver
/// ([`crate::session::respawn`]) can deliver coord's optional
/// `initial_prompt` through the SAME bounded, stand-down-on-busy watcher
/// instead of growing a second one that would drift from it. `label` only
/// names the caller in the log lines.
pub(crate) fn spawn_prompt_when_idle(terminal_id: String, prompt: String, label: &'static str) {
    use tauri::Manager;
    tauri::async_runtime::spawn(async move {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(180);
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            let Some(app) = crate::tauri_app_handle::current() else {
                return;
            };
            let Some(tm) = app.try_state::<std::sync::Arc<crate::terminal::TerminalManager>>()
            else {
                return;
            };
            let Some(session) = tm.get(&terminal_id) else {
                info!(
                    terminal_id,
                    label, "prompt-when-idle: terminal gone — standing down"
                );
                return;
            };
            let text = {
                let grid = session.grid();
                let guard = grid.lock().unwrap_or_else(|e| e.into_inner());
                guard.text_snapshot().text
            };
            let normalized = super::output_scan::normalize(&text);
            if normalized.contains(BUSY_MARKER) {
                info!(
                    terminal_id,
                    label, "prompt-when-idle: session already working — no prompt needed"
                );
                return;
            }
            if normalized.contains(READY_MARKER) {
                // Composes both sides of the 2026-08-29 rebase: OUR generalized
                // `prompt`/`label` seam (one impl for the migration nudge and the
                // respawn prompt), with MAIN's `Ok(_)` — `submit_prompt` stopped
                // returning `()` in fee48d4c, which now reports the neutralized body.
                match session.submit_prompt(&prompt) {
                    Ok(_) => info!(terminal_id, label, "prompt-when-idle submitted"),
                    Err(e) => {
                        warn!(terminal_id, label, error = %e, "prompt-when-idle submit failed")
                    }
                }
                return;
            }
            if std::time::Instant::now() >= deadline {
                warn!(
                    terminal_id,
                    label,
                    "prompt-when-idle: resumed CLI never painted its idle prompt within 3m — giving up"
                );
                return;
            }
        }
    });
}

/// Copy the session transcript from the source account's project dir into
/// the target account's matching project dir. The source file is never
/// modified or removed. Skips the copy when the destination already exists
/// and is at least as large (idempotent retry).
pub(crate) fn copy_transcript(
    src_config_dir: &str,
    dst_config_dir: &str,
    working_dir: &str,
    claude_session_id: &str,
) -> Result<std::path::PathBuf, String> {
    let src = super::transcript::session_transcript_path(
        Path::new(src_config_dir),
        working_dir,
        claude_session_id,
    );
    let dst = super::transcript::session_transcript_path(
        Path::new(dst_config_dir),
        working_dir,
        claude_session_id,
    );
    let src_meta = std::fs::metadata(&src)
        .map_err(|e| format!("source transcript missing at {}: {e}", src.display()))?;
    if let Ok(dst_meta) = std::fs::metadata(&dst) {
        if dst_meta.len() >= src_meta.len() {
            return Ok(dst); // already migrated (retry / repeated hint)
        }
    }
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("create {} failed: {e}", parent.display()))?;
    }
    std::fs::copy(&src, &dst)
        .map_err(|e| format!("copy {} -> {} failed: {e}", src.display(), dst.display()))?;
    Ok(dst)
}

/// One resumed-Claude PTY spawn, described.
///
/// The parameter block for [`spawn_resumed_pane`], which is the shipped
/// `claude --permission-mode bypassPermissions --resume <sid>` respawn — step 3
/// of [`migrate_session`], lifted out verbatim so the cross-machine respawn
/// receiver (`crate::session::respawn`) reuses it rather than growing a second,
/// drifting copy of the launch-spec + capture-hint wiring.
pub(crate) struct ResumeSpawn<'a> {
    /// The Claude session id to `--resume`. NEVER synthesized: a caller with
    /// no id must fail rather than invent one.
    pub claude_session_id: &'a str,
    /// The session's working dir — both the PTY cwd and the transcript
    /// resolver's project path.
    pub working_dir: &'a str,
    /// Transcript to sniff the session's last real model from, so `--resume`
    /// does not silently drop to the target account's default model. A missing
    /// or unreadable file simply omits `--model`.
    pub model_transcript: std::path::PathBuf,
    /// `CLAUDE_CONFIG_DIR` for the new PTY — the TARGET account's dir. It also
    /// keeps the durable restore record consistent (the record stores the dir
    /// the session actually launched under).
    pub config_dir: &'a str,
    pub title: String,
    pub page_id: String,
    pub zone_index: i32,
    /// Work unit the resumed session belongs to, carried onto the new coord
    /// session's intent. `None` on the migration path (which keeps the coord
    /// row it already had).
    pub work_unit_slug: Option<String>,
    /// Correlation topic carried onto the new coord session's intent.
    pub correlation_topic: Option<String>,
    /// `intent.repo` for the new coord session. `None` leaves it unset — the
    /// PTY cwd is `working_dir` either way.
    pub intent_repo: Option<String>,
    /// Coord lineage to stamp on the new coord session row, when this respawn
    /// continues a known coord session. `None` = no lineage claim (the
    /// account-migration path, whose coord row is the same session moving).
    pub coord_lineage: Option<crate::commands::terminal::CoordSessionLineage>,
    /// Forwarded to the spawn-time resource gate. See the two call sites — they
    /// answer it differently and each says why.
    pub resource_override: bool,
}

/// Spawn `claude --permission-mode bypassPermissions [--model m] --resume <sid>`
/// in a fresh PTY pinned to `spec.config_dir`, tracked in the lifecycle store
/// and mirrored into coord. Returns `(terminal_id, coord_session_id)`.
///
/// Same resume form the boot-restore path uses (post-#547): bypassPermissions
/// so the resumed session doesn't wedge on its first tool-approval prompt with
/// nobody watching.
///
/// The account pin is threaded through `capture_hint.config_dir`, which is BOTH
/// the `CLAUDE_CONFIG_DIR` handed to the PTY and the dir recorded on the durable
/// restore record — deliberately one value, so a restore cannot resurrect the
/// session under a different account than it ran on. It is never a
/// `switch_claude_account` mutation, which would leak this one spawn's account
/// choice into every later spawn on the box.
pub(crate) fn spawn_resumed_pane(
    app: &tauri::AppHandle,
    terminal_manager: &std::sync::Arc<crate::terminal::TerminalManager>,
    session_registry: &std::sync::Arc<crate::session::SessionRegistry>,
    spec: ResumeSpawn<'_>,
) -> Result<(String, Option<uuid::Uuid>), String> {
    let model = transcript_last_model(&spec.model_transcript);
    // Compose the respawn argv through the shared launch seam so the operator's
    // global + per-account (destination) launch flags layer onto the required
    // resume flags. `permission = BypassPermissions` preserves today's emitted
    // `--permission-mode bypassPermissions`. The #782 transcript-sniffed model is
    // fed as `spec.model`, so it wins over any template `--model` and the session
    // keeps its actual model across the hop. `resume_id` names the exact session
    // id. With no operator config the argv is the historical hand-built
    // `claude --permission-mode bypassPermissions [--model m] --resume <id>`
    // plus the `--settings <hook file>` pair below.
    let launch_cfg =
        crate::claude_session::launch_spec::LaunchConfig::from_settings(Some(spec.config_dir));
    let command = crate::claude_session::launch_spec::render_argv(
        &crate::claude_session::launch_spec::LaunchSpec {
            permission: crate::claude_session::launch_spec::PermissionMode::BypassPermissions,
            resume_id: Some(spec.claude_session_id.to_string()),
            model,
            // The hook carrier, spelled out because this respawn execs the
            // resolved `claude_bin_path()` DIRECTLY — the identity shim, which
            // is what appends `--settings` for a PATH-resolved `claude`, is not
            // in this chain. Without it the migrated session runs with no
            // `SessionStart` hook, and `SessionStart` on a `--resume` is exactly
            // when the policy injection matters most: the session carries its
            // old context but not the policies as they now stand
            // ([`crate::mcp::policy_context`]). Empty on a materialize failure
            // ⇒ no flag, which is the pre-existing behaviour.
            extra_required: crate::session::claude_hook::direct_spawn_settings_args(),
            ..Default::default()
        },
        &launch_cfg,
        &crate::agent_runtime::claude_bin_path(),
    );
    let capture_hint = crate::commands::terminal::SessionCaptureHint {
        config_dir: Some(spec.config_dir.to_string()),
        working_dir: spec.working_dir.to_string(),
        title: spec.title.clone(),
        page_id: Some(spec.page_id.clone()),
        // `--resume <id>` names the exact session id → synchronous pinned
        // record (same row, new terminal/account/zone) + verification arm.
        claude_session_id: Some(spec.claude_session_id.to_string()),
        zone_index: Some(spec.zone_index),
        // A resume preserves the original session's nature (operator or
        // autonomous); don't force the agent identity onto an unknown session.
        inject_agent_git_identity: false,
        // A respawn does not re-claim the original spawn's gate continuation
        // (the consume claim was taken once, by the original spawn), so it must
        // not inherit a gate identity it cannot honestly speak for.
        gate_identity: None,
        coord_lineage: spec.coord_lineage,
    };
    crate::commands::terminal::create_tracked_terminal_session_backend(
        terminal_manager,
        session_registry,
        app.clone(),
        spec.title,
        spec.working_dir.to_string(),
        spec.work_unit_slug,
        spec.correlation_topic,
        spec.intent_repo,
        Some(command),
        None,
        capture_hint,
        Some(spec.page_id),
        spec.resource_override,
    )
}

/// Perform the migration mechanics: transcript copy → close old pane →
/// respawn under the target account → re-record lifecycle row → emit event.
///
/// Shared by the automatic path ([`handle_usage_limit_hint`], which has
/// already confirmed exhaustion) and the manual Tauri command
/// (`terminal_migrate_session_account`, where the operator's click is the
/// confirmation).
pub fn migrate_session(
    app: &tauri::AppHandle,
    record: &TerminalSessionRecord,
    src_config_dir: &str,
    dst_config_dir: &str,
) -> Result<MigrationOutcome, String> {
    use tauri::Manager;

    let working_dir = record.working_dir.clone().ok_or_else(|| {
        "session record has no working_dir — cannot locate transcript".to_string()
    })?;

    let terminal_manager = app
        .try_state::<std::sync::Arc<crate::terminal::TerminalManager>>()
        .ok_or("TerminalManager not managed")?
        .inner()
        .clone();
    let session_registry = app
        .try_state::<std::sync::Arc<crate::session::SessionRegistry>>()
        .ok_or("SessionRegistry not managed")?
        .inner()
        .clone();
    let store = app
        .try_state::<std::sync::Arc<crate::session::session_lifecycle_store::SessionLifecycleStore>>()
        .ok_or("SessionLifecycleStore not managed")?
        .inner()
        .clone();

    // 1. Transcript first — if this fails the old pane is left untouched.
    copy_transcript(
        src_config_dir,
        dst_config_dir,
        &working_dir,
        &record.claude_session_id,
    )?;

    // 2. Close the old pane. Record the migration close-reason BEFORE the
    // PTY teardown so the exit hook's later `pty-exit` close is a no-op
    // (record_close ignores already-closed rows) and boot-restore never
    // resurrects the stranded pane.
    store.record_close(&record.claude_session_id, CLOSE_REASON_MIGRATED);
    if let Err(e) = terminal_manager.close(&record.terminal_id) {
        // Old pane may already be gone (user closed it after exhaustion) —
        // not fatal, the respawn is what matters.
        info!(
            terminal_id = %record.terminal_id,
            error = %e,
            "old terminal close failed (continuing with respawn)"
        );
    }

    // 3. Respawn under the target account, through the SHARED resume seam
    // ([`spawn_resumed_pane`]) — the same code path the respawn receiver
    // (`crate::session::respawn`) uses, so the two never drift.
    let title = record.title.clone().unwrap_or_else(|| {
        format!(
            "Resumed {}",
            &record.claude_session_id[..8.min(record.claude_session_id.len())]
        )
    });
    let new_terminal_id = spawn_resumed_pane(
        app,
        &terminal_manager,
        &session_registry,
        ResumeSpawn {
            claude_session_id: &record.claude_session_id,
            working_dir: &working_dir,
            // Model is sniffed off the SOURCE transcript: `--resume` alone
            // restores the conversation but the model falls back to the target
            // account's default.
            model_transcript: super::transcript::session_transcript_path(
                Path::new(src_config_dir),
                &working_dir,
                &record.claude_session_id,
            ),
            config_dir: dst_config_dir,
            title: title.clone(),
            page_id: record.page_id.clone(),
            zone_index: record.zone_index,
            // The migration keeps the coord row it already had — it claims no
            // new work-unit / topic / repo, and no lineage.
            work_unit_slug: None,
            correlation_topic: None,
            intent_repo: None,
            coord_lineage: None,
            // OVERRIDE — the one call site that passes `true`, and the only one
            // that should. An account migration is not the creation of a new
            // session: the operator's session already existed, this function has
            // ALREADY torn down its old PTY, and this respawn is the second half
            // of a move that is mid-flight. Refusing here would not protect a
            // live session — it would destroy one, which inverts the whole point
            // of the guard (plan §Part D step 5: never touch an already-running
            // session; this gate fires only when something NEW is created). The
            // migration is also commit-neutral by construction: one `claude`
            // goes away, one comes back. A RESPAWN, by contrast, creates
            // something genuinely new (its source is already closed) and passes
            // `false`.
            resource_override: true,
        },
    )?
    .0;

    // The lifecycle row was re-opened synchronously by the pinned-id capture
    // hint inside `create_terminal_session_backend` (`--resume <id>` keys the
    // SAME row: new terminal/account, preserved page/zone, origin "pinned").

    // 4. Optionally nudge the resumed session to pick the task back up once
    // the CLI paints its idle prompt — a bare `--resume` restores context but
    // then sits at the input box with nobody typing.
    if crate::settings::get_ai_settings()
        .claude_cli
        .auto_continue_after_migration
    {
        spawn_continue_nudge(new_terminal_id.clone());
    }

    let outcome = MigrationOutcome {
        new_terminal_id: new_terminal_id.clone(),
        from_config_dir: src_config_dir.to_string(),
        to_config_dir: dst_config_dir.to_string(),
    };
    if let Err(e) = app.emit(
        "session-account-migrated",
        json!({
            "claudeSessionId": record.claude_session_id,
            "fromConfigDir": outcome.from_config_dir,
            "toConfigDir": outcome.to_config_dir,
            "newTerminalId": outcome.new_terminal_id,
            "pageId": record.page_id,
            "zoneIndex": record.zone_index,
        }),
    ) {
        warn!(error = %e, "failed to emit session-account-migrated");
    }
    Ok(outcome)
}

fn emit_skipped(app: &tauri::AppHandle, record: &TerminalSessionRecord, src: &str, reason: &str) {
    if let Err(e) = app.emit(
        "session-account-migration-skipped",
        json!({
            "claudeSessionId": record.claude_session_id,
            "fromConfigDir": src,
            "reason": reason,
        }),
    ) {
        warn!(error = %e, "failed to emit session-account-migration-skipped");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migration_cap_allows_then_blocks() {
        let sid = "test/account_migration/cap-session";
        let base = 1_750_000_000_000i64;
        assert!(migration_cap_permits(sid, base));
        assert!(migration_cap_permits(sid, base + 1000));
        assert!(migration_cap_permits(sid, base + 2000));
        assert!(
            !migration_cap_permits(sid, base + 3000),
            "4th migration within the window must be blocked"
        );
        // Outside the 24h window the old entries age out.
        assert!(migration_cap_permits(
            sid,
            base + MIGRATION_CAP_WINDOW_MS + 4000
        ));
    }

    #[test]
    fn migration_cap_is_per_session() {
        let base = 1_750_000_000_000i64;
        for i in 0..MIGRATION_CAP {
            assert!(migration_cap_permits("test/cap/a", base + i as i64));
        }
        assert!(!migration_cap_permits("test/cap/a", base + 10));
        assert!(
            migration_cap_permits("test/cap/b", base + 10),
            "an unrelated session is not capped"
        );
    }

    #[test]
    fn copy_transcript_copies_and_is_idempotent() {
        let tmp =
            std::env::temp_dir().join(format!("qontinui-acctmig-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let src_cfg = tmp.join(".claude-hotmail");
        let dst_cfg = tmp.join(".claude-gmail");
        let working_dir = "D:\\qontinui-root";
        let sid = "11111111-2222-3333-4444-555555555555";

        let src_path =
            crate::terminal::transcript::session_transcript_path(&src_cfg, working_dir, sid);
        std::fs::create_dir_all(src_path.parent().unwrap()).unwrap();
        std::fs::write(&src_path, b"{\"type\":\"user\"}\n").unwrap();

        let dst = copy_transcript(
            src_cfg.to_str().unwrap(),
            dst_cfg.to_str().unwrap(),
            working_dir,
            sid,
        )
        .expect("copy should succeed");
        assert!(dst.exists());
        assert_eq!(
            std::fs::read(&dst).unwrap(),
            std::fs::read(&src_path).unwrap()
        );
        // Source untouched.
        assert!(src_path.exists());

        // Second call (repeated hint / retry) is a no-op success.
        let dst2 = copy_transcript(
            src_cfg.to_str().unwrap(),
            dst_cfg.to_str().unwrap(),
            working_dir,
            sid,
        )
        .expect("idempotent retry should succeed");
        assert_eq!(dst, dst2);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn transcript_last_model_prefers_latest_real_model() {
        let tmp =
            std::env::temp_dir().join(format!("qontinui-acctmig-model-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let path = tmp.join("t.jsonl");
        std::fs::write(
            &path,
            concat!(
                "{\"type\":\"user\",\"message\":{\"content\":\"hi\"}}\n",
                "{\"type\":\"assistant\",\"message\":{\"model\":\"claude-opus-4-8\",\"content\":[]}}\n",
                "{\"type\":\"assistant\",\"message\":{\"model\":\"claude-fable-5\",\"content\":[]}}\n",
                "{\"type\":\"assistant\",\"message\":{\"model\":\"<synthetic>\",\"content\":[]}}\n",
                "{\"type\":\"user\",\"message\":{\"content\":\"quoted \\\"assistant\\\" \\\"model\\\" text\"}}\n",
            ),
        )
        .unwrap();
        assert_eq!(
            transcript_last_model(&path).as_deref(),
            Some("claude-fable-5"),
            "synthetic error turns and quoting user turns must be skipped"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn transcript_last_model_missing_file_is_none() {
        assert!(transcript_last_model(Path::new("Z:/nope/never.jsonl")).is_none());
    }

    #[test]
    fn copy_transcript_missing_source_errors() {
        let tmp =
            std::env::temp_dir().join(format!("qontinui-acctmig-missing-{}", std::process::id()));
        let err = copy_transcript(
            tmp.join("nope-src").to_str().unwrap(),
            tmp.join("nope-dst").to_str().unwrap(),
            "D:\\qontinui-root",
            "00000000-0000-0000-0000-000000000000",
        )
        .unwrap_err();
        assert!(err.contains("source transcript missing"), "got: {err}");
    }
}
