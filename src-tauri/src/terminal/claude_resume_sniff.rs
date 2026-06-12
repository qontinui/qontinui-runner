//! Phase 2 of `plans/2026-06-12-runner-session-registry-and-restore-hardening.md`
//! — typed `claude --resume <id>` / `claude --session-id <id>` sniff (issue #548).
//!
//! Every restore surface TYPES such a line into a plain shell, so a backend
//! observer on completed typed input lines can register the session in the
//! durable lifecycle store deterministically — no frontend hook, no
//! transcript-mtime guessing. The same line carries the bypass-permissions
//! form, so the typed path also emits the `terminal-bypass-permissions`
//! event the spawn-argv sniff in `TerminalManager::create` already emits,
//! closing the approval-phantom gap for restored tabs.
//!
//! Consumes the SAME completed-line stream as the L3 git-warn observer
//! (`TerminalSession::observe_input`). Best-effort and SOFT throughout: never
//! blocks input; all effects dispatch on a detached async task.

use std::sync::Arc;

use tauri::{AppHandle, Emitter, Manager};
use tracing::{debug, info, warn};

use crate::session::session_lifecycle_store::{SessionLifecycleStore, TerminalSessionRecord};

/// A typed input line recognized as a `claude` invocation carrying an
/// explicit session id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypedClaudeResume {
    /// The session id extracted from `--resume <id>` / `--session-id <id>`.
    pub claude_session_id: String,
    /// Whether the line also implies bypassed tool permissions (per
    /// [`crate::terminal::manager::command_implies_bypass_permissions`]).
    pub bypass_permissions: bool,
}

/// Parse a completed typed input line as `claude … (--resume <id> |
/// --session-id <id>)`.
///
/// Heuristic, mirroring the L3 git-warn matcher's posture (soft, never a
/// shell parser): the line is split into command segments on `;`, `&`, `|`
/// (so `cd x && claude --resume …` and `$env:FOO="y"; claude …` both work);
/// within a segment, leading env-assignment tokens (containing `=`) are
/// skipped and the first real token must be the `claude` program. The id
/// must be a strict UUID (what every restore surface types), so prose
/// containing "claude" can't false-positive. A bare `claude --resume`
/// (interactive picker form) has no id and does NOT match.
pub fn parse_typed_claude_resume(line: &str) -> Option<TypedClaudeResume> {
    for segment in line.split([';', '&', '|']) {
        let mut tokens = segment.split_whitespace();
        // Skip leading env-assignment prefixes (`FOO=bar`, `$env:FOO="bar"`).
        // A segment that is ONLY assignments is skipped, not a parse failure.
        let Some(program) = tokens.by_ref().find(|t| !t.contains('=')) else {
            continue;
        };
        if !is_claude_program(program) {
            continue;
        }
        let rest: Vec<&str> = tokens.collect();
        for (i, t) in rest.iter().enumerate() {
            let candidate = if *t == "--resume" || *t == "--session-id" {
                rest.get(i + 1).copied()
            } else {
                t.strip_prefix("--resume=")
                    .or_else(|| t.strip_prefix("--session-id="))
            };
            let Some(cand) = candidate else { continue };
            let cand = cand.trim_matches(|c| c == '"' || c == '\'');
            if is_session_uuid(cand) {
                return Some(TypedClaudeResume {
                    claude_session_id: cand.to_ascii_lowercase(),
                    bypass_permissions:
                        crate::terminal::manager::command_implies_bypass_permissions(&[
                            line.to_string()
                        ]),
                });
            }
        }
    }
    None
}

/// Whether `token` names the `claude` CLI — bare, extensioned, or
/// path-qualified (`/usr/local/bin/claude`, `C:\…\claude.exe`).
fn is_claude_program(token: &str) -> bool {
    let t = token.trim_matches(|c| c == '"' || c == '\'');
    let base = t.rsplit(['/', '\\']).next().unwrap_or(t);
    matches!(base, "claude" | "claude.exe" | "claude.cmd" | "claude.ps1")
}

/// Strict 8-4-4-4-12 hex UUID check — the only id shape `claude --resume` /
/// `--session-id` accepts, and the shape every restore surface types.
fn is_session_uuid(s: &str) -> bool {
    s.len() == 36
        && s.bytes().enumerate().all(|(i, b)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                b == b'-'
            } else {
                b.is_ascii_hexdigit()
            }
        })
}

/// Apply the two effects of a recognized typed resume line: (1) `record_open`
/// the session in the lifecycle store — a NEW writer arm beside the existing
/// frontend-command / continuation-poller writers (structural dedup by
/// session id means a frontend re-record simply refreshes this row); (2) for
/// the bypass form, invoke `emit_bypass` (production emits
/// `terminal-bypass-permissions`). Split out from
/// [`spawn_register_typed_resume`] so the pair is testable without a Tauri app.
pub(crate) fn apply_typed_resume_effects(
    store: Option<&SessionLifecycleStore>,
    parsed: &TypedClaudeResume,
    terminal_id: &str,
    working_dir: &str,
    page_id: &str,
    title: &str,
    emit_bypass: impl FnOnce(),
) {
    match store {
        Some(store) => {
            store.record_open(TerminalSessionRecord {
                claude_session_id: parsed.claude_session_id.clone(),
                // config_dir None → restore scans every known config dir.
                // Zone unknown backend-side → 0 (wrong zone beats a lost
                // session). Timestamps are placeholders record_open seeds.
                config_dir: None,
                working_dir: Some(working_dir.to_string()),
                page_id: page_id.to_string(),
                zone_index: 0,
                title: Some(title.to_string()),
                terminal_id: terminal_id.to_string(),
                opened_at: 0,
                last_seen_at: 0,
                state: "open".to_string(),
                closed_at: None,
                close_reason: None,
                // Exact id lifted from the typed `--resume`/`--session-id`
                // flag — a pinned bind, same as the spawn-argv pre-pin path.
                bind_origin: Some("pinned".to_string()),
                restore_pending_at: None,
            });
            info!(
                terminal_id = %terminal_id,
                claude_session = %parsed.claude_session_id,
                bypass = parsed.bypass_permissions,
                "typed claude resume sniff: session durably recorded"
            );
        }
        None => warn!(
            terminal_id = %terminal_id,
            claude_session = %parsed.claude_session_id,
            "typed claude resume sniff: lifecycle store not managed — session not recorded"
        ),
    }
    if parsed.bypass_permissions {
        emit_bypass();
    }
}

/// Dispatch the effects of a recognized typed resume line off the PTY write
/// hot path. Mirrors `coord_warn::spawn_check_and_warn`: a detached async
/// task, all failures swallowed. `tauri::async_runtime::spawn` (not bare
/// `tokio::spawn`) so dispatch works from any caller thread —
/// `TerminalSession::write` is reachable from bare OS threads.
pub fn spawn_register_typed_resume(
    app_handle: AppHandle,
    terminal_id: String,
    working_dir: String,
    page_id: String,
    title: String,
    parsed: TypedClaudeResume,
) {
    tauri::async_runtime::spawn(async move {
        let store = app_handle
            .try_state::<Arc<SessionLifecycleStore>>()
            .map(|s| s.inner().clone());
        let emit_handle = app_handle.clone();
        let emit_terminal_id = terminal_id.clone();
        apply_typed_resume_effects(
            store.as_deref(),
            &parsed,
            &terminal_id,
            &working_dir,
            &page_id,
            &title,
            move || {
                if let Err(e) = emit_handle.emit(
                    "terminal-bypass-permissions",
                    serde_json::json!({ "id": emit_terminal_id }),
                ) {
                    debug!(
                        terminal_id = %emit_terminal_id,
                        error = %e,
                        "typed claude resume sniff: bypass event emit failed"
                    );
                }
            },
        );
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use tempfile::tempdir;

    const UUID: &str = "0d5e9a8c-1111-2222-3333-444455556666";

    fn parse(line: &str) -> Option<TypedClaudeResume> {
        parse_typed_claude_resume(line)
    }

    #[test]
    fn parses_resume_and_session_id_forms() {
        for line in [
            format!("claude --resume {UUID}"),
            format!("claude --session-id {UUID}"),
            format!("claude --resume={UUID}"),
            format!("claude --session-id={UUID}"),
        ] {
            let parsed = parse(&line).unwrap_or_else(|| panic!("must parse: {line}"));
            assert_eq!(parsed.claude_session_id, UUID, "id from: {line}");
            assert!(!parsed.bypass_permissions, "no bypass in: {line}");
        }
    }

    #[test]
    fn detects_bypass_forms() {
        let p = parse(&format!(
            "claude --resume {UUID} --permission-mode bypassPermissions"
        ))
        .expect("bypass form must parse");
        assert_eq!(p.claude_session_id, UUID);
        assert!(p.bypass_permissions);
        let p = parse(&format!(
            "claude --dangerously-skip-permissions --resume {UUID}"
        ))
        .expect("dangerously-skip form must parse");
        assert!(p.bypass_permissions);
    }

    #[test]
    fn parses_env_prefix_chain_and_path_qualified_forms() {
        for line in [
            format!("CLAUDE_CONFIG_DIR=C:/cfg claude --resume {UUID}"),
            format!("$env:CLAUDE_CONFIG_DIR=\"C:/cfg\"; claude --resume {UUID}"),
            format!("cd /d/repo && claude --resume {UUID}"),
            format!("C:\\bin\\claude.exe --resume {UUID}"),
        ] {
            assert!(parse(&line).is_some(), "must parse: {line}");
        }
    }

    #[test]
    fn rejects_non_claude_lines_and_idless_forms() {
        for line in [
            "git status".to_string(),
            "ls -la".to_string(),
            format!("echo claude --resume {UUID}"), // "claude" not in program position
            "claude \"do the thing\"".to_string(),  // no resume/session-id flag
            "claude --resume".to_string(),          // bare picker form, no id
            "claude --resume --permission-mode bypassPermissions".to_string(),
            "claude --resume not-a-uuid".to_string(),
            String::new(),
        ] {
            assert!(parse(&line).is_none(), "must NOT parse: {line:?}");
        }
    }

    #[test]
    fn apply_effects_records_open_and_emits_bypass() {
        // A typed bypass resume line must produce BOTH a record_open effect
        // and a bypass-event effect.
        let dir = tempdir().unwrap();
        let store = SessionLifecycleStore::open(dir.path().join("terminal-sessions.json")).unwrap();
        let parsed = parse(&format!(
            "claude --resume {UUID} --permission-mode bypassPermissions"
        ))
        .unwrap();

        let bypass_emitted = Cell::new(false);
        apply_typed_resume_effects(
            Some(&store),
            &parsed,
            "term-1",
            "C:/repo",
            "default",
            "Terminal 1",
            || bypass_emitted.set(true),
        );

        let open = store.open_records();
        assert_eq!(open.len(), 1, "record_open effect must land");
        assert_eq!(open[0].claude_session_id, UUID);
        assert_eq!(open[0].terminal_id, "term-1");
        assert_eq!(open[0].working_dir.as_deref(), Some("C:/repo"));
        assert_eq!(open[0].page_id, "default");
        assert_eq!(open[0].state, "open");
        assert!(bypass_emitted.get(), "bypass event effect must fire");
    }

    #[test]
    fn apply_effects_independence_of_the_two_arms() {
        // No bypass flag → record lands, no event.
        let dir = tempdir().unwrap();
        let store = SessionLifecycleStore::open(dir.path().join("terminal-sessions.json")).unwrap();
        let parsed = parse(&format!("claude --resume {UUID}")).unwrap();
        let bypass_emitted = Cell::new(false);
        apply_typed_resume_effects(
            Some(&store),
            &parsed,
            "term-1",
            "C:/repo",
            "default",
            "Terminal 1",
            || bypass_emitted.set(true),
        );
        assert_eq!(store.open_records().len(), 1);
        assert!(!bypass_emitted.get(), "no bypass flag → no event");

        // Store missing (best-effort warn + skip) → bypass mark still fires.
        let parsed = parse(&format!(
            "claude --resume {UUID} --dangerously-skip-permissions"
        ))
        .unwrap();
        let bypass_emitted = Cell::new(false);
        apply_typed_resume_effects(
            None,
            &parsed,
            "term-1",
            "C:/repo",
            "default",
            "Terminal 1",
            || bypass_emitted.set(true),
        );
        assert!(bypass_emitted.get());
    }
}
