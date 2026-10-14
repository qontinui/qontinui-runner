//! The work-unit `metadata` contract of the plan adapter: which keys the
//! scanner owns, and the RFC 7396 merge patches it sends.
//!
//! Plan `2026-10-13-plan-adapter-metadata-refresh-erases-session-written-keys`,
//! Phase 1.
//!
//! ## The contract
//!
//! - **The scanner never mentions a key outside [`SCANNER_OWNED_KEYS`].** Every
//!   push — the active-directory reconcile in every arm (create, refresh,
//!   transition pre-refresh, deferral, permanently-retired) and the archive
//!   stamp — sends a `metadata_patch` built here, never a wholesale `metadata`
//!   replace. coord merges the patch against the stored row under its row lock,
//!   so a key written by a session (`phase_vets`, `scheduling_decision`, …) or
//!   by coord itself (`citations_refreshed_at`, `dispatch`, `dispatched_at`)
//!   survives every scan. Before this module the detected-phases arm replaced
//!   the whole object and erased all of them within one reconcile cycle.
//! - **On a plan-backed unit a roster key is authored by the plan file**, which
//!   wins every cycle. That is the documented precedence, and the reason a
//!   collision is confined to these names.
//! - **Roster values are never JSON objects.** RFC 7396 merges objects
//!   recursively, so an object value would leave behind nested keys the file
//!   dropped. Arrays are replaced whole, which is what `phases` and
//!   `depends_on` rely on.
//! - **A key the scanner retires keeps a delete-when-absent entry** (a
//!   tombstone), so stored copies are cleared rather than orphaned.
//! - **Adding a key** needs two things: declare its [`AbsentRule`], and first
//!   read stored metadata on plan-backed units (`coord_work_unit_list`) for a
//!   session or coord writer already using that name.
//!
//! A stale key the scanner does NOT own is its writer's to remove with
//! `{key: null}`; the scanner cannot know whether it is stale and must not guess.

use super::parser::ParsedWorkUnit;
use serde_json::{Map, Value};

/// What the active-directory scan sends for a roster key the plan file does not
/// supply a value for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AbsentRule {
    /// The file always yields a value (`[]` for an empty list), so the key is
    /// on every active patch.
    AlwaysSent,
    /// A status-block header key: `null` (RFC 7396 delete) when the file
    /// declares none, so the plan file stays its sole author. When the parser
    /// REJECTED a declaration the key is OMITTED instead: a parse that could
    /// not read the line has no information about the stored value. Header
    /// keys also ride on the archive patch, with the same rule.
    DeleteWhenAbsent,
    /// Omitted when absent, so the stored value is kept. This is the
    /// 2026-09-16 guard for `phases`: a scan that detects none has no
    /// information about a good stored list, and `phases: []` would erase it.
    ///
    /// Accepted trade-off: a plan whose phases are removed on purpose (or
    /// whose headings stop parsing) no longer clears the stored list from the
    /// adapter push. A wiped list is unrecoverable; a stale one is visible.
    OmitWhenAbsent,
    /// Owned by the archive scan, which sets it. The active scan always sends
    /// `null`, so a plan restored from the archive directory clears it.
    SetByArchiveScan,
}

/// One scanner-owned `metadata` key.
#[derive(Debug, Clone, Copy)]
pub struct ScannerKey {
    pub name: &'static str,
    pub absent_rule: AbsentRule,
    /// Where the value comes from, for readers of the roster.
    pub source: &'static str,
}

/// The keys the plan adapter owns on a work unit's `metadata`. The scanner
/// writes these and nothing else; see the module doc for the contract.
pub const SCANNER_OWNED_KEYS: &[ScannerKey] = &[
    ScannerKey {
        name: "source_path",
        absent_rule: AbsentRule::AlwaysSent,
        source: "the plan file's repo-relative path",
    },
    ScannerKey {
        name: "depends_on",
        absent_rule: AbsentRule::AlwaysSent,
        source: "status-block `Depends-On:` (`[]` when none)",
    },
    ScannerKey {
        name: "area",
        absent_rule: AbsentRule::DeleteWhenAbsent,
        source: "status-block `Area:`",
    },
    ScannerKey {
        name: "archive_path",
        absent_rule: AbsentRule::SetByArchiveScan,
        source: "the archived file's path, stamped by `push_archive_metadata`",
    },
    ScannerKey {
        name: "phases",
        absent_rule: AbsentRule::OmitWhenAbsent,
        source: "`detect_phases`",
    },
];

/// The roster entry for `name`, if the scanner owns it.
pub fn scanner_key(name: &str) -> Option<&'static ScannerKey> {
    SCANNER_OWNED_KEYS.iter().find(|k| k.name == name)
}

/// True when the parser saw a declaration for `key` and rejected it. Only
/// `area` has a parser that reports rejection today.
fn declaration_rejected(key: &str, u: &ParsedWorkUnit) -> bool {
    key == "area" && u.area_rejected.is_some()
}

/// The values the plan file supplies, keyed by roster name. A key the file
/// does not supply is absent here, and [`active_dir_patch`] /
/// [`archive_dir_patch`] apply its [`AbsentRule`].
fn build_metadata(u: &ParsedWorkUnit) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert(
        "source_path".to_string(),
        Value::String(u.source_path.clone()),
    );
    m.insert("depends_on".to_string(), serde_json::json!(u.depends_on));
    if !u.phases.is_empty() {
        m.insert(
            "phases".to_string(),
            Value::Array(
                u.phases
                    .iter()
                    .map(|p| serde_json::json!({"index": p.index, "name": p.name}))
                    .collect(),
            ),
        );
    }
    if let Some(area) = &u.area {
        m.insert("area".to_string(), Value::String(area.clone()));
    }
    m
}

/// The `metadata_patch` an active-directory scan sends, in every push arm.
///
/// Every value the file supplies, plus each absent roster key resolved by its
/// [`AbsentRule`]. Nothing outside the roster is mentioned, so coord leaves
/// every other stored key alone.
pub fn active_dir_patch(u: &ParsedWorkUnit) -> Value {
    let mut patch = build_metadata(u);
    for key in SCANNER_OWNED_KEYS {
        if patch.contains_key(key.name) {
            continue;
        }
        match key.absent_rule {
            AbsentRule::AlwaysSent | AbsentRule::OmitWhenAbsent => {}
            AbsentRule::DeleteWhenAbsent => {
                if !declaration_rejected(key.name, u) {
                    patch.insert(key.name.to_string(), Value::Null);
                }
            }
            AbsentRule::SetByArchiveScan => {
                patch.insert(key.name.to_string(), Value::Null);
            }
        }
    }
    Value::Object(patch)
}

/// The `metadata_patch` the archive scan sends: `archive_path` (the archived
/// file's path) plus every header-derived [`AbsentRule::DeleteWhenAbsent`]
/// key, with the same absent rule as the active scan. A patch, never a
/// replace, so a plan found in the archive keeps its stored `phases`,
/// `source_path` and `depends_on`.
pub fn archive_dir_patch(u: &ParsedWorkUnit) -> Value {
    let built = build_metadata(u);
    let mut patch = Map::new();
    for key in SCANNER_OWNED_KEYS {
        match key.absent_rule {
            AbsentRule::SetByArchiveScan => {
                patch.insert(key.name.to_string(), Value::String(u.source_path.clone()));
            }
            AbsentRule::DeleteWhenAbsent => match built.get(key.name) {
                Some(v) => {
                    patch.insert(key.name.to_string(), v.clone());
                }
                None if declaration_rejected(key.name, u) => {}
                None => {
                    patch.insert(key.name.to_string(), Value::Null);
                }
            },
            AbsentRule::AlwaysSent | AbsentRule::OmitWhenAbsent => {}
        }
    }
    Value::Object(patch)
}

#[cfg(test)]
mod tests {
    use super::super::parser::{AreaRejection, ParsedPhase};
    use super::super::push::{
        push_archive_metadata, push_work_unit_with_status_write, SetDepsOutcome, StatusWrite,
        TransitionBody, UpsertBody, WorkUnitSink, ADAPTER_ACTOR,
    };
    use super::*;
    use crate::auth::TenantScope;
    use anyhow::Result;
    use std::collections::HashMap;
    use std::sync::Mutex;

    fn phase(index: u32) -> ParsedPhase {
        ParsedPhase {
            index,
            name: format!("Phase {index}"),
        }
    }

    fn unit(slug: &str, status: &str) -> ParsedWorkUnit {
        ParsedWorkUnit {
            slug: slug.to_string(),
            title: Some("T".to_string()),
            status: status.to_string(),
            depends_on: vec!["2026-01-01-dep".to_string()],
            area: None,
            area_rejected: None,
            phases: vec![phase(1)],
            source_path: format!("plans/{slug}.md"),
            content: String::new(),
        }
    }

    // ---- The coord model --------------------------------------------------

    /// coord's metadata resolution, as the test model of it.
    ///
    /// Specification, both at qontinui-coord `crates/coord/src/work_unit_registry.rs`:
    /// 1. `resolved_metadata_value` — three arms: a present `metadata` is a
    ///    wholesale replace that carries the stored `attestations` forward
    ///    unless the replace names it; a present `metadata_patch` is
    ///    `json_patch::merge` onto the stored object (`{}` for a new row);
    ///    neither keeps the stored object.
    /// 2. `UPSERT_WORK_UNIT_SQL`'s `metadata` CASE, which runs after it on the
    ///    same write: when the stored row holds `attestations`, the stored
    ///    ledger is grafted back over whatever (1) produced, an explicit value
    ///    included.
    ///
    /// coord's own test of (1) (plan Phase 2) anchors this model to coord.
    fn coord_resolve(
        metadata: Option<&Value>,
        metadata_patch: Option<&Value>,
        existing: Option<&Value>,
    ) -> Value {
        assert!(
            !(metadata.is_some() && metadata_patch.is_some()),
            "coord refuses `metadata` and `metadata_patch` together"
        );
        let mut resolved = if let Some(m) = metadata {
            let mut merged = m.clone();
            if let (Some(obj), Some(ex)) =
                (merged.as_object_mut(), existing.and_then(|e| e.as_object()))
            {
                if !obj.contains_key("attestations") {
                    if let Some(att) = ex.get("attestations") {
                        obj.insert("attestations".to_string(), att.clone());
                    }
                }
            }
            merged
        } else if let Some(p) = metadata_patch {
            let mut merged = existing
                .cloned()
                .unwrap_or_else(|| Value::Object(Map::new()));
            json_patch::merge(&mut merged, p);
            merged
        } else {
            existing
                .cloned()
                .unwrap_or_else(|| Value::Object(Map::new()))
        };
        // The SQL graft: the stored ledger wins whenever the row holds one.
        if let Some(att) = existing
            .and_then(|e| e.as_object())
            .and_then(|o| o.get("attestations"))
        {
            if let Some(obj) = resolved.as_object_mut() {
                obj.insert("attestations".to_string(), att.clone());
            }
        }
        resolved
    }

    /// A [`WorkUnitSink`] that STORES what coord would store: one metadata
    /// object and one status per slug, each upsert resolved through
    /// [`coord_resolve`] from the WIRE body (the JSON coord deserializes), so
    /// a field the adapter puts on the wire is modelled whether or not this
    /// test knows its Rust name.
    #[derive(Default)]
    struct CoordMetadataModel {
        metadata: Mutex<HashMap<String, Value>>,
        statuses: Mutex<HashMap<String, String>>,
        last_actor: Mutex<Option<String>>,
    }

    impl CoordMetadataModel {
        fn stored(&self, slug: &str) -> Value {
            self.metadata
                .lock()
                .unwrap()
                .get(slug)
                .cloned()
                .expect("row exists")
        }
        fn write(&self, slug: &str, metadata: Option<&Value>, patch: Option<&Value>) {
            let mut rows = self.metadata.lock().unwrap();
            let resolved = coord_resolve(metadata, patch, rows.get(slug));
            rows.insert(slug.to_string(), resolved);
        }
        /// A session's `coord_work_unit_upsert` with `metadata_patch`.
        fn session_patch(&self, slug: &str, patch: Value) {
            self.write(slug, None, Some(&patch));
        }
        fn set_status(&self, slug: &str, status: &str) {
            self.statuses
                .lock()
                .unwrap()
                .insert(slug.to_string(), status.to_string());
        }
        fn set_last_actor(&self, actor: Option<&str>) {
            *self.last_actor.lock().unwrap() = actor.map(str::to_string);
        }
    }

    #[async_trait::async_trait]
    impl WorkUnitSink for CoordMetadataModel {
        async fn current_status(&self, slug: &str, _scope: TenantScope) -> Result<Option<String>> {
            Ok(self.statuses.lock().unwrap().get(slug).cloned())
        }
        async fn last_actor(&self, _slug: &str, _scope: TenantScope) -> Result<Option<String>> {
            Ok(self.last_actor.lock().unwrap().clone())
        }
        async fn upsert(&self, body: &UpsertBody, _scope: TenantScope) -> Result<()> {
            let wire = serde_json::to_value(body).unwrap();
            self.write(&body.slug, wire.get("metadata"), wire.get("metadata_patch"));
            let mut statuses = self.statuses.lock().unwrap();
            match &body.status {
                Some(s) => {
                    statuses.insert(body.slug.clone(), s.clone());
                }
                None => {
                    statuses.entry(body.slug.clone()).or_default();
                }
            }
            Ok(())
        }
        async fn transition(
            &self,
            slug: &str,
            body: &TransitionBody,
            _scope: TenantScope,
        ) -> Result<()> {
            self.set_status(slug, &body.to_status);
            Ok(())
        }
        async fn set_deps(
            &self,
            _slug: &str,
            depends_on: &[String],
            _scope: TenantScope,
        ) -> Result<SetDepsOutcome> {
            Ok(SetDepsOutcome::Ok {
                edges_set: depends_on.len() as u64,
            })
        }
    }

    // ---- Push arms --------------------------------------------------------

    /// Every arm of `push_work_unit_with_status_write` that sends an upsert.
    #[derive(Debug, Clone, Copy)]
    enum Arm {
        Create,
        Refresh,
        Transition,
        Deferral,
        PermanentlyRetired,
    }

    const ARMS: [Arm; 5] = [
        Arm::Create,
        Arm::Refresh,
        Arm::Transition,
        Arm::Deferral,
        Arm::PermanentlyRetired,
    ];

    /// Push `u` through `arm` via `sink`, arranging `model`'s status and last
    /// actor so the push takes exactly that arm, and assert it did.
    async fn scan_via<S: WorkUnitSink>(
        sink: &S,
        model: &CoordMetadataModel,
        u: &ParsedWorkUnit,
        arm: Arm,
    ) {
        use super::super::push::PushOutcomeKind as K;
        let file = u.status.as_str();
        assert_ne!(
            file, "draft",
            "the arms below rely on the file not saying draft"
        );
        let (last_applied, status_write, remote, actor): (
            Option<&str>,
            StatusWrite,
            &str,
            Option<&str>,
        ) = match arm {
            Arm::Create => (None, StatusWrite::Allowed, "draft", None),
            Arm::Refresh => (Some(file), StatusWrite::Allowed, file, None),
            Arm::Transition => (Some("draft"), StatusWrite::Allowed, "draft", None),
            // A real agent drove the unit to a status the file does not want.
            Arm::Deferral => (
                Some("draft"),
                StatusWrite::Allowed,
                "in_progress",
                Some("device:d:agent:a"),
            ),
            Arm::PermanentlyRetired => (
                Some("draft"),
                StatusWrite::RetiredPermanently,
                "draft",
                None,
            ),
        };
        model.set_status(&u.slug, remote);
        model.set_last_actor(actor);
        let out = push_work_unit_with_status_write(
            sink,
            u,
            last_applied,
            None,
            status_write,
            TenantScope::Unresolved,
        )
        .await
        .unwrap();
        let took = matches!(
            (arm, &out.kind),
            (Arm::Create, K::Created)
                | (Arm::Refresh | Arm::PermanentlyRetired, K::Refreshed)
                | (Arm::Transition, K::Transitioned { .. })
                | (Arm::Deferral, K::Deferred { .. })
        );
        assert!(took, "{arm:?} took the wrong arm: {:?}", out.kind);
    }

    async fn scan(model: &CoordMetadataModel, u: &ParsedWorkUnit, arm: Arm) {
        scan_via(model, model, u, arm).await;
    }

    fn phases_unit(slug: &str, phases: &[u32]) -> ParsedWorkUnit {
        ParsedWorkUnit {
            phases: phases.iter().copied().map(phase).collect(),
            status: "vetted".to_string(),
            ..unit(slug, "vetted")
        }
    }

    // ---- T1 ---------------------------------------------------------------

    /// The measured erasure (finding `43681f14`): a key a session writes with
    /// `metadata_patch` between two scans survives the second scan — in every
    /// arm, with phases detected and with none. Includes a coord-shaped
    /// `dispatched_at` and an `attestations` entry.
    #[tokio::test]
    async fn a_session_key_written_between_two_scans_survives_the_second_scan() {
        let session = serde_json::json!({
            "scheduling_decision": {"chosen": "phase-2", "why": ["a", "b"]},
            "dispatched_at": "2026-10-13T07:10:00Z",
            "attestations": [{"status": "vetted", "independence": "context"}],
        });
        for arm in ARMS {
            for phases in [&[1u32, 2][..], &[][..]] {
                let model = CoordMetadataModel::default();
                let u = phases_unit("s", phases);
                scan(&model, &u, Arm::Create).await;
                model.session_patch("s", session.clone());
                scan(&model, &u, arm).await;

                let stored = model.stored("s");
                for (k, v) in session.as_object().unwrap() {
                    assert_eq!(
                        stored.get(k),
                        Some(v),
                        "{arm:?}, phases {phases:?}: session key `{k}` must survive: {stored}"
                    );
                }
                assert_eq!(stored["source_path"], "plans/s.md", "{arm:?}");
                if !phases.is_empty() {
                    assert_eq!(stored["phases"].as_array().unwrap().len(), phases.len());
                }
            }
        }
    }

    // ---- T2 ---------------------------------------------------------------

    /// A derived key the plan dropped is removed from the unit: `area` deleted,
    /// `depends_on` replaced by `[]`, `phases` replaced whole (not merged
    /// element-wise), and a stored `archive_path` cleared — in every arm.
    #[tokio::test]
    async fn a_derived_key_removed_from_the_plan_is_removed_from_the_unit() {
        for arm in ARMS {
            let model = CoordMetadataModel::default();
            let first = ParsedWorkUnit {
                area: Some("work-units".to_string()),
                depends_on: vec!["a".to_string()],
                ..phases_unit("s", &[1, 2, 3])
            };
            scan(&model, &first, Arm::Create).await;
            push_archive_metadata(&model, &first, TenantScope::Unresolved)
                .await
                .unwrap();
            assert!(model.stored("s")["archive_path"].is_string());

            let second = ParsedWorkUnit {
                area: None,
                depends_on: vec![],
                ..phases_unit("s", &[1, 2])
            };
            scan(&model, &second, arm).await;

            let stored = model.stored("s");
            let obj = stored.as_object().unwrap();
            assert!(!obj.contains_key("area"), "{arm:?}: area removed: {stored}");
            assert_eq!(stored["depends_on"], serde_json::json!([]), "{arm:?}");
            let idx: Vec<u64> = stored["phases"]
                .as_array()
                .unwrap()
                .iter()
                .map(|p| p["index"].as_u64().unwrap())
                .collect();
            assert_eq!(idx, vec![1, 2], "{arm:?}: phases replaced whole: {stored}");
            assert!(!obj.contains_key("archive_path"), "{arm:?}: {stored}");
        }
    }

    // ---- T3 ---------------------------------------------------------------

    /// The 2026-09-16 wipe stays prevented: with five stored phases, a scan
    /// that detects none leaves all five, on every arm. And a stored `area`
    /// survives a scan whose `Area:` line the parser rejected (D2).
    #[tokio::test]
    async fn a_scan_that_detects_no_phases_keeps_the_stored_phase_list() {
        for arm in ARMS {
            let model = CoordMetadataModel::default();
            let first = ParsedWorkUnit {
                area: Some("work-units".to_string()),
                ..phases_unit("s", &[1, 2, 3, 4, 5])
            };
            scan(&model, &first, Arm::Create).await;
            let before = model.stored("s")["phases"].clone();

            let second = ParsedWorkUnit {
                area: None,
                area_rejected: Some(AreaRejection::NotKebab("Work Units".to_string())),
                ..phases_unit("s", &[])
            };
            scan(&model, &second, arm).await;

            let stored = model.stored("s");
            assert_eq!(stored["phases"], before, "{arm:?}: phases kept: {stored}");
            assert_eq!(stored["phases"].as_array().unwrap().len(), 5);
            assert_eq!(
                stored["area"], "work-units",
                "{arm:?}: a rejected Area: keeps the stored area: {stored}"
            );
        }
        // The archive stamp follows the same rejected-area rule.
        let model = CoordMetadataModel::default();
        let first = ParsedWorkUnit {
            area: Some("work-units".to_string()),
            ..unit("s", "vetted")
        };
        scan(&model, &first, Arm::Create).await;
        let rejected = ParsedWorkUnit {
            area: None,
            area_rejected: Some(AreaRejection::Misspelled("**Area**:")),
            ..unit("s", "shipped")
        };
        push_archive_metadata(&model, &rejected, TenantScope::Unresolved)
            .await
            .unwrap();
        assert_eq!(model.stored("s")["area"], "work-units");
    }

    // ---- T4 ---------------------------------------------------------------

    /// A spread of units covering every value/absent/rejected combination.
    fn roster_probe_units() -> Vec<ParsedWorkUnit> {
        let mut out = vec![];
        for area in [None, Some("work-units")] {
            for rejected in [false, true] {
                for phases in [&[][..], &[1u32, 2][..]] {
                    for deps in [vec![], vec!["a".to_string()]] {
                        out.push(ParsedWorkUnit {
                            area: area.map(str::to_string),
                            area_rejected: rejected
                                .then(|| AreaRejection::NotKebab("X".to_string())),
                            depends_on: deps,
                            ..phases_unit("s", phases)
                        });
                    }
                }
            }
        }
        out
    }

    /// The roster is the whole contract: every key either patch emits is in
    /// it; every always-sent, delete-when-absent and archive-owned key is on
    /// the active patch (as a value or `null`, except a rejected header
    /// declaration); no value is a JSON object; and the archive patch emits
    /// `archive_path` and header keys only.
    #[test]
    fn the_roster_covers_every_key_either_patch_emits() {
        let mut names: Vec<&str> = SCANNER_OWNED_KEYS.iter().map(|k| k.name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(
            names.len(),
            SCANNER_OWNED_KEYS.len(),
            "roster names are unique"
        );

        for u in roster_probe_units() {
            let active = active_dir_patch(&u);
            let archive = archive_dir_patch(&u);
            for (which, patch) in [("active", &active), ("archive", &archive)] {
                for (k, v) in patch.as_object().expect("a patch is an object") {
                    let key = scanner_key(k)
                        .unwrap_or_else(|| panic!("{which} patch emits unregistered key `{k}`"));
                    assert!(
                        !v.is_object(),
                        "{which} patch: `{k}` is a JSON object, which RFC 7396 would merge: {v}"
                    );
                    if which == "archive" {
                        assert!(
                            matches!(
                                key.absent_rule,
                                AbsentRule::SetByArchiveScan | AbsentRule::DeleteWhenAbsent
                            ),
                            "archive patch emits non-header key `{k}`"
                        );
                    }
                }
            }
            let a = active.as_object().unwrap();
            for key in SCANNER_OWNED_KEYS {
                let present = a.contains_key(key.name);
                match key.absent_rule {
                    AbsentRule::AlwaysSent => assert!(present, "`{}` always sent", key.name),
                    AbsentRule::SetByArchiveScan => {
                        assert!(a[key.name].is_null(), "active scan clears `{}`", key.name)
                    }
                    AbsentRule::DeleteWhenAbsent => {
                        let declared = build_metadata(&u).contains_key(key.name);
                        if declared || !declaration_rejected(key.name, &u) {
                            assert!(present, "`{}` sent as value or null: {active}", key.name)
                        }
                    }
                    AbsentRule::OmitWhenAbsent => {}
                }
            }
            assert_eq!(archive["archive_path"], serde_json::json!(u.source_path));
        }
    }

    /// The patches never put a wholesale `metadata` on the wire.
    #[tokio::test]
    async fn every_push_sends_a_patch_and_never_a_wholesale_metadata() {
        struct Rec(Mutex<Vec<Value>>, CoordMetadataModel);
        #[async_trait::async_trait]
        impl WorkUnitSink for Rec {
            async fn current_status(&self, s: &str, sc: TenantScope) -> Result<Option<String>> {
                self.1.current_status(s, sc).await
            }
            async fn last_actor(&self, s: &str, sc: TenantScope) -> Result<Option<String>> {
                self.1.last_actor(s, sc).await
            }
            async fn upsert(&self, b: &UpsertBody, sc: TenantScope) -> Result<()> {
                self.0
                    .lock()
                    .unwrap()
                    .push(serde_json::to_value(b).unwrap());
                self.1.upsert(b, sc).await
            }
            async fn transition(&self, s: &str, b: &TransitionBody, sc: TenantScope) -> Result<()> {
                self.1.transition(s, b, sc).await
            }
            async fn set_deps(
                &self,
                s: &str,
                d: &[String],
                sc: TenantScope,
            ) -> Result<SetDepsOutcome> {
                self.1.set_deps(s, d, sc).await
            }
        }
        for phases in [&[1u32][..], &[][..]] {
            let u = phases_unit("s", phases);
            let rec = Rec(Mutex::default(), CoordMetadataModel::default());
            scan_via(&rec, &rec.1, &u, Arm::Create).await;
            for arm in ARMS {
                scan_via(&rec, &rec.1, &u, arm).await;
            }
            push_archive_metadata(&rec, &u, TenantScope::Unresolved)
                .await
                .unwrap();
            let bodies = rec.0.lock().unwrap();
            assert!(bodies.len() > ARMS.len());
            for b in bodies.iter() {
                assert!(b.get("metadata").is_none(), "no wholesale metadata: {b}");
                assert!(b["metadata_patch"].is_object(), "a patch is sent: {b}");
                assert_eq!(b["by_actor"], ADAPTER_ACTOR);
            }
        }
    }

    // ---- T5 ---------------------------------------------------------------

    /// Frozen copy of the pre-Phase-1 `push.rs` `build_metadata` +
    /// `metadata_arms` (qontinui-runner `cdf536b68`), kept ONLY so T5 can
    /// compare against it. Returns `(metadata, metadata_patch)`.
    fn frozen_metadata_arms(u: &ParsedWorkUnit) -> (Option<Value>, Option<Value>) {
        let mut m = serde_json::json!({
            "depends_on": u.depends_on,
            "phases": u
                .phases
                .iter()
                .map(|p| serde_json::json!({"index": p.index, "name": p.name}))
                .collect::<Vec<_>>(),
            "source_path": u.source_path,
        });
        if let Some(area) = &u.area {
            m["area"] = Value::String(area.clone());
        }
        if !u.phases.is_empty() {
            return (Some(m), None);
        }
        let mut patch = m;
        if let Some(obj) = patch.as_object_mut() {
            obj.remove("phases");
            obj.entry("area").or_insert(Value::Null);
            obj.insert("archive_path".to_string(), Value::Null);
        }
        (None, Some(patch))
    }

    /// Where nobody but the scanner writes, the change is invisible: for a
    /// stored object holding only scanner keys and `attestations`, the patch
    /// resolves to exactly what the old arms resolved to. (A REJECTED `Area:`
    /// is excluded on purpose: D2 changed it from delete to keep.)
    #[test]
    fn a_unit_only_the_scanner_writes_ends_identical_to_the_old_arms() {
        let stored_states = [
            None,
            Some(serde_json::json!({})),
            Some(serde_json::json!({
                "source_path": "plans/old.md",
                "depends_on": ["x", "y"],
                "phases": [{"index": 1, "name": "a"}, {"index": 2, "name": "b"}, {"index": 3, "name": "c"}],
                "area": "old-area",
                "archive_path": "archive/old.md",
                "attestations": [{"status": "vetted"}],
            })),
            Some(serde_json::json!({
                "source_path": "plans/s.md",
                "depends_on": [],
                "attestations": [],
            })),
        ];
        let mut compared = 0;
        for u in roster_probe_units()
            .into_iter()
            .filter(|u| u.area_rejected.is_none())
        {
            for stored in &stored_states {
                let (m, p) = frozen_metadata_arms(&u);
                let old = coord_resolve(m.as_ref(), p.as_ref(), stored.as_ref());
                let new = coord_resolve(None, Some(&active_dir_patch(&u)), stored.as_ref());
                assert_eq!(new, old, "unit {u:?}, stored {stored:?}");
                compared += 1;
            }
        }
        assert_eq!(compared, 8 * stored_states.len());
    }

    // ---- Moved from push.rs, rewritten against the patches ----------------

    #[test]
    fn build_metadata_carries_enrichment() {
        let m = build_metadata(&unit("s", "vetted"));
        assert_eq!(m["depends_on"][0], "2026-01-01-dep");
        assert_eq!(m["phases"][0]["index"], 1);
        assert_eq!(m["source_path"], "plans/s.md");
    }

    /// `area` rides on the active patch as a value when declared and as `null`
    /// (RFC 7396 delete) when not, in both phase arms.
    #[test]
    fn active_patch_area_is_a_value_when_declared_and_null_when_not() {
        for phases in [&[1u32][..], &[][..]] {
            let with = ParsedWorkUnit {
                area: Some("published-parity".to_string()),
                ..phases_unit("s", phases)
            };
            assert_eq!(active_dir_patch(&with)["area"], "published-parity");
            let p = active_dir_patch(&phases_unit("s", phases));
            assert!(p.as_object().unwrap().contains_key("area") && p["area"].is_null());
            assert_eq!(p["source_path"], "plans/s.md");
        }
    }

    /// The archive stamp carries a declared area, and an explicit `null` when
    /// none is declared.
    #[tokio::test]
    async fn archive_stamp_keeps_a_declared_area() {
        let with = ParsedWorkUnit {
            area: Some("ci-runners".to_string()),
            ..unit("2026-01-01-archived", "shipped")
        };
        let p = archive_dir_patch(&with);
        assert_eq!(p["area"], "ci-runners");
        assert_eq!(p["archive_path"], serde_json::json!(with.source_path));
        let none = archive_dir_patch(&unit("2026-01-01-archived-2", "shipped"));
        assert!(none.as_object().unwrap().contains_key("area") && none["area"].is_null());
    }

    /// The archive stamp is a PATCH that cannot drop stored phases, source
    /// path or dependencies.
    #[tokio::test]
    async fn archive_stamp_is_a_patch_that_cannot_drop_stored_phases() {
        let model = CoordMetadataModel::default();
        let u = unit("2026-01-01-archived", "vetted");
        scan(&model, &u, Arm::Create).await;
        push_archive_metadata(&model, &u, TenantScope::Unresolved)
            .await
            .unwrap();
        let stored = model.stored(&u.slug);
        assert_eq!(stored["archive_path"], serde_json::json!(u.source_path));
        assert_eq!(stored["phases"][0]["index"], 1);
        assert_eq!(stored["source_path"], serde_json::json!(u.source_path));
        assert_eq!(stored["depends_on"][0], "2026-01-01-dep");
        let p = archive_dir_patch(&u);
        for k in ["phases", "source_path", "depends_on"] {
            assert!(
                !p.as_object().unwrap().contains_key(k),
                "{k} not in the archive patch"
            );
        }
    }

    /// No phases detected: the patch has no `phases` key on any arm, so
    /// coord keeps its stored list; a stale `archive_path` is cleared.
    #[tokio::test]
    async fn no_detected_phases_sends_a_patch_that_omits_phases_on_every_upsert() {
        let none = phases_unit("s", &[]);
        let p = active_dir_patch(&none);
        let obj = p.as_object().unwrap();
        assert!(!obj.contains_key("phases"), "{p}");
        assert_eq!(p["source_path"], "plans/s.md");
        assert_eq!(p["depends_on"][0], "2026-01-01-dep");
        assert!(p["area"].is_null());
        assert!(obj.contains_key("archive_path") && p["archive_path"].is_null());
    }
}
