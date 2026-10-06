//! The transcript watcher binds EVERY transcript it sees to a coord session.
//! Plan `2026-10-06-closed-sessions-whose-work-is-unfinished-are-found-fleet-wide-and-resumed`
//! Phase 1.
//!
//! ## Why
//!
//! The tailer only emits for sessions the registrar's in-memory R4 index maps.
//! Only the `claude --resume` sniffer and `POST /sessions/transcript-bind`
//! wrote to that index, so on merytshost 1 of 30 transcripts was tailed
//! (`sessions_tailed=1 sessions_unbound=29`). A session that never reaches R4
//! also has no coord row that carries its transcript, account or finished mark
//! — which is what makes a closed-but-unfinished session unfindable.
//!
//! ## What
//!
//! For each transcript the watcher tails, identified by its file-named id
//! (`<config>/projects/<cwd>/<id>.jsonl`):
//!
//! 1. ask coord which `coord.sessions` row answers for that harness id
//!    ([`crate::mcp::session_work_status::resolve_coord_row`] — the latest row,
//!    since the id is not unique);
//! 2. [`decide`]: adopt that row, MINT one only when coord answered `unknown`,
//!    and DEFER (never mint) on anything that does not settle the question —
//!    a second row beside an existing one is the duplicate-row defect;
//! 3. bind through the one binder, `SessionTranscriptTailer::bind_and_replay`
//!    (-> `AiCoordRegistrar::bind_transcript_session`), which also replays the
//!    unsent prefix on first bind and stamps `account_label` / `config_dir` on
//!    a minted row.
//!
//! Consent is unchanged: Gate 1 (`cloud_sync_enabled`) is checked here before
//! any coord traffic and again inside `bind_and_replay`; the tenant's
//! `transcript_sync_enabled` is enforced downstream by the emitter exactly as
//! for a sniffed pane. Attempts are throttled per session ([`AttemptLedger`])
//! so a coord outage costs one bounded probe per window, not one per append.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::claude_session::coord_register::ResumeParams;
use crate::mcp::session_work_status::RowResolution;
use crate::session::session_transcript_tailer::{
    AutoBindKind, BindRefusal, BindRequest, SessionTranscriptTailer,
};

/// Minimum spacing between two attempts for one session.
pub const ATTEMPT_WINDOW: Duration = Duration::from_secs(60);

/// Who a transcript file belongs to, read from its path alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptIdentity {
    /// The file stem — the Claude Code session id.
    pub claude_session_id: String,
    /// The account home holding `projects/`.
    pub config_dir: String,
    /// The fleet's label for that home (`paktis`, `gmail`, ... or `unknown`).
    pub account_label: String,
}

/// Read `<config>/projects/<cwd>/<id>.jsonl`. `None` unless the stem is a
/// UUID and the file sits exactly two directories under a `projects` dir.
pub fn identity_from_path(path: &Path) -> Option<TranscriptIdentity> {
    let stem = path.file_stem()?.to_str()?;
    Uuid::parse_str(stem).ok()?;
    if path.extension()?.to_str()? != "jsonl" {
        return None;
    }
    let projects = path.parent()?.parent()?;
    if projects.file_name()?.to_str()? != "projects" {
        return None;
    }
    let config_dir = projects.parent()?.to_str()?.to_string();
    let (account_label, _wrapper) =
        qontinui_runner_lib::session_archive::discovery::account_label_for(Some(&config_dir));
    Some(TranscriptIdentity {
        claude_session_id: stem.to_string(),
        config_dir,
        account_label,
    })
}

/// What to do with a transcript given what coord said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Bind to the row coord already has.
    Adopt(Uuid),
    /// coord has no session for this id: register a fresh row.
    Mint,
    /// coord's answer does not settle it; try again next window.
    Defer(String),
}

/// Pure: the only place a row may be minted is an explicit `unknown`.
pub fn decide(resolution: &RowResolution) -> Decision {
    match resolution {
        RowResolution::Existing(id) => Decision::Adopt(*id),
        RowResolution::Unknown => Decision::Mint,
        RowResolution::Unresolved(why) => Decision::Defer(why.clone()),
    }
}

/// Per-session attempt throttle.
#[derive(Default)]
pub struct AttemptLedger {
    last: Mutex<HashMap<String, Instant>>,
}

impl AttemptLedger {
    /// `true` (and records `now`) when no attempt for `key` started within
    /// [`ATTEMPT_WINDOW`] of `now`.
    pub fn try_begin(&self, key: &str, now: Instant) -> bool {
        let mut g = self
            .last
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match g.get(key) {
            Some(prev) if now.saturating_duration_since(*prev) < ATTEMPT_WINDOW => false,
            _ => {
                g.insert(key.to_string(), now);
                true
            }
        }
    }
}

static LEDGER: std::sync::LazyLock<AttemptLedger> = std::sync::LazyLock::new(AttemptLedger::default);

/// Apply a [`Decision`] through the tailer's binder. Blocking (file I/O in the
/// replay). Returns what to count, or `None` when the key was already bound.
pub fn apply_decision(
    tailer: &SessionTranscriptTailer,
    identity: &TranscriptIdentity,
    path: &Path,
    decision: &Decision,
    cloud_sync_enabled: bool,
) -> Option<AutoBindKind> {
    let adopt = match decision {
        Decision::Adopt(id) => Some(*id),
        Decision::Mint => None,
        Decision::Defer(why) => {
            debug!(
                "transcript_autobind: {} deferred: {why}",
                identity.claude_session_id
            );
            return Some(AutoBindKind::Deferred);
        }
    };
    let req = BindRequest {
        adopt,
        tenant: None,
        resume: ResumeParams {
            account_label: Some(identity.account_label.clone()),
            config_dir: Some(identity.config_dir.clone()),
        },
    };
    match tailer.bind_and_replay(&identity.claude_session_id, path, req, cloud_sync_enabled) {
        Ok(o) if o.already_bound => None,
        Ok(o) => {
            info!(
                "transcript_autobind: bound {} to coord session {} ({}; replayed {} bytes)",
                identity.claude_session_id,
                o.coord_session_id,
                if o.adopted { "adopted" } else { "minted" },
                o.replayed_bytes
            );
            Some(if o.adopted {
                AutoBindKind::Adopted
            } else {
                AutoBindKind::Minted
            })
        }
        Err(BindRefusal::Unreadable(d)) => {
            warn!(
                "transcript_autobind: {} IS bound but its prefix replay failed: {d}",
                identity.claude_session_id
            );
            Some(if adopt.is_some() {
                AutoBindKind::Adopted
            } else {
                AutoBindKind::Minted
            })
        }
        Err(e) => {
            debug!(
                "transcript_autobind: {} not bound: {e:?}",
                identity.claude_session_id
            );
            Some(AutoBindKind::Deferred)
        }
    }
}

/// Bind `path`'s session if it is not bound yet. Called by the watcher when it
/// starts tailing a transcript and again when an append finds it unbound; the
/// per-session throttle makes the second call cheap. Never fails the tail.
pub async fn ensure_bound(tailer: &Arc<SessionTranscriptTailer>, path: &Path) {
    let Some(identity) = identity_from_path(path) else {
        return;
    };
    if tailer.is_bound(&identity.claude_session_id) {
        return;
    }
    tailer.note_seen_unbound(&identity.claude_session_id);
    let cloud_sync = crate::settings::get_cloud_sync_enabled();
    if !cloud_sync {
        // Consent withheld: no coord traffic, no binding.
        return;
    }
    if !LEDGER.try_begin(&identity.claude_session_id, Instant::now()) {
        return;
    }
    let resolution = crate::mcp::session_work_status::resolve_coord_row(&identity.claude_session_id).await;
    let decision = decide(&resolution);
    let (t, id, p) = (tailer.clone(), identity, path.to_path_buf());
    let joined = qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked(move || {
        apply_decision(&t, &id, &p, &decision, cloud_sync).inspect(|k| t.note_autobind(*k))
    })
    .await;
    if let Err(e) = joined {
        warn!("transcript_autobind: bind task failed to run: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude_session::coord_register::AiCoordRegistrar;
    use crate::session::local_store::OutboxWriter;
    use crate::session::transcript_emitter::TranscriptEmitter;
    use crate::session::SessionEventKind;

    const CSID: &str = "7e0b5d6a-9b8e-4f2c-a3d1-c1d9f0e7a2b4";

    #[test]
    fn identity_reads_config_dir_and_account_from_the_path() {
        let p = Path::new("/home/u/.claude-gmail/projects/-home-u-proj")
            .join(format!("{CSID}.jsonl"));
        let id = identity_from_path(&p).expect("identity");
        assert_eq!(id.claude_session_id, CSID);
        assert_eq!(id.config_dir, "/home/u/.claude-gmail");
        assert_eq!(id.account_label, "gmail");
    }

    #[test]
    fn identity_refuses_paths_that_are_not_a_session_transcript() {
        // Not a UUID stem.
        assert!(identity_from_path(Path::new("/c/.claude/projects/p/notes.jsonl")).is_none());
        // Wrong depth: not directly under projects/<cwd>/.
        assert!(
            identity_from_path(Path::new(&format!("/c/.claude/other/p/{CSID}.jsonl"))).is_none()
        );
        // Wrong extension.
        assert!(
            identity_from_path(Path::new(&format!("/c/.claude/projects/p/{CSID}.txt"))).is_none()
        );
        // The default home carries the stable `unknown` label, not a failure.
        let id = identity_from_path(Path::new(&format!("/home/u/.claude/projects/p/{CSID}.jsonl")))
            .unwrap();
        assert_eq!(id.account_label, "unknown");
    }

    #[test]
    fn only_an_explicit_unknown_mints() {
        let row = Uuid::new_v4();
        assert_eq!(decide(&RowResolution::Existing(row)), Decision::Adopt(row));
        assert_eq!(decide(&RowResolution::Unknown), Decision::Mint);
        assert!(matches!(
            decide(&RowResolution::Unresolved("coord down".into())),
            Decision::Defer(_)
        ));
    }

    #[test]
    fn the_ledger_spaces_attempts_per_session() {
        let l = AttemptLedger::default();
        let t0 = Instant::now();
        assert!(l.try_begin("a", t0));
        assert!(!l.try_begin("a", t0 + Duration::from_secs(5)), "inside window");
        assert!(l.try_begin("b", t0), "another session is independent");
        assert!(l.try_begin("a", t0 + ATTEMPT_WINDOW), "window elapsed");
    }

    fn tailer(dir: &Path) -> (Arc<SessionTranscriptTailer>, Arc<AiCoordRegistrar>, Arc<OutboxWriter>) {
        let outbox = Arc::new(OutboxWriter::open(dir.join("outbox.jsonl")).unwrap());
        let machine_id = Uuid::new_v4();
        let registrar = Arc::new(AiCoordRegistrar::with_tenant_resolver(
            outbox.clone(),
            machine_id,
            || None,
        ));
        let emitter = Arc::new(TranscriptEmitter::new(outbox.clone(), machine_id, registrar.clone()));
        (
            Arc::new(SessionTranscriptTailer::new(emitter, registrar.clone())),
            registrar,
            outbox,
        )
    }

    fn transcript(dir: &Path, csid: &str, body: &str) -> std::path::PathBuf {
        let d = dir.join(".claude-paktis").join("projects").join("proj");
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join(format!("{csid}.jsonl"));
        std::fs::write(&f, body).unwrap();
        f
    }

    /// `register_inner` is gated on a process-global env var other suites
    /// toggle under their own lock, so retry briefly rather than flake.
    fn apply_retrying(
        t: &SessionTranscriptTailer,
        id: &TranscriptIdentity,
        p: &Path,
        d: &Decision,
    ) -> Option<AutoBindKind> {
        for _ in 0..100 {
            let r = apply_decision(t, id, p, d, true);
            if r != Some(AutoBindKind::Deferred) {
                return r;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        Some(AutoBindKind::Deferred)
    }

    #[test]
    fn mint_binds_stamps_the_account_and_replays_the_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let (t, registrar, outbox) = tailer(dir.path());
        let path = transcript(dir.path(), CSID, "{\"type\":\"user\"}\n{\"type\":\"assistant\"}\n");
        let id = identity_from_path(&path).unwrap();
        t.note_seen_unbound(CSID);
        assert_eq!(t.coverage().sessions_unbound, 1, "a seen-but-unbound transcript is a hole");

        let kind = apply_retrying(&t, &id, &path, &Decision::Mint);
        assert_eq!(kind, Some(AutoBindKind::Minted));
        assert!(registrar.session_id_for(CSID).is_some(), "R4 now maps it");
        assert_eq!(t.coverage().sessions_unbound, 0, "binding clears the hole");

        let started: Vec<_> = outbox
            .pending()
            .unwrap()
            .into_iter()
            .filter(|r| r.event_kind == SessionEventKind::Started.as_str())
            .collect();
        assert_eq!(started.len(), 1, "exactly one Started row");
        assert_eq!(started[0].payload["account_label"], "paktis");
        assert_eq!(
            started[0].payload["config_dir"],
            dir.path().join(".claude-paktis").to_str().unwrap()
        );
        assert_eq!(started[0].payload["claude_code_session_id"], CSID);
        assert!(
            outbox
                .pending()
                .unwrap()
                .iter()
                .any(|r| r.event_kind == SessionEventKind::OutputChunk.as_str()),
            "the unsent prefix was backfilled"
        );
    }

    #[test]
    fn adopt_binds_to_coords_row_and_writes_no_started() {
        let dir = tempfile::tempdir().unwrap();
        let (t, registrar, outbox) = tailer(dir.path());
        let path = transcript(dir.path(), CSID, "{\"type\":\"user\"}\n");
        let id = identity_from_path(&path).unwrap();
        let row = Uuid::new_v4();

        let kind = apply_retrying(&t, &id, &path, &Decision::Adopt(row));
        assert_eq!(kind, Some(AutoBindKind::Adopted));
        assert_eq!(registrar.session_id_for(CSID), Some(row));
        assert!(
            !outbox
                .pending()
                .unwrap()
                .iter()
                .any(|r| r.event_kind == SessionEventKind::Started.as_str()),
            "an adopted row already exists coord-side"
        );
        // A second pass never re-registers or double-counts.
        assert_eq!(apply_decision(&t, &id, &path, &Decision::Mint, true), None);
        assert_eq!(registrar.session_id_for(CSID), Some(row));
    }

    #[test]
    fn defer_and_consent_off_bind_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let (t, registrar, _outbox) = tailer(dir.path());
        let path = transcript(dir.path(), CSID, "{\"type\":\"user\"}\n");
        let id = identity_from_path(&path).unwrap();

        assert_eq!(
            apply_decision(&t, &id, &path, &Decision::Defer("coord down".into()), true),
            Some(AutoBindKind::Deferred)
        );
        // Gate 1 off: bind_and_replay refuses before registering anything.
        assert_eq!(
            apply_decision(&t, &id, &path, &Decision::Mint, false),
            Some(AutoBindKind::Deferred)
        );
        assert!(registrar.session_id_for(CSID).is_none());
    }

    #[test]
    fn autobind_outcomes_reach_the_coverage_report() {
        let dir = tempfile::tempdir().unwrap();
        let (t, _r, _o) = tailer(dir.path());
        t.note_autobind(AutoBindKind::Adopted);
        t.note_autobind(AutoBindKind::Minted);
        t.note_autobind(AutoBindKind::Minted);
        t.note_autobind(AutoBindKind::Deferred);
        let c = t.coverage();
        assert_eq!((c.autobind_adopted, c.autobind_minted, c.autobind_deferred), (1, 2, 1));
    }
}
