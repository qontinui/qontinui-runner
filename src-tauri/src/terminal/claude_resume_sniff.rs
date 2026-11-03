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
//!
//! ## A line typed INTO a running `claude` is not a launch
//!
//! The stream is every line written to the pane, so a prompt pasted into a
//! running `claude` that happens to start `claude --resume <id>` parses like a
//! shell command. Recording that authoritatively binds a session the pane does
//! not run (plan `2026-10-31-runner-binds-a-terminal-to-the-wrong-live-session`,
//! Phase 3c). So the effects first ask where the line went
//! ([`typed_line_target`]): only a line the pane's SHELL read is recorded
//! authoritatively and registered with coord. Otherwise the record is
//! PROVISIONAL — `reconciled`, unconfirmed: it supersedes nothing, and the
//! store's binding guard never lets such a weak bind take a row from a live
//! pane whose own record names the session — pending the pane's own registry
//! record, which the reconcile pass binds from (rung 2b) if the session really
//! runs there.

use std::sync::Arc;

use tauri::{AppHandle, Emitter, Manager};
use tracing::{debug, info, warn};

use crate::session::session_lifecycle_store::{
    RecordOpenOutcome, SessionLifecycleStore, TerminalSessionRecord, ORIGIN_AUTHORITATIVE,
    ORIGIN_RECONCILED,
};
use crate::terminal::graceful_exit::ClaudeProbe;

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

/// Who read a typed line — decides how far the sniff may trust it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TypedLineTarget {
    /// The pane's shell read it: whatever `claude` the line launches runs the
    /// id it names. Recorded authoritatively.
    Shell,
    /// A `claude` already running in the pane and reading the terminal read it
    /// — the line was input to that session, not a launch. Recorded
    /// provisionally.
    Claude,
    /// The pane's processes could not be read, or a `claude` reading the
    /// terminal has no start time to place before or after the line. Recorded
    /// provisionally.
    Unknown,
}

/// How long the sniff waits before its one retry of an unreadable pane probe.
const PROBE_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(500);

/// A `claude` that started at least this many seconds before the line was the
/// one reading it; one that started later is the one the line launched.
/// Process start times are whole seconds, so one second of rounding either way
/// plus a beat for the probe.
const LAUNCH_SKEW_SECS: i64 = 2;

/// Where the line completed at `line_at_ms` went, from a probe of the pane
/// taken just after it. `reads_terminal` says whether a pid owns the pane's
/// terminal now (the foreground process group on Linux — a `claude` suspended
/// with Ctrl-Z or started with `&` leaves the shell reading). On Windows a
/// console has no job control, so a `claude` in the pane cannot have left the
/// console to the shell: there `reads_terminal` is existence
/// ([`crate::mcp::session_message_poller::pid_reads_its_terminal`], the same
/// predicate the message poller routes by), and a `claude` that started before
/// the line read it. Pure.
///
/// The probe runs AFTER the line, so the `claude` the line itself launched may
/// already be in it; that one started at or after the line and is not counted.
/// A `claude` that started before the line and reads the terminal read the
/// line.
pub(crate) fn typed_line_target(
    probe: &ClaudeProbe,
    line_at_ms: i64,
    reads_terminal: impl Fn(u32) -> bool,
) -> TypedLineTarget {
    let ClaudeProbe::Readable(pane) = probe else {
        return TypedLineTarget::Unknown;
    };
    let line_at_secs = line_at_ms / 1000;
    let mut start_unknown = false;
    for claude in &pane.subtree_claude {
        if !reads_terminal(claude.pid) {
            continue;
        }
        if claude.started_at <= 0 {
            start_unknown = true;
        } else if claude.started_at + LAUNCH_SKEW_SECS <= line_at_secs {
            return TypedLineTarget::Claude;
        }
    }
    if start_unknown {
        TypedLineTarget::Unknown
    } else {
        TypedLineTarget::Shell
    }
}

/// Apply the three effects of a recognized typed resume line: (1) `record_open`
/// the session in the lifecycle store — a NEW writer arm beside the existing
/// frontend-command / continuation-poller writers (structural dedup by
/// session id means a frontend re-record simply refreshes this row); (2)
/// invoke `register_coord` (production routes the sniffed session through
/// [`crate::claude_session::coord_register::AiCoordRegistrar::register_sniffed_session`]
/// — session-identity fabric Phase 3) — AFTER the record_open, so the
/// registrar's handle hook can read the record's `terminal_id`, and ONLY when
/// the store recorded (a session the runner can't durably track shouldn't get
/// a coord identity the runner can't resolve later); (3) for the bypass form,
/// invoke `emit_bypass` (production emits `terminal-bypass-permissions`).
/// Split out from [`spawn_register_typed_resume`] so the trio is testable
/// without a Tauri app.
///
/// `target` decides how far each effect goes ([`TypedLineTarget`]):
///
/// | target | record | coord | bypass event |
/// |---|---|---|---|
/// | `Shell` | authoritative | registered (when recorded) | when the line says so |
/// | `Unknown` | provisional ([`SessionLifecycleStore::record_open_provisional`]) | no | when the line says so |
/// | `Claude` | none | no | no — nothing was launched |
///
/// A line a running `claude` read launched nothing, so it records nothing: a
/// write would re-open a CLOSED session onto this pane (the binding guard only
/// stands between OPEN rows and a move) and its asserted `reconciled` origin
/// would downgrade an existing row's grade. A line nothing could place
/// (`Unknown`) may have launched something, so it is recorded — but
/// provisionally: unconfirmed, never re-opening a closed row, and graded
/// `reconciled` only on a row it creates, never over an existing row's origin.
#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_typed_resume_effects(
    store: Option<&SessionLifecycleStore>,
    parsed: &TypedClaudeResume,
    target: TypedLineTarget,
    terminal_id: &str,
    working_dir: &str,
    page_id: &str,
    title: &str,
    register_coord: impl FnOnce(),
    emit_bypass: impl FnOnce(),
) {
    if target == TypedLineTarget::Claude {
        debug!(
            terminal_id = %terminal_id,
            claude_session = %parsed.claude_session_id,
            "typed claude resume sniff: the line was input to a running claude — nothing launched, nothing recorded"
        );
        return;
    }
    let launched = target == TypedLineTarget::Shell;
    match store {
        Some(store) => {
            let rec = TerminalSessionRecord {
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
                provider: crate::session::session_lifecycle_store::DEFAULT_PROVIDER.to_string(),
                // A line the shell read: the exact id lifted from the typed
                // `--resume`/`--session-id` flag — authoritative, same as the
                // spawn-argv pre-pin path. Any other line is a claim nothing
                // has launched: provisional, the conservative grade.
                origin: Some(
                    if launched {
                        ORIGIN_AUTHORITATIVE
                    } else {
                        ORIGIN_RECONCILED
                    }
                    .to_string(),
                ),
                restore_pending_at: None,
                awaiting_account_since: None,
                confirmed_at: None,
                handle: None,
                account_label: None,
                account_wrapper: None,
                session_name: None,
                name_source: None,
                tenant_id: None,
                task_run_id: None,
                // D1: the typed `--resume` line the sniff just parsed IS the
                // evidence for this — the same signal that drives the
                // `terminal-bypass-permissions` event below, now durable.
                bypass_permissions: Some(parsed.bypass_permissions),
                restored_from_boot_at: None,
                restore_tier: None,
                finished_at: None,
                wind_down_outcome: None,
                wind_down_at: None,
                wind_down_arm: None,
                finish_reason: None,
                finish_synced: false,
                spawn_device_default: None,
                adopted_from: None,
                last_session_start_at: None,
                gate_id: None,
                gate_consuming_device_id: None,
                gate_bound_boot_ms: None,
                binding_state: None,
                placed_strongly: false,
            };
            let outcome = if launched {
                Some(store.record_open_checked(rec))
            } else {
                store.record_open_provisional(rec)
            };
            match outcome {
                None => debug!(
                    terminal_id = %terminal_id,
                    claude_session = %parsed.claude_session_id,
                    "typed claude resume sniff: the session's row is closed and nothing shows the line launched it — left closed"
                ),
                Some(RecordOpenOutcome::Recorded) => {
                    info!(
                        terminal_id = %terminal_id,
                        claude_session = %parsed.claude_session_id,
                        bypass = parsed.bypass_permissions,
                        target = ?target,
                        "typed claude resume sniff: session durably recorded"
                    );
                    if launched {
                        register_coord();
                    }
                }
                Some(outcome) => warn!(
                    terminal_id = %terminal_id,
                    claude_session = %parsed.claude_session_id,
                    target = ?target,
                    outcome = ?outcome,
                    "typed claude resume sniff: session NOT recorded"
                ),
            }
        }
        None => warn!(
            terminal_id = %terminal_id,
            claude_session = %parsed.claude_session_id,
            "typed claude resume sniff: lifecycle store not managed — session not recorded"
        ),
    }
    if parsed.bypass_permissions && target != TypedLineTarget::Claude {
        emit_bypass();
    }
}

/// Dispatch the effects of a recognized typed resume line off the PTY write
/// hot path. Mirrors `coord_warn::spawn_check_and_warn`: a detached async
/// task, all failures swallowed. `tauri::async_runtime::spawn` (not bare
/// `tokio::spawn`) so dispatch works from any caller thread —
/// `TerminalSession::write` is reachable from bare OS threads.
///
/// `child_pid` is the pane's PTY child and `line_at_ms` the instant the line
/// completed: the task probes the pane (one process-table snapshot) to decide
/// [`TypedLineTarget`] before any effect. A probe that cannot read the table is
/// retried once after [`PROBE_RETRY_DELAY`]: an unreadable pane records the
/// line provisionally and registers no coord row, so one transient snapshot
/// failure must not cost a launch its coord identity. (The retry sees any
/// `claude` the line itself launched; one that started at or after the line is
/// never counted as the reader.)
#[allow(clippy::too_many_arguments)]
pub fn spawn_register_typed_resume(
    app_handle: AppHandle,
    terminal_id: String,
    working_dir: String,
    page_id: String,
    title: String,
    parsed: TypedClaudeResume,
    child_pid: Option<u32>,
    line_at_ms: i64,
) {
    tauri::async_runtime::spawn(async move {
        let mut probe =
            crate::terminal::graceful_exit::probe_claude_under(child_pid, Vec::new()).await;
        if let ClaudeProbe::Unreadable(reason) = &probe {
            debug!(
                reason = %reason,
                "typed claude resume sniff: pane probe unreadable — retrying once"
            );
            tokio::time::sleep(PROBE_RETRY_DELAY).await;
            probe = crate::terminal::graceful_exit::probe_claude_under(child_pid, Vec::new()).await;
        }
        let target = typed_line_target(
            &probe,
            line_at_ms,
            crate::mcp::session_message_poller::pid_reads_its_terminal,
        );
        let store = app_handle
            .try_state::<Arc<SessionLifecycleStore>>()
            .map(|s| s.inner().clone());
        let emit_handle = app_handle.clone();
        let emit_terminal_id = terminal_id.clone();
        // Session-identity fabric Phase 3 — register the sniffed session with
        // coord through the SAME AiCoordRegistrar the pinned plane uses (R6
        // dedupe on claude_session_id absorbs re-sniffs; the Phase-1 handle
        // hook then mints/rebinds the fsh_ handle with task_run_id: None).
        // Best-effort: an unmanaged registrar (tests, early boot) is a no-op.
        let register_handle = app_handle.clone();
        let register_csid = parsed.claude_session_id.clone();
        let register_title = title.clone();
        let register_coord = move || {
            let Some(registrar) = register_handle
                .try_state::<Arc<crate::claude_session::coord_register::AiCoordRegistrar>>()
            else {
                debug!(
                    claude_session = %register_csid,
                    "typed claude resume sniff: AiCoordRegistrar not managed — skipping coord registration"
                );
                return;
            };
            registrar.register_sniffed_session(&register_csid, &register_title, None);
        };
        apply_typed_resume_effects(
            store.as_deref(),
            &parsed,
            target,
            &terminal_id,
            &working_dir,
            &page_id,
            &title,
            register_coord,
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
    fn apply_effects_records_open_registers_coord_and_emits_bypass() {
        // A typed bypass resume line must produce all three effects: a
        // record_open, a coord-registration invocation, and a bypass event.
        let dir = tempdir().unwrap();
        let store = SessionLifecycleStore::open(dir.path().join("terminal-sessions.json")).unwrap();
        let parsed = parse(&format!(
            "claude --resume {UUID} --permission-mode bypassPermissions"
        ))
        .unwrap();

        let coord_registered = Cell::new(false);
        let bypass_emitted = Cell::new(false);
        apply_typed_resume_effects(
            Some(&store),
            &parsed,
            TypedLineTarget::Shell,
            "term-1",
            "C:/repo",
            "default",
            "Terminal 1",
            || coord_registered.set(true),
            || bypass_emitted.set(true),
        );

        let open = store.open_records();
        assert_eq!(open.len(), 1, "record_open effect must land");
        assert_eq!(open[0].claude_session_id, UUID);
        assert_eq!(open[0].terminal_id, "term-1");
        assert_eq!(open[0].working_dir.as_deref(), Some("C:/repo"));
        assert_eq!(open[0].page_id, "default");
        assert_eq!(open[0].state, "open");
        assert!(
            coord_registered.get(),
            "coord registration effect must fire when the store recorded"
        );
        assert!(bypass_emitted.get(), "bypass event effect must fire");
    }

    #[test]
    fn apply_effects_independence_of_the_arms() {
        // No bypass flag → record lands + coord registration fires, no event.
        let dir = tempdir().unwrap();
        let store = SessionLifecycleStore::open(dir.path().join("terminal-sessions.json")).unwrap();
        let parsed = parse(&format!("claude --resume {UUID}")).unwrap();
        let coord_registered = Cell::new(false);
        let bypass_emitted = Cell::new(false);
        apply_typed_resume_effects(
            Some(&store),
            &parsed,
            TypedLineTarget::Shell,
            "term-1",
            "C:/repo",
            "default",
            "Terminal 1",
            || coord_registered.set(true),
            || bypass_emitted.set(true),
        );
        assert_eq!(store.open_records().len(), 1);
        assert!(coord_registered.get());
        assert!(!bypass_emitted.get(), "no bypass flag → no event");

        // Store missing (best-effort warn + skip) → coord registration must
        // NOT fire (a session the runner can't durably track shouldn't get a
        // coord identity), but the bypass mark still fires.
        let parsed = parse(&format!(
            "claude --resume {UUID} --dangerously-skip-permissions"
        ))
        .unwrap();
        let coord_registered = Cell::new(false);
        let bypass_emitted = Cell::new(false);
        apply_typed_resume_effects(
            None,
            &parsed,
            TypedLineTarget::Shell,
            "term-1",
            "C:/repo",
            "default",
            "Terminal 1",
            || coord_registered.set(true),
            || bypass_emitted.set(true),
        );
        assert!(
            !coord_registered.get(),
            "no store record → no coord registration"
        );
        assert!(bypass_emitted.get());
    }

    use crate::terminal::graceful_exit::{PaneProcesses, ProcIdentity};

    /// The line completed at t = 10 000 s.
    const LINE_AT_MS: i64 = 10_000_000;

    fn pane(claude: &[(u32, i64)]) -> ClaudeProbe {
        ClaudeProbe::Readable(PaneProcesses {
            subtree_claude: claude
                .iter()
                .map(|&(pid, started_at)| ProcIdentity { pid, started_at })
                .collect(),
            top_level_children: 0,
            tracked_alive: Vec::new(),
        })
    }

    /// Plan `2026-10-31-runner-binds-a-terminal-to-the-wrong-live-session`,
    /// Phase 3c: only a line the pane's SHELL read is a launch.
    #[test]
    fn typed_line_target_tells_a_shell_line_from_a_prompt_typed_into_claude() {
        let all = |_: u32| true;
        // No claude in the pane: the shell read it.
        assert_eq!(
            typed_line_target(&pane(&[]), LINE_AT_MS, all),
            TypedLineTarget::Shell
        );
        // A claude running since long before the line, reading the terminal:
        // the line was a prompt to it.
        assert_eq!(
            typed_line_target(&pane(&[(7, 9_000)]), LINE_AT_MS, all),
            TypedLineTarget::Claude
        );
        // The claude the line itself launched (it started with the line) is
        // not the reader.
        assert_eq!(
            typed_line_target(&pane(&[(7, 10_000)]), LINE_AT_MS, all),
            TypedLineTarget::Shell
        );
        assert_eq!(
            typed_line_target(&pane(&[(7, 9_999)]), LINE_AT_MS, all),
            TypedLineTarget::Shell,
            "within the launch skew"
        );
        // A claude that does not own the terminal (Ctrl-Z, `&`) leaves the
        // shell reading.
        assert_eq!(
            typed_line_target(&pane(&[(7, 9_000)]), LINE_AT_MS, |_| false),
            TypedLineTarget::Shell
        );
        // A reading claude with no start time cannot be placed: unknown.
        assert_eq!(
            typed_line_target(&pane(&[(7, 0)]), LINE_AT_MS, all),
            TypedLineTarget::Unknown
        );
        // ... unless another one demonstrably predates the line.
        assert_eq!(
            typed_line_target(&pane(&[(7, 0), (8, 9_000)]), LINE_AT_MS, all),
            TypedLineTarget::Claude
        );
        // An unreadable pane says nothing.
        assert_eq!(
            typed_line_target(&ClaudeProbe::Unreadable("x".into()), LINE_AT_MS, all),
            TypedLineTarget::Unknown
        );
    }

    /// Round-2 review L5: a line a running `claude` read launched nothing, so it
    /// records nothing — not even over a CLOSED row of the session it names,
    /// which a write would re-open onto this pane, and not over an existing
    /// row's origin — and registers no coord identity and emits no bypass.
    #[test]
    fn a_line_a_running_claude_read_records_nothing() {
        let dir = tempdir().unwrap();
        let store = SessionLifecycleStore::open(dir.path().join("terminal-sessions.json")).unwrap();
        let mut ended = crate::session::session_lifecycle_store::test_open_record(UUID);
        ended.terminal_id = "term-elsewhere".to_string();
        ended.origin = Some(ORIGIN_AUTHORITATIVE.to_string());
        store.record_open(ended);
        store.record_close(UUID, "explicit");

        let parsed = parse(&format!(
            "claude --resume {UUID} --dangerously-skip-permissions"
        ))
        .unwrap();
        let coord_registered = Cell::new(false);
        let bypass_emitted = Cell::new(false);
        apply_typed_resume_effects(
            Some(&store),
            &parsed,
            TypedLineTarget::Claude,
            "term-1",
            "C:/repo",
            "default",
            "Terminal 1",
            || coord_registered.set(true),
            || bypass_emitted.set(true),
        );

        let row = store.get(UUID).unwrap();
        assert_eq!(
            (row.state.as_str(), row.terminal_id.as_str()),
            ("closed", "term-elsewhere")
        );
        assert_eq!(row.origin.as_deref(), Some(ORIGIN_AUTHORITATIVE));
        assert!(!coord_registered.get());
        assert!(!bypass_emitted.get());
    }

    /// A line on an unreadable pane is recorded PROVISIONALLY: reconciled and
    /// unconfirmed — so it supersedes nothing on the pane — with no coord
    /// identity, keeping the bypass mark the line carries.
    #[test]
    fn a_line_not_read_by_the_shell_is_recorded_provisionally() {
        let dir = tempdir().unwrap();
        let store = SessionLifecycleStore::open(dir.path().join("terminal-sessions.json")).unwrap();
        // The pane's own session, pinned at spawn.
        let mut own = crate::session::session_lifecycle_store::test_open_record("pane-own");
        own.terminal_id = "term-1".to_string();
        own.origin = Some(ORIGIN_AUTHORITATIVE.to_string());
        store.record_open(own);

        let parsed = parse(&format!(
            "claude --resume {UUID} --dangerously-skip-permissions"
        ))
        .unwrap();
        let coord_registered = Cell::new(false);
        let bypass_emitted = Cell::new(false);
        apply_typed_resume_effects(
            Some(&store),
            &parsed,
            TypedLineTarget::Unknown,
            "term-1",
            "C:/repo",
            "default",
            "Terminal 1",
            || coord_registered.set(true),
            || bypass_emitted.set(true),
        );

        let sniffed = store.get(UUID).expect("provisional row recorded");
        assert_eq!(sniffed.origin.as_deref(), Some(ORIGIN_RECONCILED));
        assert!(sniffed.confirmed_at.is_none());
        assert_eq!(
            store.get("pane-own").unwrap().state,
            "open",
            "a provisional record supersedes nothing"
        );
        assert!(!coord_registered.get(), "no coord identity");
        assert!(bypass_emitted.get(), "the line's bypass mark is kept");
    }

    /// Round-3 review L2: a line nothing could place is a claim, not a
    /// launch. It never re-opens a CLOSED row of the session it names, and it
    /// never replaces an existing open row's origin with its own `reconciled`
    /// — the row's terminal, confirmation and grade stay what they were.
    #[test]
    fn a_line_not_read_by_the_shell_neither_reopens_nor_downgrades_a_row() {
        let unplaced = |store: &SessionLifecycleStore, id: &str| {
            let parsed = parse(&format!("claude --resume {id}")).unwrap();
            apply_typed_resume_effects(
                Some(store),
                &parsed,
                TypedLineTarget::Unknown,
                "term-1",
                "C:/repo",
                "default",
                "Terminal 1",
                || {},
                || {},
            );
        };

        // A closed row stays closed, on the terminal it closed on.
        let dir = tempdir().unwrap();
        let store = SessionLifecycleStore::open(dir.path().join("terminal-sessions.json")).unwrap();
        let mut ended = crate::session::session_lifecycle_store::test_open_record(UUID);
        ended.terminal_id = "term-elsewhere".to_string();
        ended.origin = Some(ORIGIN_AUTHORITATIVE.to_string());
        store.record_open(ended);
        store.record_close(UUID, "explicit");
        unplaced(&store, UUID);
        let row = store.get(UUID).unwrap();
        assert_eq!(
            (row.state.as_str(), row.terminal_id.as_str()),
            ("closed", "term-elsewhere")
        );
        assert_eq!(row.origin.as_deref(), Some(ORIGIN_AUTHORITATIVE));

        // An open row keeps its grade: the claim's `reconciled` is not written
        // over an `authoritative` one.
        let dir = tempdir().unwrap();
        let store = SessionLifecycleStore::open(dir.path().join("terminal-sessions.json")).unwrap();
        let mut live = crate::session::session_lifecycle_store::test_open_record(UUID);
        live.terminal_id = "term-1".to_string();
        live.origin = Some(ORIGIN_AUTHORITATIVE.to_string());
        live.confirmed_at = Some(1_000);
        store.record_open(live);
        unplaced(&store, UUID);
        let row = store.get(UUID).unwrap();
        assert_eq!(row.state, "open");
        assert_eq!(row.origin.as_deref(), Some(ORIGIN_AUTHORITATIVE));
        assert_eq!(row.confirmed_at, Some(1_000));
    }
}
