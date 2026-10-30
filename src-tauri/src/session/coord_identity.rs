//! The ONE seam that re-keys a pane's `coord.sessions` row to the harness
//! (Claude Code) session id the runner has pid-exact evidence for.
//!
//! Plan `2026-10-23-a-hookless-resume-in-a-runner-pane-leaves-the-session-with-no-active-coord-row`
//! D1 / Phase 1a / Phase 2.
//!
//! Before this module the only thing that re-keyed a pane's row was the
//! provider's SessionStart hook (`POST /control/session-open`). A pane whose
//! `claude` bypassed the identity shim (an absolute-path account launcher) never
//! fires that hook, so its row kept whatever id it was first registered under —
//! and an R2-resumed pane row kept the FIRST-EVER pin it carried, across every
//! restart. Every source of identity evidence now goes through
//! [`confirm_for_terminal`] / [`confirm_for_session`]:
//!
//! | source              | evidence                                            |
//! |---------------------|-----------------------------------------------------|
//! | `hook`              | the SessionStart hook (`/control/session-open`)     |
//! | `rung2`             | the anchor's own argv (`--session-id` / `--resume`) |
//! | `provider_registry` | `<config>/sessions/<pid>.json` (Phase 1b)           |
//! | `r2_resume`         | an R2 pane resume re-keyed to the pane's pin        |
//!
//! Only pid-exact evidence may re-key a row (D5): a rung-3 transcript
//! correlation or a rung-4 guess never reaches this seam.
//!
//! ## What is counted, and when
//!
//! `Queued` means a `state_change` entered the outbox — nothing more. coord can
//! still refuse it `409 harness_session_already_bound` (another ACTIVE row on
//! the device holds the id). So the drain reports each re-key's answer back into
//! the [`RekeyLedger`] the session's `CoordSync` owns: `landed` moves only on a
//! 2xx, `refused` on that 409. A refused re-key is RESTORED here (the local
//! record is moved back to what coord last accepted, because
//! `confirm_claude_code_session_id` moves it before coord answers) so a later
//! tick re-queues it. The drain never calls back into the registry: it writes
//! the ledger, and this module reads it.
//!
//! `/health` `sessionTracking.coordRekeys` serves the ledger's counters.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value as JsonValue};
use uuid::Uuid;

use super::reconcile::{BindOrigin, ReconcileAction};
use super::{SessionError, SessionRegistry};

/// Which evidence asked for a re-key. The label rides the outbox row
/// (`rekey_source`, stripped before the PATCH) so the drain can count coord's
/// answer per source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RekeySource {
    /// The provider's SessionStart hook — the id it adopted, self-reported.
    Hook,
    /// Rung 2: the id the anchor claude's own argv names.
    Rung2,
    /// The provider's `<config>/sessions/<pid>.json` for the anchor pid
    /// (Phase 1b; no caller on this build).
    ProviderRegistry,
    /// An R2 pane resume, re-keyed to the pane's current pin (Phase 2).
    R2Resume,
}

impl RekeySource {
    /// Every source, in `/health` order.
    pub const ALL: [RekeySource; 4] = [
        RekeySource::Hook,
        RekeySource::Rung2,
        RekeySource::ProviderRegistry,
        RekeySource::R2Resume,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            RekeySource::Hook => "hook",
            RekeySource::Rung2 => "rung2",
            RekeySource::ProviderRegistry => "provider_registry",
            RekeySource::R2Resume => "r2_resume",
        }
    }

    /// Parse an outbox row's `rekey_source`. A row with NO label was queued by
    /// a build that predates this module, where the hook was the only writer —
    /// so an absent label is `Hook`. An unknown label is `None` (not counted).
    pub fn from_payload(payload: &JsonValue) -> Option<Self> {
        match payload.get("rekey_source").and_then(JsonValue::as_str) {
            None => Some(RekeySource::Hook),
            Some(s) => Self::ALL.into_iter().find(|src| src.as_str() == s),
        }
    }

    fn index(self) -> usize {
        match self {
            RekeySource::Hook => 0,
            RekeySource::Rung2 => 1,
            RekeySource::ProviderRegistry => 2,
            RekeySource::R2Resume => 3,
        }
    }

    /// Sources whose evidence is NOT re-supplied every reconcile tick. A refused
    /// re-key from one of these is retried by [`settle_refusals`] itself;
    /// `rung2` / `provider_registry` re-supply their id on the next tick.
    fn retried_by_settle(self) -> bool {
        matches!(self, RekeySource::Hook | RekeySource::R2Resume)
    }
}

/// What the seam did with one piece of identity evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RekeyOutcome {
    /// A `state_change{claude_code_session_id}` entered the outbox. Says
    /// nothing about coord's answer — that is the ledger's `landed`/`refused`.
    Queued,
    /// The row already carries this id (locally, with no unsettled refusal).
    AlreadyCurrent,
    /// The terminal has no coord row yet (or the registry does not hold it).
    NoCoordRow,
    /// The id is not a UUID; coord's PATCH would fail whole on it.
    Invalid,
    /// The local outbox write failed; the record was left as it was.
    OutboxFailed,
}

/// A re-key coord refused `409 harness_session_already_bound`, waiting for the
/// seam to restore the local record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    /// The id coord refused to bind.
    pub refused_id: String,
    /// The record's id before that re-key moved it (from the outbox row).
    pub previous: Option<String>,
    pub source: RekeySource,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SourceCounts {
    pub queued: u64,
    pub landed: u64,
    pub refused: u64,
}

#[derive(Debug, Default)]
struct SessionRekeyState {
    /// The `claude_code_session_id` coord last ACCEPTED (2xx) on a
    /// `state_change` for this session.
    last_landed: Option<String>,
    /// An unsettled refusal, newest wins.
    refused: Option<Refusal>,
}

#[derive(Debug, Default)]
struct LedgerInner {
    sessions: HashMap<Uuid, SessionRekeyState>,
    counts: [SourceCounts; 4],
    no_coord_row: u64,
    invalid: u64,
}

/// Per-`CoordSync` record of coord's answers to re-keys. Written by the drain
/// (landed / refused) and by the seam (queued / no_coord_row / invalid); read by
/// the seam and by `/health`.
#[derive(Debug, Default)]
pub struct RekeyLedger {
    inner: Mutex<LedgerInner>,
}

impl RekeyLedger {
    fn with<R>(&self, f: impl FnOnce(&mut LedgerInner) -> R) -> R {
        let mut g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        f(&mut g)
    }

    pub(crate) fn note_queued(&self, source: RekeySource) {
        self.with(|l| l.counts[source.index()].queued += 1);
    }

    pub(crate) fn note_no_coord_row(&self) {
        self.with(|l| l.no_coord_row += 1);
    }

    pub(crate) fn note_invalid(&self) {
        self.with(|l| l.invalid += 1);
    }

    /// The drain saw a 2xx on a `state_change` carrying `id`.
    pub(crate) fn note_landed(&self, session: Uuid, id: &str, source: RekeySource) {
        self.with(|l| {
            l.counts[source.index()].landed += 1;
            let s = l.sessions.entry(session).or_default();
            s.last_landed = Some(id.to_string());
            // A refusal of THIS id is moot once a later push of it landed.
            if s.refused.as_ref().is_some_and(|r| r.refused_id == id) {
                s.refused = None;
            }
        });
    }

    /// The drain saw `409 harness_session_already_bound` on a `state_change`
    /// carrying `id`.
    pub(crate) fn note_refused(
        &self,
        session: Uuid,
        id: &str,
        previous: Option<String>,
        source: RekeySource,
    ) {
        self.with(|l| {
            l.counts[source.index()].refused += 1;
            l.sessions.entry(session).or_default().refused = Some(Refusal {
                refused_id: id.to_string(),
                previous,
                source,
            });
        });
    }

    /// The id coord last accepted for `session`, if any re-key ever landed.
    pub fn last_landed(&self, session: Uuid) -> Option<String> {
        self.with(|l| l.sessions.get(&session).and_then(|s| s.last_landed.clone()))
    }

    fn take_refusal(&self, session: Uuid) -> Option<Refusal> {
        self.with(|l| l.sessions.get_mut(&session).and_then(|s| s.refused.take()))
    }

    fn refused_sessions(&self) -> Vec<Uuid> {
        self.with(|l| {
            l.sessions
                .iter()
                .filter(|(_, s)| s.refused.is_some())
                .map(|(id, _)| *id)
                .collect()
        })
    }

    /// One source's counters (tests and `/health`).
    pub fn counts(&self, source: RekeySource) -> SourceCounts {
        self.with(|l| l.counts[source.index()])
    }

    pub fn no_coord_row(&self) -> u64 {
        self.with(|l| l.no_coord_row)
    }

    pub fn invalid(&self) -> u64 {
        self.with(|l| l.invalid)
    }

    /// `/health` `sessionTracking.coordRekeys`:
    /// `{queued:{<source>:n}, landed:{…}, refused:{…}, no_coord_row, invalid}`.
    /// Every source key is always present — a zero is observed, not absent.
    pub fn health_json(&self) -> JsonValue {
        self.with(|l| {
            let per = |pick: fn(&SourceCounts) -> u64| {
                let mut m = serde_json::Map::new();
                for src in RekeySource::ALL {
                    m.insert(src.as_str().into(), json!(pick(&l.counts[src.index()])));
                }
                JsonValue::Object(m)
            };
            json!({
                "queued": per(|c| c.queued),
                "landed": per(|c| c.landed),
                "refused": per(|c| c.refused),
                "no_coord_row": l.no_coord_row,
                "invalid": l.invalid,
            })
        })
    }
}

/// Restore a refused re-key on `session`: move the local record back to what
/// coord holds (the last-landed id, else the row's id before the refused
/// re-key) — only if the record still holds the refused id; a newer re-key has
/// otherwise superseded it. Returns the refusal when the record was restored.
fn settle_session_refusal(registry: &SessionRegistry, session: Uuid) -> Option<Refusal> {
    let ledger = registry.coord_sync().rekeys();
    let refusal = ledger.take_refusal(session)?;
    let restore_to = ledger
        .last_landed(session)
        .or_else(|| refusal.previous.clone());
    match registry.restore_claude_code_session_id(session, &refusal.refused_id, restore_to.clone())
    {
        Ok(true) => {
            tracing::info!(
                coord_session_id = %session,
                refused = %refusal.refused_id,
                restored_to = ?restore_to,
                source = refusal.source.as_str(),
                "coord identity: re-key REFUSED by coord (harness_session_already_bound) — \
                 local record restored so a later tick re-queues it"
            );
            Some(refusal)
        }
        Ok(false) => None,
        Err(_) => None,
    }
}

/// Settle every refused re-key the drain has recorded: restore each record,
/// and re-queue the refused id for the sources whose evidence is not
/// re-supplied every tick (`hook`, `r2_resume`). Called once per reconcile
/// tick, BEFORE the tick's own seam calls; bounded by that 45 s cadence.
pub fn settle_refusals(registry: &SessionRegistry) {
    for session in registry.coord_sync().rekeys().refused_sessions() {
        if let Some(refusal) = settle_session_refusal(registry, session) {
            if refusal.source.retried_by_settle() {
                confirm_for_session(registry, session, &refusal.refused_id, refusal.source);
            }
        }
    }
}

/// THE seam, by coord session id. Validates `claude_id`, settles any refusal
/// the drain recorded for this session, then asks the registry to re-key —
/// which queues its own `state_change` only when the record moves.
pub fn confirm_for_session(
    registry: &SessionRegistry,
    coord_session_id: Uuid,
    claude_id: &str,
    source: RekeySource,
) -> RekeyOutcome {
    let ledger = registry.coord_sync().rekeys();
    let claude_id = claude_id.trim();
    if Uuid::parse_str(claude_id).is_err() {
        ledger.note_invalid();
        tracing::warn!(
            coord_session_id = %coord_session_id,
            session_id = %claude_id,
            source = source.as_str(),
            "coord identity: harness id is not a uuid — not re-keyed"
        );
        return RekeyOutcome::Invalid;
    }
    settle_session_refusal(registry, coord_session_id);

    match registry.confirm_claude_code_session_id(coord_session_id, claude_id, source) {
        Ok(true) => {
            ledger.note_queued(source);
            tracing::info!(
                session_id = %claude_id,
                coord_session_id = %coord_session_id,
                source = source.as_str(),
                "coord row re-keyed to the CONFIRMED harness session id (queued)"
            );
            RekeyOutcome::Queued
        }
        Ok(false) => {
            tracing::debug!(
                coord_session_id = %coord_session_id,
                source = source.as_str(),
                last_landed = ?ledger.last_landed(coord_session_id),
                "coord identity: row already carries the confirmed harness session id"
            );
            RekeyOutcome::AlreadyCurrent
        }
        Err(SessionError::NotFound(_)) => {
            ledger.note_no_coord_row();
            tracing::info!(
                coord_session_id = %coord_session_id,
                source = source.as_str(),
                "coord identity: the registry holds no record for this coord row — not re-keyed"
            );
            RekeyOutcome::NoCoordRow
        }
        Err(SessionError::InvalidClaudeCodeSessionId(_)) => {
            ledger.note_invalid();
            RekeyOutcome::Invalid
        }
        Err(e) => {
            tracing::warn!(
                coord_session_id = %coord_session_id,
                session_id = %claude_id,
                source = source.as_str(),
                "coord identity: re-key not queued: {e}"
            );
            RekeyOutcome::OutboxFailed
        }
    }
}

/// THE seam, by terminal: `coord_row` is that terminal's
/// `TerminalSession::coord_session_id()` (resolved by the caller, so this stays
/// testable without a `TerminalManager`).
pub fn confirm_for_terminal(
    registry: &SessionRegistry,
    terminal_id: &str,
    coord_row: Option<Uuid>,
    claude_id: &str,
    source: RekeySource,
) -> RekeyOutcome {
    if Uuid::parse_str(claude_id.trim()).is_err() {
        // Same order as the row-bound arm: a malformed id is Invalid whether
        // or not the terminal has a row yet.
        return confirm_for_session(registry, Uuid::nil(), claude_id, source);
    }
    let Some(coord_session_id) = coord_row else {
        // Normal and transient: the coord mirror is registered after the pane
        // exists. Not a warn.
        registry.coord_sync().rekeys().note_no_coord_row();
        tracing::info!(
            terminal_id = %terminal_id,
            session_id = %claude_id,
            source = source.as_str(),
            "coord identity: no coord session bound to this terminal yet — not re-keyed"
        );
        return RekeyOutcome::NoCoordRow;
    };
    confirm_for_session(registry, coord_session_id, claude_id, source)
}

/// Feed one reconcile pass's rung-2 binds through the seam (D5): only a
/// `Bind` graded [`BindOrigin::Authoritative`] (the anchor's own argv) — never
/// a rung-3 `Observed` or rung-4 `Reconciled` correlation. At most one seam
/// call per terminal, and none for a terminal whose anchor has a valid
/// provider row this tick (`has_valid_provider_row`; the provider-registry
/// source is the stronger, current evidence). Until Phase 1b lands that
/// predicate is always `false`.
pub fn rekey_rung2_binds(
    registry: &SessionRegistry,
    actions: &[ReconcileAction],
    coord_row_for: impl Fn(&str) -> Option<Uuid>,
    has_valid_provider_row: impl Fn(&str) -> bool,
) -> Vec<(String, RekeyOutcome)> {
    let mut seen: HashSet<&str> = HashSet::new();
    let mut out = Vec::new();
    for action in actions {
        let ReconcileAction::Bind {
            terminal_id,
            session_id,
            origin: BindOrigin::Authoritative,
            ..
        } = action
        else {
            continue;
        };
        if !seen.insert(terminal_id.as_str()) || has_valid_provider_row(terminal_id) {
            continue;
        }
        let outcome = confirm_for_terminal(
            registry,
            terminal_id,
            coord_row_for(terminal_id),
            session_id,
            RekeySource::Rung2,
        );
        out.push((terminal_id.clone(), outcome));
    }
    out
}

// ---------------------------------------------------------------------------
// Tauri-state adapters (production callers)
// ---------------------------------------------------------------------------

fn app_parts(
    app: &tauri::AppHandle,
) -> Option<(Arc<crate::terminal::TerminalManager>, Arc<SessionRegistry>)> {
    use tauri::Manager;
    Some((
        app.try_state::<Arc<crate::terminal::TerminalManager>>()?
            .inner()
            .clone(),
        app.try_state::<Arc<SessionRegistry>>()?.inner().clone(),
    ))
}

/// [`confirm_for_terminal`] against the app's own terminal manager + registry.
/// `None` when either is not in Tauri state (a unit-test handle, or a boot
/// window before `app.manage`).
pub fn confirm_for_terminal_in_app(
    app: &tauri::AppHandle,
    terminal_id: &str,
    claude_id: &str,
    source: RekeySource,
) -> Option<RekeyOutcome> {
    let Some((terminals, registry)) = app_parts(app) else {
        tracing::warn!(
            terminal_id = %terminal_id,
            session_id = %claude_id,
            source = source.as_str(),
            "coord identity: terminal manager or session registry not in Tauri state — \
             re-key skipped (best-effort)"
        );
        return None;
    };
    let coord_row = terminals
        .get(terminal_id)
        .and_then(|s| s.coord_session_id());
    Some(confirm_for_terminal(
        &registry,
        terminal_id,
        coord_row,
        claude_id,
        source,
    ))
}

/// One reconcile tick's coord-identity work, in order: settle the refusals the
/// drain recorded since the last tick, then feed the pass's rung-2 binds
/// through the seam. Pure over its inputs (the terminal → coord-row lookup is
/// injected), so the whole tick composition is testable against a fake coord.
pub fn apply_reconcile_tick(
    registry: &SessionRegistry,
    actions: &[ReconcileAction],
    coord_row_for: impl Fn(&str) -> Option<Uuid>,
) -> Vec<(String, RekeyOutcome)> {
    settle_refusals(registry);
    rekey_rung2_binds(
        registry,
        actions,
        coord_row_for,
        // Phase 1b (the provider-registry source) is not on this build: no
        // terminal has a valid provider row yet, so rung 2 is never pre-empted.
        |_| false,
    )
}

/// [`apply_reconcile_tick`] against the app's terminal manager + registry.
/// Fire-and-forget; a no-op when either is not in Tauri state.
pub fn apply_reconcile_tick_in_app(app: &tauri::AppHandle, actions: &[ReconcileAction]) {
    let Some((terminals, registry)) = app_parts(app) else {
        return;
    };
    apply_reconcile_tick(&registry, actions, |t| {
        terminals.get(t).and_then(|s| s.coord_session_id())
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::coord_sync::CoordSync;
    use crate::session::local_store::{OutboxRecord, OutboxWriter};
    use crate::session::{
        DynTransport, Intent, SessionEventKind, SessionKind, SessionTransports, Transport,
        TransportError, TransportHandle,
    };
    use std::time::Duration;

    struct Noop;
    impl Transport for Noop {
        fn start(&self, _i: &Intent) -> Result<TransportHandle, TransportError> {
            Ok(TransportHandle::External)
        }
        fn write_input(&self, _h: &TransportHandle, _b: &[u8]) -> Result<(), TransportError> {
            Ok(())
        }
        fn resize(&self, _h: &TransportHandle, _c: u16, _r: u16) -> Result<(), TransportError> {
            Ok(())
        }
        fn close(&self, _h: &TransportHandle) -> Result<(), TransportError> {
            Ok(())
        }
        fn tap_output(
            &self,
            _h: &TransportHandle,
        ) -> Option<tokio::sync::broadcast::Receiver<String>> {
            None
        }
    }

    fn intent() -> Intent {
        Intent {
            kind: SessionKind::TerminalShell,
            purpose: "coord identity test".into(),
            repo: Some("qontinui-runner".into()),
            branch: Some("main".into()),
            work_unit_slug: None,
            plan_slug: None,
            correlation_topic: None,
            page_id: None,
            declared_paths: vec![],
            share_output: false,
            redact_secrets: None,
            tenant_id: None,
        }
    }

    /// A registry whose coord is unreachable — the seam's outbox side is all
    /// these tests observe.
    fn registry(dir: &std::path::Path) -> (Arc<SessionRegistry>, Arc<OutboxWriter>) {
        let outbox = Arc::new(OutboxWriter::open(dir.join("outbox.jsonl")).unwrap());
        let coord = CoordSync::new_for_test(
            outbox.clone(),
            "http://127.0.0.1:1".into(),
            Duration::from_secs(3600),
            Duration::from_secs(3600),
        );
        let transports = SessionTransports {
            pty: Arc::new(Noop) as DynTransport,
            claude_cli: Arc::new(Noop) as DynTransport,
            workflow: Arc::new(Noop) as DynTransport,
        };
        let reg = SessionRegistry::new(Uuid::new_v4(), transports, coord.clone());
        coord.attach_registry(&reg);
        (reg, outbox)
    }

    fn rekeys_for(outbox: &OutboxWriter, session: Uuid) -> Vec<OutboxRecord> {
        outbox
            .pending()
            .unwrap()
            .into_iter()
            .filter(|r| {
                r.session_id == session
                    && r.event_kind == SessionEventKind::StateChange.as_str()
                    && r.payload.get("claude_code_session_id").is_some()
            })
            .collect()
    }

    fn bind(terminal: &str, id: &str, origin: BindOrigin) -> ReconcileAction {
        ReconcileAction::Bind {
            terminal_id: terminal.into(),
            session_id: id.into(),
            config_dir: String::new(),
            origin,
            confirmed: false,
        }
    }

    const PIN: &str = "2711c738-0000-4000-8000-000000000001";
    const LIVE: &str = "44348633-0000-4000-8000-000000000002";

    /// A rung-2 bind for a terminal with coord row R queues exactly one
    /// `state_change` for R carrying the id; the next tick queues none.
    #[test]
    fn a_rung2_bind_queues_one_rekey_and_the_next_tick_none() {
        let _amb = crate::test_env::isolated_ambient();
        let dir = tempfile::tempdir().unwrap();
        let (reg, outbox) = registry(dir.path());
        let row = reg
            .register_external_with_lineage(intent(), None, Some(PIN.into()))
            .unwrap();
        let actions = vec![
            bind("t1", LIVE, BindOrigin::Authoritative),
            // A duplicate bind for the same terminal in one pass is ignored.
            bind("t1", LIVE, BindOrigin::Authoritative),
        ];
        let lookup = |t: &str| (t == "t1").then_some(row);

        let first = rekey_rung2_binds(&reg, &actions, lookup, |_| false);
        assert_eq!(first, vec![("t1".to_string(), RekeyOutcome::Queued)]);
        let rows = rekeys_for(&outbox, row);
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].payload["claude_code_session_id"], json!(LIVE));
        assert_eq!(rows[0].payload["rekey_source"], json!("rung2"));
        assert_eq!(
            reg.coord_sync().rekeys().counts(RekeySource::Rung2).queued,
            1
        );

        let second = rekey_rung2_binds(&reg, &actions, lookup, |_| false);
        assert_eq!(
            second,
            vec![("t1".to_string(), RekeyOutcome::AlreadyCurrent)]
        );
        assert_eq!(
            rekeys_for(&outbox, row).len(),
            1,
            "the next tick queues none"
        );
    }

    /// A terminal with a valid provider row is not re-keyed from rung 2
    /// (the precedence hook Phase 1b fills in).
    #[test]
    fn a_valid_provider_row_pre_empts_rung2() {
        let _amb = crate::test_env::isolated_ambient();
        let dir = tempfile::tempdir().unwrap();
        let (reg, outbox) = registry(dir.path());
        let row = reg
            .register_external_with_lineage(intent(), None, Some(PIN.into()))
            .unwrap();
        let out = rekey_rung2_binds(
            &reg,
            &[bind("t1", LIVE, BindOrigin::Authoritative)],
            |_| Some(row),
            |_| true,
        );
        assert!(out.is_empty());
        assert!(rekeys_for(&outbox, row).is_empty());
    }

    /// Rung-3 (`Observed`) and rung-4 (`Reconciled`) binds never re-key.
    #[test]
    fn a_rung3_or_rung4_bind_queues_nothing() {
        let _amb = crate::test_env::isolated_ambient();
        let dir = tempfile::tempdir().unwrap();
        let (reg, outbox) = registry(dir.path());
        let row = reg
            .register_external_with_lineage(intent(), None, Some(PIN.into()))
            .unwrap();
        let out = rekey_rung2_binds(
            &reg,
            &[
                bind("t1", LIVE, BindOrigin::Observed),
                bind("t2", LIVE, BindOrigin::Reconciled),
            ],
            |_| Some(row),
            |_| false,
        );
        assert!(out.is_empty(), "{out:?}");
        assert!(rekeys_for(&outbox, row).is_empty());
        assert_eq!(
            reg.coord_sync().rekeys().counts(RekeySource::Rung2),
            SourceCounts::default()
        );
    }

    /// An id the row already carries queues nothing.
    #[test]
    fn an_already_current_id_queues_nothing() {
        let _amb = crate::test_env::isolated_ambient();
        let dir = tempfile::tempdir().unwrap();
        let (reg, outbox) = registry(dir.path());
        let row = reg
            .register_external_with_lineage(intent(), None, Some(PIN.into()))
            .unwrap();
        assert_eq!(
            confirm_for_terminal(&reg, "t1", Some(row), PIN, RekeySource::Hook),
            RekeyOutcome::AlreadyCurrent
        );
        assert!(rekeys_for(&outbox, row).is_empty());
        assert_eq!(
            reg.coord_sync().rekeys().counts(RekeySource::Hook).queued,
            0
        );
    }

    /// No coord row on the terminal → `NoCoordRow`, counted; a row id the
    /// registry does not hold is the same answer.
    #[test]
    fn a_terminal_with_no_coord_row_is_no_coord_row() {
        let _amb = crate::test_env::isolated_ambient();
        let dir = tempfile::tempdir().unwrap();
        let (reg, outbox) = registry(dir.path());
        assert_eq!(
            confirm_for_terminal(&reg, "t1", None, LIVE, RekeySource::Rung2),
            RekeyOutcome::NoCoordRow
        );
        assert_eq!(
            confirm_for_terminal(&reg, "t1", Some(Uuid::new_v4()), LIVE, RekeySource::Rung2),
            RekeyOutcome::NoCoordRow
        );
        assert_eq!(reg.coord_sync().rekeys().no_coord_row(), 2);
        assert!(outbox.pending().unwrap().is_empty());
    }

    /// A non-UUID id → `Invalid`, counted, nothing queued, record unmoved.
    #[test]
    fn a_non_uuid_id_is_invalid() {
        let _amb = crate::test_env::isolated_ambient();
        let dir = tempfile::tempdir().unwrap();
        let (reg, outbox) = registry(dir.path());
        let row = reg
            .register_external_with_lineage(intent(), None, Some(PIN.into()))
            .unwrap();
        assert_eq!(
            confirm_for_terminal(&reg, "t1", Some(row), "not-a-uuid", RekeySource::Rung2),
            RekeyOutcome::Invalid
        );
        assert_eq!(
            confirm_for_terminal(&reg, "t1", None, "not-a-uuid", RekeySource::Hook),
            RekeyOutcome::Invalid
        );
        assert_eq!(reg.coord_sync().rekeys().invalid(), 2);
        assert!(rekeys_for(&outbox, row).is_empty());
        assert_eq!(
            reg.describe_by_id(row)
                .unwrap()
                .claude_code_session_id
                .as_deref(),
            Some(PIN)
        );
    }

    /// `/health` shape: every source present under each counter.
    #[test]
    fn health_json_serves_every_source_under_every_counter() {
        let ledger = RekeyLedger::default();
        ledger.note_queued(RekeySource::R2Resume);
        ledger.note_landed(Uuid::new_v4(), LIVE, RekeySource::Rung2);
        let j = ledger.health_json();
        for k in ["queued", "landed", "refused"] {
            for src in RekeySource::ALL {
                assert!(j[k][src.as_str()].is_u64(), "{k}.{}: {j}", src.as_str());
            }
        }
        assert_eq!(j["queued"]["r2_resume"], json!(1));
        assert_eq!(j["landed"]["rung2"], json!(1));
        assert_eq!(j["no_coord_row"], json!(0));
        assert_eq!(j["invalid"], json!(0));
    }

    #[test]
    fn rekey_source_from_payload() {
        assert_eq!(
            RekeySource::from_payload(&json!({})),
            Some(RekeySource::Hook)
        );
        assert_eq!(
            RekeySource::from_payload(&json!({"rekey_source": "r2_resume"})),
            Some(RekeySource::R2Resume)
        );
        assert_eq!(
            RekeySource::from_payload(&json!({"rekey_source": "x"})),
            None
        );
    }
}
