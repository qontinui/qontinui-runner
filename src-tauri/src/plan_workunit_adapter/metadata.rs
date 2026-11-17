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
//! - **The phase declaration is three keys resolved jointly** —
//!   `phases` (numbered), `phase_labels` (lettered, `[{"label", "name"}]`) and
//!   `phases_undeclared` (title tokens neither grammar reads). When the scan
//!   declares anything in any of them, ALL THREE are sent, an empty one as
//!   `[]`; when it declares nothing, all three are omitted, so the stored
//!   declaration is kept ([`AbsentRule::OmitJointlyWhenNothingDeclared`], the
//!   2026-09-16 wipe guard widened to three keys). So a plan renumbered from
//!   letters to numbers clears its stale labels, and the reverse. Labels ride
//!   in their own key rather than inside `phases` because a coord that does not
//!   read them would see a label entry among numbered ones as an unreadable
//!   declaration and un-gate the unit; a separate key is ignored by it (plan
//!   `2026-11-09-lettered-phase-headings-never-reach-coords-phase-declaration`,
//!   D2). Roster precondition, measured: that plan's Phase 0 step 3 read
//!   tenant `c231d9da` on 2026-11-11 and found 0 of 6006 work units carrying
//!   `phase_labels` or `phases_undeclared` (coord finding
//!   `4e77fe68-1d31-46a8-8c8d-daedd887746b`); the device's other bound tenants
//!   were not read.
//!
//! A stale key the scanner does NOT own is its writer's to remove with
//! `{key: null}`; the scanner cannot know whether it is stale and must not guess.

use super::parser::ParsedWorkUnit;
use serde_json::{Map, Value};

/// What the active-directory scan sends for a roster key the plan file does not
/// supply a value for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AbsentRule {
    /// The file always yields a value (`[]` for an empty list), so the key is
    /// on every active patch.
    AlwaysSent,
    /// A status-block header key: `null` (RFC 7396 delete) when the file
    /// declares none, so the plan file stays its sole author. When the parser
    /// REJECTED a declaration the key is OMITTED instead: a parse that could
    /// not read the line has no information about the stored value. Header
    /// keys also ride on the archive patch, with the same rule.
    DeleteWhenAbsent,
    /// Omitted when absent, so the stored value is kept: a scan that finds no
    /// value has no information about a good stored one (`repos`).
    OmitWhenAbsent,
    /// One of the three phase-declaration keys (`phases`, `phase_labels`,
    /// `phases_undeclared`), resolved TOGETHER: when the scan declares
    /// anything in any of them, all three are sent, an empty one as `[]`;
    /// when it declares nothing, all three are omitted. The omission is the
    /// 2026-09-16 guard: a scan that detects no phases at all has no
    /// information about a good stored declaration, and `phases: []` would
    /// erase it. Sending the empties when something IS declared is what lets
    /// a plan renumbered from letters to numbers (or back) clear the stale
    /// half.
    ///
    /// Accepted trade-off: a plan whose phases are all removed on purpose (or
    /// whose headings all stop parsing) no longer clears the stored
    /// declaration from the adapter push. A wiped list is unrecoverable; a
    /// stale one is visible.
    OmitJointlyWhenNothingDeclared,
    /// Owned by the archive scan, which sets it. The active scan always sends
    /// `null`, so a plan restored from the archive directory clears it.
    SetByArchiveScan,
}

/// One scanner-owned `metadata` key.
#[derive(Debug, Clone, Copy)]
struct ScannerKey {
    name: &'static str,
    absent_rule: AbsentRule,
}

/// The keys the plan adapter owns on a work unit's `metadata`. The scanner
/// writes these and nothing else; see the module doc for the contract.
///
/// Private with its types on purpose: nothing outside this module may decide
/// what a patch holds.
const SCANNER_OWNED_KEYS: &[ScannerKey] = &[
    // The plan file's repo-relative path.
    ScannerKey {
        name: "source_path",
        absent_rule: AbsentRule::AlwaysSent,
    },
    // Status-block `Depends-On:` (`[]` when none).
    ScannerKey {
        name: "depends_on",
        absent_rule: AbsentRule::AlwaysSent,
    },
    // Status-block `Area:`.
    ScannerKey {
        name: "area",
        absent_rule: AbsentRule::DeleteWhenAbsent,
    },
    // Status-block `Initiative-Item:`.
    ScannerKey {
        name: "initiative_item",
        absent_rule: AbsentRule::DeleteWhenAbsent,
    },
    // The archived file's path, stamped by `push_archive_metadata`.
    ScannerKey {
        name: "archive_path",
        absent_rule: AbsentRule::SetByArchiveScan,
    },
    // `detect_phases`: numbered phases, `[{"index", "name"}]`.
    ScannerKey {
        name: "phases",
        absent_rule: AbsentRule::OmitJointlyWhenNothingDeclared,
    },
    // `detect_phases`: lettered arm-A phases, `[{"label", "name"}]` (plan
    // `2026-11-09-lettered-phase-headings-never-reach-coords-phase-declaration`
    // D1/D2).
    ScannerKey {
        name: "phase_labels",
        absent_rule: AbsentRule::OmitJointlyWhenNothingDeclared,
    },
    // `detect_phases`: arm-A phase-title tokens neither grammar reads,
    // `["Two"]` — so coord reads the declaration as partial (same plan, D3).
    ScannerKey {
        name: "phases_undeclared",
        absent_rule: AbsentRule::OmitJointlyWhenNothingDeclared,
    },
    // The plan file's `Repo(s):` / `Repos:` line (`body_push::extract_repos`).
    // `repos` has OTHER writers: sessions hand-write `metadata.repos` with
    // `coord_work_unit_upsert`, in both `qontinui/<repo>` and bare spellings
    // (measured on stored plan-backed units by plan
    // `2026-11-02-plan-backed-work-units-carry-no-repos-so-coords-repo-readers-read-empty`). So it
    // is OmitWhenAbsent, never DeleteWhenAbsent: a plan with no `Repo(s):`
    // line (most of the corpus) leaves a hand-written list alone, while a plan
    // that declares one overwrites it — the roster's documented precedence.
    // coord reads it in `project_state.rs` (one-screen grouping) and
    // `next_step.rs` `resolve_plan_repos` (continuation worktrees). Plan
    // `2026-11-02-plan-backed-work-units-carry-no-repos-so-coords-repo-readers-read-empty` D3.
    ScannerKey {
        name: "repos",
        absent_rule: AbsentRule::OmitWhenAbsent,
    },
];

/// True when the parser saw a declaration for `key` and rejected it. The two
/// status-block header keys, `area` and `initiative_item`, have parsers that
/// report rejection.
fn declaration_rejected(key: &str, u: &ParsedWorkUnit) -> bool {
    match key {
        "area" => u.area_rejected.is_some(),
        "initiative_item" => u.initiative_item_rejected.is_some(),
        _ => false,
    }
}

/// True when the scan declares anything in any of the three phase-declaration
/// keys — a numbered phase, a label, or an undeclared title token.
fn declares_any_phase(u: &ParsedWorkUnit) -> bool {
    !u.phases.is_empty() || !u.phase_labels.is_empty() || !u.phases_undeclared.is_empty()
}

/// The scan's phase declaration: the three keys `phases` (`[{"index",
/// "name"}]`), `phase_labels` (`[{"label", "name"}]`) and `phases_undeclared`
/// (`["Two"]`) as one JSON object, the empty ones as `[]` — or `None` when the
/// plan declares nothing in any of them ([`AbsentRule::OmitJointlyWhenNothingDeclared`]).
///
/// The ONE place the declaration's wire shape is built: [`build_metadata`]
/// writes exactly this object into every active-directory patch, and
/// `qontinui-pr plan-phase-declaration` prints it, so a session that mints a
/// plan work unit by hand sends the runner's own declaration rather than a
/// re-derivation of it (plan
/// `2026-11-17-a-session-minted-plan-work-unit-carries-no-phase-declaration`).
pub fn phase_declaration(u: &ParsedWorkUnit) -> Option<Value> {
    if !declares_any_phase(u) {
        return None;
    }
    let mut d = Map::new();
    d.insert(
        "phases".to_string(),
        Value::Array(
            u.phases
                .iter()
                .map(|p| serde_json::json!({"index": p.index, "name": p.name}))
                .collect(),
        ),
    );
    d.insert(
        "phase_labels".to_string(),
        Value::Array(
            u.phase_labels
                .iter()
                .map(|p| serde_json::json!({"label": p.label, "name": p.name}))
                .collect(),
        ),
    );
    d.insert(
        "phases_undeclared".to_string(),
        serde_json::json!(u.phases_undeclared),
    );
    Some(Value::Object(d))
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
    // AbsentRule::OmitJointlyWhenNothingDeclared: all three, or none.
    if let Some(Value::Object(declaration)) = phase_declaration(u) {
        m.extend(declaration);
    }
    if let Some(area) = &u.area {
        m.insert("area".to_string(), Value::String(area.clone()));
    }
    if let Some(item) = &u.initiative_item {
        m.insert("initiative_item".to_string(), Value::String(item.clone()));
    }
    if !u.repos.is_empty() {
        m.insert("repos".to_string(), serde_json::json!(u.repos));
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
            AbsentRule::AlwaysSent
            | AbsentRule::OmitWhenAbsent
            | AbsentRule::OmitJointlyWhenNothingDeclared => {}
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
            AbsentRule::AlwaysSent
            | AbsentRule::OmitWhenAbsent
            | AbsentRule::OmitJointlyWhenNothingDeclared => {}
        }
    }
    Value::Object(patch)
}

#[cfg(test)]
mod tests {
    use super::super::parser::{HeaderRejection, ParsedLabelPhase, ParsedPhase};
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

    fn label(label: &str) -> ParsedLabelPhase {
        ParsedLabelPhase {
            label: label.to_string(),
            name: format!("Phase {label}"),
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
            initiative_item: None,
            initiative_item_rejected: None,
            phases: vec![phase(1)],
            phase_labels: vec![],
            phases_undeclared: vec![],
            repos: vec![],
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
                area_rejected: Some(HeaderRejection::NotKebab("Work Units".to_string())),
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
            area_rejected: Some(HeaderRejection::Misspelled("**Area**:")),
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
                        for repos in [vec![], vec!["qontinui-coord".to_string()]] {
                            out.push(ParsedWorkUnit {
                                area: area.map(str::to_string),
                                area_rejected: rejected
                                    .then(|| HeaderRejection::NotKebab("X".to_string())),
                                depends_on: deps.clone(),
                                repos,
                                ..phases_unit("s", phases)
                            });
                        }
                    }
                }
            }
        }
        out
    }

    /// Probe units for the phase-declaration dimension: every combination of
    /// numbered phases, labels and undeclared tokens being present or not.
    fn phase_declaration_probe_units() -> Vec<ParsedWorkUnit> {
        let mut out = vec![];
        for phases in [&[][..], &[0u32][..]] {
            for labels in [&[][..], &["A", "B"][..]] {
                for undeclared in [&[][..], &["Two"][..]] {
                    out.push(ParsedWorkUnit {
                        phase_labels: labels.iter().copied().map(label).collect(),
                        phases_undeclared: undeclared.iter().map(|t| t.to_string()).collect(),
                        ..phases_unit("s", phases)
                    });
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

        for u in roster_probe_units()
            .into_iter()
            .chain(phase_declaration_probe_units())
        {
            let active = active_dir_patch(&u);
            let archive = archive_dir_patch(&u);
            for (which, patch) in [("active", &active), ("archive", &archive)] {
                for (k, v) in patch.as_object().expect("a patch is an object") {
                    let key = SCANNER_OWNED_KEYS
                        .iter()
                        .find(|key| key.name == k)
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
                    AbsentRule::OmitWhenAbsent => assert_eq!(
                        present,
                        build_metadata(&u).contains_key(key.name),
                        "`{}` is sent exactly when the file supplies it: {active}",
                        key.name
                    ),
                    AbsentRule::OmitJointlyWhenNothingDeclared => {
                        assert_eq!(
                            present,
                            declares_any_phase(&u),
                            "`{}` is sent exactly when the scan declares any phase: {active}",
                            key.name
                        );
                        if present {
                            assert!(a[key.name].is_array(), "`{}` is a list: {active}", key.name);
                        }
                    }
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
        // Not in the frozen arms: `repos` postdates them. Passed through so a
        // unit that declares repos compares exactly on every OTHER key, and
        // T5 still runs over the roster's `repos` dimension.
        if !u.repos.is_empty() {
            m["repos"] = serde_json::json!(u.repos);
        }
        // Not in the frozen arms either: `phase_labels` and `phases_undeclared`
        // postdate them (plan
        // `2026-11-09-lettered-phase-headings-never-reach-coords-phase-declaration`).
        // Under the joint absent rule a unit that declares numbered phases also
        // sends both as `[]`; passed through so T5 compares every OTHER key
        // exactly. The probe units carry no labels and no undeclared tokens.
        if !u.phases.is_empty() {
            m["phase_labels"] = serde_json::json!([]);
            m["phases_undeclared"] = serde_json::json!([]);
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
        assert_eq!(compared, 16 * stored_states.len());
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
    #[test]
    fn archive_stamp_keeps_a_declared_area() {
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

    /// No phases detected: the active patch has no `phases` key, so
    /// coord keeps its stored list; a stale `archive_path` is cleared.
    #[test]
    fn active_patch_omits_phases_when_none_detected() {
        let none = phases_unit("s", &[]);
        let p = active_dir_patch(&none);
        let obj = p.as_object().unwrap();
        assert!(!obj.contains_key("phases"), "{p}");
        assert_eq!(p["source_path"], "plans/s.md");
        assert_eq!(p["depends_on"][0], "2026-01-01-dep");
        assert!(p["area"].is_null());
        assert!(obj.contains_key("archive_path") && p["archive_path"].is_null());
    }

    /// `initiative_item` follows `area`'s header rule on the active patch: a
    /// value when declared, `null` (RFC 7396 delete) when not, and OMITTED
    /// when the parser rejected the declaration — in both phase arms.
    #[test]
    fn active_patch_initiative_item_follows_the_header_rule() {
        for phases in [&[1u32][..], &[][..]] {
            let with = ParsedWorkUnit {
                initiative_item: Some("one-screen-project-state".to_string()),
                ..phases_unit("s", phases)
            };
            assert_eq!(
                active_dir_patch(&with)["initiative_item"],
                "one-screen-project-state"
            );
            let p = active_dir_patch(&phases_unit("s", phases));
            assert!(
                p.as_object().unwrap().contains_key("initiative_item")
                    && p["initiative_item"].is_null(),
                "an undeclared initiative item is an explicit null: {p}"
            );
            let rejected = ParsedWorkUnit {
                initiative_item_rejected: Some(HeaderRejection::NotKebab("One Screen".to_string())),
                ..phases_unit("s", phases)
            };
            let p = active_dir_patch(&rejected);
            assert!(
                !p.as_object().unwrap().contains_key("initiative_item"),
                "a rejected initiative item is omitted: {p}"
            );
        }
    }

    /// The archive stamp carries a declared initiative item, an explicit
    /// `null` when none is declared, and omits a rejected one.
    #[test]
    fn archive_stamp_keeps_a_declared_initiative_item() {
        let with = ParsedWorkUnit {
            initiative_item: Some("one-screen-project-state".to_string()),
            ..unit("2026-01-01-archived", "shipped")
        };
        assert_eq!(
            archive_dir_patch(&with)["initiative_item"],
            "one-screen-project-state"
        );
        let none = archive_dir_patch(&unit("2026-01-01-archived-2", "shipped"));
        assert!(
            none.as_object().unwrap().contains_key("initiative_item")
                && none["initiative_item"].is_null()
        );
        let rejected = ParsedWorkUnit {
            initiative_item_rejected: Some(HeaderRejection::Misspelled("**Initiative-Item**:")),
            ..unit("2026-01-01-archived-3", "shipped")
        };
        assert!(!archive_dir_patch(&rejected)
            .as_object()
            .unwrap()
            .contains_key("initiative_item"));
    }

    /// End to end through coord's merge in every arm: a declared initiative
    /// item is stored, survives a scan whose `Initiative-Item:` line the
    /// parser rejected, and is deleted by a scan that declares none — while a
    /// session-written key beside it survives throughout.
    #[tokio::test]
    async fn initiative_item_is_kept_on_rejection_and_deleted_when_undeclared() {
        for arm in ARMS {
            let model = CoordMetadataModel::default();
            let declared = ParsedWorkUnit {
                initiative_item: Some("one-screen".to_string()),
                ..phases_unit("s", &[1])
            };
            scan(&model, &declared, Arm::Create).await;
            model.session_patch("s", serde_json::json!({"phase_vets": ["x"]}));
            assert_eq!(model.stored("s")["initiative_item"], "one-screen");

            let rejected = ParsedWorkUnit {
                initiative_item_rejected: Some(HeaderRejection::NotKebab("One Screen".to_string())),
                ..phases_unit("s", &[1])
            };
            scan(&model, &rejected, arm).await;
            assert_eq!(
                model.stored("s")["initiative_item"],
                "one-screen",
                "{arm:?}: a rejected Initiative-Item: keeps the stored value"
            );

            scan(&model, &phases_unit("s", &[1]), arm).await;
            let stored = model.stored("s");
            assert!(
                !stored.as_object().unwrap().contains_key("initiative_item"),
                "{arm:?}: an undeclared initiative item is deleted: {stored}"
            );
            assert_eq!(stored["phase_vets"], serde_json::json!(["x"]), "{arm:?}");
        }
    }

    // ---- repos (plan 2026-11-02-…-repo-readers-read-empty, Phase 2) ----------

    /// D3: a session hand-writes `metadata.repos`; a scan of a plan with NO
    /// `Repo(s):` line keeps it, in every arm. The active patch omits the key.
    /// Fails under `AbsentRule::DeleteWhenAbsent` (the patch would send
    /// `repos: null` and coord's merge would delete the hand-written list).
    #[tokio::test]
    async fn a_session_written_repos_survives_a_scan_of_a_plan_with_no_repo_line() {
        for arm in ARMS {
            for phases in [&[1u32][..], &[][..]] {
                let model = CoordMetadataModel::default();
                let u = phases_unit("s", phases);
                assert!(u.repos.is_empty());
                scan(&model, &u, Arm::Create).await;
                model.session_patch(
                    "s",
                    serde_json::json!({"repos": ["qontinui/qontinui-runner", "qontinui-coord"]}),
                );
                assert!(
                    !active_dir_patch(&u)
                        .as_object()
                        .unwrap()
                        .contains_key("repos"),
                    "no declaration, no `repos` on the patch"
                );
                scan(&model, &u, arm).await;
                assert_eq!(
                    model.stored("s")["repos"],
                    serde_json::json!(["qontinui/qontinui-runner", "qontinui-coord"]),
                    "{arm:?}, phases {phases:?}: hand-written repos kept"
                );
            }
        }
    }

    /// A plan that DECLARES `Repo(s):` emits `repos`, so the plan file wins
    /// over a hand-written value — the roster's documented precedence. Never
    /// on the archive patch (OmitWhenAbsent keys do not ride it).
    #[tokio::test]
    async fn a_repo_declaration_overwrites_a_hand_written_repos() {
        for arm in ARMS {
            let model = CoordMetadataModel::default();
            let first = phases_unit("s", &[1]);
            scan(&model, &first, Arm::Create).await;
            model.session_patch("s", serde_json::json!({"repos": ["no", "so"]}));
            let declared = ParsedWorkUnit {
                repos: vec!["qontinui-runner".to_string(), "qontinui-coord".to_string()],
                ..phases_unit("s", &[1])
            };
            scan(&model, &declared, arm).await;
            assert_eq!(
                model.stored("s")["repos"],
                serde_json::json!(["qontinui-runner", "qontinui-coord"]),
                "{arm:?}: the declaration overwrites"
            );
            assert!(!archive_dir_patch(&declared)
                .as_object()
                .unwrap()
                .contains_key("repos"));
        }
    }

    /// Through the real parser: a census-prose `Repo(s):` line puts only repo
    /// names in the emitted metadata — no `no`, `so`, `main`, owner prefix or
    /// trailing punctuation.
    #[test]
    fn census_prose_repo_lines_emit_only_repo_names() {
        use super::super::parser::{parse_work_unit, PlanConvention};
        let cases: [(&str, &[&str]); 4] = [
            (
                "> **Status: VETTED 2026-11-05.**\n> **Repo(s):** qontinui-coord only — no web migration, no schema change\n",
                &["qontinui-coord"],
            ),
            (
                "> **Repo(s):** `qontinui/qontinui-web` and `qontinui/qontinui-coord`. Two repos, two PRs.\n",
                &["qontinui-web", "qontinui-coord"],
            ),
            (
                "**Repo(s):** qontinui-coord (all phases). qontinui-web / qontinui-runner are read for a consumer census only (Phase 4), not modified.\n",
                &["qontinui-coord"],
            ),
            (
                "> **Repo(s):** `qontinui-runner` only. CI-only change: no Rust, no TypeScript, no migration, no DDL.\n",
                &["qontinui-runner"],
            ),
        ];
        for (body, want) in cases {
            let u = parse_work_unit(
                "s",
                "plans/s.md",
                &format!("# T\n\n{body}"),
                &PlanConvention::operator_default(),
            );
            assert_eq!(
                active_dir_patch(&u)["repos"],
                serde_json::json!(want),
                "{body}"
            );
        }
        let none = parse_work_unit(
            "s",
            "plans/s.md",
            "# T\n\nno repo line\n",
            &PlanConvention::operator_default(),
        );
        assert!(!active_dir_patch(&none)
            .as_object()
            .unwrap()
            .contains_key("repos"));
    }

    // ---- phase_labels / phases_undeclared (plan
    // `2026-11-09-lettered-phase-headings-never-reach-coords-phase-declaration`,
    // Phase 2) -------------------------------------------------------------------

    fn phase_keys(p: &Value) -> Vec<&'static str> {
        let obj = p.as_object().expect("a patch is an object");
        ["phases", "phase_labels", "phases_undeclared"]
            .into_iter()
            .filter(|k| obj.contains_key(*k))
            .collect()
    }

    /// D2's joint absent rule on the active patch: anything declared sends all
    /// three keys, the empty ones as `[]`; nothing declared sends none of them.
    /// The archive patch never carries any of the three. Fails if the empties
    /// are not sent (a renumbered plan would keep its stale half) or if the
    /// omission is dropped (the 2026-09-16 wipe).
    #[test]
    fn the_phase_declaration_keys_are_sent_jointly_or_not_at_all() {
        let labels_only = ParsedWorkUnit {
            phase_labels: vec![label("A"), label("B")],
            ..phases_unit("s", &[])
        };
        let p = active_dir_patch(&labels_only);
        assert_eq!(p["phases"], serde_json::json!([]), "{p}");
        assert_eq!(
            p["phase_labels"],
            serde_json::json!([
                {"label": "A", "name": "Phase A"},
                {"label": "B", "name": "Phase B"},
            ]),
            "{p}"
        );
        assert_eq!(p["phases_undeclared"], serde_json::json!([]), "{p}");

        let numeric_only = phases_unit("s", &[1]);
        let p = active_dir_patch(&numeric_only);
        assert_eq!(p["phases"][0]["index"], 1, "{p}");
        assert_eq!(p["phase_labels"], serde_json::json!([]), "{p}");
        assert_eq!(p["phases_undeclared"], serde_json::json!([]), "{p}");

        let undeclared_only = ParsedWorkUnit {
            phases_undeclared: vec!["Two".to_string()],
            ..phases_unit("s", &[])
        };
        let p = active_dir_patch(&undeclared_only);
        assert_eq!(p["phases"], serde_json::json!([]), "{p}");
        assert_eq!(p["phase_labels"], serde_json::json!([]), "{p}");
        assert_eq!(p["phases_undeclared"], serde_json::json!(["Two"]), "{p}");

        let nothing = phases_unit("s", &[]);
        assert!(
            phase_keys(&active_dir_patch(&nothing)).is_empty(),
            "nothing declared, none of the three sent: {}",
            active_dir_patch(&nothing)
        );

        for u in [&labels_only, &numeric_only, &undeclared_only, &nothing] {
            assert!(
                phase_keys(&archive_dir_patch(u)).is_empty(),
                "the archive patch never carries a phase key: {}",
                archive_dir_patch(u)
            );
        }
    }

    /// End to end through coord's merge, in every arm: a plan renumbered from
    /// letters to numbers clears its stale labels, the reverse clears its
    /// stale numbers, an undeclared token is cleared once the title parses,
    /// and a scan that declares nothing keeps all three stored keys.
    #[tokio::test]
    async fn a_renumbered_plan_clears_its_stale_half_and_an_empty_scan_keeps_all_three() {
        let indices = |v: &Value| -> Vec<u64> {
            v["phases"]
                .as_array()
                .unwrap()
                .iter()
                .map(|p| p["index"].as_u64().unwrap())
                .collect()
        };
        let labels = |v: &Value| -> Vec<String> {
            v["phase_labels"]
                .as_array()
                .unwrap()
                .iter()
                .map(|p| p["label"].as_str().unwrap().to_string())
                .collect()
        };
        for arm in ARMS {
            let model = CoordMetadataModel::default();
            let mixed = ParsedWorkUnit {
                phase_labels: vec![label("A"), label("B")],
                phases_undeclared: vec!["Two".to_string()],
                ..phases_unit("s", &[0])
            };
            scan(&model, &mixed, Arm::Create).await;
            let stored = model.stored("s");
            assert_eq!(indices(&stored), vec![0], "{stored}");
            assert_eq!(labels(&stored), vec!["A", "B"], "{stored}");
            assert_eq!(stored["phases_undeclared"], serde_json::json!(["Two"]));

            // Letters renumbered to numbers: the stale labels and token go.
            scan(&model, &phases_unit("s", &[1, 2]), arm).await;
            let stored = model.stored("s");
            assert_eq!(indices(&stored), vec![1, 2], "{arm:?}: {stored}");
            assert!(
                labels(&stored).is_empty(),
                "{arm:?}: stale labels cleared: {stored}"
            );
            assert_eq!(
                stored["phases_undeclared"],
                serde_json::json!([]),
                "{arm:?}"
            );

            // And back: the stale numbers go.
            let lettered = ParsedWorkUnit {
                phase_labels: vec![label("C")],
                ..phases_unit("s", &[])
            };
            scan(&model, &lettered, arm).await;
            let stored = model.stored("s");
            assert!(
                indices(&stored).is_empty(),
                "{arm:?}: stale numbers cleared: {stored}"
            );
            assert_eq!(labels(&stored), vec!["C"], "{arm:?}: {stored}");

            // A scan that declares nothing keeps all three as stored.
            let before = model.stored("s");
            scan(&model, &phases_unit("s", &[]), arm).await;
            let stored = model.stored("s");
            for k in ["phases", "phase_labels", "phases_undeclared"] {
                assert_eq!(stored[k], before[k], "{arm:?}: `{k}` kept: {stored}");
            }
        }
    }

    /// Through the real parser: the census unit's shape (`### Phase 0 …`, then
    /// `### Phase A:` … `### Phase F:`) reaches the patch as `phases: [0]`
    /// beside `phase_labels: A..F` and an empty `phases_undeclared`.
    #[test]
    fn the_census_shaped_plan_reaches_the_patch_with_its_labels() {
        use super::super::parser::{parse_work_unit, PlanConvention};
        let body = "# T\n\n> **Status: IN PROGRESS 2026-11-11.**\n\n### Phase 0 (read-only): re-verify\n\n### Phase A: one\n\n### Phase B: two\n\n### Phase C: three\n\n### Phase D: four\n\n### Phase E: five\n\n### Phase F: six\n";
        let u = parse_work_unit("s", "plans/s.md", body, &PlanConvention::operator_default());
        let p = active_dir_patch(&u);
        assert_eq!(
            p["phases"],
            serde_json::json!([{"index": 0, "name": "Phase 0 (read-only): re-verify"}]),
            "{p}"
        );
        let got: Vec<&str> = p["phase_labels"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| l["label"].as_str().unwrap())
            .collect();
        assert_eq!(got, vec!["A", "B", "C", "D", "E", "F"], "{p}");
        assert_eq!(p["phase_labels"][0]["name"], "Phase A: one", "{p}");
        assert_eq!(p["phases_undeclared"], serde_json::json!([]), "{p}");
    }

    // ---- phase_declaration (plan
    // `2026-11-17-a-session-minted-plan-work-unit-carries-no-phase-declaration`,
    // Phase 1) -------------------------------------------------------------------

    /// Every `# `-headed body containing `Phase` in `parser.rs`'s test module,
    /// copied verbatim (the `format!` templates excluded). Bodies that declare
    /// nothing stay in: they prove the `None` arm.
    const PARSER_TEST_PHASE_BODIES: &[&str] = &[
        "# T\n\n> **Status: in progress 2026-06-19.** Implementing Phase 1 only; gate Phases 2-4.\n\n**Phase 1 — parser.** body\n\n## Phase 2: push client\n\nProse mentioning Phase 1 again should not count.\n",
        "# T\n\n## Phases\n\n- **Phase 1 — schema.** body\n1. **Phase 2 — wiring** (~40 LOC)\n  - **Phase 9 — nested** indented\n",
        "# T\n\n## 9. What this unblocks\n\n- **Phase 2 — the reaper** (another plan)\n- **Phase 3 — the dashboard**\n",
        "# T\n\n## 4. Phases\n\n1. **Schema freeze** — author the schemas\n2. Rubric semantics\n   3. nested step\n\n## 5. Risks\n\n7. not a phase\n",
        "# T\n\n## 5. Phases\n\n### Phase 1 — the guard\n\n1. hook\n2. prefilter\n3. message\n",
        "# T\n\n| Phase | Deliverable |\n|---|---|\n| 1 | re-export forms |\n| **2** | basis |\n| Phase 3 | label |\n| ~~4~~ | gone |\n\n| # | Claim |\n|---|---|\n| 5 | not a phase |\n",
        "# T\n\n## Phases\n\n0. probe\n1. ~~relay~~ gone\n2. migrate\n3. **~~relay two~~** gone\n",
        "# T\n\n## Phases\n\n- **Phase 1 — schema**\n- **Phase 4 (not scheduled)** later\n- **Phase 6 — relay** (deferred)\n7. **Relay** — SPLIT to a follow-up plan\n\n| Phase | Work | State |\n|---|---|---|\n| 2 | wiring | open |\n| **3 — further instances (DELEGATED to their own plans)** | x | |\n| 5 | audit | Not built |\n| 8 | ~~gone~~ | |\n",
        "# T\n\n> **Phase 4 is HELD**, not forgotten.\n\n```\n## Phase 7\n**Phase 8 — example**\n| Phase | x |\n|---|---|\n| 9 | y |\n```\n\n~~~\n- **Phase 6 — example**\n~~~\n",
        "# T\n\n````md\n```\n## Phase 7\n```\n## Phase 8\n````\n\n```\n~~~\n## Phase 9\n```rust\n## Phase 10\n```\n\n## Phase 1\n",
        "# T\n\n```md\nexample\n\n## Phase 3 — after a stray fence\n",
        "# T\n\n## Phases\n\n**Phase 1 — the guard**\n\n1. hook\n2. prefilter\n3. message\n4. register\n",
        "# T\n\n## Phases\n\n**Phase 0 is a gate, not a step.**\n\n0. probe\n1. build\n",
        "# T\n\n## Phases\n\n1. build\n2. ship\n\nThen, once it lands:\n\n1. a\n2. b\n3. c\n",
        "# T\n\n## 6. Phases\n\n**Definition of done, corrected**\n\n1. a\n2. b\n3. c\n",
        "# T\n\n## Phases\n\n- **Phase 3's check could ossify** x\n- **Phase 2.2 automatic re-stamping narrows** y\n- **Phase 1.1 may queue rather than land**\n",
        "# T\n\n## 5. Phase 0 — measure\n\n## 6. Phase 1 — build\n",
        "# T\n\n| Phase | Status | Deliverable |\n|---|---|---|\n| 1 | SHIPPED | Foundation |\n| **2a — base theory doc** | open | x |\n| 3 | open | y |\n| 0.5 | open | sub-item |\n| 4 – 6 | open | range |\n| 5—6 | open | range |\n| 33 of 52 rustc units | open | timing |\n| 7(a) | open | sub-item |\n| 8 (coord) | open | z |\n\n### Phase 3 — the real name\n",
        "# T\n\n| Phase | Outcome |\n|---|---|\n| 1 | built |\n| 4 | deferred |\n| 3b | x (deferred) |\n\n## Security\n\n| Phase | Touches credentials? |\n|---|---|\n| 3 | no |\n| 4 | yes |\n",
        "# T\n\n| Phase | Outcome |\n|---|---|\n| 4 | deferred |\n\n## Phase 4 — the real heading\n",
        "# T\n\n## Phases\n\n- **Phase 1 — ~~make it~~ ALREADY EXECUTED BY A PEER**\n- **Phase 2 — the remaining work**\n",
        "# T\n\n## Phases\n\n1. build\n2. frontend. **NOT NEEDED**; do not open a web PR\n3. polish (optional, later)\n4. rollout — likely its own plan\n5. migration — MOOT\n",
        "# T\n\n## Phases\n\nIn order:\n\n1. **Phase 1 — schema**\n2. wiring\n",
        "# T\n\n## Phases\n\n> These phases describe Option A only. Under Option B, Phases 1-4 do not exist.\n\n- **Phase 1 — a**\n- **Phase 2 — b**\n",
        "# T\n\n**Phase 4 gap, verified** in review.\n\n## Phase 4 — tests\n",
        "# T\n\n## Phase 1 — a\n\nx\n\n**Phase 6**, near-verbatim. **Resolved 2026-09-26: deliver it here** (see \"Vet\nfindings\").\n\n## Phase 2 — b\n\ny\n\n## Phase 3 — c\n",
        "# T\n\n## Phase 1 — a\n\n**Phase 2**.\n",
        "# T\n\n## Phases\n\n**Phase 4** is deferred to a separate plan.\n\n| Phase | Work |\n|---|---|\n| 1 | schema |\n| 2 | wiring |\n",
        "# T\n\n## Phases\n\nFor each object:\n\nLINE\n\n1. probe\n2. build\n",
        "# T\n\n## Phases\n\n- **Phase 1 — a**\nLINE\n- **Phase 2 — b**\n3. c\n",
        "# T\n\n```x` inline\n\n## Phase 1 — real\n\n```\n",
        "# T\n\n## Phases\n\nDeferred to a follow-up plan.\n\n1. a\n2. b\n",
        "# T\n\n## Phases\n\nThese phases were superseded.\n\n| Phase | Work |\n|---|---|\n| 1 | a |\n\n## Now\n\n| Phase | Work |\n|---|---|\n| 2 | b |\n",
        "# T\n\n## Phases\n\n1. **(Layer 1 — shipping separately, prerequisite)**\n2. build\n3. **(separate)** scope TBD\n4. **Gates already exist.**\n",
        "# T\n\n### A. Phase 1e: extend Phase 1d\n\n## 2. Phase 2 — real\n",
        "# T\n\n### Phase 1e — x\n",
        "# T\n\n## 3. Phase 2b — x\n",
        "# T\n\n## Divergent design for B\n\n| Phase | Work |\n|---|---|\n| 1 | the design not taken |\n\n## Phases\n\n| Phase | Work |\n|---|---|\n| 1 | the real phase |\n\n- **Phase 2 — remove engineering**\n\n## Phase 2 execution record (2026-08-16)\n",
        "# T\n\n## Phase 4 execution record\n\n| Phase | Outcome |\n|---|---|\n| 4 | deferred |\n",
        "# T\n\n## Phase 4 execution record\n\n## Phases\n\n| Phase | Work |\n|---|---|\n| 4 | tests |\n\n## Later\n\n| Phase | Outcome |\n|---|---|\n| 4 | deferred |\n",
        "# T\n\n## 4. Phases\n\n### Phase 0 (implementation session, read-only, ≤ 1 h): re-verify before building\n\nx\n\n### Phase A: presence history\n\n### Phase B: split the census\n\n### Phase C: device-relative freshness\n\n### Phase D: the reader\n\n### Phase E: the detector\n\n### Phase F: close out\n",
        "# T\n\n### Phase C1 — first half\n\n### Phase C2: second half\n\n**Phase R2 — the bold one**\n\n## 5. Phase T3 — enumerated\n\n### Phase 1B — numeric sub-phase\n",
        "# T\n\n### 6.7 Phase D.3 (the other plan's sub-phase)\n",
        "# T\n\n### Phase D. The reader\n",
        "# T\n\n### Phase A/B testing\n",
        "# T\n\n## Phases\n\n1. schema\n  **Phase A is a gate**\n2. wiring\n",
        "# T\n\n## Phase 1 — a\n\n### Phase Two — the second\n\n## Phase II: roman\n\n**Phase AB — two letters**\n\n### Phase Two — again\n\n### Phase Two is a sentence\n\n### Phase ordering\n\n**Phase Three**, cited in prose.\n",
        "# T\n\n### Phase Two —\n",
        "# T\n\n## Phases\n\n- **Phase A — schema**\n- **Phase B — wiring**\n\n| Phase | Work |\n|---|---|\n| A | x |\n| Phase C | y |\n\n> **Phase D — status narration**\n\n```\n### Phase E — example\n### Phase Two — example\n```\n",
        "# T\n\n**Phase A gap, verified** in review.\n\n## Phase B — b\n\n## Phase A — tests\n\n### Phase A execution record\n",
        "# The published-runner parity count comes from a run, not from reports\n\
\n\
> **Status: IN PROGRESS 2026-09-22.** Implementation started by session\n\
> Depends-On: 2026-08-31-published-build-parity-check\n\
>\n\
> **Repo(s):** `qontinui-runner` (harness, workflow, scanner `Area:` parse, sweep),\n\
> `qontinui-claude-config` (`/create-plan` header convention, knowledge-base page,\n\
> capability-doctor cross-link), `qontinui-dev-notes` (plan stamps only). No\n\
> `qontinui-coord` or `qontinui-web` change; no `coord.*` schema change.\n\
> **Area:** `published-parity` (work-unit `metadata.area`, exactly that key — and see\n\
> Phase 3: today this line reaches no work unit).\n\
",
    ];

    /// Every `fixtures/*.md` plan.
    const FIXTURE_PLANS: &[(&str, &str)] = &[
        (
            "plans/2026-06-18-coord-decommission-markdown-plan-ingest.md",
            include_str!("fixtures/2026-06-18-coord-decommission-markdown-plan-ingest.md"),
        ),
        (
            "plans/2026-06-18-coord-generic-work-unit-primitive.md",
            include_str!("fixtures/2026-06-18-coord-generic-work-unit-primitive.md"),
        ),
        (
            "plans/2026-06-18-coord-merge-scheduler-db-test-harness.md",
            include_str!("fixtures/2026-06-18-coord-merge-scheduler-db-test-harness.md"),
        ),
        (
            "plans/2026-06-18-coord-orchestration-generalize-off-plans.md",
            include_str!("fixtures/2026-06-18-coord-orchestration-generalize-off-plans.md"),
        ),
        (
            "plans/2026-06-18-harness-markdown-to-workunit-adapter.md",
            include_str!("fixtures/2026-06-18-harness-markdown-to-workunit-adapter.md"),
        ),
        (
            "plans/2026-09-04-coord-jetstream-durable-consumers-redis-subscriber-migration.md",
            include_str!("fixtures/2026-09-04-coord-jetstream-durable-consumers-redis-subscriber-migration.md"),
        ),
    ];

    /// The declaration a scan writes, derived from the parsed unit's fields
    /// independently of `phase_declaration`, so a key the function drops or
    /// reshapes cannot also vanish from the expectation.
    fn expected_declaration(u: &ParsedWorkUnit) -> Value {
        serde_json::json!({
            "phases": u.phases.iter().map(|p| serde_json::json!({"index": p.index, "name": p.name})).collect::<Vec<_>>(),
            "phase_labels": u.phase_labels.iter().map(|p| serde_json::json!({"label": p.label, "name": p.name})).collect::<Vec<_>>(),
            "phases_undeclared": u.phases_undeclared,
        })
    }

    /// Gate: for every phase-declaring body in the parser tests and the
    /// fixtures, `phase_declaration` is exactly the three keys `build_metadata`
    /// writes (and the field-derived expectation); for every body that declares
    /// nothing it is `None` and `build_metadata` writes none of the three.
    /// Fails if `phase_declaration` drops, adds or reshapes a key, or if
    /// `build_metadata` stops writing what it returns.
    #[test]
    fn phase_declaration_is_the_three_keys_build_metadata_writes() {
        use super::super::parser::{parse_work_unit, slug_from_filename, PlanConvention};
        let conv = PlanConvention::operator_default();
        let mut cases: Vec<(String, &str)> = PARSER_TEST_PHASE_BODIES
            .iter()
            .enumerate()
            .map(|(i, b)| (format!("parser-test-body-{i}"), *b))
            .collect();
        cases.extend(FIXTURE_PLANS.iter().map(|(p, b)| (p.to_string(), *b)));
        let (mut declaring, mut with_labels, mut with_undeclared, mut fixtures_declaring) =
            (0, 0, 0, 0);
        for (name, body) in &cases {
            let u = parse_work_unit(&slug_from_filename(name), name, body, &conv);
            let written = build_metadata(&u);
            let written_keys: Vec<&str> = ["phases", "phase_labels", "phases_undeclared"]
                .into_iter()
                .filter(|k| written.contains_key(*k))
                .collect();
            match phase_declaration(&u) {
                None => {
                    assert!(!declares_any_phase(&u), "{name}: declares but got None");
                    assert!(written_keys.is_empty(), "{name}: {written_keys:?}");
                }
                Some(decl) => {
                    declaring += 1;
                    if name.starts_with("plans/") {
                        fixtures_declaring += 1;
                    }
                    if !u.phase_labels.is_empty() {
                        with_labels += 1;
                    }
                    if !u.phases_undeclared.is_empty() {
                        with_undeclared += 1;
                    }
                    let obj = decl.as_object().expect("the declaration is an object");
                    let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
                    keys.sort_unstable();
                    assert_eq!(
                        keys,
                        vec!["phase_labels", "phases", "phases_undeclared"],
                        "{name}: {decl}"
                    );
                    let from_patch: Map<String, Value> = written_keys
                        .iter()
                        .map(|k| (k.to_string(), written[*k].clone()))
                        .collect();
                    assert_eq!(decl, Value::Object(from_patch), "{name}");
                    assert_eq!(decl, expected_declaration(&u), "{name}");
                }
            }
        }
        // The corpus must keep exercising every key, or the gate goes vacuous.
        assert!(declaring >= 30, "declaring bodies shrank: {declaring}");
        assert!(
            with_labels >= 3,
            "label-declaring bodies shrank: {with_labels}"
        );
        assert!(
            with_undeclared >= 1,
            "undeclared-reporting bodies: {with_undeclared}"
        );
        assert!(fixtures_declaring >= 1, "no fixture declares a phase");
        assert!(
            cases.len() - declaring >= 1,
            "no non-declaring body left to prove the None arm"
        );
    }
}
