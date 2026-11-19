//! Compare-and-set for the identity overlay (plan
//! `2026-11-16-boot-reconcile-reverts-a-migrated-sessions-config-dir-so-the-next-migration-carries-a-stale-transcript`,
//! D2, review round 1).
//!
//! [`SessionLifecycleStore::update_identity`] writes whatever it is handed. A
//! caller that DECIDED the write from a read taken earlier needs more: the
//! write must land only on the record it decided about. The config-dir repair
//! decides from a snapshot taken before the tick's provider evidence, and in
//! between the migration's `record_open`, a reconcile bind's re-terminal, the
//! SessionStart hook or the zone backstop may rewrite the record. Writing then
//! would stamp evidence about one pane onto a record that has since moved to
//! another, which is the defect the repair exists to remove.
//!
//! The expectation is checked and the overlay applied under the store's own
//! lock, so no write can land between the check and the apply.
//!
//! What it detects is narrower than "any write in between". The key is the
//! record's `(state, terminal_id, config_dir)`, not a version: a write that
//! changed none of the three (another field, or an A -> B -> A rewrite of one
//! of them) passes the check. It catches a record that MOVED (another
//! terminal, another dir, closed), which is the case the repair needs.

use super::*;

/// What [`SessionLifecycleStore::update_identity_if_unchanged`] did.
#[derive(Debug, Clone)]
pub enum IdentityCas {
    /// The overlay was applied; carries the record as written.
    Written(Box<TerminalSessionRecord>),
    /// The record still matched, but the overlay changed nothing.
    NoChange,
    /// The record is no longer open, or no longer carries the expected
    /// terminal or config dir: nothing was written. The caller re-reads its
    /// evidence next tick.
    Stale,
    /// No record for the session.
    Missing,
    /// The store's lock was poisoned: nothing was written.
    Unavailable,
}

impl SessionLifecycleStore {
    /// [`Self::update_identity`], applied ONLY when the record is still open on
    /// `expected_terminal_id` with exactly `expected_config_dir`.
    ///
    /// Same sticky merge and write-only-on-change as `update_identity`. A blank
    /// overlay is [`IdentityCas::NoChange`] without reading the record.
    pub fn update_identity_if_unchanged(
        &self,
        claude_session_id: &str,
        expected_terminal_id: &str,
        expected_config_dir: Option<&str>,
        update: &SessionIdentityUpdate,
    ) -> IdentityCas {
        let update = update.clone().normalized();
        if update.is_empty() {
            return IdentityCas::NoChange;
        }
        let mut m = match self.map.lock() {
            Ok(m) => m,
            Err(e) => {
                warn!(error = %e, "session_lifecycle_store: lock poisoned on update_identity_if_unchanged");
                return IdentityCas::Unavailable;
            }
        };
        let Some(rec) = m.get_mut(claude_session_id) else {
            return IdentityCas::Missing;
        };
        if rec.state != "open"
            || rec.terminal_id != expected_terminal_id
            || rec.config_dir.as_deref() != expected_config_dir
        {
            return IdentityCas::Stale;
        }
        if !update.apply(rec) {
            return IdentityCas::NoChange;
        }
        let changed = rec.clone();
        debug!(
            claude_session_id,
            config_dir = changed.config_dir.as_deref().unwrap_or("?"),
            account = changed.account_label.as_deref().unwrap_or("?"),
            "session_lifecycle_store: session identity refreshed (compare-and-set)"
        );
        self.persist(
            m,
            &[LifecycleDelta::Upsert {
                rec: Box::new(changed.clone()),
            }],
        );
        IdentityCas::Written(Box::new(changed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    const DIR_A: &str = "/home/u/.claude-gmail";
    const DIR_B: &str = "/home/u/.claude-paktis";

    fn open_store() -> (tempfile::TempDir, SessionLifecycleStore) {
        let dir = tempdir().unwrap();
        let store = SessionLifecycleStore::open(dir.path().join("s.json")).unwrap();
        (dir, store)
    }

    /// An open record for `sid` on `terminal_id` under `config_dir`.
    fn rec(sid: &str, terminal_id: &str, config_dir: Option<&str>) -> TerminalSessionRecord {
        TerminalSessionRecord {
            claude_session_id: sid.to_string(),
            config_dir: config_dir.map(str::to_string),
            working_dir: Some("/repo".to_string()),
            page_id: "default".to_string(),
            zone_index: UNZONED_ZONE_INDEX,
            title: None,
            terminal_id: terminal_id.to_string(),
            opened_at: 0,
            last_seen_at: 0,
            state: "open".to_string(),
            closed_at: None,
            close_reason: None,
            provider: DEFAULT_PROVIDER.to_string(),
            origin: Some(ORIGIN_AUTHORITATIVE.to_string()),
            restore_pending_at: None,
            awaiting_account_since: None,
            confirmed_at: Some(1),
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
            finished_at: None,
            wind_down_outcome: None,
            wind_down_at: None,
            wind_down_arm: None,
            finish_reason: None,
            finish_synced: false,
            finish_coord_session_id: None,
            finish_coord_readdressed: false,
            spawn_device_default: None,
            adopted_from: None,
            last_session_start_at: None,
            last_alive_at: None,
            gate_id: None,
            gate_consuming_device_id: None,
            gate_bound_boot_ms: None,
        }
    }

    fn dir_update(dir: &str) -> SessionIdentityUpdate {
        SessionIdentityUpdate {
            config_dir: Some(dir.to_string()),
            ..Default::default()
        }
    }

    /// The write lands only while the record still matches the expectation.
    /// A moved terminal, a changed dir or a closed record is `Stale`, and
    /// nothing is written.
    #[test]
    fn a_compare_and_set_identity_write_lands_only_on_the_expected_record() {
        let (_d, store) = open_store();
        store.record_open(rec("cas-1", "term-1", Some(DIR_A)));

        // Moved to another terminal since the caller's read.
        assert!(matches!(
            store.update_identity_if_unchanged("cas-1", "term-0", Some(DIR_A), &dir_update(DIR_B)),
            IdentityCas::Stale
        ));
        // Config dir changed since the caller's read.
        assert!(matches!(
            store.update_identity_if_unchanged("cas-1", "term-1", None, &dir_update(DIR_B)),
            IdentityCas::Stale
        ));
        assert_eq!(
            store.get("cas-1").unwrap().config_dir.as_deref(),
            Some(DIR_A),
            "a stale expectation writes nothing"
        );

        // Still matching: written.
        let written =
            store.update_identity_if_unchanged("cas-1", "term-1", Some(DIR_A), &dir_update(DIR_B));
        assert!(matches!(written, IdentityCas::Written(_)), "{written:?}");
        assert_eq!(
            store.get("cas-1").unwrap().config_dir.as_deref(),
            Some(DIR_B)
        );
        // The same overlay again changes nothing.
        assert!(matches!(
            store.update_identity_if_unchanged("cas-1", "term-1", Some(DIR_B), &dir_update(DIR_B)),
            IdentityCas::NoChange
        ));

        // A closed record is never written.
        store.record_close("cas-1", "exited");
        assert!(matches!(
            store.update_identity_if_unchanged("cas-1", "term-1", Some(DIR_B), &dir_update(DIR_A)),
            IdentityCas::Stale
        ));
        assert!(matches!(
            store.update_identity_if_unchanged("cas-ghost", "term-1", None, &dir_update(DIR_A)),
            IdentityCas::Missing
        ));
    }

    /// A blank or whitespace `config_dir` is "not reported": it never clears a
    /// stored dir, and an overlay carrying only that is empty. A real dir is
    /// not.
    #[test]
    fn a_blank_config_dir_in_an_identity_update_is_not_reported() {
        let (_d, store) = open_store();
        store.record_open(rec("cas-blank", "term-1", Some(DIR_A)));

        for blank in ["", "   "] {
            assert!(!store.update_identity("cas-blank", &dir_update(blank)));
            assert!(dir_update(blank).normalized().is_empty(), "{blank:?}");
        }
        assert_eq!(
            store.get("cas-blank").unwrap().config_dir.as_deref(),
            Some(DIR_A)
        );
        assert!(!dir_update(DIR_B).normalized().is_empty());
    }
}
