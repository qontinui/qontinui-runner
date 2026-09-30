//! The pending-edge state machine — PURE (no I/O, time passed in).
//!
//! Plan D3 as amended at review: there is ONE node-identity source. Every
//! action — including execute-with-diff, whose SDK `SemanticSnapshot`s the
//! spec evaluator cannot read — opens a PENDING edge from the last node this
//! runner resolved for its cursor, and the next CONTROL / SDK snapshot of that
//! cursor closes it. A diff contributes only an OUTCOME HINT (`error` /
//! `settle_timeout`); the destination is always the closing snapshot's node,
//! so the same page resolves to one key whichever route acted on it.
//!
//! Cursor scope: one pending edge per `(app_id, runner_instance, scope)`,
//! where `scope` is the control surface's `windowLabel` or the SDK relay's
//! `tabId` (`None` = the main window / the relay's primary tab). An action in
//! a pop-out window is closed by a snapshot of that window, not the main one.
//!
//! `no_change` needs more than equal node keys: the two snapshots'
//! interactive-affordance digests must be equal too. Two `unmodelled:unknown`
//! nodes share a key while being any two pages, so without the digest a dead
//! click could not be told from a navigation.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use qontinui_types::journey::{
    ChokePoint, EdgeOutcome, JourneyNode, JourneyTrigger, NavigationTriggerKind,
};

use super::node::{component_action_fingerprint, unknown_node, AffordanceIndex};

/// A pending edge older than this closes as `to_node_unobserved` when the
/// next event for its cursor arrives (or when the retention tick sweeps it):
/// a snapshot two minutes after an action is not evidence of where that
/// action led.
pub(crate) const PENDING_TTL: Duration = Duration::from_secs(120);

/// One pending-edge slot per app, runner instance and scope.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct CursorKey {
    pub app_id: String,
    pub runner_instance: String,
    /// `windowLabel` (control) or `tabId` (SDK); `None` = main window /
    /// primary tab.
    pub scope: Option<String>,
}

impl CursorKey {
    /// A key for this runner instance. A blank scope is the default scope.
    pub(crate) fn new(app_id: impl Into<String>, scope: Option<&str>) -> Self {
        Self {
            app_id: app_id.into(),
            runner_instance: super::capture::runner_instance(),
            scope: scope
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from),
        }
    }
}

/// Who acted, as far as the request says. Nothing here is invented: an absent
/// value is `None` and lands as SQL NULL ("not reported").
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Provenance {
    /// `SdkAppInfo.version` of the app acted on; `None` = not reported (U4).
    pub app_version: Option<String>,
    /// The caller's run: the `task_run_id` query parameter the action routes
    /// already read for `ui_bridge_events` persistence; `None` when absent.
    pub run_id: Option<String>,
}

// ---------------------------------------------------------------------------
// Triggers
// ---------------------------------------------------------------------------

/// What an action targeted, by the identifiers the request itself names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TriggerTarget {
    /// An element, by its registry id (resolved to a fingerprint against the
    /// snapshot the action was taken from).
    Element(String),
    /// A component action.
    Component {
        component_id: String,
        action_id: String,
    },
    /// The request named no resolvable target (a selector, a text query, a
    /// natural-language instruction, a route).
    Unresolved,
}

/// An action as a handler describes it. CLOSED over structure: there is no
/// field for a typed value, and the constructors read only an action NAME and
/// a TARGET id out of a request — never `params`, text or a URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ActionSpec {
    /// Wire `actionType`: the action verb, a component action id, or a
    /// route-kind name (`batch:<n>`, `ai_execute`, `transition:<id>`, …).
    pub action_type: String,
    /// The action whose declared effect the trigger carries (a custom action
    /// id on an element). `None` when no single action applies.
    pub effect_action: Option<String>,
    pub target: TriggerTarget,
    pub choke_point: ChokePoint,
    /// `affordance` for an activated element/component; `push` / `replace` /
    /// `pop` / `initial` for a navigation route.
    pub navigation_trigger: NavigationTriggerKind,
}

impl ActionSpec {
    /// An element action (`/control/element/{id}/action`, the SDK twin).
    pub(crate) fn element(element_id: &str, action: &str, choke_point: ChokePoint) -> Self {
        Self {
            action_type: action.to_string(),
            effect_action: Some(action.to_string()),
            target: TriggerTarget::Element(element_id.to_string()),
            choke_point,
            navigation_trigger: NavigationTriggerKind::Affordance,
        }
    }

    /// An element-kind action whose target is not an element id (a selector,
    /// a text match, the focused element): `targetFingerprint` is null.
    pub(crate) fn untargeted_element(action_type: &str) -> Self {
        Self {
            action_type: action_type.to_string(),
            effect_action: None,
            target: TriggerTarget::Unresolved,
            choke_point: ChokePoint::ElementAction,
            navigation_trigger: NavigationTriggerKind::Affordance,
        }
    }

    /// A batch of element actions is ONE trigger: `actionType` is
    /// `batch:<n>` and the target is the FIRST step's element. `None` for an
    /// empty batch — nothing acted.
    pub(crate) fn batch(steps: &[serde_json::Value]) -> Option<Self> {
        let first = steps.first()?;
        Some(Self::batch_kind(
            &format!("batch:{}", steps.len()),
            first_target(first),
        ))
    }

    /// A composite action recorded as `batch_action` with a route-kind
    /// `actionType` (`ai_execute`, `transition:<id>`, `action_plan:<n>`, …)
    /// and the first target when the request names one.
    pub(crate) fn batch_kind(action_type: &str, first_target_id: Option<String>) -> Self {
        Self {
            action_type: action_type.to_string(),
            effect_action: None,
            target: first_target_id
                .map(TriggerTarget::Element)
                .unwrap_or(TriggerTarget::Unresolved),
            choke_point: ChokePoint::BatchAction,
            navigation_trigger: NavigationTriggerKind::Affordance,
        }
    }

    /// A component action.
    pub(crate) fn component(component_id: &str, action_id: &str) -> Self {
        Self {
            action_type: action_id.to_string(),
            effect_action: None,
            target: TriggerTarget::Component {
                component_id: component_id.to_string(),
                action_id: action_id.to_string(),
            },
            choke_point: ChokePoint::ComponentAction,
            navigation_trigger: NavigationTriggerKind::Affordance,
        }
    }

    /// A navigation route (`navigate`, `navigate-to`, back/forward, tab
    /// switch, reload). The target is never a URL (it can carry user input),
    /// so `targetFingerprint` is null.
    pub(crate) fn navigation(action_type: &str, trigger: NavigationTriggerKind) -> Self {
        Self {
            action_type: action_type.to_string(),
            effect_action: None,
            target: TriggerTarget::Unresolved,
            choke_point: ChokePoint::Navigation,
            navigation_trigger: trigger,
        }
    }

    /// An execute-with-diff request body, in either spelling the routes
    /// accept (`elementAction: {elementId, action}` or the flat
    /// `elementId` + `operation`/`action`). A body carrying only an
    /// `instruction` has no structural target.
    pub(crate) fn with_diff(body: &serde_json::Value) -> Self {
        let envelope = body.get("elementAction");
        let element_id = envelope
            .and_then(|e| e.get("elementId"))
            .or_else(|| body.get("elementId"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty());
        let action = envelope
            .and_then(|e| e.get("action"))
            .or_else(|| body.get("operation"))
            .or_else(|| body.get("action"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty());
        let action_type = match (action, body.get("instruction").is_some()) {
            (Some(a), _) => a.to_string(),
            (None, true) => "instruction".to_string(),
            (None, false) => "unknown".to_string(),
        };
        Self {
            effect_action: action.map(String::from),
            action_type,
            target: element_id
                .map(|id| TriggerTarget::Element(id.to_string()))
                .unwrap_or(TriggerTarget::Unresolved),
            choke_point: ChokePoint::ExecuteWithDiff,
            navigation_trigger: NavigationTriggerKind::Affordance,
        }
    }
}

/// The element id a step names (`elementId` / `element_id`), if any.
pub(crate) fn first_target(step: &serde_json::Value) -> Option<String> {
    step.get("elementId")
        .or_else(|| step.get("element_id"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from)
}

/// Build the wire trigger, resolving the target against the affordances of
/// the snapshot the action was taken FROM.
///
/// An element id absent from that snapshot yields `targetFingerprint: None`
/// ("not resolved") rather than a fingerprint of something else. A component
/// action's fingerprint is derived from its ids, so it resolves without a
/// snapshot; its declared effect still needs one.
pub(crate) fn resolve_trigger(action: &ActionSpec, from: &AffordanceIndex) -> JourneyTrigger {
    let (target_fingerprint, target_role, declared_effect) = match &action.target {
        TriggerTarget::Element(id) => match from.elements.get(id) {
            Some(el) => (
                Some(el.affordance.fingerprint.clone()),
                el.affordance.role.clone(),
                action
                    .effect_action
                    .as_ref()
                    .and_then(|a| el.action_effects.get(a).copied().flatten()),
            ),
            None => (None, None, None),
        },
        TriggerTarget::Component {
            component_id,
            action_id,
        } => (
            Some(component_action_fingerprint(component_id, action_id)),
            None,
            from.component_actions
                .get(&(component_id.clone(), action_id.clone()))
                .and_then(|a| a.declared_effect),
        ),
        TriggerTarget::Unresolved => (None, None, None),
    };
    JourneyTrigger {
        action_type: action.action_type.clone(),
        target_fingerprint,
        target_role,
        declared_effect,
        navigation_trigger: action.navigation_trigger,
        choke_point: action.choke_point,
    }
}

// ---------------------------------------------------------------------------
// Outcome
// ---------------------------------------------------------------------------

/// A snapshot resolved into a node, with the digest of its interactive
/// affordances.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Observed {
    pub node: JourneyNode,
    pub digest: String,
}

/// How an edge closed.
///
/// - no destination → `to_node_unobserved`;
/// - a hint (`error` from a failed action, `settle_timeout` from a diff) wins
///   over the comparison: the destination is data, the outcome is the hint;
/// - `no_change` only when the node keys are equal AND the from-snapshot's
///   affordance digest is known and equal to the destination's;
/// - otherwise `changed` (which may join equal keys).
pub(crate) fn outcome_of(
    from: &JourneyNode,
    from_digest: Option<&str>,
    to: Option<&Observed>,
    hint: Option<EdgeOutcome>,
) -> EdgeOutcome {
    match (to, hint) {
        (None, _) => EdgeOutcome::ToNodeUnobserved,
        (Some(_), Some(EdgeOutcome::Error)) => EdgeOutcome::Error,
        (Some(_), Some(EdgeOutcome::SettleTimeout)) => EdgeOutcome::SettleTimeout,
        (Some(to), _) if to.node.key() == from.key() && from_digest == Some(to.digest.as_str()) => {
            EdgeOutcome::NoChange
        }
        (Some(_), _) => EdgeOutcome::Changed,
    }
}

/// The outcome hint of an action: `error` when it failed.
pub(crate) fn failure_hint(failed: bool) -> Option<EdgeOutcome> {
    failed.then_some(EdgeOutcome::Error)
}

// ---------------------------------------------------------------------------
// State machine
// ---------------------------------------------------------------------------

/// An edge ready to be finalized into a row.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct EdgeDraft {
    pub key: CursorKey,
    pub provenance: Provenance,
    pub from_node: JourneyNode,
    pub to_node: Option<JourneyNode>,
    pub trigger: JourneyTrigger,
    pub outcome: EdgeOutcome,
}

#[derive(Debug, Clone, PartialEq)]
struct PendingEdge {
    from_node: JourneyNode,
    from_digest: Option<String>,
    trigger: JourneyTrigger,
    hint: Option<EdgeOutcome>,
    provenance: Provenance,
    opened_at: Instant,
}

impl PendingEdge {
    fn is_stale(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.opened_at) >= PENDING_TTL
    }

    fn close(self, key: &CursorKey, to: Option<&Observed>) -> EdgeDraft {
        let outcome = outcome_of(&self.from_node, self.from_digest.as_deref(), to, self.hint);
        EdgeDraft {
            key: key.clone(),
            provenance: self.provenance,
            from_node: self.from_node,
            to_node: to.map(|o| o.node.clone()),
            trigger: self.trigger,
            outcome,
        }
    }
}

#[derive(Debug, Default)]
struct Cursor {
    last: Option<Observed>,
    last_affordances: AffordanceIndex,
    pending: Option<PendingEdge>,
}

/// Per-cursor journey state: the last node resolved, its affordances, and at
/// most ONE pending edge.
#[derive(Debug, Default)]
pub(crate) struct Cursors {
    map: HashMap<CursorKey, Cursor>,
}

impl Cursors {
    /// An action. Opens a pending edge from the last node this runner
    /// resolved for the cursor (an unmodelled unknown node, with no digest,
    /// when none is known yet — the edge is still recorded, never skipped).
    ///
    /// Returns the edge this action DISPLACED: a still-pending previous action
    /// closes with an unobserved destination.
    pub(crate) fn open(
        &mut self,
        key: &CursorKey,
        action: &ActionSpec,
        provenance: Provenance,
        hint: Option<EdgeOutcome>,
        now: Instant,
    ) -> Option<EdgeDraft> {
        let cursor = self.map.entry(key.clone()).or_default();
        let displaced = cursor.pending.take().map(|p| p.close(key, None));
        cursor.pending = Some(PendingEdge {
            from_node: cursor
                .last
                .as_ref()
                .map(|o| o.node.clone())
                .unwrap_or_else(unknown_node),
            from_digest: cursor.last.as_ref().map(|o| o.digest.clone()),
            trigger: resolve_trigger(action, &cursor.last_affordances),
            hint,
            provenance,
            opened_at: now,
        });
        displaced
    }

    /// A snapshot resolved into `observed`. Closes the pending edge (if any)
    /// with it as the destination — or, when the pending edge is older than
    /// [`PENDING_TTL`], as unobserved — and makes it the from-node of whatever
    /// comes next.
    pub(crate) fn observe(
        &mut self,
        key: &CursorKey,
        observed: Observed,
        affordances: AffordanceIndex,
        now: Instant,
    ) -> Option<EdgeDraft> {
        let cursor = self.map.entry(key.clone()).or_default();
        let closed = cursor.pending.take().map(|p| {
            if p.is_stale(now) {
                p.close(key, None)
            } else {
                p.close(key, Some(&observed))
            }
        });
        cursor.last = Some(observed);
        cursor.last_affordances = affordances;
        closed
    }

    /// Close every pending edge older than [`PENDING_TTL`] as unobserved.
    pub(crate) fn sweep(&mut self, now: Instant) -> Vec<EdgeDraft> {
        let mut drafts = Vec::new();
        for (key, cursor) in &mut self.map {
            if cursor.pending.as_ref().is_some_and(|p| p.is_stale(now)) {
                if let Some(p) = cursor.pending.take() {
                    drafts.push(p.close(key, None));
                }
            }
        }
        drafts
    }

    /// Pending edges currently open, across every cursor.
    pub(crate) fn pending_count(&self) -> usize {
        self.map.values().filter(|c| c.pending.is_some()).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journey::node::{Affordance, ElementAffordance};
    use qontinui_types::ir::IrEffect;
    use serde_json::json;
    use std::collections::BTreeMap;

    pub(crate) fn key() -> CursorKey {
        CursorKey {
            app_id: "qontinui-web".into(),
            runner_instance: "primary".into(),
            scope: None,
        }
    }

    pub(crate) fn node(spec: &str, states: &[&str]) -> JourneyNode {
        JourneyNode::new(
            Some(spec.to_string()),
            states.iter().map(|s| s.to_string()),
            None,
            None,
        )
    }

    fn seen(spec: &str, states: &[&str], digest: &str) -> Observed {
        Observed {
            node: node(spec, states),
            digest: digest.to_string(),
        }
    }

    fn click(id: &str) -> ActionSpec {
        ActionSpec::element(id, "click", ChokePoint::ElementAction)
    }

    fn affordances_with(id: &str, fp: &str) -> AffordanceIndex {
        let mut idx = AffordanceIndex::default();
        let mut action_effects = BTreeMap::new();
        action_effects.insert("wipe".to_string(), Some(IrEffect::Destructive));
        idx.elements.insert(
            id.to_string(),
            ElementAffordance {
                affordance: Affordance {
                    fingerprint: fp.to_string(),
                    role: Some("button".into()),
                    declared_effect: Some(IrEffect::Destructive),
                    navigation: false,
                },
                action_effects,
            },
        );
        idx
    }

    fn t0() -> Instant {
        Instant::now()
    }

    #[test]
    fn an_action_then_a_snapshot_closes_one_changed_edge() {
        let now = t0();
        let mut c = Cursors::default();
        assert!(c
            .observe(
                &key(),
                seen("home", &["idle"], "d1"),
                affordances_with("b", "fp-b"),
                now
            )
            .is_none());
        assert!(c
            .open(&key(), &click("b"), Provenance::default(), None, now)
            .is_none());
        assert_eq!(c.pending_count(), 1);
        let edge = c
            .observe(
                &key(),
                seen("detail", &["open"], "d2"),
                AffordanceIndex::default(),
                now,
            )
            .expect("the snapshot closes the pending edge");
        assert_eq!(c.pending_count(), 0);
        assert_eq!(edge.outcome, EdgeOutcome::Changed);
        assert_eq!(edge.from_node.key(), "home#idle");
        assert_eq!(
            edge.to_node.as_ref().map(|n| n.key()).as_deref(),
            Some("detail#open")
        );
        assert_eq!(edge.trigger.target_fingerprint.as_deref(), Some("fp-b"));
        assert_eq!(edge.trigger.target_role.as_deref(), Some("button"));
        assert_eq!(edge.trigger.choke_point, ChokePoint::ElementAction);
    }

    // ---- M6: no_change needs the digest ---------------------------------

    #[test]
    fn equal_keys_and_equal_digests_are_no_change() {
        let now = t0();
        let mut c = Cursors::default();
        c.observe(
            &key(),
            seen("home", &["idle"], "d1"),
            AffordanceIndex::default(),
            now,
        );
        c.open(&key(), &click("b"), Provenance::default(), None, now);
        let edge = c
            .observe(
                &key(),
                seen("home", &["idle"], "d1"),
                AffordanceIndex::default(),
                now,
            )
            .unwrap();
        assert_eq!(edge.outcome, EdgeOutcome::NoChange);
    }

    #[test]
    fn equal_keys_with_different_digests_are_changed() {
        let now = t0();
        let mut c = Cursors::default();
        c.observe(
            &key(),
            seen("home", &["idle"], "d1"),
            AffordanceIndex::default(),
            now,
        );
        c.open(&key(), &click("b"), Provenance::default(), None, now);
        let edge = c
            .observe(
                &key(),
                seen("home", &["idle"], "d2"),
                AffordanceIndex::default(),
                now,
            )
            .unwrap();
        assert_eq!(edge.outcome, EdgeOutcome::Changed);
    }

    #[test]
    fn unknown_to_unknown_is_never_no_change_without_a_digest() {
        let now = t0();
        let mut c = Cursors::default();
        // No snapshot before the action: the from-node is unknown with no digest.
        c.open(&key(), &click("b"), Provenance::default(), None, now);
        let to = Observed {
            node: unknown_node(),
            digest: "d".into(),
        };
        let edge = c
            .observe(&key(), to, AffordanceIndex::default(), now)
            .unwrap();
        assert_eq!(edge.from_node.key(), "unmodelled:unknown");
        assert_eq!(edge.to_node.as_ref().unwrap().key(), "unmodelled:unknown");
        assert_eq!(edge.outcome, EdgeOutcome::Changed);
    }

    #[test]
    fn a_second_action_overwrites_the_first_as_unobserved() {
        let now = t0();
        let mut c = Cursors::default();
        c.observe(
            &key(),
            seen("home", &["idle"], "d"),
            AffordanceIndex::default(),
            now,
        );
        assert!(c
            .open(&key(), &click("a"), Provenance::default(), None, now)
            .is_none());
        let displaced = c
            .open(&key(), &click("b"), Provenance::default(), None, now)
            .expect("the first pending edge is displaced");
        assert_eq!(displaced.outcome, EdgeOutcome::ToNodeUnobserved);
        assert!(displaced.to_node.is_none());
        assert_eq!(c.pending_count(), 1);
        let second = c
            .observe(
                &key(),
                seen("x", &["y"], "e"),
                AffordanceIndex::default(),
                now,
            )
            .unwrap();
        assert_eq!(second.from_node.key(), "home#idle");
    }

    #[test]
    fn a_failed_action_closes_as_error() {
        let now = t0();
        let mut c = Cursors::default();
        c.observe(
            &key(),
            seen("home", &["idle"], "d"),
            AffordanceIndex::default(),
            now,
        );
        c.open(
            &key(),
            &click("b"),
            Provenance::default(),
            failure_hint(true),
            now,
        );
        let edge = c
            .observe(
                &key(),
                seen("home", &["idle"], "d"),
                AffordanceIndex::default(),
                now,
            )
            .unwrap();
        assert_eq!(edge.outcome, EdgeOutcome::Error);
    }

    // ---- M5: diffs are pending edges with an outcome hint -----------------

    #[test]
    fn a_diff_is_a_pending_edge_closed_by_the_next_snapshot() {
        let now = t0();
        let mut c = Cursors::default();
        c.observe(
            &key(),
            seen("home", &["idle"], "d"),
            affordances_with("b", "fp-b"),
            now,
        );
        let action = ActionSpec::with_diff(&json!({"elementId": "b", "operation": "wipe"}));
        assert!(c
            .open(
                &key(),
                &action,
                Provenance::default(),
                Some(EdgeOutcome::SettleTimeout),
                now
            )
            .is_none());
        let edge = c
            .observe(
                &key(),
                seen("home", &["empty"], "e"),
                AffordanceIndex::default(),
                now,
            )
            .unwrap();
        assert_eq!(edge.trigger.choke_point, ChokePoint::ExecuteWithDiff);
        assert_eq!(edge.trigger.declared_effect, Some(IrEffect::Destructive));
        assert_eq!(
            edge.outcome,
            EdgeOutcome::SettleTimeout,
            "the hint overrides"
        );
        assert_eq!(
            edge.to_node.unwrap().key(),
            "home#empty",
            "the snapshot decides the node"
        );
    }

    #[test]
    fn outcome_precedence() {
        let a = node("a", &["1"]);
        let same = Observed {
            node: a.clone(),
            digest: "d".into(),
        };
        let other = Observed {
            node: node("b", &["1"]),
            digest: "d".into(),
        };
        assert_eq!(
            outcome_of(&a, Some("d"), None, Some(EdgeOutcome::Error)),
            EdgeOutcome::ToNodeUnobserved
        );
        assert_eq!(
            outcome_of(&a, Some("d"), Some(&same), Some(EdgeOutcome::Error)),
            EdgeOutcome::Error
        );
        assert_eq!(
            outcome_of(&a, Some("d"), Some(&same), Some(EdgeOutcome::SettleTimeout)),
            EdgeOutcome::SettleTimeout
        );
        assert_eq!(
            outcome_of(&a, Some("d"), Some(&same), None),
            EdgeOutcome::NoChange
        );
        assert_eq!(
            outcome_of(&a, None, Some(&same), None),
            EdgeOutcome::Changed
        );
        assert_eq!(
            outcome_of(&a, Some("d"), Some(&other), None),
            EdgeOutcome::Changed
        );
    }

    // ---- m1: scopes -------------------------------------------------------

    #[test]
    fn scopes_have_independent_pending_edges() {
        let now = t0();
        let mut c = Cursors::default();
        let main = key();
        let popout = CursorKey {
            scope: Some("term-1".into()),
            ..key()
        };
        c.observe(
            &main,
            seen("main", &["m"], "d"),
            AffordanceIndex::default(),
            now,
        );
        c.open(&popout, &click("x"), Provenance::default(), None, now);
        assert!(
            c.observe(
                &main,
                seen("main", &["m"], "d"),
                AffordanceIndex::default(),
                now
            )
            .is_none(),
            "a main-window snapshot must not close a pop-out window's action"
        );
        let edge = c
            .observe(
                &popout,
                seen("term", &["t"], "d"),
                AffordanceIndex::default(),
                now,
            )
            .expect("the pop-out's own snapshot closes it");
        assert_eq!(edge.key.scope.as_deref(), Some("term-1"));
        assert_eq!(edge.from_node.key(), "unmodelled:unknown");
    }

    #[test]
    fn a_blank_scope_is_the_default_scope() {
        assert_eq!(CursorKey::new("a", Some("  ")).scope, None);
        assert_eq!(CursorKey::new("a", Some("t1")).scope.as_deref(), Some("t1"));
    }

    // ---- m2: stale pending edges -----------------------------------------

    #[test]
    fn a_stale_pending_edge_closes_as_unobserved_on_the_next_snapshot() {
        let now = t0();
        let mut c = Cursors::default();
        c.observe(
            &key(),
            seen("a", &["1"], "d"),
            AffordanceIndex::default(),
            now,
        );
        c.open(&key(), &click("b"), Provenance::default(), None, now);
        let later = now + PENDING_TTL + Duration::from_secs(1);
        let edge = c
            .observe(
                &key(),
                seen("b", &["2"], "e"),
                AffordanceIndex::default(),
                later,
            )
            .unwrap();
        assert_eq!(edge.outcome, EdgeOutcome::ToNodeUnobserved);
        assert!(edge.to_node.is_none());
    }

    #[test]
    fn the_sweep_closes_only_stale_pending_edges() {
        let now = t0();
        let mut c = Cursors::default();
        let fresh = CursorKey {
            scope: Some("fresh".into()),
            ..key()
        };
        c.open(&key(), &click("a"), Provenance::default(), None, now);
        let later = now + PENDING_TTL;
        c.open(&fresh, &click("b"), Provenance::default(), None, later);
        let swept = c.sweep(later);
        assert_eq!(swept.len(), 1);
        assert_eq!(swept[0].key, key());
        assert_eq!(swept[0].outcome, EdgeOutcome::ToNodeUnobserved);
        assert_eq!(c.pending_count(), 1, "the fresh edge stays pending");
    }

    // ---- triggers ----------------------------------------------------------

    #[test]
    fn a_batch_is_one_trigger_on_its_first_target() {
        let steps = vec![
            json!({"elementId": "first", "action": "type", "params": {"text": "x"}}),
            json!({"elementId": "second", "action": "click"}),
        ];
        let spec = ActionSpec::batch(&steps).unwrap();
        assert_eq!(spec.action_type, "batch:2");
        assert_eq!(spec.target, TriggerTarget::Element("first".into()));
        assert_eq!(spec.choke_point, ChokePoint::BatchAction);
        assert!(
            ActionSpec::batch(&[]).is_none(),
            "an empty batch acted on nothing"
        );
    }

    #[test]
    fn a_navigation_carries_its_trigger_and_no_target() {
        let t = resolve_trigger(
            &ActionSpec::navigation("navigate", NavigationTriggerKind::Replace),
            &AffordanceIndex::default(),
        );
        assert_eq!(t.choke_point, ChokePoint::Navigation);
        assert_eq!(t.navigation_trigger, NavigationTriggerKind::Replace);
        assert_eq!(t.target_fingerprint, None);
    }

    #[test]
    fn a_component_trigger_fingerprint_matches_the_frontier_key() {
        let t = resolve_trigger(
            &ActionSpec::component("grid", "purge"),
            &AffordanceIndex::default(),
        );
        assert_eq!(
            t.target_fingerprint.as_deref(),
            Some("component:grid:purge")
        );
        assert_eq!(t.choke_point, ChokePoint::ComponentAction);
    }

    #[test]
    fn with_diff_reads_both_spellings() {
        let a =
            ActionSpec::with_diff(&json!({"elementAction": {"elementId": "e", "action": "click"}}));
        assert_eq!(a.target, TriggerTarget::Element("e".into()));
        assert_eq!(a.action_type, "click");
        let b = ActionSpec::with_diff(&json!({"instruction": "open the settings"}));
        assert_eq!(b.target, TriggerTarget::Unresolved);
        assert_eq!(b.action_type, "instruction");
    }
}
