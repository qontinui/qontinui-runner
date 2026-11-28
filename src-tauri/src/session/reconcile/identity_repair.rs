//! Repair a record's identity — its `config_dir`, and its session id — from
//! the pane's own provider row (plan
//! `2026-11-16-boot-reconcile-reverts-a-migrated-sessions-config-dir-so-the-next-migration-carries-a-stale-transcript`,
//! D2; the re-key: plan
//! `2026-11-27-coord-message-to-cross-account-pane-never-pushed`, item 1).
//!
//! ## Why this exists
//!
//! A record's `config_dir` is the account the migration reads as its SOURCE.
//! Once that value is wrong, the next usage-limit migration confirms exhaustion
//! against the wrong account and carries the wrong root's copy of the
//! transcript. The bind pass cannot correct it: [`super::run_reconcile`] leaves
//! a CONFIRMED terminal alone before any work. On device 4b426e2b (2026-11-15)
//! a session sat confirmed on `~/.claude` for 7 h while it ran under
//! `.claude-erik`; the provider registry logged `.claude-erik` evidence for its
//! pane repeatedly, and nothing read it.
//!
//! The provider row (`<config_dir>/sessions/<pid>.json`, validated against the
//! pane's claude anchor pid, [`super::ProviderRows`]) is the pid-exact answer to
//! which account a live session runs under. This pass runs on every tick,
//! confirmed rows included, and writes that answer onto the record through the
//! sticky identity overlay, together with the account label and wrapper derived
//! from the same dir, so the three cannot disagree.
//!
//! ## The re-key
//!
//! The same row also says WHICH session the pane runs. A pane spawned with a
//! pinned `--session-id` whose claude was then started under another account
//! (an account shortcut typed into the pane) runs a different id than its
//! record is keyed by. The coord row and the session registry follow the
//! provider id; the lifecycle record did not, so the session-message poller,
//! which resolves `to_session` through the record, never found the pane and
//! every coord message to the live id stayed pending. When a valid row on a
//! terminal names a session no open record on that terminal is keyed by, and
//! exactly one open record sits on that terminal, that record is re-keyed to
//! the row's id ([`SessionLifecycleStore::rekey_identity_if_unchanged`]), with
//! the row's grade (`observed`, confirmed) and its config dir in the same
//! write. Every guard below applies to it too, plus one: an id that already
//! keys another open record is never re-keyed onto (logged once per pair).
//!
//! ## Guards
//!
//! - **Decided from a pre-evidence snapshot, written by compare-and-set.** The
//!   caller takes [`PreEvidenceSnapshot`] BEFORE it reads the tick's provider
//!   rows. Each write lands, under the store's lock
//!   ([`SessionLifecycleStore::update_identity_if_unchanged`]), only when the
//!   record is still open on the snapshot's terminal with the snapshot's dir.
//!   A write in between that changed either (the migration's `record_open`, a
//!   reconcile bind's re-terminal, the SessionStart hook, the zone backstop)
//!   makes this tick skip it, and the next tick decides again from fresh
//!   evidence. The key is `(state, terminal_id, config_dir)`, not a version, so
//!   writes in between that leave all three as they were (an A -> B -> A
//!   rewrite) pass. Without it, a boot bind that moved a migrated row back onto
//!   its still-live old pane handed the repair that pane's row, and the repair
//!   wrote the old account: the incident's end state.
//! - **Own pane only.** A record is repaired only from a valid row on ITS OWN
//!   `terminal_id` naming ITS OWN session id. A row on another pane says nothing
//!   about where this record's session runs.
//! - **Overlap.** When valid rows on two or more terminals name the same session
//!   id (an account migration's settle window: the old and the new pane both run
//!   `claude --resume <sid>`), nothing is written for that session this tick.
//! - **Another pane's argv.** A valid row is not the only sign of a second pane:
//!   a migration's new pane may not have written its provider row yet. So
//!   nothing is written for a session whose id is named by the claude anchor
//!   argv (`--resume` / `--session-id`, [`AnchorArgvIds`]) of any OTHER live
//!   pane. This closes the boot-then-next-tick path: the boot bind re-terminals
//!   a migrated row onto the old pane keeping the new dir, and on the next tick
//!   the snapshot agrees with the record, the old pane's row is the only valid
//!   one, and only the new pane's argv says the session has moved. An
//!   unreadable argv table ([`AnchorArgvIds::Unknown`]) writes nothing at all.
//! - **No-op on agreement.** Spellings of one root compare equal
//!   ([`config_dir_compare_key`]), so the pass writes only a real change.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use super::{ProviderRows, SessionBoundPayload};
use crate::process_capture::process_tree::{command_lines_for_pids, parse_session_id_from_cmdline};
use crate::session::past_sessions::account_from_config_dir;
use crate::session::session_lifecycle_store::{
    IdentityCas, RekeyOutcome, SessionIdentityUpdate, SessionLifecycleStore,
};
use qontinui_runner_lib::session_archive::discovery::config_dir_compare_key;

/// Every open record's `(terminal_id, config_dir)`, read BEFORE the tick's
/// provider evidence. The repair decides from it and writes only while the
/// record still matches it.
#[derive(Debug, Clone, Default)]
pub struct PreEvidenceSnapshot {
    by_session: HashMap<String, (String, Option<String>)>,
}

impl PreEvidenceSnapshot {
    /// Read the store now. Call it before `compute_provider_rows`, so every
    /// write that lands after the read is visible to the compare-and-set.
    pub fn take(store: &SessionLifecycleStore) -> Self {
        Self {
            by_session: store
                .open_records()
                .into_iter()
                .map(|r| (r.claude_session_id, (r.terminal_id, r.config_dir)))
                .collect(),
        }
    }

    /// The open records on each terminal, as `(session id, config dir)`.
    fn by_terminal(&self) -> HashMap<&str, Vec<(&str, Option<&str>)>> {
        let mut out: HashMap<&str, Vec<(&str, Option<&str>)>> = HashMap::new();
        for (session_id, (terminal_id, dir)) in &self.by_session {
            out.entry(terminal_id.as_str())
                .or_default()
                .push((session_id.as_str(), dir.as_deref()));
        }
        out
    }
}

/// The session id each live pane's claude anchor was started with, read
/// from its argv (`--resume` / `-r` / `--session-id`,
/// [`parse_session_id_from_cmdline`]) once per tick.
#[derive(Debug, Clone)]
pub enum AnchorArgvIds {
    /// The command-line read failed: whether another pane runs a session is
    /// UNKNOWN, so [`repair_identities`] writes nothing this tick.
    Unknown,
    /// Terminal id -> the session id its anchor's argv names. A terminal whose
    /// argv names no id, or whose anchor had exited (a dead pid is absent from
    /// the read), is absent here.
    Known(HashMap<String, String>),
}

impl AnchorArgvIds {
    /// Read the argv of every anchor `rows` holds (valid row or not), in ONE
    /// targeted command-line query on the blocking pool. No query at all when
    /// no record is a repair candidate this tick, so a settled fleet pays
    /// nothing.
    pub async fn read(snapshot: &PreEvidenceSnapshot, rows: &ProviderRows) -> Self {
        if plan_repairs(snapshot, rows).is_empty() && plan_rekeys(snapshot, rows).is_empty() {
            return Self::Known(HashMap::new());
        }
        let mut terminals_by_pid: HashMap<u32, Vec<String>> = HashMap::new();
        for (terminal_id, (pid, _), _) in rows.entries() {
            if pid != 0 {
                terminals_by_pid
                    .entry(pid)
                    .or_default()
                    .push(terminal_id.to_string());
            }
        }
        let pids: Vec<u32> = terminals_by_pid.keys().copied().collect();
        let Some(lines) = command_lines_for_pids(&pids).await else {
            return Self::Unknown;
        };
        let mut by_terminal = HashMap::new();
        for (pid, line) in &lines {
            let (Some(sid), Some(terminals)) = (
                parse_session_id_from_cmdline(line),
                terminals_by_pid.get(pid),
            ) else {
                continue;
            };
            for terminal_id in terminals {
                by_terminal.insert(terminal_id.clone(), sid.clone());
            }
        }
        Self::Known(by_terminal)
    }

    /// Known ids from `(terminal id, session id)` pairs.
    #[cfg(test)]
    pub fn from_pairs<'a>(pairs: impl IntoIterator<Item = (&'a str, &'a str)>) -> Self {
        Self::Known(
            pairs
                .into_iter()
                .map(|(t, s)| (t.to_string(), s.to_string()))
                .collect(),
        )
    }

    /// No pane's argv names any session (a read that found none).
    #[cfg(test)]
    pub fn none() -> Self {
        Self::Known(HashMap::new())
    }
}

/// One identity correction the pass wrote (for logging, the tab-stamp event
/// and tests): a `config_dir` repair, or a re-key (which carries the dir too).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentityRepair {
    pub terminal_id: String,
    /// The session id the record is keyed by after the write.
    pub session_id: String,
    /// The id the record was keyed by before a re-key; `None` for a
    /// `config_dir`-only repair.
    pub rekeyed_from: Option<String>,
    /// The value the record held before the repair (`None` = unknown).
    pub from: Option<String>,
    /// The dir the pane's provider row was read from.
    pub to: String,
    /// The record's evidence grade as written (empty when it has none).
    pub origin: String,
    /// Whether the record is confirmed.
    pub confirmed: bool,
}

/// A repair the snapshot and the valid rows alone call for, before the argv
/// guard and the compare-and-set.
struct PlannedRepair<'a> {
    session_id: &'a str,
    terminal_id: &'a str,
    from: Option<&'a str>,
    to: &'a str,
}

/// Every open record in `snapshot` whose own pane's valid row names its
/// session under another root, with no other VALID row naming that session.
fn plan_repairs<'a>(
    snapshot: &'a PreEvidenceSnapshot,
    rows: &'a ProviderRows,
) -> Vec<PlannedRepair<'a>> {
    let mut row_by_terminal: HashMap<&str, (&str, &Path)> = HashMap::new();
    let mut terminals_by_session: HashMap<&str, HashSet<&str>> = HashMap::new();
    for (terminal_id, session_id, config_dir) in rows.valid() {
        row_by_terminal.insert(terminal_id, (session_id, config_dir));
        terminals_by_session
            .entry(session_id)
            .or_default()
            .insert(terminal_id);
    }
    if row_by_terminal.is_empty() {
        return Vec::new();
    }

    let mut out = Vec::new();
    for (session_id, (terminal_id, from)) in &snapshot.by_session {
        let Some(&(row_session, row_dir)) = row_by_terminal.get(terminal_id.as_str()) else {
            continue;
        };
        if row_session != session_id {
            continue; // the pane runs another session: `plan_rekeys`' case
        }
        if terminals_by_session
            .get(row_session)
            .is_some_and(|terminals| terminals.len() > 1)
        {
            continue; // the overlap guard
        }
        let Some(to) = row_dir.to_str() else {
            continue;
        };
        if from
            .as_deref()
            .is_some_and(|from| config_dir_compare_key(from) == config_dir_compare_key(to))
        {
            continue; // no-op on agreement
        }
        out.push(PlannedRepair {
            session_id,
            terminal_id,
            from: from.as_deref(),
            to,
        });
    }
    out
}

/// A re-key the snapshot and the valid rows alone call for, before the argv
/// guard and the compare-and-set.
struct PlannedRekey<'a> {
    old_id: &'a str,
    new_id: &'a str,
    terminal_id: &'a str,
    from: Option<&'a str>,
    to: &'a str,
}

/// Every terminal whose valid row names a session that no open record on that
/// terminal is keyed by, where exactly ONE open record sits on the terminal
/// (two would leave which one the pane runs ambiguous), no other terminal's
/// valid row names the row's session (the overlap guard), and no open record
/// anywhere is already keyed by it (the occupied guard, logged once per pair).
fn plan_rekeys<'a>(
    snapshot: &'a PreEvidenceSnapshot,
    rows: &'a ProviderRows,
) -> Vec<PlannedRekey<'a>> {
    let mut terminals_by_session: HashMap<&str, usize> = HashMap::new();
    for (_, session_id, _) in rows.valid() {
        *terminals_by_session.entry(session_id).or_default() += 1;
    }
    if terminals_by_session.is_empty() {
        return Vec::new();
    }
    let by_terminal = snapshot.by_terminal();
    let mut out = Vec::new();
    for (terminal_id, new_id, row_dir) in rows.valid() {
        let Some(records) = by_terminal.get(terminal_id) else {
            continue; // no open record on the pane: the bind pass's case
        };
        if records.iter().any(|(sid, _)| *sid == new_id) {
            continue; // already keyed by the live id: at most a dir repair
        }
        let [(old_id, from)] = records.as_slice() else {
            continue; // two open records on one pane: which one runs is unknown
        };
        if terminals_by_session.get(new_id).copied().unwrap_or(0) > 1 {
            continue; // the overlap guard
        }
        if let Some((other_terminal, _)) = snapshot.by_session.get(new_id) {
            note_occupied(old_id, new_id, other_terminal);
            continue; // the occupied guard
        }
        let Some(to) = row_dir.to_str() else {
            continue;
        };
        out.push(PlannedRekey {
            old_id,
            new_id,
            terminal_id,
            from: *from,
            to,
        });
    }
    out
}

/// Log, at `info!` and ONCE per `(old id, new id)` pair, that a re-key was
/// refused because the pane's live id already keys another record.
fn note_occupied(old_id: &str, new_id: &str, other_terminal: &str) {
    static SEEN: std::sync::OnceLock<std::sync::Mutex<HashSet<(String, String)>>> =
        std::sync::OnceLock::new();
    let first = SEEN
        .get_or_init(Default::default)
        .lock()
        .map(|mut seen| seen.insert((old_id.to_string(), new_id.to_string())))
        .unwrap_or(false);
    if first {
        tracing::info!(
            record_session = %old_id,
            provider_session = %new_id,
            other_terminal = %other_terminal,
            "session reconcile: re-key refused — the pane's live session id already keys another record"
        );
    }
}

/// Correct every open record whose `config_dir` disagreed, at `snapshot` time,
/// with its own pane's valid provider row this tick, and re-key every record
/// whose pane's valid row names another session ([`plan_rekeys`]). Returns the
/// corrections that landed.
///
/// Fail-open: no valid rows, a record that changed since `snapshot`, a session
/// another live pane's argv names, an UNKNOWN argv read, or a dir that is not
/// valid UTF-8 writes nothing.
pub fn repair_identities(
    store: &SessionLifecycleStore,
    snapshot: &PreEvidenceSnapshot,
    rows: &ProviderRows,
    argv: &AnchorArgvIds,
) -> Vec<IdentityRepair> {
    if let Ok(mut log) = repair_log().lock() {
        log.retain_sessions(|sid| snapshot.by_session.contains_key(sid));
    }
    let planned = plan_repairs(snapshot, rows);
    let rekeys = plan_rekeys(snapshot, rows);
    if planned.is_empty() && rekeys.is_empty() {
        return Vec::new();
    }
    let argv_ids = match argv {
        AnchorArgvIds::Known(ids) => ids,
        AnchorArgvIds::Unknown => {
            tracing::debug!(
                candidates = planned.len() + rekeys.len(),
                "session reconcile: identity repair skipped — the anchors' command lines could not be read"
            );
            return Vec::new();
        }
    };

    let mut out = Vec::new();
    for PlannedRepair {
        session_id,
        terminal_id,
        from,
        to,
    } in planned
    {
        if argv_ids
            .iter()
            .any(|(t, sid)| t != terminal_id && sid == session_id)
        {
            tracing::debug!(
                terminal_id = %terminal_id,
                claude_session = %session_id,
                "session reconcile: config_dir repair skipped — another live pane's argv names this session"
            );
            continue; // the argv guard
        }
        let account = account_from_config_dir(Some(to));
        let update = SessionIdentityUpdate {
            config_dir: Some(to.to_string()),
            account_label: Some(account.label),
            account_wrapper: Some(account.wrapper),
            ..Default::default()
        };
        match store.update_identity_if_unchanged(session_id, terminal_id, from, &update) {
            IdentityCas::Written(rec) => {
                note_repair(terminal_id, session_id, from, to);
                out.push(IdentityRepair {
                    terminal_id: terminal_id.to_string(),
                    session_id: session_id.to_string(),
                    rekeyed_from: None,
                    from: from.map(str::to_string),
                    to: to.to_string(),
                    origin: rec.origin.clone().unwrap_or_default(),
                    confirmed: rec.confirmed_at.is_some(),
                });
            }
            IdentityCas::Stale => {
                tracing::debug!(
                    terminal_id = %terminal_id,
                    claude_session = %session_id,
                    "session reconcile: config_dir repair skipped — the record changed after this tick's pre-evidence snapshot"
                );
            }
            IdentityCas::NoChange | IdentityCas::Missing | IdentityCas::Unavailable => {}
        }
    }

    let now_ms = chrono::Utc::now().timestamp_millis();
    for PlannedRekey {
        old_id,
        new_id,
        terminal_id,
        from,
        to,
    } in rekeys
    {
        if argv_ids
            .iter()
            .any(|(t, sid)| t != terminal_id && (sid == new_id || sid == old_id))
        {
            tracing::debug!(
                terminal_id = %terminal_id,
                record_session = %old_id,
                provider_session = %new_id,
                "session reconcile: re-key skipped — another live pane's argv names one of the two sessions"
            );
            continue; // the argv guard
        }
        let account = account_from_config_dir(Some(to));
        let update = SessionIdentityUpdate {
            config_dir: Some(to.to_string()),
            account_label: Some(account.label),
            account_wrapper: Some(account.wrapper),
            ..Default::default()
        };
        match store.rekey_identity_if_unchanged(old_id, new_id, terminal_id, from, &update, now_ms)
        {
            RekeyOutcome::Rekeyed(rec) => {
                // `rekey_session`'s contract: launch-failure liveness is keyed
                // by id, so the new id is noted alive here.
                crate::terminal::launch_failure::registry().note_alive(new_id, now_ms);
                tracing::info!(
                    terminal_id = %terminal_id,
                    from_session = %old_id,
                    to_session = %new_id,
                    config_dir = %to,
                    "session reconcile: re-keyed the lifecycle record to the pane's provider session id"
                );
                out.push(IdentityRepair {
                    terminal_id: terminal_id.to_string(),
                    session_id: new_id.to_string(),
                    rekeyed_from: Some(old_id.to_string()),
                    from: from.map(str::to_string),
                    to: to.to_string(),
                    origin: rec.origin.clone().unwrap_or_default(),
                    confirmed: rec.confirmed_at.is_some(),
                });
            }
            RekeyOutcome::Occupied => note_occupied(old_id, new_id, terminal_id),
            RekeyOutcome::Stale => {
                tracing::debug!(
                    terminal_id = %terminal_id,
                    record_session = %old_id,
                    "session reconcile: re-key skipped — the record changed after this tick's pre-evidence snapshot"
                );
            }
            RekeyOutcome::Missing | RekeyOutcome::Unavailable => {}
        }
    }
    out
}

/// The `session-bound` tab-stamp payloads for the repairs that landed, each
/// carrying the repaired config dir. Pure; the caller emits.
pub fn repair_bound_payloads(repairs: &[IdentityRepair]) -> Vec<SessionBoundPayload> {
    repairs
        .iter()
        .map(|r| SessionBoundPayload {
            terminal_id: r.terminal_id.clone(),
            session_id: r.session_id.clone(),
            config_dir: r.to.clone(),
            origin: r.origin.clone(),
            confirmed: r.confirmed,
            // A dir repair is inferred by the runner. A re-key carries the id
            // Claude Code wrote about itself into its own registry row for
            // this pane's pid, so it may correct the id the tab holds.
            provider_reported: r.rekeyed_from.is_some(),
        })
        .collect()
}

/// How often each `(from, to)` repair of each open session has landed.
///
/// A writer that keeps putting the old dir back makes the repair recur, and a
/// log line printed only the first time would go quiet on exactly that. So the
/// 1st, 2nd, 4th, 8th… repair of one `(session, from, to)` is logged, with its
/// count. Bounded: sessions no longer open are forgotten on every pass.
#[derive(Debug, Default)]
struct RepairLog {
    counts: HashMap<String, HashMap<(String, String), u64>>,
}

impl RepairLog {
    /// Count one repair. `Some(count)` when this one is logged.
    fn note(&mut self, session_id: &str, from: &str, to: &str) -> Option<u64> {
        let count = self
            .counts
            .entry(session_id.to_string())
            .or_default()
            .entry((from.to_string(), to.to_string()))
            .or_insert(0);
        *count += 1;
        count.is_power_of_two().then_some(*count)
    }

    /// Keep only the sessions `open` says are still open.
    fn retain_sessions(&mut self, open: impl Fn(&str) -> bool) {
        self.counts.retain(|session_id, _| open(session_id));
    }
}

fn repair_log() -> &'static std::sync::Mutex<RepairLog> {
    static LOG: std::sync::OnceLock<std::sync::Mutex<RepairLog>> = std::sync::OnceLock::new();
    LOG.get_or_init(Default::default)
}

/// Count a landed repair and log it at `info!` when [`RepairLog::note`] says so.
fn note_repair(terminal_id: &str, session_id: &str, from: Option<&str>, to: &str) {
    let from = from.unwrap_or("<none>");
    let Some(count) = repair_log()
        .lock()
        .ok()
        .and_then(|mut log| log.note(session_id, from, to))
    else {
        return;
    };
    tracing::info!(
        terminal_id = %terminal_id,
        claude_session = %session_id,
        from = %from,
        to = %to,
        repairs = count,
        "session reconcile: repaired config_dir from the pane's provider row"
    );
}

#[cfg(test)]
mod tests {
    use super::super::{bind_record, run_reconcile, LivePty, TranscriptCandidate, TranscriptIndex};
    use super::*;
    use crate::process_capture::process_tree::ProcessSnapshot;
    use crate::session::claude_activity::ProviderRowValidity;
    use crate::session::session_lifecycle_store::{
        ORIGIN_AUTHORITATIVE, ORIGIN_OBSERVED, UNZONED_ZONE_INDEX,
    };
    use std::path::PathBuf;
    use tempfile::tempdir;

    const ROOT_A: &str = "/home/u/.claude";
    const ROOT_B: &str = "/home/u/.claude-paktis";
    const ROOT_C: &str = "/home/u/.claude-gmail";

    fn live(terminal_id: &str, ai_pid: u32) -> LivePty {
        LivePty {
            terminal_id: terminal_id.to_string(),
            pid: Some(1),
            working_dir: "/repo".to_string(),
            page_id: "default".to_string(),
            title: "T".to_string(),
            ai_pid: Some(ai_pid),
            ai_start_unix: Some(1_000),
        }
    }

    /// An open, CONFIRMED record for `sid` on `terminal_id` under `config_dir`.
    fn open_confirmed(store: &SessionLifecycleStore, sid: &str, terminal_id: &str, dir: &str) {
        store.record_open(bind_record(
            &live(terminal_id, 1),
            sid,
            dir,
            ORIGIN_AUTHORITATIVE,
            true,
            UNZONED_ZONE_INDEX,
        ));
    }

    fn valid(terminal_id: &str, sid: &str, dir: &str) -> (String, ProviderRowValidity) {
        (
            terminal_id.to_string(),
            ProviderRowValidity::Valid {
                session_id: sid.to_string(),
                config_dir: PathBuf::from(dir),
            },
        )
    }

    fn store() -> (tempfile::TempDir, SessionLifecycleStore) {
        let dir = tempdir().unwrap();
        let store = SessionLifecycleStore::open(dir.path().join("s.json")).unwrap();
        (dir, store)
    }

    /// The defect's shape: a confirmed record holds A while its own pane's
    /// valid provider row reads B. The pass writes B, and the account label
    /// and wrapper follow the dir.
    #[test]
    fn a_confirmed_record_is_corrected_from_its_own_panes_provider_row() {
        let (_d, store) = store();
        open_confirmed(&store, "cfgrep-own", "term-1", ROOT_A);
        let rows = ProviderRows::from_pairs([valid("term-1", "cfgrep-own", ROOT_B)]);

        let repairs = repair_identities(
            &store,
            &PreEvidenceSnapshot::take(&store),
            &rows,
            &AnchorArgvIds::none(),
        );

        assert_eq!(
            repairs,
            vec![IdentityRepair {
                terminal_id: "term-1".to_string(),
                session_id: "cfgrep-own".to_string(),
                rekeyed_from: None,
                from: Some(ROOT_A.to_string()),
                to: ROOT_B.to_string(),
                origin: ORIGIN_AUTHORITATIVE.to_string(),
                confirmed: true,
            }]
        );
        let rec = store.get("cfgrep-own").unwrap();
        assert_eq!(rec.config_dir.as_deref(), Some(ROOT_B));
        assert_eq!(rec.account_label.as_deref(), Some("paktis"));
        assert_eq!(rec.account_wrapper.as_deref(), Some("clp"));
        assert!(
            rec.confirmed_at.is_some(),
            "the repair leaves confirmation alone"
        );

        // The next tick, with the same row, has nothing to repair.
        assert!(repair_identities(
            &store,
            &PreEvidenceSnapshot::take(&store),
            &rows,
            &AnchorArgvIds::none()
        )
        .is_empty());
    }

    /// Compare-and-set: the repair decided from the snapshot, and the record
    /// moved to another terminal, or took another dir, before the write. Each
    /// skips this tick, and nothing is written.
    #[test]
    fn a_record_that_changed_after_the_snapshot_is_not_repaired_this_tick() {
        let (_d, store) = store();
        let rows = ProviderRows::from_pairs([
            valid("term-1", "cfgrep-moved", ROOT_B),
            valid("term-1b", "cfgrep-redir", ROOT_B),
        ]);
        open_confirmed(&store, "cfgrep-moved", "term-1", ROOT_A);
        open_confirmed(&store, "cfgrep-redir", "term-1b", ROOT_A);
        let snapshot = PreEvidenceSnapshot::take(&store);

        // Between the snapshot and the repair: one row moves to another pane
        // (a migration's or a bind's record_open), the other takes a new dir.
        open_confirmed(&store, "cfgrep-moved", "term-2", ROOT_A);
        open_confirmed(&store, "cfgrep-redir", "term-1b", ROOT_C);

        assert!(repair_identities(&store, &snapshot, &rows, &AnchorArgvIds::none()).is_empty());
        let moved = store.get("cfgrep-moved").unwrap();
        assert_eq!(
            (moved.terminal_id.as_str(), moved.config_dir.as_deref()),
            ("term-2", Some(ROOT_A))
        );
        assert_eq!(
            store.get("cfgrep-redir").unwrap().config_dir.as_deref(),
            Some(ROOT_C)
        );
    }

    /// A valid row on ANOTHER terminal naming the record's session says nothing
    /// about where the record's own pane runs it: it moves nothing.
    #[test]
    fn a_row_on_another_terminal_moves_nothing() {
        let (_d, store) = store();
        open_confirmed(&store, "cfgrep-other", "term-1", ROOT_A);

        let rows = ProviderRows::from_pairs([valid("term-2", "cfgrep-other", ROOT_B)]);
        assert!(repair_identities(
            &store,
            &PreEvidenceSnapshot::take(&store),
            &rows,
            &AnchorArgvIds::none()
        )
        .is_empty());
        assert_eq!(
            store.get("cfgrep-other").unwrap().config_dir.as_deref(),
            Some(ROOT_A)
        );
    }

    /// The defect of plan `2026-11-27-coord-message-to-cross-account-pane-never-pushed`:
    /// the record keeps the spawn-pinned id while its own pane's valid row
    /// names the id the pane's claude actually runs, under another account.
    /// The record is re-keyed to the row's id, with the row's dir, account and
    /// grade in the same write, and the payload may correct the tab's id.
    #[test]
    fn a_row_naming_another_session_on_the_records_own_pane_rekeys_the_record() {
        let (_d, store) = store();
        store.record_open(bind_record(
            &live("term-1", 1),
            "rekey-pinned",
            ROOT_A,
            ORIGIN_AUTHORITATIVE,
            false,
            UNZONED_ZONE_INDEX,
        ));
        let rows = ProviderRows::from_pairs([valid("term-1", "rekey-live", ROOT_B)]);

        let repairs = repair_identities(
            &store,
            &PreEvidenceSnapshot::take(&store),
            &rows,
            &AnchorArgvIds::none(),
        );

        assert_eq!(
            repairs,
            vec![IdentityRepair {
                terminal_id: "term-1".to_string(),
                session_id: "rekey-live".to_string(),
                rekeyed_from: Some("rekey-pinned".to_string()),
                from: Some(ROOT_A.to_string()),
                to: ROOT_B.to_string(),
                origin: ORIGIN_OBSERVED.to_string(),
                confirmed: true,
            }]
        );
        assert!(store.get("rekey-pinned").is_none());
        let rec = store.get("rekey-live").unwrap();
        assert_eq!(rec.terminal_id, "term-1");
        assert_eq!(rec.config_dir.as_deref(), Some(ROOT_B));
        assert_eq!(rec.account_label.as_deref(), Some("paktis"));
        assert!(rec.confirmed_at.is_some());
        assert!(repair_bound_payloads(&repairs)[0].provider_reported);

        // The next tick has nothing left to do.
        assert!(repair_identities(
            &store,
            &PreEvidenceSnapshot::take(&store),
            &rows,
            &AnchorArgvIds::none()
        )
        .is_empty());
    }

    /// The identity source is the account dir the pane's claude ACTUALLY runs
    /// under, read from disk (manual-test-loop iteration 2): the record names
    /// the runner's dir (`.claude-support`) and a pinned id, the runner's dir
    /// holds only a stale row for an earlier holder of the anchor pid, and the
    /// live row sits in `.claude-ben/sessions/<pid>.json`. The real reader
    /// ([`read_provider_rows`], every discovered dir) finds the live row, and
    /// the record is re-keyed to its id under `.claude-ben`.
    #[test]
    fn a_pane_under_another_config_dir_is_rekeyed_from_that_dirs_provider_row() {
        use crate::session::claude_activity::read_provider_rows;

        const PID: u32 = 3_999_917;
        const CREATED: i64 = 1_795_762_191;
        let homes = tempdir().unwrap();
        let support = homes.path().join(".claude-support");
        let ben = homes.path().join(".claude-ben");
        // A Windows-domain row: its FILETIME `procStart` is checked against the
        // anchor's creation time on every OS.
        let write_row = |dir: &PathBuf, sid: &str, started: i64| {
            std::fs::create_dir_all(dir.join("sessions")).unwrap();
            let row = serde_json::json!({
                "pid": PID,
                "sessionId": sid,
                "pidDomain": "win32:test",
                "procStart": ((started + 11_644_473_600) * 10_000_000).to_string(),
            });
            std::fs::write(
                dir.join("sessions").join(format!("{PID}.json")),
                row.to_string(),
            )
            .unwrap();
        };
        write_row(&support, "rk-dir-pinned", CREATED - 3_600);
        write_row(&ben, "rk-dir-live", CREATED);
        let (support_str, ben_str) = (support.to_str().unwrap(), ben.to_str().unwrap());

        let (_d, store) = store();
        store.record_open(bind_record(
            &live("term-1", PID),
            "rk-dir-pinned",
            support_str,
            ORIGIN_AUTHORITATIVE,
            true,
            UNZONED_ZONE_INDEX,
        ));
        let snapshot = PreEvidenceSnapshot::take(&store);
        let mut verdicts =
            read_provider_rows(&[support.clone(), ben.clone()], &[(PID, Some(CREATED))]);
        let rows = ProviderRows::from_entries([(
            "term-1".to_string(),
            (PID, Some(CREATED)),
            verdicts.remove(&PID).unwrap(),
        )]);

        let repairs = repair_identities(&store, &snapshot, &rows, &AnchorArgvIds::none());

        assert_eq!(repairs.len(), 1, "{repairs:?}");
        assert_eq!(repairs[0].session_id, "rk-dir-live");
        assert_eq!(repairs[0].rekeyed_from.as_deref(), Some("rk-dir-pinned"));
        assert_eq!(repairs[0].from.as_deref(), Some(support_str));
        assert_eq!(repairs[0].to, ben_str);
        assert!(store.get("rk-dir-pinned").is_none());
        let rec = store.get("rk-dir-live").unwrap();
        assert_eq!(rec.terminal_id, "term-1");
        assert_eq!(rec.config_dir.as_deref(), Some(ben_str));
    }

    /// The re-key's guards: an overlap-window row (two panes' valid rows name
    /// the live id), a live id that already keys another open record, two
    /// open records on the pane, and another pane's argv naming either id
    /// each re-key nothing.
    #[test]
    fn the_rekey_is_refused_under_each_guard() {
        // Overlap.
        let (_d, store) = store();
        open_confirmed(&store, "rk-overlap", "term-1", ROOT_A);
        let rows = ProviderRows::from_pairs([
            valid("term-1", "rk-overlap-live", ROOT_B),
            valid("term-2", "rk-overlap-live", ROOT_C),
        ]);
        assert!(repair_identities(
            &store,
            &PreEvidenceSnapshot::take(&store),
            &rows,
            &AnchorArgvIds::none()
        )
        .is_empty());
        assert!(store.get("rk-overlap").is_some());

        // Occupied: the live id keys an open record on another pane.
        let (_d, store) = fresh_store();
        open_confirmed(&store, "rk-occ", "term-1", ROOT_A);
        open_confirmed(&store, "rk-occ-live", "term-9", ROOT_B);
        let rows = ProviderRows::from_pairs([valid("term-1", "rk-occ-live", ROOT_B)]);
        assert!(repair_identities(
            &store,
            &PreEvidenceSnapshot::take(&store),
            &rows,
            &AnchorArgvIds::none()
        )
        .is_empty());
        assert!(store.get("rk-occ").is_some());
        assert_eq!(store.get("rk-occ-live").unwrap().terminal_id, "term-9");

        // Two open records on the pane: which one it runs is unknown.
        // `record_open` supersedes a sibling on the same terminal, so the
        // second record is put into the snapshot the guard reads directly.
        let (_d, store) = fresh_store();
        open_confirmed(&store, "rk-two-a", "term-1", ROOT_A);
        let mut snapshot = PreEvidenceSnapshot::take(&store);
        snapshot.by_session.insert(
            "rk-two-b".to_string(),
            ("term-1".to_string(), Some(ROOT_A.to_string())),
        );
        let rows = ProviderRows::from_pairs([valid("term-1", "rk-two-live", ROOT_B)]);
        assert!(repair_identities(&store, &snapshot, &rows, &AnchorArgvIds::none()).is_empty());
        assert!(store.get("rk-two-a").is_some());

        // Another pane's argv names the record's id or the live id.
        for other in ["rk-argv", "rk-argv-live"] {
            let (_d, store) = fresh_store();
            open_confirmed(&store, "rk-argv", "term-1", ROOT_A);
            let rows = ProviderRows::from_pairs([valid("term-1", "rk-argv-live", ROOT_B)]);
            let argv = AnchorArgvIds::from_pairs([("term-2", other)]);
            assert!(
                repair_identities(&store, &PreEvidenceSnapshot::take(&store), &rows, &argv)
                    .is_empty(),
                "{other}"
            );
            assert!(store.get("rk-argv").is_some(), "{other}");
        }
    }

    fn fresh_store() -> (tempfile::TempDir, SessionLifecycleStore) {
        store()
    }

    /// The overlap guard: an account migration's settle window keeps the old
    /// pane alive with `claude --resume <sid>` beside the new one, so two
    /// terminals' valid rows name one session. Nothing is written for it this
    /// tick, even though the record's own pane has a row that disagrees.
    #[test]
    fn two_terminals_naming_one_session_write_nothing() {
        let (_d, store) = store();
        open_confirmed(&store, "cfgrep-overlap", "term-new", ROOT_A);
        let rows = ProviderRows::from_pairs([
            valid("term-new", "cfgrep-overlap", ROOT_B),
            valid("term-old", "cfgrep-overlap", ROOT_C),
        ]);

        assert!(repair_identities(
            &store,
            &PreEvidenceSnapshot::take(&store),
            &rows,
            &AnchorArgvIds::none()
        )
        .is_empty());
        let rec = store.get("cfgrep-overlap").unwrap();
        assert_eq!(rec.config_dir.as_deref(), Some(ROOT_A));
        assert_eq!(rec.account_label, None, "no identity write at all");
    }

    /// The argv guard: another live pane's claude anchor was started with
    /// `--resume <sid>`, though its provider row is not valid yet. Nothing is
    /// written for that session. The record's OWN pane naming the session in
    /// its argv is the ordinary case and blocks nothing.
    #[test]
    fn another_panes_argv_naming_the_session_blocks_the_repair_and_its_own_does_not() {
        let (_d, store) = store();
        open_confirmed(&store, "cfgrep-argv", "term-old", ROOT_B);
        let rows = ProviderRows::from_pairs([
            valid("term-old", "cfgrep-argv", ROOT_A),
            ("term-new".to_string(), ProviderRowValidity::Unknown),
        ]);

        let both =
            AnchorArgvIds::from_pairs([("term-old", "cfgrep-argv"), ("term-new", "cfgrep-argv")]);
        let snapshot = PreEvidenceSnapshot::take(&store);
        assert!(repair_identities(&store, &snapshot, &rows, &both).is_empty());
        assert_eq!(
            store.get("cfgrep-argv").unwrap().config_dir.as_deref(),
            Some(ROOT_B)
        );

        let own_only = AnchorArgvIds::from_pairs([("term-old", "cfgrep-argv")]);
        let repairs = repair_identities(&store, &snapshot, &rows, &own_only);
        assert_eq!(repairs.len(), 1, "{repairs:?}");
        assert_eq!(
            store.get("cfgrep-argv").unwrap().config_dir.as_deref(),
            Some(ROOT_A)
        );
    }

    /// An argv read that failed is UNKNOWN, not "no other pane": nothing is
    /// written this tick, even for a record the rows alone would repair.
    #[test]
    fn an_unknown_argv_read_writes_nothing() {
        let (_d, store) = store();
        open_confirmed(&store, "cfgrep-unknown", "term-1", ROOT_A);
        let rows = ProviderRows::from_pairs([valid("term-1", "cfgrep-unknown", ROOT_B)]);

        let snapshot = PreEvidenceSnapshot::take(&store);
        assert!(repair_identities(&store, &snapshot, &rows, &AnchorArgvIds::Unknown).is_empty());
        assert_eq!(
            store.get("cfgrep-unknown").unwrap().config_dir.as_deref(),
            Some(ROOT_A)
        );
    }

    /// No-op on agreement: a record holding a respelling of its pane's root (a
    /// trailing slash, a backslash) is not repaired, so a spelling never costs
    /// a write or a log line.
    #[test]
    fn a_respelled_record_of_its_panes_root_is_not_repaired() {
        let (_d, store) = store();
        open_confirmed(
            &store,
            "cfgrep-respell",
            "term-1",
            "/home/u/.claude-paktis/",
        );
        open_confirmed(
            &store,
            "cfgrep-backslash",
            "term-2",
            "/home/u\\.claude-paktis",
        );
        let rows = ProviderRows::from_pairs([
            valid("term-1", "cfgrep-respell", ROOT_B),
            valid("term-2", "cfgrep-backslash", ROOT_B),
        ]);

        assert!(repair_identities(
            &store,
            &PreEvidenceSnapshot::take(&store),
            &rows,
            &AnchorArgvIds::none()
        )
        .is_empty());
        let rec = store.get("cfgrep-respell").unwrap();
        assert_eq!(rec.config_dir.as_deref(), Some("/home/u/.claude-paktis/"));
        assert_eq!(rec.account_label, None, "no identity write at all");
    }

    /// The log keeps a recurring repair visible: the 1st, 2nd, 4th and 8th of
    /// one `(session, from, to)` are logged, with their count, and other
    /// transitions count separately.
    #[test]
    fn a_recurring_repair_is_logged_at_doubling_counts() {
        let mut log = RepairLog::default();
        let logged: Vec<Option<u64>> = (0..9).map(|_| log.note("s", ROOT_A, ROOT_B)).collect();
        assert_eq!(
            logged,
            vec![
                Some(1),
                Some(2),
                None,
                Some(4),
                None,
                None,
                None,
                Some(8),
                None
            ]
        );
        assert_eq!(log.note("s", ROOT_B, ROOT_A), Some(1));
        assert_eq!(log.note("t", ROOT_A, ROOT_B), Some(1));
    }

    /// The log is bounded to open sessions: a session that is no longer open
    /// is forgotten, so its counts start again if it ever recurs.
    #[test]
    fn the_repair_log_forgets_sessions_that_are_no_longer_open() {
        let mut log = RepairLog::default();
        log.note("gone", ROOT_A, ROOT_B);
        log.note("kept", ROOT_A, ROOT_B);
        log.retain_sessions(|sid| sid == "kept");
        assert_eq!(log.counts.len(), 1);
        assert_eq!(log.note("gone", ROOT_A, ROOT_B), Some(1));
        assert_eq!(log.note("kept", ROOT_A, ROOT_B), Some(2));
    }

    /// Each landed repair becomes a `session-bound` payload carrying the
    /// repaired dir, never the empty "unknown".
    #[test]
    fn a_landed_repair_becomes_a_session_bound_payload_with_its_new_dir() {
        let payloads = repair_bound_payloads(&[IdentityRepair {
            terminal_id: "term-1".to_string(),
            session_id: "cfgrep-pay".to_string(),
            rekeyed_from: None,
            from: Some(ROOT_A.to_string()),
            to: ROOT_B.to_string(),
            origin: ORIGIN_AUTHORITATIVE.to_string(),
            confirmed: true,
        }]);
        assert_eq!(payloads.len(), 1);
        assert_eq!(payloads[0].terminal_id, "term-1");
        assert_eq!(payloads[0].session_id, "cfgrep-pay");
        assert_eq!(payloads[0].config_dir, ROOT_B);
        assert!(payloads[0].confirmed);
        assert!(!payloads[0].provider_reported);
    }

    struct TwoRootIndex(Vec<TranscriptCandidate>);
    impl TranscriptIndex for TwoRootIndex {
        fn candidates_for(&self, _working_dir: &str) -> Vec<TranscriptCandidate> {
            self.0.clone()
        }
        fn transcript_exists(&self, _session_id: &str, _wd: Option<&str>) -> bool {
            true
        }
    }

    /// Composition with Phase 1, as the ticks run them. Tick 1: the bind pass
    /// sees the session's transcript under roots [A, B] and writes no config
    /// dir, and the record it created is past that tick's snapshot, so the
    /// repair leaves it. Tick 2: the repair writes B from the pane's row.
    #[test]
    fn a_two_root_bind_then_the_next_ticks_repair_leaves_the_provider_rows_root() {
        let (_d, store) = store();
        let index = TwoRootIndex(
            [ROOT_A, ROOT_B]
                .into_iter()
                .map(|root| TranscriptCandidate {
                    session_id: "cfgrep-compose".to_string(),
                    config_dir: root.to_string(),
                    started_at_unix: Some(1_500),
                })
                .collect(),
        );
        let cmdlines: HashMap<u32, String> = [(777u32, "cfgrep-compose".to_string())]
            .into_iter()
            .collect();
        let rows = ProviderRows::from_pairs([valid("term-1", "cfgrep-compose", ROOT_B)]);

        // Tick 1.
        let snapshot = PreEvidenceSnapshot::take(&store);
        run_reconcile(
            &store,
            &[live("term-1", 777)],
            &ProcessSnapshot::default(),
            &index,
            &cmdlines,
        );
        assert!(repair_identities(&store, &snapshot, &rows, &AnchorArgvIds::none()).is_empty());
        assert_eq!(
            store.get("cfgrep-compose").unwrap().config_dir,
            None,
            "the bind carries no roster-order guess"
        );

        // Tick 2.
        let repairs = repair_identities(
            &store,
            &PreEvidenceSnapshot::take(&store),
            &rows,
            &AnchorArgvIds::none(),
        );
        assert_eq!(repairs.len(), 1, "{repairs:?}");
        assert_eq!(repairs[0].from, None);
        assert_eq!(
            store.get("cfgrep-compose").unwrap().config_dir.as_deref(),
            Some(ROOT_B)
        );
    }
}
