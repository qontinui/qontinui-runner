//! Node resolution and affordance extraction — the PURE half of the journey
//! producer.
//!
//! Everything here is a function of a snapshot (plus, for the modelled case,
//! a spec-check result the caller already computed), so node identity is a
//! deterministic predicate rather than a judgment (plan D1). The async half —
//! looking the spec up and evaluating it — lives in [`super::capture`] and only
//! ever runs on the journey worker task, never on a request path (Phase 0
//! decision 1).
//!
//! ## Privacy
//!
//! Nothing a row can carry is read from user input:
//! - `pageLabel` comes from [`resolve_declared_page_label`] (tabId → activeTab →
//!   slugged `pageContext.name`), or on a `SemanticSnapshot` from the slugged
//!   `page.pageName` — never from `page.pathname`, raw or slugged;
//! - `pathnameTemplate` is only ever the framework's route PATTERN
//!   (`page.route.pattern` / `page.routePattern`), never derived;
//! - an affordance is a structural fingerprint (a hash), an ARIA role and a
//!   declared effect — never an element's label, text or value.
//!
//! `spec_id` IS looked up through the full [`resolve_page_label`] (pathname arm
//! included) because that is how a real-URL app's page is matched to its spec,
//! but it is only STORED when a spec with that id exists — so a stored
//! `spec_id` always names an app-authored spec document, never a slugged path.

use std::collections::{BTreeMap, HashMap};

use qontinui_types::ir::IrEffect;
use qontinui_types::journey::JourneyNode;
use qontinui_types::spec_check::{ClassificationStatus, SpecCheckResult};

use crate::spec_api::slug::pathname_to_spec_id;
use crate::state_discovery::capture::{resolve_declared_page_label, resolve_page_label};
use crate::state_discovery::fingerprint::{extract_role, stable_element_fingerprint};

/// The classification a state must carry to count as PRESENT in a node.
///
/// `Green` is the evaluator's matched class: its match rate is at or above the
/// app's yellow threshold (`ThresholdConfig::classify_match_rate`). `Yellow` is
/// the ambiguous band the helper spot-check exists to resolve, and `Red` is a
/// failed match — counting either would put states into a node that the
/// evaluator itself does not call matched.
pub(crate) const PRESENT_CLASSIFICATION: ClassificationStatus = ClassificationStatus::Green;

/// The page identity a snapshot declares, split into what each consumer may
/// use. See the module doc for why `spec_lookup_label` and `page_label` differ.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PageIdentity {
    /// The id a page spec would be filed under — may be a slugged pathname.
    /// Used ONLY to look a spec up; never stored unless the spec exists.
    pub spec_lookup_label: Option<String>,
    /// The app-declared page label that may be stored in a node.
    pub page_label: Option<String>,
    /// The framework route pattern, when the snapshot carries one.
    pub pathname_template: Option<String>,
}

fn non_blank(v: Option<&serde_json::Value>) -> Option<&str> {
    v.and_then(|v| v.as_str()).filter(|s| !s.trim().is_empty())
}

/// Read a snapshot's page identity.
///
/// Two snapshot shapes reach the producer: the UI Bridge CONTROL snapshot
/// (`/control/snapshot` — `page.pageContext`, `page.route.pattern`,
/// `activeTab`) and the SDK's `SemanticSnapshot` (the `beforeSnapshot` /
/// `afterSnapshot` of an execute-with-diff — `page.pageName`,
/// `page.routePattern`). Both spellings of the same two facts are read;
/// neither shape carries the other's.
pub(crate) fn page_identity(snapshot: &serde_json::Value) -> PageIdentity {
    let page = snapshot.get("page");
    let semantic_page_name =
        non_blank(page.and_then(|p| p.get("pageName"))).map(pathname_to_spec_id);

    let page_label = resolve_declared_page_label(snapshot).or(semantic_page_name);
    let spec_lookup_label = page_label.clone().or_else(|| resolve_page_label(snapshot));

    let pathname_template = non_blank(
        page.and_then(|p| p.get("route"))
            .and_then(|r| r.get("pattern")),
    )
    .or_else(|| non_blank(page.and_then(|p| p.get("routePattern"))))
    .map(|s| s.trim().to_string());

    PageIdentity {
        spec_lookup_label,
        page_label,
        pathname_template,
    }
}

/// What the spec lookup found for a snapshot's page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SpecLookup {
    /// No spec exists for this page (or the app is not registered), or the
    /// lookup itself failed — in which case the failure was logged and the
    /// node is recorded without a spec rather than dropped.
    NoSpec,
    /// A spec exists and was evaluated against the snapshot; these are the
    /// ids of its states classified [`PRESENT_CLASSIFICATION`].
    Evaluated {
        spec_id: String,
        present_state_ids: Vec<String>,
    },
    /// A spec exists but could not be evaluated against this snapshot (e.g.
    /// the snapshot is a `SemanticSnapshot`, which the evaluator cannot read).
    NotEvaluable { spec_id: String },
}

/// The states of an evaluation that count as present in the node.
pub(crate) fn present_state_ids(result: &SpecCheckResult) -> Vec<String> {
    present_among(&result.state_results)
}

fn present_among(states: &[qontinui_types::spec_check::StateMatchResult]) -> Vec<String> {
    states
        .iter()
        .filter(|s| s.classification == PRESENT_CLASSIFICATION)
        .map(|s| s.state_id.clone())
        .collect()
}

/// Build the node for a snapshot from its identity and the spec lookup.
///
/// - `Evaluated` with at least one present state → MODELLED node
///   `(spec_id, those state ids)`;
/// - `Evaluated` with none present, or `NotEvaluable` → UNMODELLED node that
///   still carries the `spec_id` (a spec exists for the page; no state of it
///   was matched);
/// - `NoSpec` → UNMODELLED node with no `spec_id`.
///
/// An unmodelled node's key is `unmodelled:<template ?? label ?? unknown>`.
pub(crate) fn build_node(identity: &PageIdentity, lookup: &SpecLookup) -> JourneyNode {
    let (spec_id, state_ids): (Option<String>, Vec<String>) = match lookup {
        SpecLookup::NoSpec => (None, Vec::new()),
        SpecLookup::NotEvaluable { spec_id } => (Some(spec_id.clone()), Vec::new()),
        SpecLookup::Evaluated {
            spec_id,
            present_state_ids,
        } => (Some(spec_id.clone()), present_state_ids.clone()),
    };
    JourneyNode::new(
        spec_id,
        state_ids,
        identity.pathname_template.clone(),
        identity.page_label.clone(),
    )
}

/// The node used as `from_node` when an action arrives before this runner
/// has resolved ANY node for the app: unmodelled, nothing known.
pub(crate) fn unknown_node() -> JourneyNode {
    JourneyNode::new(None, Vec::<String>::new(), None, None)
}

// ---------------------------------------------------------------------------
// Affordances
// ---------------------------------------------------------------------------

/// Severity order of a declared effect: `read < write < destructive`.
fn severity(effect: IrEffect) -> u8 {
    match effect {
        IrEffect::Read => 0,
        IrEffect::Write => 1,
        IrEffect::Destructive => 2,
    }
}

fn parse_effect(v: Option<&serde_json::Value>) -> Option<IrEffect> {
    v.and_then(|v| serde_json::from_value::<IrEffect>(v.clone()).ok())
}

/// One interactive affordance seen in a snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Affordance {
    /// Structural fingerprint — [`stable_element_fingerprint`] for an element,
    /// [`component_action_fingerprint`] for a component action.
    pub fingerprint: String,
    pub role: Option<String>,
    /// The affordance's declared effect, fail-closed (see
    /// [`element_declared_effect`]).
    pub declared_effect: Option<IrEffect>,
    /// Plan D4 rule 3's explicit navigation arm: `role` ∈ {link, menuitem,
    /// tab} or `type` ∈ {link, menuitem}. Nothing else counts (not
    /// `semanticType`, not a parent context — see D4 "Resolved").
    pub navigation: bool,
}

/// D4 rule 3: an explicit navigation role or type.
pub(crate) fn is_navigation_affordance(role: Option<&str>, element_type: Option<&str>) -> bool {
    let norm = |s: &str| s.trim().to_ascii_lowercase();
    role.map(norm)
        .is_some_and(|r| matches!(r.as_str(), "link" | "menuitem" | "tab"))
        || element_type
            .map(norm)
            .is_some_and(|t| matches!(t.as_str(), "link" | "menuitem"))
}

/// Digest of a snapshot's interactive ELEMENT affordances: the sorted,
/// deduped set of their fingerprints, hashed. Two snapshots with equal node
/// keys but different digests exposed different affordances, so an edge
/// between them is `changed`, never `no_change`.
pub(crate) fn affordance_digest(index: &AffordanceIndex) -> String {
    use sha2::{Digest, Sha256};
    let fingerprints: std::collections::BTreeSet<&str> = index
        .elements
        .values()
        .map(|e| e.affordance.fingerprint.as_str())
        .collect();
    let mut hasher = Sha256::new();
    for fp in fingerprints {
        hasher.update(fp.as_bytes());
        hasher.update(b"\n");
    }
    hasher
        .finalize()
        .iter()
        .take(16)
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// An element's affordance plus its per-action declared effects, kept so a
/// trigger can name the effect of the ONE action it fired.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ElementAffordance {
    pub affordance: Affordance,
    /// `customActions[].id` → its declared effect (absent = undeclared).
    pub action_effects: BTreeMap<String, Option<IrEffect>>,
}

/// Every affordance a snapshot exposes, indexed the way triggers name them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct AffordanceIndex {
    /// Element id → affordance.
    pub elements: HashMap<String, ElementAffordance>,
    /// `(component id, action id)` → affordance.
    pub component_actions: HashMap<(String, String), Affordance>,
}

impl AffordanceIndex {
    /// Distinct affordances by fingerprint, deterministic order. Two elements
    /// that fingerprint identically are ONE affordance (the frontier key is
    /// the fingerprint), so a duplicate never reaches the batched upsert,
    /// where it would make `ON CONFLICT DO UPDATE` touch a row twice.
    pub fn distinct(&self) -> Vec<Affordance> {
        let mut by_fp: BTreeMap<String, Affordance> = BTreeMap::new();
        let all = self
            .elements
            .values()
            .map(|e| &e.affordance)
            .chain(self.component_actions.values());
        for a in all {
            by_fp
                .entry(a.fingerprint.clone())
                .and_modify(|existing| {
                    // Keep the MOST severe declaration and the first role seen:
                    // the merge must not launder a destructive affordance into
                    // a read one.
                    existing.declared_effect =
                        merge_effect(existing.declared_effect, a.declared_effect);
                    if existing.role.is_none() {
                        existing.role = a.role.clone();
                    }
                })
                .or_insert_with(|| a.clone());
        }
        by_fp.into_values().collect()
    }
}

fn merge_effect(a: Option<IrEffect>, b: Option<IrEffect>) -> Option<IrEffect> {
    match (a, b) {
        (Some(x), Some(y)) => Some(if severity(x) >= severity(y) { x } else { y }),
        (Some(x), None) | (None, Some(x)) => {
            // One side undeclared: a known write/destructive still dominates,
            // but a lone `read` cannot vouch for an undeclared sibling.
            (severity(x) > 0).then_some(x)
        }
        (None, None) => None,
    }
}

/// The fingerprint of a component action: `component:<componentId>:<actionId>`.
/// Shared by the frontier (what was seen) and the component-action trigger
/// (what was fired), which is what lets an edge clear its frontier row.
pub(crate) fn component_action_fingerprint(component_id: &str, action_id: &str) -> String {
    format!("component:{component_id}:{action_id}")
}

/// An element's declared effect, FAIL-CLOSED.
///
/// Standard actions (`actions: ["click", …]`) carry no declaration; custom
/// actions (`customActions: [{id, effect}]`) may. The element's effect is the
/// most severe DECLARED effect among its custom actions, except that a `read`
/// is only trusted when every action the element exposes is a custom action
/// declared `read` — otherwise an undeclared `click` would inherit "safe" from
/// a neighbour. A declared `write` / `destructive` always stands: knowing that
/// one action is dangerous is enough to never treat the element as safe.
pub(crate) fn element_declared_effect(element: &serde_json::Value) -> Option<IrEffect> {
    let standard = element
        .get("actions")
        .and_then(|a| a.as_array())
        .map(|a| !a.is_empty())
        .unwrap_or(false);
    let custom: Vec<Option<IrEffect>> = element
        .get("customActions")
        .and_then(|a| a.as_array())
        .map(|arr| arr.iter().map(|a| parse_effect(a.get("effect"))).collect())
        .unwrap_or_default();

    let max = custom
        .iter()
        .flatten()
        .copied()
        .max_by_key(|e| severity(*e));
    match max {
        Some(e) if severity(e) > 0 => Some(e),
        Some(e) if !standard && custom.iter().all(Option::is_some) => Some(e),
        _ => None,
    }
}

fn element_is_interactive(element: &serde_json::Value) -> bool {
    let non_empty = |key: &str| {
        element
            .get(key)
            .and_then(|a| a.as_array())
            .is_some_and(|a| !a.is_empty())
    };
    non_empty("actions") || non_empty("customActions")
}

/// Index every interactive affordance of a snapshot: each element with at
/// least one action (standard or custom), and each component action.
pub(crate) fn extract_affordances(snapshot: &serde_json::Value) -> AffordanceIndex {
    let mut index = AffordanceIndex::default();

    for element in snapshot
        .get("elements")
        .and_then(|e| e.as_array())
        .into_iter()
        .flatten()
    {
        let Some(id) = element.get("id").and_then(|v| v.as_str()) else {
            continue;
        };
        if !element_is_interactive(element) {
            continue;
        }
        let action_effects = element
            .get("customActions")
            .and_then(|a| a.as_array())
            .into_iter()
            .flatten()
            .filter_map(|a| {
                let id = a.get("id").and_then(|v| v.as_str())?;
                Some((id.to_string(), parse_effect(a.get("effect"))))
            })
            .collect();
        index.elements.insert(
            id.to_string(),
            ElementAffordance {
                affordance: Affordance {
                    fingerprint: stable_element_fingerprint(element),
                    navigation: is_navigation_affordance(
                        extract_role(element).as_deref(),
                        element
                            .get("type")
                            .or_else(|| element.get("tagName"))
                            .and_then(|v| v.as_str()),
                    ),
                    role: extract_role(element),
                    declared_effect: element_declared_effect(element),
                },
                action_effects,
            },
        );
    }

    for component in snapshot
        .get("components")
        .and_then(|c| c.as_array())
        .into_iter()
        .flatten()
    {
        let Some(component_id) = component.get("id").and_then(|v| v.as_str()) else {
            continue;
        };
        for action in component
            .get("actions")
            .and_then(|a| a.as_array())
            .into_iter()
            .flatten()
        {
            let Some(action_id) = action.get("id").and_then(|v| v.as_str()) else {
                continue;
            };
            index.component_actions.insert(
                (component_id.to_string(), action_id.to_string()),
                Affordance {
                    fingerprint: component_action_fingerprint(component_id, action_id),
                    role: None,
                    declared_effect: parse_effect(action.get("effect")),
                    navigation: false,
                },
            );
        }
    }

    index
}

#[cfg(test)]
mod tests {
    use super::*;
    use qontinui_types::spec_check::StateMatchResult;
    use serde_json::json;

    fn state(id: &str, classification: ClassificationStatus) -> StateMatchResult {
        StateMatchResult {
            state_id: id.to_string(),
            state_name: id.to_string(),
            match_rate: 1.0,
            classification,
            assertions: Vec::new(),
        }
    }

    fn identity(label: Option<&str>, template: Option<&str>) -> PageIdentity {
        PageIdentity {
            spec_lookup_label: label.map(String::from),
            page_label: label.map(String::from),
            pathname_template: template.map(String::from),
        }
    }

    #[test]
    fn only_green_states_are_present() {
        let states = [
            state("b-open", ClassificationStatus::Green),
            state("a-idle", ClassificationStatus::Green),
            state("c-half", ClassificationStatus::Yellow),
            state("d-miss", ClassificationStatus::Red),
        ];
        assert_eq!(present_among(&states), vec!["b-open", "a-idle"]);
    }

    #[test]
    fn modelled_node_from_an_evaluated_spec() {
        let node = build_node(
            &identity(Some("settings"), None),
            &SpecLookup::Evaluated {
                spec_id: "settings".into(),
                present_state_ids: vec!["b-open".into(), "a-idle".into()],
            },
        );
        assert!(node.modelled);
        assert_eq!(
            node.key(),
            "settings#a-idle,b-open",
            "states sort byte-wise"
        );
        assert!(node.validate().is_ok());
    }

    #[test]
    fn evaluated_spec_with_no_present_state_is_unmodelled_but_keeps_the_spec() {
        let node = build_node(
            &identity(Some("settings"), None),
            &SpecLookup::Evaluated {
                spec_id: "settings".into(),
                present_state_ids: vec![],
            },
        );
        assert!(!node.modelled);
        assert_eq!(node.spec_id.as_deref(), Some("settings"));
        assert_eq!(node.key(), "unmodelled:settings");
    }

    #[test]
    fn no_spec_is_unmodelled_and_prefers_the_route_template() {
        let node = build_node(
            &identity(Some("run-detail"), Some("/runs/[id]")),
            &SpecLookup::NoSpec,
        );
        assert!(!node.modelled);
        assert_eq!(node.spec_id, None);
        assert_eq!(node.key(), "unmodelled:/runs/[id]");
        assert!(node.validate().is_ok());
    }

    #[test]
    fn not_evaluable_spec_is_unmodelled() {
        let node = build_node(
            &identity(Some("p"), None),
            &SpecLookup::NotEvaluable {
                spec_id: "p".into(),
            },
        );
        assert!(!node.modelled);
        assert_eq!(node.key(), "unmodelled:p");
    }

    #[test]
    fn page_identity_never_labels_from_the_pathname() {
        let id = page_identity(&json!({ "page": { "pathname": "/search/secret" } }));
        assert_eq!(
            id.page_label, None,
            "pathname must never become a pageLabel"
        );
        assert_eq!(id.pathname_template, None);
        assert_eq!(
            id.spec_lookup_label.as_deref(),
            Some("search-secret"),
            "the pathname may still LOOK UP a spec (only stored if that spec exists)"
        );
    }

    #[test]
    fn page_identity_reads_the_control_snapshot_shape() {
        let id = page_identity(&json!({
            "activeTab": "config-log-sources",
            "page": {
                "pathname": "/x/42",
                "route": { "pattern": "/x/[id]" },
                "pageContext": { "name": "Import / Export" }
            }
        }));
        assert_eq!(id.page_label.as_deref(), Some("config-log-sources"));
        assert_eq!(id.pathname_template.as_deref(), Some("/x/[id]"));
    }

    #[test]
    fn page_identity_reads_the_semantic_snapshot_shape() {
        let id = page_identity(&json!({
            "snapshotId": "s1",
            "page": { "pathname": "/x/42", "pageName": "Import / Export", "routePattern": "/x/:id" }
        }));
        assert_eq!(id.page_label.as_deref(), Some("import-export"));
        assert!(!id.page_label.unwrap().contains('/'));
        assert_eq!(id.pathname_template.as_deref(), Some("/x/:id"));
    }

    #[test]
    fn element_effect_is_fail_closed() {
        // A lone declared `read` cannot vouch for an undeclared click.
        let el = json!({"actions": ["click"], "customActions": [{"id": "peek", "effect": "read"}]});
        assert_eq!(element_declared_effect(&el), None);
        // All-custom, all-read: read.
        let el = json!({"customActions": [{"id": "peek", "effect": "read"}]});
        assert_eq!(element_declared_effect(&el), Some(IrEffect::Read));
        // A declared destructive always stands.
        let el = json!({"actions": ["click"], "customActions": [
            {"id": "peek", "effect": "read"}, {"id": "wipe", "effect": "destructive"}]});
        assert_eq!(element_declared_effect(&el), Some(IrEffect::Destructive));
        // One undeclared custom action keeps a read from being trusted.
        let el = json!({"customActions": [{"id": "peek", "effect": "read"}, {"id": "x"}]});
        assert_eq!(element_declared_effect(&el), None);
        // Nothing declared.
        assert_eq!(
            element_declared_effect(&json!({"actions": ["click"]})),
            None
        );
    }

    #[test]
    fn affordances_cover_interactive_elements_and_component_actions() {
        let snap = json!({
            "elements": [
                {"id": "btn-1", "type": "button", "label": "Save", "actions": ["click"]},
                {"id": "txt-1", "type": "text", "label": "Hello"},
                {"id": "term", "type": "div", "customActions": [{"id": "sendKeys", "effect": "write"}]}
            ],
            "components": [
                {"id": "grid", "name": "Grid", "actions": [{"id": "refresh", "effect": "read"}, {"id": "purge"}]}
            ]
        });
        let idx = extract_affordances(&snap);
        assert_eq!(
            idx.elements.len(),
            2,
            "the non-interactive text element is not an affordance"
        );
        assert!(idx.elements.contains_key("btn-1") && idx.elements.contains_key("term"));
        assert_eq!(
            idx.elements["term"].action_effects.get("sendKeys"),
            Some(&Some(IrEffect::Write))
        );
        assert_eq!(idx.component_actions.len(), 2);
        assert_eq!(
            idx.component_actions[&("grid".to_string(), "refresh".to_string())].fingerprint,
            "component:grid:refresh"
        );
        assert_eq!(idx.distinct().len(), 4);
    }

    #[test]
    fn the_digest_is_order_free_and_sees_a_changed_affordance_set() {
        let a = extract_affordances(&json!({"elements": [
            {"id": "x", "label": "X", "actions": ["click"]},
            {"id": "y", "label": "Y", "actions": ["click"]}]}));
        let b = extract_affordances(&json!({"elements": [
            {"id": "y", "label": "Y", "actions": ["click"]},
            {"id": "x", "label": "X", "actions": ["click"]}]}));
        let c = extract_affordances(&json!({"elements": [
            {"id": "x", "label": "X", "actions": ["click"]}]}));
        assert_eq!(affordance_digest(&a), affordance_digest(&b));
        assert_ne!(affordance_digest(&a), affordance_digest(&c));
    }

    #[test]
    fn only_explicit_roles_and_types_are_navigation() {
        assert!(is_navigation_affordance(Some("link"), None));
        assert!(is_navigation_affordance(Some("Tab"), None));
        assert!(is_navigation_affordance(None, Some("menuitem")));
        assert!(!is_navigation_affordance(Some("button"), Some("button")));
        assert!(
            !is_navigation_affordance(None, Some("tab")),
            "tab is a ROLE arm only"
        );
    }

    #[test]
    fn distinct_dedups_by_fingerprint_keeping_the_worst_effect() {
        let mut idx = AffordanceIndex::default();
        for (id, effect) in [
            ("a", Some(IrEffect::Read)),
            ("b", Some(IrEffect::Destructive)),
        ] {
            idx.elements.insert(
                id.into(),
                ElementAffordance {
                    affordance: Affordance {
                        fingerprint: "same".into(),
                        role: None,
                        declared_effect: effect,
                        navigation: false,
                    },
                    action_effects: BTreeMap::new(),
                },
            );
        }
        let d = idx.distinct();
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].declared_effect, Some(IrEffect::Destructive));
    }
}
