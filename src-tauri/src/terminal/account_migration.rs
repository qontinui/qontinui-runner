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
const MIGRATION_CAP: usize = 3;
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
fn migration_cap_permits(claude_session_id: &str, now_ms: i64) -> bool {
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
    crate::commands::ai_settings::refresh_account_usage_snapshot().await;
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
    // resolved dir so unrelated new sessions stop landing on it.
    crate::ai_provider::mark_account_rate_limited_with_duration(&src, EXHAUSTED_ACCOUNT_COOLDOWN);
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

/// Copy the session transcript from the source account's project dir into
/// the target account's matching project dir. The source file is never
/// modified or removed. Skips the copy when the destination already exists
/// and is at least as large (idempotent retry).
fn copy_transcript(
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

    // 3. Respawn under the target account. Same resume form the boot-restore
    // path uses (post-#547): bypassPermissions so the resumed session doesn't
    // wedge on its first tool-approval prompt with nobody watching.
    let title = record.title.clone().unwrap_or_else(|| {
        format!(
            "Resumed {}",
            &record.claude_session_id[..8.min(record.claude_session_id.len())]
        )
    });
    let command = vec![
        crate::agent_runtime::claude_bin_path(),
        "--permission-mode".to_string(),
        "bypassPermissions".to_string(),
        "--resume".to_string(),
        record.claude_session_id.clone(),
    ];
    let capture_hint = crate::commands::terminal::SessionCaptureHint {
        config_dir: Some(dst_config_dir.to_string()),
        working_dir: working_dir.clone(),
        title: title.clone(),
        page_id: Some(record.page_id.clone()),
        // `--resume <id>` names the exact session id → synchronous pinned
        // record (same row, new terminal/account/zone) + verification arm.
        claude_session_id: Some(record.claude_session_id.clone()),
        zone_index: Some(record.zone_index),
        // A migration respawn preserves the original session's nature (operator
        // or autonomous); don't force the agent identity onto an unknown session.
        inject_agent_git_identity: false,
    };
    let (new_terminal_id, _coord_id) =
        crate::commands::terminal::create_tracked_terminal_session_backend(
            &terminal_manager,
            &session_registry,
            app.clone(),
            title.clone(),
            working_dir.clone(),
            None,
            None,
            None,
            Some(command),
            None,
            capture_hint,
            Some(record.page_id.clone()),
        )?;

    // The lifecycle row was re-opened synchronously by the pinned-id capture
    // hint inside `create_terminal_session_backend` (`--resume <id>` keys the
    // SAME row: new terminal/account, preserved page/zone, origin "pinned").

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
