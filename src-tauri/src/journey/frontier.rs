//! The journey frontier — affordances seen at a node and not yet the trigger
//! of an observed edge from it (`project.journey_frontier`).
//!
//! An open frontier is what keeps *unexplored* distinguishable from
//! *unreachable* (plan D6). Lifecycle:
//!
//! - on every snapshot resolved into a node, ONE batched upsert
//!   ([`FRONTIER_UPSERT_SQL`]) adds a row per interactive affordance present,
//!   skipping any affordance that is already the trigger of a live edge from
//!   that node;
//! - when an edge from `(node_key, affordance_fingerprint)` is written, the
//!   edge statement itself deletes that row
//!   (`super::capture::EDGE_WRITE_SQL`).
//!
//! Both are fire-and-forget on the journey worker, behind the same
//! `journey_schema_supported` probe as the edge ledger.

use qontinui_types::ir::IrEffect;
use qontinui_types::journey::{FrontierReason, JourneyNode};

use super::node::AffordanceIndex;

/// The canonical `JourneyNode::key()` of an edge's `from_node`, computed in
/// SQL from the stored JSON — so "is this affordance already the trigger of an
/// edge from this node?" compares keys, never jsonb equality (a node's JSON
/// also carries non-identity detail such as `pageLabel`).
///
/// Mirrors `JourneyNode::key` exactly: modelled → `<specId>#<stateIds joined
/// by ','>` (in stored order, which is canonical — sorted and deduped by
/// `JourneyNode::new` and checked by `validate` before every write);
/// unmodelled → `unmodelled:<pathnameTemplate ?? pageLabel ?? 'unknown'>`.
macro_rules! from_node_key_sql {
    () => {
        "CASE WHEN (e.from_node->>'modelled')::boolean \
     THEN (e.from_node->>'specId') || '#' || array_to_string(ARRAY(\
         SELECT s.v FROM jsonb_array_elements_text(e.from_node->'stateIds') \
         WITH ORDINALITY AS s(v, i) ORDER BY s.i), ',') \
     ELSE 'unmodelled:' || COALESCE(e.from_node->>'pathnameTemplate', \
         e.from_node->>'pageLabel', 'unknown') END"
    };
}

/// See [`from_node_key_sql!`]; the expression as a `&str` for tests and reuse.
pub(crate) const FROM_NODE_KEY_SQL: &str = from_node_key_sql!();

/// Batched frontier upsert: one row per element of the four parallel arrays.
///
/// - `NOT EXISTS` skips an affordance already EXPLORED from this node: a live
///   (not invalidated) edge whose outcome is `changed` / `no_change` /
///   `settle_timeout` (M4 — an `error` or `to_node_unobserved` edge explored
///   nothing). The `= ANY($5)` pre-filter narrows the scan to this batch's
///   fingerprints before the node-key expression is computed (m7);
/// - each row's `reason` follows plan D4 ([`d4_reason`]);
/// - `ON CONFLICT` refreshes `last_seen_*`, the role / effect and the D4
///   reason — except that an `activation_failed` reason (set when an edge
///   from here errored) is kept: seeing the affordance again does not undo a
///   failed activation.
///
/// The caller dedups fingerprints first ([`AffordanceIndex::distinct`]): a
/// duplicate key in one statement makes `ON CONFLICT DO UPDATE` fail with
/// "cannot affect row a second time".
pub(crate) const FRONTIER_UPSERT_SQL: &str = concat!(
    "INSERT INTO project.journey_frontier\n",
    "    (app_id, node_key, node, affordance_fingerprint, affordance_role,\n",
    "     declared_effect, reason, last_seen_run_id)\n",
    "SELECT $1::text, $2::text, $3::jsonb, a.fp, a.role, a.effect, a.reason, $4::text\n",
    "  FROM unnest($5::text[], $6::text[], $7::text[], $8::text[])\n",
    "       AS a(fp, role, effect, reason)\n",
    " WHERE NOT EXISTS (\n",
    "     SELECT 1 FROM project.journey_edge_observations e\n",
    "      WHERE e.app_id = $1\n",
    "        AND e.invalidated_at IS NULL\n",
    "        AND e.trigger->>'targetFingerprint' = ANY($5::text[])\n",
    "        AND e.trigger->>'targetFingerprint' = a.fp\n",
    "        AND e.outcome IN ('changed', 'no_change', 'settle_timeout')\n",
    "        AND (",
    from_node_key_sql!(),
    ") = $2)\n",
    "ON CONFLICT (app_id, node_key, affordance_fingerprint) DO UPDATE\n",
    "   SET node = EXCLUDED.node,\n",
    "       affordance_role = EXCLUDED.affordance_role,\n",
    "       declared_effect = EXCLUDED.declared_effect,\n",
    "       reason = CASE WHEN journey_frontier.reason = 'activation_failed'\n",
    "                     THEN journey_frontier.reason ELSE EXCLUDED.reason END,\n",
    "       last_seen_at = now(),\n",
    "       last_seen_run_id = EXCLUDED.last_seen_run_id"
);

/// Number of parameters [`FRONTIER_UPSERT_SQL`] binds.
pub(crate) const FRONTIER_UPSERT_BINDS: usize = 8;

/// A frontier row's reason, by plan D4's closed predicate, in order:
/// declared `write` → `effect_write`; declared `destructive` →
/// `effect_destructive`; declared `read` → `not_yet_activated`; no
/// declaration and an explicit navigation role/type → `not_yet_activated`;
/// anything else → `effect_undeclared`. Never guesses an effect.
pub(crate) fn d4_reason(declared_effect: Option<IrEffect>, navigation: bool) -> FrontierReason {
    match declared_effect {
        Some(IrEffect::Write) => FrontierReason::EffectWrite,
        Some(IrEffect::Destructive) => FrontierReason::EffectDestructive,
        Some(IrEffect::Read) => FrontierReason::NotYetActivated,
        None if navigation => FrontierReason::NotYetActivated,
        None => FrontierReason::EffectUndeclared,
    }
}

pub(crate) fn effect_str(e: IrEffect) -> &'static str {
    match e {
        IrEffect::Read => "read",
        IrEffect::Write => "write",
        IrEffect::Destructive => "destructive",
    }
}

/// The binds of one frontier upsert.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct FrontierBatch {
    pub app_id: String,
    pub node_key: String,
    pub node: serde_json::Value,
    pub run_id: Option<String>,
    pub fingerprints: Vec<String>,
    pub roles: Vec<Option<String>>,
    pub effects: Vec<Option<String>>,
    pub reasons: Vec<String>,
}

/// Build the upsert binds for a node's affordances.
///
/// `Ok(None)` when the node exposes no affordance (nothing to write).
/// `Err` when the node violates the contract — `JourneyNode::validate` runs
/// before every write, and the caller counts a refusal as a write failure.
pub(crate) fn frontier_batch(
    app_id: &str,
    node: &JourneyNode,
    affordances: &AffordanceIndex,
    run_id: Option<String>,
) -> Result<Option<FrontierBatch>, String> {
    node.validate()
        .map_err(|e| format!("contract validation (frontier node): {e}"))?;
    let distinct = affordances.distinct();
    if distinct.is_empty() {
        return Ok(None);
    }
    let node_json = serde_json::to_value(node).map_err(|e| e.to_string())?;
    let mut fingerprints = Vec::with_capacity(distinct.len());
    let mut roles = Vec::with_capacity(distinct.len());
    let mut effects = Vec::with_capacity(distinct.len());
    let mut reasons = Vec::with_capacity(distinct.len());
    for a in distinct {
        reasons.push(
            d4_reason(a.declared_effect, a.navigation)
                .as_str()
                .to_string(),
        );
        fingerprints.push(a.fingerprint);
        roles.push(a.role);
        effects.push(a.declared_effect.map(|e| effect_str(e).to_string()));
    }
    Ok(Some(FrontierBatch {
        app_id: app_id.to_string(),
        node_key: node.key(),
        node: node_json,
        run_id,
        fingerprints,
        roles,
        effects,
        reasons,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journey::capture::EDGE_WRITE_SQL;
    use crate::journey::node::{extract_affordances, Affordance, ElementAffordance};
    use serde_json::json;
    use std::collections::BTreeMap;

    fn node() -> JourneyNode {
        JourneyNode::new(Some("home".into()), ["idle".to_string()], None, None)
    }

    #[test]
    fn frontier_lifecycle_add_then_delete_share_one_key() {
        // ADD: the snapshot's affordance is upserted under (node_key, fp).
        let snap = json!({"elements": [{"id": "b", "type": "button", "label": "Go", "actions": ["click"]}]});
        let aff = extract_affordances(&snap);
        let batch = frontier_batch("app", &node(), &aff, Some("7".into()))
            .unwrap()
            .expect("one affordance to upsert");
        assert_eq!(batch.node_key, "home#idle");
        assert_eq!(batch.fingerprints.len(), 1);
        assert_eq!(batch.run_id.as_deref(), Some("7"));

        // DELETE: an edge fired from that node at that affordance names the
        // SAME (node_key, fingerprint) pair in its frontier DELETE.
        use crate::journey::cursor::{ActionSpec, CursorKey, Cursors, Observed};
        let now = std::time::Instant::now();
        let seen = || Observed {
            node: node(),
            digest: "d".into(),
        };
        let mut cursors = Cursors::default();
        let key = CursorKey {
            app_id: "app".into(),
            runner_instance: "primary".into(),
            scope: None,
        };
        cursors.observe(&key, seen(), aff, now);
        cursors.open(
            &key,
            &ActionSpec::element(
                "b",
                "click",
                qontinui_types::journey::ChokePoint::ElementAction,
            ),
            Default::default(),
            None,
            now,
        );
        let edge = cursors
            .observe(&key, seen(), Default::default(), now)
            .expect("closed");
        let binds =
            crate::journey::capture::edge_insert(&crate::journey::capture::finalize(edge)).unwrap();
        assert_eq!(binds.from_node_key, batch.node_key);
        assert_eq!(
            binds.target_fingerprint.as_ref(),
            batch.fingerprints.first()
        );
        assert!(EDGE_WRITE_SQL.contains("f.node_key = $13"));
        assert!(EDGE_WRITE_SQL.contains("f.affordance_fingerprint = $14"));
    }

    #[test]
    fn a_node_without_affordances_writes_nothing() {
        assert_eq!(
            frontier_batch("app", &node(), &AffordanceIndex::default(), None).unwrap(),
            None
        );
    }

    #[test]
    fn an_invalid_node_is_refused_not_written() {
        let bad = JourneyNode::new(None, Vec::<String>::new(), None, Some("a/b".into()));
        let snap = json!({"elements": [{"id": "b", "actions": ["click"]}]});
        let err = frontier_batch("app", &bad, &extract_affordances(&snap), None).unwrap_err();
        assert!(err.contains("contract validation"), "{err}");
    }

    #[test]
    fn batch_arrays_are_parallel_and_deduped() {
        let mut aff = AffordanceIndex::default();
        for (id, fp, effect) in [
            ("a", "fp-1", Some(IrEffect::Write)),
            ("b", "fp-1", None),
            ("c", "fp-2", None),
        ] {
            aff.elements.insert(
                id.into(),
                ElementAffordance {
                    affordance: Affordance {
                        fingerprint: fp.into(),
                        role: Some("button".into()),
                        declared_effect: effect,
                        navigation: false,
                    },
                    action_effects: BTreeMap::new(),
                },
            );
        }
        let batch = frontier_batch("app", &node(), &aff, None).unwrap().unwrap();
        assert_eq!(batch.fingerprints, vec!["fp-1", "fp-2"]);
        assert_eq!(batch.roles.len(), 2);
        assert_eq!(batch.effects, vec![Some("write".to_string()), None]);
    }

    #[test]
    fn upsert_sql_shape() {
        let max = (1..=20)
            .rev()
            .find(|n| FRONTIER_UPSERT_SQL.contains(&format!("${n}")))
            .unwrap_or(0);
        assert_eq!(max, FRONTIER_UPSERT_BINDS);
        assert!(
            FRONTIER_UPSERT_SQL.contains(FROM_NODE_KEY_SQL),
            "the key expression is shared"
        );
        assert!(
            FRONTIER_UPSERT_SQL.contains("unnest($5::text[], $6::text[], $7::text[], $8::text[])")
        );
        assert!(FRONTIER_UPSERT_SQL.contains("e.invalidated_at IS NULL"));
        assert!(FRONTIER_UPSERT_SQL
            .contains("ON CONFLICT (app_id, node_key, affordance_fingerprint) DO UPDATE"));
    }

    // ---- M4: outcome-aware frontier -------------------------------------

    #[test]
    fn only_explored_outcomes_suppress_or_clear_a_frontier_row() {
        // The skip counts only outcomes that observed something after the
        // activation; error / to_node_unobserved edges explored nothing.
        assert!(
            FRONTIER_UPSERT_SQL.contains("e.outcome IN ('changed', 'no_change', 'settle_timeout')")
        );
        // The edge statement: explored outcomes DELETE, error UPSERTs
        // activation_failed, to_node_unobserved matches neither branch.
        let sql = EDGE_WRITE_SQL;
        assert!(sql.contains("$12 IN ('changed', 'no_change', 'settle_timeout')"));
        assert!(sql.contains("WHERE $12 = 'error'"));
        assert!(sql.contains("SET reason = 'activation_failed'"));
        assert!(
            !sql.contains("to_node_unobserved"),
            "unobserved touches no frontier row"
        );
        // A later observation does not undo a failed activation.
        assert!(FRONTIER_UPSERT_SQL.contains("WHEN journey_frontier.reason = 'activation_failed'"));
    }

    #[test]
    fn the_skip_prefilters_on_the_batch_fingerprints() {
        // m7: the ANY($5) narrowing precedes the per-row key expression.
        let any = FRONTIER_UPSERT_SQL
            .find("= ANY($5::text[])")
            .expect("prefilter present");
        let key_expr = FRONTIER_UPSERT_SQL
            .find(FROM_NODE_KEY_SQL)
            .expect("key expr");
        assert!(any < key_expr);
    }

    // ---- m8: D4 reasons ----------------------------------------------------

    #[test]
    fn frontier_reason_follows_d4() {
        assert_eq!(
            d4_reason(Some(IrEffect::Write), true),
            FrontierReason::EffectWrite
        );
        assert_eq!(
            d4_reason(Some(IrEffect::Destructive), true),
            FrontierReason::EffectDestructive,
            "a declared effect overrides navigation-ness"
        );
        assert_eq!(
            d4_reason(Some(IrEffect::Read), false),
            FrontierReason::NotYetActivated
        );
        assert_eq!(d4_reason(None, true), FrontierReason::NotYetActivated);
        assert_eq!(d4_reason(None, false), FrontierReason::EffectUndeclared);
    }

    #[test]
    fn the_batch_carries_a_d4_reason_per_affordance() {
        let snap = json!({"elements": [
            {"id": "l", "type": "link", "label": "Home", "actions": ["click"]},
            {"id": "b", "type": "button", "label": "Go", "actions": ["click"]}
        ]});
        let batch = frontier_batch("app", &node(), &extract_affordances(&snap), None)
            .unwrap()
            .unwrap();
        let mut reasons = batch.reasons.clone();
        reasons.sort();
        assert_eq!(reasons, vec!["effect_undeclared", "not_yet_activated"]);
        assert_eq!(batch.reasons.len(), batch.fingerprints.len());
    }

    #[test]
    fn node_key_sql_mirrors_journey_node_key() {
        // Both arms of `JourneyNode::key`, by the same field names the node
        // serializes with.
        let modelled = serde_json::to_value(node()).unwrap();
        for field in ["modelled", "specId", "stateIds"] {
            assert!(modelled.get(field).is_some());
            assert!(FROM_NODE_KEY_SQL.contains(&format!("'{field}'")));
        }
        let unmodelled = serde_json::to_value(JourneyNode::new(
            None,
            Vec::<String>::new(),
            Some("/t".into()),
            Some("l".into()),
        ))
        .unwrap();
        for field in ["pathnameTemplate", "pageLabel"] {
            assert!(unmodelled.get(field).is_some());
            assert!(FROM_NODE_KEY_SQL.contains(&format!("'{field}'")));
        }
        assert!(FROM_NODE_KEY_SQL.contains("'unmodelled:'"));
        assert!(FROM_NODE_KEY_SQL.contains("'unknown'"));
        assert!(FROM_NODE_KEY_SQL.contains("'#'"));
        assert!(FROM_NODE_KEY_SQL.contains("WITH ORDINALITY"));
    }
}
