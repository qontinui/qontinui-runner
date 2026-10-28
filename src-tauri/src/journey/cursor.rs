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
//!
//! **Held edges** (plan
//! `2026-10-09-journey-ledger-stores-a-concrete-url-path-as-a-route-pattern`
//! D4). A fire-and-forget navigation — the SDK relay answers `pageNavigate` /
//! `pageRefresh` with `delivered: true, executed: false` before the page has
//! loaded — must not be closed by the next snapshot, which can precede the
//! load and read as a false `no_change`. Such an edge is HELD:
//! - [`HoldRule::UntilChanged`] (navigate): closes on the first snapshot
//!   [`outcome_of`] would NOT call `no_change` — a different node key OR a
//!   different affordance digest. With NO snapshot before the action there is
//!   nothing to compare against, and the first snapshot after it may still
//!   show the page being left. Snapshots can also arrive out of the order
//!   they were taken, so no early decision is safe: the edge COLLECTS every
//!   in-window snapshot and decides once it closes — ONLY at the hold's
//!   deadline plus [`HOLD_GRACE`], on a displacing action, or at the TTL; a
//!   snapshot taken after the deadline does not end it early, so a slow
//!   in-window snapshot can still count. Ties in taken time are ordered by the
//!   full serialized node, then digest, so the decision never depends on arrival order. The EARLIEST-TAKEN
//!   snapshot is the BASELINE (the page being left, or the page already
//!   reached — either way not evidence of a change); the destination is the
//!   first snapshot after it (in taken order) that differs from it; when none does,
//!   the edge closes as `to_node_unobserved` (unknown, never a guessed
//!   destination). Such an edge is therefore written up to deadline + grace
//!   (~35 s) after the action;
//! - [`HoldRule::UntilNextSnapshot`] (refresh, which legitimately lands on the
//!   same page): closes on the first snapshot TAKEN after the hold began;
//! - either way, a snapshot taken before the hold began is never evidence of
//!   where the action led, and after [`HOLD_WINDOW`] the edge closes as the
//!   existing `to_node_unobserved` (destination unknown) — never a new outcome.

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

/// How long a HELD edge waits for its closing snapshot before it closes as
/// `to_node_unobserved` (D4).
pub(crate) const HOLD_WINDOW: Duration = Duration::from_secs(5);

/// How long after a hold's deadline the worker's TIMER waits before it closes
/// the edge as `to_node_unobserved`. A snapshot requested inside the window can
/// answer after the deadline — at most the SDK dispatch timeout later (the
/// relay's `DEFAULT_COMMAND_TIMEOUT`, and the HTTP dispatch's own 30 s
/// `HTTP_DISPATCH_TIMEOUT` in `mcp/app_dispatch.rs`) — so the timer must not
/// close the edge before such a snapshot can still arrive. Inside the grace a
/// snapshot decides by when it was TAKEN ([`Cursors::observe_taken`]).
pub(crate) const HOLD_GRACE: Duration = crate::mcp::command_relay::DEFAULT_COMMAND_TIMEOUT;

/// How a held edge decides which snapshot closes it (D4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HoldRule {
    /// A fire-and-forget navigation: close on the first snapshot that
    /// [`outcome_of`] would not classify `no_change`.
    UntilChanged,
    /// A fire-and-forget refresh: close on the first snapshot taken after the
    /// hold began, whatever it shows.
    UntilNextSnapshot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Hold {
    rule: HoldRule,
    /// When the hold began. A snapshot taken before this is not evidence.
    began: Instant,
    /// When the hold expires into `to_node_unobserved`.
    deadline: Instant,
}

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

/// A fill-form body's field count and first target element id, if any
/// (`fields` as an array of `{elementId|id, …}` or as an id → value map).
/// Values are never read.
pub(crate) fn form_fields(body: &serde_json::Value) -> (usize, Option<String>) {
    match body.get("fields") {
        Some(serde_json::Value::Array(arr)) => (
            arr.len(),
            arr.first().and_then(|f| {
                first_target(f).or_else(|| {
                    f.get("id")
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                        .map(String::from)
                })
            }),
        ),
        Some(serde_json::Value::Object(map)) => (map.len(), None),
        _ => (0, None),
    }
}

/// The navigation trigger a request asks for: `replace` when it says so
/// (`replace: true`, or `mode`/`history` = `"replace"`), else `push`.
pub(crate) fn push_or_replace(body: &serde_json::Value) -> NavigationTriggerKind {
    let says_replace = body.get("replace") == Some(&serde_json::Value::Bool(true))
        || ["mode", "history", "navigation"]
            .iter()
            .any(|k| body.get(*k).and_then(|v| v.as_str()) == Some("replace"));
    if says_replace {
        NavigationTriggerKind::Replace
    } else {
        NavigationTriggerKind::Push
    }
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
    hold: Option<Hold>,
    /// A held navigate with no from-digest ([`Self::defers`]): every
    /// in-window snapshot seen, sorted by when it was TAKEN, at most
    /// [`MAX_DEFERRED_SNAPSHOTS`]. The edge's destination is decided from
    /// them when it closes ([`Self::deferred_destination`]).
    seen: Vec<(Observed, Instant)>,
}

/// Upper bound on the in-window snapshots a deferred navigate keeps. The
/// EARLIEST-taken are the ones that decide (the baseline and the first change
/// after it), so past the bound the newest-taken is dropped.
pub(crate) const MAX_DEFERRED_SNAPSHOTS: usize = 64;

impl PendingEdge {
    /// May a snapshot TAKEN at `taken_at` that did not close the edge end it
    /// as `to_node_unobserved`? Past the TTL, always. For a held edge: a
    /// snapshot taken at or after the deadline ends it (by the producer's own
    /// clock the window is over, and nothing taken later can count); one taken
    /// BEFORE the deadline ends it only once the deadline plus [`HOLD_GRACE`]
    /// has passed too — exactly the timer's rule ([`Self::sweepable`]) —
    /// because a slower in-window snapshot may still be in flight. What that
    /// guarantees, and no more: an in-window snapshot that is merely PROCESSED
    /// late never ends the hold by itself. A snapshot TAKEN after the deadline
    /// does end it, even while a slower in-window snapshot is still in flight;
    /// that one is then lost (per contract: the window is judged by the
    /// producer's clock, and the edge closes `to_node_unobserved`).
    ///
    /// A DEFERRED navigate ([`Self::defers`]) is exempt from that early end: it
    /// decides only at the deadline plus grace, on displacement or at the TTL,
    /// because its decision is made from every in-window snapshot and a slow
    /// one must still be able to count.
    fn is_stale(&self, now: Instant, taken_at: Instant) -> bool {
        let early_end = !self.defers();
        self.ttl_expired(now)
            || self.hold.is_some_and(|h| {
                (early_end && taken_at >= h.deadline) || now >= h.deadline + HOLD_GRACE
            })
    }

    /// May the TIMER close it? Past the TTL, or a hold past its deadline PLUS
    /// [`HOLD_GRACE`] — a snapshot taken inside the window may still be in
    /// flight until then.
    fn sweepable(&self, now: Instant) -> bool {
        self.ttl_expired(now) || self.hold.is_some_and(|h| now >= h.deadline + HOLD_GRACE)
    }

    fn ttl_expired(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.opened_at) >= PENDING_TTL
    }

    /// May this snapshot close the edge NOW? An unheld edge: any snapshot
    /// TAKEN at or after the action — one taken before it shows the page the
    /// action started from, never where it led. A held edge needs a snapshot
    /// taken inside its window — at or after the hold began and before its
    /// deadline — that satisfies the rule. A deferred navigate
    /// ([`Self::defers`]) never closes here: it decides when it closes.
    fn closes_on(&self, observed: &Observed, taken_at: Instant) -> bool {
        let Some(hold) = self.hold else {
            return taken_at >= self.opened_at;
        };
        if taken_at < hold.began || taken_at >= hold.deadline {
            return false;
        }
        match hold.rule {
            HoldRule::UntilNextSnapshot => true,
            // Compared against the page before the action. With no snapshot
            // before it, the decision is deferred to the close.
            HoldRule::UntilChanged => match &self.from_digest {
                Some(d) => {
                    outcome_of(&self.from_node, Some(d.as_str()), Some(observed), None)
                        != EdgeOutcome::NoChange
                }
                None => false,
            },
        }
    }

    /// A held navigate with no snapshot before it: its destination is decided
    /// from every in-window snapshot when it closes.
    fn defers(&self) -> bool {
        self.from_digest.is_none() && self.hold.is_some_and(|h| h.rule == HoldRule::UntilChanged)
    }

    /// Keep an in-window snapshot of a deferred navigate, in taken order (ties
    /// by the full serialized node, then digest, so arrival order never
    /// decides — not even which `pathnameTemplate` / `pageLabel` the written
    /// `to_node` carries, which `key()` leaves out). The list never holds two
    /// consecutive snapshots equal in node key AND digest: one equal to its
    /// predecessor adds nothing and is skipped, and one equal to its SUCCESSOR
    /// replaces it (the earlier-taken occurrence is the one that counts). So a
    /// page polled many times — in any arrival order — does not fill the
    /// [`MAX_DEFERRED_SNAPSHOTS`] cap. Past the cap the newest-taken is
    /// dropped, with a warning.
    fn remember(&mut self, observed: &Observed, taken_at: Instant) {
        let Some(hold) = self.hold else { return };
        if !self.defers() || taken_at < hold.began || taken_at >= hold.deadline {
            return;
        }
        let order = |o: &Observed, t: Instant| {
            (
                t,
                serde_json::to_string(&o.node).unwrap_or_default(),
                o.digest.clone(),
            )
        };
        let new_key = order(observed, taken_at);
        let at = self.seen.partition_point(|(o, t)| order(o, *t) <= new_key);
        let same =
            |o: &Observed| o.node.key() == observed.node.key() && o.digest == observed.digest;
        if at
            .checked_sub(1)
            .and_then(|i| self.seen.get(i))
            .is_some_and(|(prev, _)| same(prev))
        {
            return;
        }
        if let Some(next) = self.seen.get_mut(at).filter(|(next, _)| same(next)) {
            *next = (observed.clone(), taken_at);
            return;
        }
        self.seen.insert(at, (observed.clone(), taken_at));
        if self.seen.len() > MAX_DEFERRED_SNAPSHOTS {
            self.seen.truncate(MAX_DEFERRED_SNAPSHOTS);
            tracing::warn!(
                "journey: a deferred navigate kept {MAX_DEFERRED_SNAPSHOTS} distinct in-window \
                 snapshots; the newest-taken was dropped"
            );
        }
    }

    /// The destination of a deferred navigate: the first snapshot AFTER the
    /// baseline (the first one) in taken order — ties broken by the full serialized
    /// node, then digest — that differs from it. Two snapshots taken at the same instant
    /// have no observable order, so the tie-break is arbitrary but
    /// deterministic: arrival order never decides.
    fn deferred_destination(&self) -> Option<Observed> {
        if !self.defers() {
            return None;
        }
        let ((base, _), rest) = self.seen.split_first()?;
        rest.iter()
            .find(|(o, _)| {
                outcome_of(&base.node, Some(base.digest.as_str()), Some(o), None)
                    != EdgeOutcome::NoChange
            })
            .map(|(o, _)| o.clone())
    }

    /// Close with no closing snapshot: `to_node_unobserved`, unless a deferred
    /// navigate's collected snapshots decide a destination.
    fn close_unobserved(self, key: &CursorKey) -> EdgeDraft {
        let dest = self.deferred_destination();
        self.close(key, dest.as_ref())
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
    /// When `last` was TAKEN: a snapshot taken earlier but processed later
    /// never moves the cursor back to an older page.
    last_taken: Option<Instant>,
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
        self.open_held(key, action, provenance, hint, now, None)
    }

    /// [`Self::open`], HOLDING the edge under `hold` (D4). A failed action
    /// (an `error` hint) is never held: nothing was delivered to wait for.
    pub(crate) fn open_held(
        &mut self,
        key: &CursorKey,
        action: &ActionSpec,
        provenance: Provenance,
        hint: Option<EdgeOutcome>,
        now: Instant,
        hold: Option<HoldRule>,
    ) -> Option<EdgeDraft> {
        let hold = hold
            .filter(|_| hint != Some(EdgeOutcome::Error))
            .map(|rule| Hold {
                rule,
                began: now,
                deadline: now + HOLD_WINDOW,
            });
        let cursor = self.map.entry(key.clone()).or_default();
        let displaced = cursor.pending.take().map(|p| p.close_unobserved(key));
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
            hold,
            seen: Vec::new(),
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
        self.observe_taken(key, observed, affordances, now, now)
    }

    /// [`Self::observe`] for a snapshot TAKEN at `taken_at` (its request
    /// began then) and processed at `now`. A HELD edge stays pending until a
    /// snapshot satisfies its [`HoldRule`]. The snapshot becomes the cursor's
    /// last node (and its affordances the next action's) only when it is not
    /// older than the last one: a late arrival of an older snapshot must not
    /// open the next edge from a page the user already left.
    pub(crate) fn observe_taken(
        &mut self,
        key: &CursorKey,
        observed: Observed,
        affordances: AffordanceIndex,
        taken_at: Instant,
        now: Instant,
    ) -> Option<EdgeDraft> {
        let cursor = self.map.entry(key.clone()).or_default();
        let closed = match cursor.pending.take() {
            None => None,
            Some(mut p) if p.ttl_expired(now) => {
                p.remember(&observed, taken_at);
                Some(p.close_unobserved(key))
            }
            Some(p) if p.closes_on(&observed, taken_at) => Some(p.close(key, Some(&observed))),
            Some(mut p) if p.is_stale(now, taken_at) => {
                // An in-window snapshot processed past the grace still counts
                // toward a deferred navigate's decision.
                p.remember(&observed, taken_at);
                Some(p.close_unobserved(key))
            }
            Some(mut held) => {
                held.remember(&observed, taken_at);
                cursor.pending = Some(held);
                None
            }
        };
        if cursor.last_taken.is_none_or(|last| taken_at >= last) {
            cursor.last = Some(observed);
            cursor.last_affordances = affordances;
            cursor.last_taken = Some(taken_at);
        }
        closed
    }

    /// When the worker's timer should next wake: the earliest hold deadline
    /// plus [`HOLD_GRACE`], so an expired hold closes without waiting for the
    /// hourly sweep, but never before an in-window snapshot could land.
    pub(crate) fn next_hold_deadline(&self) -> Option<Instant> {
        self.map
            .values()
            .filter_map(|c| c.pending.as_ref().and_then(|p| p.hold))
            .map(|h| h.deadline + HOLD_GRACE)
            .min()
    }

    /// Close every pending edge older than [`PENDING_TTL`], or held past its
    /// deadline plus [`HOLD_GRACE`], as unobserved.
    pub(crate) fn sweep(&mut self, now: Instant) -> Vec<EdgeDraft> {
        let mut drafts = Vec::new();
        for (key, cursor) in &mut self.map {
            if cursor.pending.as_ref().is_some_and(|p| p.sweepable(now)) {
                if let Some(p) = cursor.pending.take() {
                    drafts.push(p.close_unobserved(key));
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

    // ---- D4: held fire-and-forget navigations ------------------------------

    fn navigate() -> ActionSpec {
        ActionSpec::navigation("navigate", NavigationTriggerKind::Push)
    }

    fn refresh() -> ActionSpec {
        ActionSpec::navigation("refresh", NavigationTriggerKind::Initial)
    }

    /// The 2026-10-09 false `no_change` (rows aa5a8fdd / d21c5a93): an
    /// `executed: false` navigate followed only by snapshots of the page it
    /// left closes as `to_node_unobserved` once the window passes — never
    /// `no_change`.
    #[test]
    fn an_unexecuted_navigate_then_an_unchanged_snapshot_is_unobserved_not_no_change() {
        let now = t0();
        let mut c = Cursors::default();
        c.observe(
            &key(),
            seen("home", &["idle"], "d1"),
            AffordanceIndex::default(),
            now,
        );
        c.open_held(
            &key(),
            &navigate(),
            Provenance::default(),
            None,
            now,
            Some(HoldRule::UntilChanged),
        );
        let soon = now + Duration::from_millis(200);
        assert!(
            c.observe_taken(
                &key(),
                seen("home", &["idle"], "d1"),
                AffordanceIndex::default(),
                soon,
                soon
            )
            .is_none(),
            "an unchanged snapshot inside the window does not close a held navigate"
        );
        assert_eq!(c.pending_count(), 1);
        assert_eq!(c.next_hold_deadline(), Some(now + HOLD_WINDOW + HOLD_GRACE));
        let late = now + HOLD_WINDOW;
        let edge = c
            .observe_taken(
                &key(),
                seen("home", &["idle"], "d1"),
                AffordanceIndex::default(),
                late,
                late,
            )
            .expect("the expired hold closes");
        assert_eq!(edge.outcome, EdgeOutcome::ToNodeUnobserved);
        assert!(edge.to_node.is_none());
    }

    /// Warning 1 of the review: a snapshot TAKEN inside the window that shows
    /// the change closes the edge as `changed`, even when the worker reaches it
    /// after the deadline.
    #[test]
    fn a_change_taken_inside_the_window_closes_even_if_processed_late() {
        let now = t0();
        let mut c = Cursors::default();
        c.observe(
            &key(),
            seen("home", &["idle"], "d1"),
            AffordanceIndex::default(),
            now,
        );
        c.open_held(
            &key(),
            &navigate(),
            Provenance::default(),
            None,
            now,
            Some(HoldRule::UntilChanged),
        );
        let edge = c
            .observe_taken(
                &key(),
                seen("detail", &["open"], "d2"),
                AffordanceIndex::default(),
                now + Duration::from_secs(4),
                now + HOLD_WINDOW + Duration::from_secs(3),
            )
            .expect("closes");
        assert_eq!(edge.outcome, EdgeOutcome::Changed);
        // ...while a change TAKEN after the deadline does not count.
        let mut c = Cursors::default();
        c.observe(
            &key(),
            seen("home", &["idle"], "d1"),
            AffordanceIndex::default(),
            now,
        );
        c.open_held(
            &key(),
            &navigate(),
            Provenance::default(),
            None,
            now,
            Some(HoldRule::UntilChanged),
        );
        let late = now + HOLD_WINDOW + Duration::from_secs(1);
        let edge = c
            .observe_taken(
                &key(),
                seen("detail", &["open"], "d2"),
                AffordanceIndex::default(),
                late,
                late,
            )
            .expect("closes");
        assert_eq!(edge.outcome, EdgeOutcome::ToNodeUnobserved);
    }

    /// No snapshot before the navigate: the first snapshot after it may still
    /// be the page being LEFT, so it is the baseline, not the destination.
    /// The decision is deferred to the close, which picks the earliest-taken
    /// snapshot after the baseline that differs from it.
    #[test]
    fn a_held_navigate_with_no_prior_snapshot_closes_on_the_first_change_after_it() {
        let now = t0();
        let mut c = Cursors::default();
        open_unknown_navigate(&mut c, now);
        for ms in [100, 400, 900] {
            let (name, digest) = if ms < 900 {
                ("old", "d0")
            } else {
                ("new", "d1")
            };
            let t = now + Duration::from_millis(ms);
            assert!(
                c.observe_taken(
                    &key(),
                    seen(name, &["1"], digest),
                    AffordanceIndex::default(),
                    t,
                    t
                )
                .is_none(),
                "{ms} ms: a deferred navigate decides only when it closes"
            );
        }
        assert!(
            c.sweep(now + HOLD_WINDOW).is_empty(),
            "the grace runs first"
        );
        let swept = c.sweep(now + HOLD_WINDOW + HOLD_GRACE);
        assert_eq!(swept.len(), 1);
        assert_eq!(swept[0].outcome, EdgeOutcome::Changed);
        assert_eq!(swept[0].to_node, Some(node("new", &["1"])));
        assert_eq!(
            swept[0].from_node,
            unknown_node(),
            "nothing is claimed about the page left"
        );
    }

    fn open_unknown_navigate(c: &mut Cursors, now: Instant) {
        c.open_held(
            &key(),
            &navigate(),
            Provenance::default(),
            None,
            now,
            Some(HoldRule::UntilChanged),
        );
    }

    /// The earliest-taken snapshot is the baseline whatever order the worker
    /// processes them in: a new-page snapshot processed first and an
    /// old-page one TAKEN earlier but processed later close as `changed` to
    /// the new page.
    #[test]
    fn an_out_of_order_snapshot_taken_earlier_replaces_the_baseline() {
        let now = t0();
        let mut c = Cursors::default();
        open_unknown_navigate(&mut c, now);
        let t1 = now + Duration::from_millis(100); // the page being left
        let t2 = now + Duration::from_millis(300); // the new page
        let late = now + Duration::from_millis(600);
        assert!(c
            .observe_taken(
                &key(),
                seen("new", &["2"], "d1"),
                AffordanceIndex::default(),
                t2,
                t2
            )
            .is_none());
        assert!(c
            .observe_taken(
                &key(),
                seen("old", &["1"], "d0"),
                AffordanceIndex::default(),
                t1,
                late
            )
            .is_none());
        let swept = c.sweep(now + HOLD_WINDOW + HOLD_GRACE);
        assert_eq!(swept[0].outcome, EdgeOutcome::Changed);
        assert_eq!(
            swept[0].to_node,
            Some(node("new", &["2"])),
            "the destination is the new page, never the late old one"
        );
    }

    /// W2 of the round-4 review: old@100, new@200, old@300 processed in the
    /// order 300, 200, 100. The change is the FIRST differing snapshot after
    /// the baseline — the new page at 200 — not the page returned to at 300.
    #[test]
    fn the_earliest_change_after_the_baseline_wins_whatever_the_processing_order() {
        let now = t0();
        let mut c = Cursors::default();
        open_unknown_navigate(&mut c, now);
        let late = now + Duration::from_millis(800);
        for (ms, name, digest) in [(300, "old", "d0"), (200, "new", "d1"), (100, "old", "d0")] {
            assert!(c
                .observe_taken(
                    &key(),
                    seen(name, &["1"], digest),
                    AffordanceIndex::default(),
                    now + Duration::from_millis(ms),
                    late,
                )
                .is_none());
        }
        let swept = c.sweep(now + HOLD_WINDOW + HOLD_GRACE);
        assert_eq!(swept.len(), 1);
        assert_eq!(swept[0].outcome, EdgeOutcome::Changed);
        assert_eq!(swept[0].to_node, Some(node("new", &["1"])));
    }

    /// Nothing differs from the earliest-taken snapshot: the destination is
    /// unknown, and a displacing action closes it the same way.
    #[test]
    fn a_deferred_navigate_with_no_change_is_unobserved_even_when_displaced() {
        let now = t0();
        let mut c = Cursors::default();
        open_unknown_navigate(&mut c, now);
        let t1 = now + Duration::from_millis(100);
        let t2 = now + Duration::from_millis(300);
        for t in [t2, t1] {
            assert!(c
                .observe_taken(
                    &key(),
                    seen("old", &["1"], "d0"),
                    AffordanceIndex::default(),
                    t,
                    t2
                )
                .is_none());
        }
        let displaced = c
            .open(
                &key(),
                &navigate(),
                Provenance::default(),
                None,
                now + Duration::from_secs(1),
            )
            .expect("the next action displaces the deferred navigate");
        assert_eq!(displaced.outcome, EdgeOutcome::ToNodeUnobserved);
    }

    /// Round 5, item 5: 64 identical polls of the page being left must not
    /// fill the cap and push the change out.
    #[test]
    fn repeated_identical_snapshots_do_not_crowd_out_the_change() {
        let now = t0();
        let mut c = Cursors::default();
        open_unknown_navigate(&mut c, now);
        for i in 0..MAX_DEFERRED_SNAPSHOTS as u64 {
            let t = now + Duration::from_millis(10 + i * 10);
            assert!(c
                .observe_taken(
                    &key(),
                    seen("old", &["1"], "d0"),
                    AffordanceIndex::default(),
                    t,
                    t
                )
                .is_none());
        }
        let t = now + Duration::from_millis(10 + MAX_DEFERRED_SNAPSHOTS as u64 * 10 + 10);
        assert!(c
            .observe_taken(
                &key(),
                seen("new", &["2"], "d1"),
                AffordanceIndex::default(),
                t,
                t
            )
            .is_none());
        let swept = c.sweep(now + HOLD_WINDOW + HOLD_GRACE);
        assert_eq!(swept[0].outcome, EdgeOutcome::Changed);
        assert_eq!(swept[0].to_node, Some(node("new", &["2"])));
    }

    /// Round 5, item 6: a snapshot TAKEN after the deadline does not end a
    /// deferred navigate early — a slow in-window snapshot still counts.
    #[test]
    fn a_post_deadline_snapshot_does_not_end_a_deferred_navigate_early() {
        let now = t0();
        let mut c = Cursors::default();
        open_unknown_navigate(&mut c, now);
        let t1 = now + Duration::from_millis(100);
        assert!(c
            .observe_taken(
                &key(),
                seen("old", &["1"], "d0"),
                AffordanceIndex::default(),
                t1,
                t1
            )
            .is_none());
        let after = now + HOLD_WINDOW + Duration::from_secs(1);
        assert!(
            c.observe_taken(
                &key(),
                seen("late", &["9"], "d9"),
                AffordanceIndex::default(),
                after,
                after
            )
            .is_none(),
            "taken after the deadline: no early end for a deferred navigate"
        );
        // The slow in-window snapshot lands afterwards and decides.
        let slow = now + Duration::from_secs(2);
        assert!(c
            .observe_taken(
                &key(),
                seen("new", &["2"], "d1"),
                AffordanceIndex::default(),
                slow,
                after + Duration::from_secs(1),
            )
            .is_none());
        let swept = c.sweep(now + HOLD_WINDOW + HOLD_GRACE);
        assert_eq!(swept[0].outcome, EdgeOutcome::Changed);
        assert_eq!(swept[0].to_node, Some(node("new", &["2"])));
    }

    /// Round 5, item 7a: a displacing action closes a deferred navigate from
    /// its collected snapshots — `changed` when they show a change.
    #[test]
    fn a_displaced_deferred_navigate_that_saw_a_change_closes_changed() {
        let now = t0();
        let mut c = Cursors::default();
        open_unknown_navigate(&mut c, now);
        for (ms, name, digest) in [(100, "old", "d0"), (300, "new", "d1")] {
            let t = now + Duration::from_millis(ms);
            assert!(c
                .observe_taken(
                    &key(),
                    seen(name, &["1"], digest),
                    AffordanceIndex::default(),
                    t,
                    t
                )
                .is_none());
        }
        let displaced = c
            .open(
                &key(),
                &navigate(),
                Provenance::default(),
                None,
                now + Duration::from_secs(1),
            )
            .expect("the next action displaces it");
        assert_eq!(displaced.outcome, EdgeOutcome::Changed);
        assert_eq!(displaced.to_node, Some(node("new", &["1"])));
    }

    /// Round 5, item 7b: the stale arm REMEMBERS the arriving in-window
    /// snapshot before it decides, so a change seen only in a snapshot
    /// processed past the grace still closes the edge `changed`.
    #[test]
    fn the_stale_arm_counts_the_snapshot_that_ended_the_hold() {
        let now = t0();
        let mut c = Cursors::default();
        open_unknown_navigate(&mut c, now);
        let t1 = now + Duration::from_millis(100);
        assert!(c
            .observe_taken(
                &key(),
                seen("old", &["1"], "d0"),
                AffordanceIndex::default(),
                t1,
                t1
            )
            .is_none());
        let edge = c
            .observe_taken(
                &key(),
                seen("new", &["2"], "d1"),
                AffordanceIndex::default(),
                now + Duration::from_secs(2),
                now + HOLD_WINDOW + HOLD_GRACE,
            )
            .expect("past the grace: the hold ends on this snapshot");
        assert_eq!(edge.outcome, EdgeOutcome::Changed);
        assert_eq!(edge.to_node, Some(node("new", &["2"])));
    }

    /// Round 5, item 8: snapshots TAKEN at the same instant are ordered by
    /// node key, then digest, so the result does not depend on arrival order.
    #[test]
    fn tied_snapshots_decide_the_same_whatever_the_arrival_order() {
        let now = t0();
        let tie = now + Duration::from_millis(100);
        let later = now + Duration::from_millis(300);
        let outcome = |first: (&str, &str), second: (&str, &str)| {
            let mut c = Cursors::default();
            open_unknown_navigate(&mut c, now);
            for (name, digest) in [first, second] {
                c.observe_taken(
                    &key(),
                    seen(name, &["1"], digest),
                    AffordanceIndex::default(),
                    tie,
                    tie,
                );
            }
            c.observe_taken(
                &key(),
                seen("b", &["1"], "db"),
                AffordanceIndex::default(),
                later,
                later,
            );
            let swept = c.sweep(now + HOLD_WINDOW + HOLD_GRACE);
            (swept[0].outcome, swept[0].to_node.clone())
        };
        let ab = outcome(("a", "da"), ("b", "db"));
        let ba = outcome(("b", "db"), ("a", "da"));
        assert_eq!(ab, ba, "arrival order must not decide");
        // "a" sorts first, so it is the baseline and "b" (later) the change.
        assert_eq!(ab, (EdgeOutcome::Changed, Some(node("b", &["1"]))));
    }

    /// Round 6: on an exact tie, two snapshots with the SAME node key but a
    /// different `pageLabel` must write the same `to_node` whatever the
    /// arrival order — the tie breaks on the full node, not `key()`.
    #[test]
    fn a_tie_with_equal_keys_writes_the_same_node_whatever_the_arrival_order() {
        let now = t0();
        let base = now + Duration::from_millis(50);
        let tie = now + Duration::from_millis(100);
        // Same key (`unmodelled:x`: template ?? label), different full node.
        let a = Observed {
            node: JourneyNode::new(None, Vec::<String>::new(), None, Some("x".into())),
            digest: "dx".into(),
        };
        let b = Observed {
            node: JourneyNode::new(None, Vec::<String>::new(), Some("x".into()), None),
            digest: "dx".into(),
        };
        let first = Observed {
            node: JourneyNode::new(Some("spec".into()), ["s".to_string()], None, None),
            digest: "d0".into(),
        };
        let run = |x: &Observed, y: &Observed| {
            let mut c = Cursors::default();
            open_unknown_navigate(&mut c, now);
            c.observe_taken(
                &key(),
                first.clone(),
                AffordanceIndex::default(),
                base,
                base,
            );
            for o in [x, y] {
                c.observe_taken(&key(), o.clone(), AffordanceIndex::default(), tie, tie);
            }
            c.sweep(now + HOLD_WINDOW + HOLD_GRACE).remove(0).to_node
        };
        assert_eq!(a.node.key(), b.node.key());
        assert_eq!(
            run(&a, &b),
            run(&b, &a),
            "arrival order must not pick the written node"
        );
    }

    /// Round 6: the dedupe keeps the EARLIER of two equal snapshots, and an
    /// out-of-order insert between them still decides: A@1 kept, A@3 skipped
    /// as a duplicate, then B@2 arrives and is the change.
    #[test]
    fn a_skipped_duplicate_does_not_hide_a_later_out_of_order_change() {
        let now = t0();
        let mut c = Cursors::default();
        open_unknown_navigate(&mut c, now);
        for (ms, name, digest) in [(100, "a", "da"), (300, "a", "da"), (200, "b", "db")] {
            let t = now + Duration::from_millis(ms);
            assert!(c
                .observe_taken(
                    &key(),
                    seen(name, &["1"], digest),
                    AffordanceIndex::default(),
                    t,
                    t
                )
                .is_none());
        }
        let swept = c.sweep(now + HOLD_WINDOW + HOLD_GRACE);
        assert_eq!(swept[0].outcome, EdgeOutcome::Changed);
        assert_eq!(swept[0].to_node, Some(node("b", &["1"])));
    }

    /// Round 6: the 64-cap truncation path. More than
    /// [`MAX_DEFERRED_SNAPSHOTS`] DISTINCT snapshots: the newest-taken are
    /// dropped and the decision is still made from the earliest-taken.
    #[test]
    fn the_cap_drops_the_newest_and_keeps_the_deciding_snapshots() {
        let now = t0();
        let mut c = Cursors::default();
        open_unknown_navigate(&mut c, now);
        let extra = 10u64;
        // Taken newest-first, so every insert past the cap lands at the front
        // and the truncation must drop from the BACK.
        for i in (0..MAX_DEFERRED_SNAPSHOTS as u64 + extra).rev() {
            let t = now + Duration::from_millis(10 + i * 10);
            let name = format!("p{i}");
            assert!(c
                .observe_taken(
                    &key(),
                    seen(&name, &["1"], &format!("d{i}")),
                    AffordanceIndex::default(),
                    t,
                    t,
                )
                .is_none());
        }
        let swept = c.sweep(now + HOLD_WINDOW + HOLD_GRACE);
        assert_eq!(swept[0].outcome, EdgeOutcome::Changed);
        assert_eq!(
            swept[0].to_node,
            Some(node("p1", &["1"])),
            "baseline p0, first change p1 — both earliest-taken, both kept"
        );
    }

    /// Round 7: more than [`MAX_DEFERRED_SNAPSHOTS`] polls of ONE page arriving
    /// NEWEST-FIRST, then the change. Each poll lands before an equal
    /// successor and must replace it, so the cap never fills and the change
    /// still decides.
    #[test]
    fn newest_first_polls_of_one_page_do_not_crowd_out_the_change() {
        let now = t0();
        let mut c = Cursors::default();
        open_unknown_navigate(&mut c, now);
        let polls = MAX_DEFERRED_SNAPSHOTS as u64 + 6;
        for i in (0..polls).rev() {
            let t = now + Duration::from_millis(10 + i * 10);
            assert!(c
                .observe_taken(
                    &key(),
                    seen("old", &["1"], "d0"),
                    AffordanceIndex::default(),
                    t,
                    t
                )
                .is_none());
        }
        let t = now + Duration::from_millis(10 + polls * 10 + 10);
        assert!(c
            .observe_taken(
                &key(),
                seen("new", &["2"], "d1"),
                AffordanceIndex::default(),
                t,
                t
            )
            .is_none());
        let swept = c.sweep(now + HOLD_WINDOW + HOLD_GRACE);
        assert_eq!(swept[0].outcome, EdgeOutcome::Changed);
        assert_eq!(swept[0].to_node, Some(node("new", &["2"])));
    }

    /// Round 5, item 8: the TTL arm remembers the arriving snapshot too.
    #[test]
    fn the_ttl_arm_counts_the_snapshot_that_arrived() {
        let now = t0();
        let mut c = Cursors::default();
        open_unknown_navigate(&mut c, now);
        let t1 = now + Duration::from_millis(100);
        c.observe_taken(
            &key(),
            seen("old", &["1"], "d0"),
            AffordanceIndex::default(),
            t1,
            t1,
        );
        let edge = c
            .observe_taken(
                &key(),
                seen("new", &["2"], "d1"),
                AffordanceIndex::default(),
                now + Duration::from_secs(2),
                now + PENDING_TTL,
            )
            .expect("past the TTL");
        assert_eq!(edge.outcome, EdgeOutcome::Changed);
        assert_eq!(edge.to_node, Some(node("new", &["2"])));
    }

    /// W1 of the round-4 review: an UNHELD edge is never closed by a snapshot
    /// taken before the action — that one shows the page the action started
    /// from, not where it led.
    #[test]
    fn a_snapshot_taken_before_the_action_never_closes_its_edge() {
        let now = t0();
        let mut c = Cursors::default();
        let t100 = now + Duration::from_millis(100);
        let t200 = now + Duration::from_millis(200);
        let t300 = now + Duration::from_millis(300);
        c.observe_taken(
            &key(),
            seen("b", &["1"], "db"),
            AffordanceIndex::default(),
            t200,
            t200,
        );
        c.open(&key(), &navigate(), Provenance::default(), None, t300);
        assert!(
            c.observe_taken(
                &key(),
                seen("a", &["0"], "da"),
                AffordanceIndex::default(),
                t100,
                t300 + Duration::from_millis(50),
            )
            .is_none(),
            "taken before the action: not its destination"
        );
        let edge = c
            .observe(
                &key(),
                seen("c", &["2"], "dc"),
                AffordanceIndex::default(),
                t300 + Duration::from_millis(100),
            )
            .expect("a snapshot taken after the action closes it");
        assert_eq!(edge.from_node, node("b", &["1"]));
        assert_eq!(edge.to_node, Some(node("c", &["2"])));
    }

    /// Fix 2: a late older snapshot must not move the cursor back — the next
    /// action opens from the NEWEST-taken node, with its affordances.
    #[test]
    fn a_late_older_snapshot_never_moves_the_cursor_back() {
        let now = t0();
        let mut c = Cursors::default();
        let t1 = now + Duration::from_millis(100);
        let t2 = now + Duration::from_millis(300);
        c.observe_taken(
            &key(),
            seen("new", &["2"], "d1"),
            AffordanceIndex::default(),
            t2,
            t2,
        );
        c.observe_taken(
            &key(),
            seen("old", &["1"], "d0"),
            AffordanceIndex::default(),
            t1,
            now + Duration::from_millis(500),
        );
        c.open(
            &key(),
            &navigate(),
            Provenance::default(),
            None,
            now + Duration::from_millis(600),
        );
        let edge = c
            .observe(
                &key(),
                seen("next", &["3"], "d2"),
                AffordanceIndex::default(),
                now + Duration::from_millis(700),
            )
            .expect("closes");
        assert_eq!(
            edge.from_node,
            node("new", &["2"]),
            "opened from the newest-taken node"
        );
    }

    /// W1: an unchanged snapshot TAKEN inside the window but processed after
    /// the deadline does not end the hold while the grace runs — a slower
    /// in-window snapshot showing the change may still arrive, and closes it.
    #[test]
    fn a_late_in_window_snapshot_waits_out_the_grace() {
        let now = t0();
        let mut c = Cursors::default();
        c.observe(
            &key(),
            seen("home", &["idle"], "d1"),
            AffordanceIndex::default(),
            now,
        );
        c.open_held(
            &key(),
            &navigate(),
            Provenance::default(),
            None,
            now,
            Some(HoldRule::UntilChanged),
        );
        let after_deadline = now + HOLD_WINDOW + Duration::from_secs(2);
        assert!(
            c.observe_taken(
                &key(),
                seen("home", &["idle"], "d1"),
                AffordanceIndex::default(),
                now + Duration::from_secs(1),
                after_deadline,
            )
            .is_none(),
            "an in-window, unchanged snapshot inside the grace leaves the hold open"
        );
        let edge = c
            .observe_taken(
                &key(),
                seen("detail", &["open"], "d2"),
                AffordanceIndex::default(),
                now + Duration::from_secs(3),
                after_deadline + Duration::from_secs(1),
            )
            .expect("the in-window change closes it");
        assert_eq!(edge.outcome, EdgeOutcome::Changed);
        // Past the grace, an in-window unchanged snapshot does end it.
        let mut c = Cursors::default();
        c.observe(
            &key(),
            seen("home", &["idle"], "d1"),
            AffordanceIndex::default(),
            now,
        );
        c.open_held(
            &key(),
            &navigate(),
            Provenance::default(),
            None,
            now,
            Some(HoldRule::UntilChanged),
        );
        let edge = c
            .observe_taken(
                &key(),
                seen("home", &["idle"], "d1"),
                AffordanceIndex::default(),
                now + Duration::from_secs(1),
                now + HOLD_WINDOW + HOLD_GRACE,
            )
            .expect("past the grace");
        assert_eq!(edge.outcome, EdgeOutcome::ToNodeUnobserved);
    }

    /// ...and when nothing ever differs from the baseline, the destination is
    /// UNKNOWN: `to_node_unobserved`, never the old page dressed as `changed`.
    #[test]
    fn a_held_navigate_with_no_prior_snapshot_and_no_change_is_unobserved() {
        let now = t0();
        let mut c = Cursors::default();
        open_unknown_navigate(&mut c, now);
        let t = now + Duration::from_millis(100);
        assert!(c
            .observe_taken(
                &key(),
                seen("old", &["1"], "d0"),
                AffordanceIndex::default(),
                t,
                t
            )
            .is_none());
        let swept = c.sweep(now + HOLD_WINDOW + HOLD_GRACE);
        assert_eq!(swept.len(), 1);
        assert_eq!(swept[0].outcome, EdgeOutcome::ToNodeUnobserved);
        assert!(swept[0].to_node.is_none());
    }

    #[test]
    fn the_grace_covers_the_sdk_snapshot_timeout() {
        assert!(HOLD_GRACE >= Duration::from_secs(30));
    }

    #[test]
    fn an_expired_hold_is_swept_as_unobserved() {
        let now = t0();
        let mut c = Cursors::default();
        c.open_held(
            &key(),
            &navigate(),
            Provenance::default(),
            None,
            now,
            Some(HoldRule::UntilChanged),
        );
        assert!(c.sweep(now + Duration::from_secs(1)).is_empty());
        assert!(
            c.sweep(now + HOLD_WINDOW).is_empty(),
            "the timer waits out the grace: an in-window snapshot may still be in flight"
        );
        let swept = c.sweep(now + HOLD_WINDOW + HOLD_GRACE);
        assert_eq!(swept.len(), 1);
        assert_eq!(swept[0].outcome, EdgeOutcome::ToNodeUnobserved);
        assert_eq!(c.next_hold_deadline(), None);
    }

    #[test]
    fn a_held_navigate_closes_on_a_changed_key_or_a_changed_digest() {
        for (to, label) in [
            (seen("detail", &["open"], "d1"), "different key"),
            (seen("home", &["idle"], "d2"), "same key, different digest"),
        ] {
            let now = t0();
            let mut c = Cursors::default();
            c.observe(
                &key(),
                seen("home", &["idle"], "d1"),
                AffordanceIndex::default(),
                now,
            );
            c.open_held(
                &key(),
                &navigate(),
                Provenance::default(),
                None,
                now,
                Some(HoldRule::UntilChanged),
            );
            let t = now + Duration::from_secs(1);
            let edge = c
                .observe_taken(&key(), to, AffordanceIndex::default(), t, t)
                .unwrap_or_else(|| panic!("{label}: must close"));
            assert_eq!(edge.outcome, EdgeOutcome::Changed, "{label}");
            assert!(edge.to_node.is_some(), "{label}");
        }
    }

    #[test]
    fn a_snapshot_taken_before_the_hold_began_never_closes_it() {
        let now = t0();
        let mut c = Cursors::default();
        c.observe(
            &key(),
            seen("home", &["idle"], "d1"),
            AffordanceIndex::default(),
            now,
        );
        let action_at = now + Duration::from_millis(500);
        c.open_held(
            &key(),
            &refresh(),
            Provenance::default(),
            None,
            action_at,
            Some(HoldRule::UntilNextSnapshot),
        );
        // Requested before the refresh answered, processed after it.
        assert!(c
            .observe_taken(
                &key(),
                seen("other", &["x"], "d9"),
                AffordanceIndex::default(),
                now,
                action_at + Duration::from_millis(10),
            )
            .is_none());
        assert_eq!(c.pending_count(), 1);
    }

    #[test]
    fn a_held_refresh_closes_on_the_next_snapshot_even_if_unchanged() {
        let now = t0();
        let mut c = Cursors::default();
        c.observe(
            &key(),
            seen("home", &["idle"], "d1"),
            AffordanceIndex::default(),
            now,
        );
        c.open_held(
            &key(),
            &refresh(),
            Provenance::default(),
            None,
            now,
            Some(HoldRule::UntilNextSnapshot),
        );
        let t = now + Duration::from_millis(300);
        let edge = c
            .observe_taken(
                &key(),
                seen("home", &["idle"], "d1"),
                AffordanceIndex::default(),
                t,
                t,
            )
            .expect("a refresh closes on the first snapshot after it");
        assert_eq!(edge.outcome, EdgeOutcome::NoChange);
        assert_eq!(edge.trigger.action_type, "refresh");
    }

    #[test]
    fn a_failed_navigate_is_never_held() {
        let now = t0();
        let mut c = Cursors::default();
        c.observe(
            &key(),
            seen("home", &["idle"], "d1"),
            AffordanceIndex::default(),
            now,
        );
        c.open_held(
            &key(),
            &navigate(),
            Provenance::default(),
            failure_hint(true),
            now,
            Some(HoldRule::UntilChanged),
        );
        assert_eq!(c.next_hold_deadline(), None);
        let edge = c
            .observe(
                &key(),
                seen("home", &["idle"], "d1"),
                AffordanceIndex::default(),
                now,
            )
            .unwrap();
        assert_eq!(edge.outcome, EdgeOutcome::Error);
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
    fn request_readers_never_read_values() {
        let (n, first) =
            form_fields(&json!({"fields": [{"elementId": "a", "value": "secret"}, {"id": "b"}]}));
        assert_eq!((n, first.as_deref()), (2, Some("a")));
        assert_eq!(
            form_fields(&json!({"fields": {"a": "x", "b": "y"}})),
            (2, None)
        );
        assert_eq!(
            push_or_replace(&json!({"url": "/x"})),
            NavigationTriggerKind::Push
        );
        assert_eq!(
            push_or_replace(&json!({"replace": true})),
            NavigationTriggerKind::Replace
        );
        assert_eq!(
            push_or_replace(&json!({"mode": "replace"})),
            NavigationTriggerKind::Replace
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
