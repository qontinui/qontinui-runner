//! Who holds a slot lease that is not ours — the PURE core of the supervisor's
//! holder observation (plan
//! `2026-10-30-looping-agent-slot-holder-is-never-named`, Phases 2-3).
//!
//! The acquire reply names the holder on every live `held` once
//! [`super::lease::parse_acquire_body`] reads the reserve door's `holder` key.
//! This module is the bounded guard for the shapes that parse cannot name: a
//! coord build that stops emitting the holder, or the empty-string vanish race
//! (where an earlier acquire reply's name is NOT re-served: the holder reads
//! UNKNOWN, `acquire_unnamed`, until the next read is due).
//! It decides WHEN a `GET /coord/claims/by-resource` fallback read is due (at
//! most once per [`LEASE_TTL_SECS`] per resource key, failures included, so a
//! down coord costs one retry per TTL rather than one per 5 s tick) and parses
//! that read strictly.
//!
//! ## Fail-open invariant
//!
//! Everything here is INFORMATIONAL. No value this module produces is ever an
//! input to [`super::lease::spawn_permitted`] or [`super::lease::gate_action`]:
//! a `Held` slot stays held, and an `Unavailable` acquire stays unavailable,
//! whatever the holder read says. Naming the holder changes a log line and a
//! status field, never a spawn decision.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use super::lease::{SlotHolder, LEASE_TTL_SECS};

/// The coord read that names a claim's holder. `/coord`-prefixed: only the
/// claim WRITE doors (`/claims/acquire|heartbeat|release`) are bare, and the
/// bare `/claims/by-resource` 404s (coord `routes.rs:3604`).
pub const BY_RESOURCE_PATH: &str = "/coord/claims/by-resource";

/// The full by-resource URL for a coord base. Query parameters (`kind`, `key`)
/// are added by the caller through reqwest's `.query(..)`, which URL-encodes
/// the colons in a slot resource key.
pub fn by_resource_url(base: &str) -> String {
    format!("{}{BY_RESOURCE_PATH}", base.trim_end_matches('/'))
}

/// How long one holder observation is reused before the next by-resource read:
/// the lease TTL. This is a RATE bound, not a freshness guarantee — a release
/// frees a slot at once, so a cached name can be up to one TTL stale. The
/// status view carries `observedAtUnixMs`; the log lines do not show the age.
pub fn holder_observation_ttl() -> Duration {
    Duration::from_secs(LEASE_TTL_SECS.unsigned_abs())
}

/// Where a holder observation came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HolderSource {
    /// The `/claims/acquire` `held` reply named the holder.
    Acquire,
    /// The `/coord/claims/by-resource` fallback named the holder.
    ByResource,
    /// The fallback read failed (transport, non-2xx, or an unparseable body).
    /// The holder is UNKNOWN — never a fabricated id.
    LookupFailed(String),
    /// The fallback read answered `null`: nothing held the key by the time we
    /// looked (the lease turned over between the acquire and the read). Coord
    /// also answers `null` for a holder its read scope hides, so this is "not
    /// visible to this runner", not proof the slot is free.
    NotHeldAtLookup,
    /// The acquire reply named no holder, and the name we last had came from
    /// an EARLIER acquire reply, so the holder may have turned over since.
    /// UNKNOWN until the next by-resource read is due — never the earlier name.
    AcquireUnnamed,
}

/// One observation of a slot's holder, keyed by resource key in the
/// supervisor's cache.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HolderObservation {
    pub holder: Option<SlotHolder>,
    pub source: HolderSource,
    pub observed_at: Instant,
}

impl HolderObservation {
    /// The acquire reply named the holder.
    pub fn from_acquire(holder: SlotHolder, now: Instant) -> Self {
        Self {
            holder: Some(holder),
            source: HolderSource::Acquire,
            observed_at: now,
        }
    }

    /// The acquire reply named nobody and the name we had came from an earlier
    /// acquire reply: an UNKNOWN holder, observed `now`. It is served, never
    /// stored (see [`plan_observation`]), so the cached prior keeps the
    /// one-read-per-TTL clock.
    pub fn unnamed_after_acquire(now: Instant) -> Self {
        Self {
            holder: None,
            source: HolderSource::AcquireUnnamed,
            observed_at: now,
        }
    }

    /// Fold a by-resource lookup result into an observation. A failure is
    /// recorded too (with its timestamp), which is what bounds a down coord to
    /// one retry per TTL.
    pub fn from_lookup(result: Result<Option<SlotHolder>, String>, now: Instant) -> Self {
        let (holder, source) = match result {
            Ok(Some(h)) => (Some(h), HolderSource::ByResource),
            Ok(None) => (None, HolderSource::NotHeldAtLookup),
            Err(e) => (None, HolderSource::LookupFailed(e)),
        };
        Self {
            holder,
            source,
            observed_at: now,
        }
    }
}

/// Is a by-resource fallback read due for this resource key?
///
/// True only when the acquire reply named no holder AND (there is no prior
/// observation, or the prior one is at least `ttl` old). The caller only asks
/// on the `Held` arm. A prior FAILED lookup counts as an observation, so a down
/// coord is not retried every tick.
pub fn holder_lookup_due(
    acquire_holder: Option<&SlotHolder>,
    prior: Option<&HolderObservation>,
    now: Instant,
    ttl: Duration,
) -> bool {
    if acquire_holder.is_some() {
        return false;
    }
    match prior {
        None => true,
        Some(p) => now.saturating_duration_since(p.observed_at) >= ttl,
    }
}

/// What the supervisor does with one `Held` acquire reply. Pure — the decision
/// is here so it is testable; the supervisor keeps only the I/O.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObservationPlan {
    /// Record this observation in the per-key cache, then serve it (the reply
    /// named the holder).
    Record(HolderObservation),
    /// Serve this observation and leave the cache as it is.
    Serve(HolderObservation),
    /// A `GET /coord/claims/by-resource` read is due; record its result.
    Lookup,
}

/// Decide how to observe a slot's holder on the `Held` arm.
///
/// * The reply named the holder → `Record` it.
/// * Else a read is due ([`holder_lookup_due`]: no prior, or the prior is at
///   least `ttl` old) → `Lookup`.
/// * Else the prior name came from an EARLIER acquire reply → `Serve` an
///   UNKNOWN (`acquire_unnamed`), never that earlier name: the holder may have
///   turned over. Nothing is recorded, so no extra read happens.
/// * Else → `Serve` the prior (a by-resource answer, or a failed read).
pub fn plan_observation(
    acquire_holder: Option<&SlotHolder>,
    prior: Option<&HolderObservation>,
    now: Instant,
    ttl: Duration,
) -> ObservationPlan {
    if let Some(h) = acquire_holder {
        return ObservationPlan::Record(HolderObservation::from_acquire(h.clone(), now));
    }
    match prior {
        Some(p) if !holder_lookup_due(None, Some(p), now, ttl) => {
            if p.source == HolderSource::Acquire {
                ObservationPlan::Serve(HolderObservation::unnamed_after_acquire(now))
            } else {
                ObservationPlan::Serve(p.clone())
            }
        }
        _ => ObservationPlan::Lookup,
    }
}

/// Parse a `/coord/claims/by-resource` body (coord `by_resource_body`,
/// `routes.rs:7164-7200`; `ClaimHolder`, `claims.rs:3393-3432`).
///
/// * `null` → `Ok(None)` — nothing holds the key.
/// * an object with a non-empty string `machine_id` → `Ok(Some(..))`, the
///   session from `session_id` (an empty or non-string session is `None`).
/// * anything else — including an empty `machine_id` → `Err`.
///
/// Stricter than `agent_worktree::fs_backstop::parse_by_resource_holder` on
/// purpose: that reader only needs "is it held by someone other than me?", so
/// it tolerates a degenerate holder; this one feeds an operator-facing NAME,
/// and an empty id rendered as a holder would be a fabricated answer. A shape
/// we do not recognise is an unknown, reported as such. There is no
/// `include_last_release` envelope branch: our request never sends that flag,
/// so coord never returns that shape (`routes.rs:7170-7175`).
pub fn parse_by_resource_body(v: &serde_json::Value) -> Result<Option<SlotHolder>, String> {
    if v.is_null() {
        return Ok(None);
    }
    let obj = v
        .as_object()
        .ok_or_else(|| format!("by-resource body is not an object or null: {v}"))?;
    let machine_id = obj
        .get("machine_id")
        .and_then(|m| m.as_str())
        .filter(|m| !m.is_empty())
        .ok_or_else(|| "by-resource body has no non-empty `machine_id`".to_string())?;
    let session = obj
        .get("session_id")
        .and_then(|s| s.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    Ok(Some(SlotHolder {
        machine_id: machine_id.to_string(),
        session,
    }))
}

impl HolderSource {
    /// The stable wire/log word for this source.
    pub fn as_str(&self) -> &'static str {
        match self {
            HolderSource::Acquire => "acquire",
            HolderSource::ByResource => "by_resource",
            HolderSource::LookupFailed(_) => "lookup_failed",
            HolderSource::NotHeldAtLookup => "not_held_at_lookup",
            HolderSource::AcquireUnnamed => "acquire_unnamed",
        }
    }
}

/// The operator-facing view of who holds a slot this runner could not take:
/// the supervisor's edge-triggered INFO line and `LoopingAgentStatus.slotHolder`.
///
/// `machine_id: None` is an UNKNOWN holder, and `source` (plus `lookup_error`
/// for a failed read) says why — never a fabricated id.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SlotHolderView {
    pub resource_key: String,
    pub machine_id: Option<String>,
    pub session: Option<String>,
    /// [`HolderSource::as_str`] of the observation this view was built from.
    pub source: &'static str,
    /// The holder's machine id is THIS runner's: a session-scoped token on this
    /// box holds the slot (our own token is bare-machine, so our own
    /// re-acquire answers `renewed`, never `held`). `None` when the holder is
    /// UNKNOWN — whose machine it is cannot be said either way.
    pub is_this_machine: Option<bool>,
    /// The [`HolderSource::LookupFailed`] detail `source` cannot carry.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lookup_error: Option<String>,
    /// Wall-clock time (Unix ms) of the observation this view was built from,
    /// so an operator can see its age. Not part of [`holder_changed`]'s key.
    pub observed_at_unix_ms: i64,
}

impl SlotHolderView {
    /// Build the view of one observation. `own_machine_id` is this runner's
    /// `SessionRegistry::machine_id()`; `observed_at_unix_ms` is the wall-clock
    /// equivalent of `obs.observed_at` (the caller owns the clock).
    pub fn from_observation(
        resource_key: &str,
        obs: &HolderObservation,
        own_machine_id: &str,
        observed_at_unix_ms: i64,
    ) -> Self {
        let machine_id = obs.holder.as_ref().map(|h| h.machine_id.clone());
        let is_this_machine = machine_id
            .as_deref()
            .map(|m| m.eq_ignore_ascii_case(own_machine_id));
        let lookup_error = match &obs.source {
            HolderSource::LookupFailed(e) => Some(e.clone()),
            _ => None,
        };
        Self {
            resource_key: resource_key.to_string(),
            machine_id,
            session: obs.holder.as_ref().and_then(|h| h.session.clone()),
            source: obs.source.as_str(),
            is_this_machine,
            lookup_error,
            observed_at_unix_ms,
        }
    }

    /// The `holder` field of the per-tick debug line: the machine id, or
    /// `unknown(<source>)` — e.g. `unknown(lookup_failed: coord status 503)` —
    /// so an unknown names why.
    pub fn holder_label(&self) -> String {
        match (&self.machine_id, &self.lookup_error) {
            (Some(m), _) => m.clone(),
            (None, Some(e)) => format!("unknown({}: {e})", self.source),
            (None, None) => format!("unknown({})", self.source),
        }
    }
}

/// Is `next` a different holder from `prev`? Compares on
/// `(resource_key, machine_id, session)` only: `source`, `lookup_error` and
/// `observed_at_unix_ms` are left out, so re-observing the same holder through a different door (or a
/// second failed read) is not a change and does not re-log at INFO.
pub fn holder_changed(prev: Option<&SlotHolderView>, next: &SlotHolderView) -> bool {
    match prev {
        None => true,
        Some(p) => {
            (&p.resource_key, &p.machine_id, &p.session)
                != (&next.resource_key, &next.machine_id, &next.session)
        }
    }
}

/// The last-logged holder view per slot resource key, for ONE agent.
pub type HolderViewsByKey = HashMap<String, SlotHolderView>;

/// Record `view` as the last-logged holder of its resource key and return
/// whether it is a CHANGE for that key (the INFO edge).
///
/// Keyed per resource key, not per agent: an agent walking several held slots
/// in one tick must not compare slot 1's holder against slot 0's and log a
/// "change" on every tick.
pub fn record_holder_view(last_by_key: &mut HolderViewsByKey, view: &SlotHolderView) -> bool {
    let changed = holder_changed(last_by_key.get(&view.resource_key), view);
    last_by_key.insert(view.resource_key.clone(), view.clone());
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(machine_id: &str) -> SlotHolder {
        SlotHolder {
            machine_id: machine_id.to_string(),
            session: None,
        }
    }

    const TTL: Duration = Duration::from_secs(120);

    // -- holder_lookup_due ---------------------------------------------------

    #[test]
    fn lookup_is_due_on_the_first_held_with_no_acquire_holder() {
        assert!(holder_lookup_due(None, None, Instant::now(), TTL));
    }

    #[test]
    fn lookup_is_not_due_when_the_acquire_reply_named_the_holder() {
        let now = Instant::now();
        assert!(!holder_lookup_due(Some(&h("m")), None, now, TTL));
        // Even with a stale prior: the acquire answer is fresher than any read.
        let stale = HolderObservation::from_lookup(Err("x".into()), now);
        assert!(!holder_lookup_due(
            Some(&h("m")),
            Some(&stale),
            now + TTL * 2,
            TTL
        ));
    }

    #[test]
    fn lookup_is_not_due_within_ttl_after_a_success() {
        let t0 = Instant::now();
        let prior = HolderObservation::from_lookup(Ok(Some(h("m"))), t0);
        assert!(!holder_lookup_due(
            None,
            Some(&prior),
            t0 + Duration::from_secs(5),
            TTL
        ));
        assert!(!holder_lookup_due(
            None,
            Some(&prior),
            t0 + TTL - Duration::from_millis(1),
            TTL
        ));
    }

    /// A down coord must cost one retry per TTL, not one per 5 s tick. A
    /// mutation inverting the TTL comparison turns this red.
    #[test]
    fn lookup_is_not_due_within_ttl_after_a_failed_lookup() {
        let t0 = Instant::now();
        let prior = HolderObservation::from_lookup(Err("coord status 503".into()), t0);
        assert_eq!(
            prior.source,
            HolderSource::LookupFailed("coord status 503".into())
        );
        for secs in [0, 5, 10, 60, 119] {
            assert!(
                !holder_lookup_due(None, Some(&prior), t0 + Duration::from_secs(secs), TTL),
                "a failed lookup must not be retried {secs}s later (retry storm)"
            );
        }
    }

    #[test]
    fn lookup_is_due_again_at_exactly_ttl() {
        let t0 = Instant::now();
        let prior = HolderObservation::from_lookup(Ok(None), t0);
        assert!(holder_lookup_due(None, Some(&prior), t0 + TTL, TTL));
        assert!(holder_lookup_due(
            None,
            Some(&prior),
            t0 + TTL + Duration::from_secs(1),
            TTL
        ));
    }

    #[test]
    fn observation_ttl_is_the_lease_ttl() {
        assert_eq!(
            holder_observation_ttl(),
            Duration::from_secs(LEASE_TTL_SECS.unsigned_abs()),
            "one by-resource read per LEASE TTL — derived, never a second constant"
        );
    }

    #[test]
    fn lookup_results_fold_to_their_sources() {
        let now = Instant::now();
        let o = HolderObservation::from_lookup(Ok(Some(h("m"))), now);
        assert_eq!(
            (o.holder, o.source),
            (Some(h("m")), HolderSource::ByResource)
        );
        let o = HolderObservation::from_lookup(Ok(None), now);
        assert_eq!((o.holder, o.source), (None, HolderSource::NotHeldAtLookup));
        let o = HolderObservation::from_lookup(Err("boom".into()), now);
        assert_eq!(
            (o.holder, o.source),
            (None, HolderSource::LookupFailed("boom".into()))
        );
        let o = HolderObservation::from_acquire(h("m"), now);
        assert_eq!((o.holder, o.source), (Some(h("m")), HolderSource::Acquire));
    }

    // -- parse_by_resource_body ----------------------------------------------

    #[test]
    fn by_resource_ground_truth_body_names_the_holder() {
        let v: serde_json::Value = serde_json::from_str(
            r#"{"kind":"semantic_resource","machine_id":"3e7e4b04-75de-4efb-b718-c8ce8fcf7b17","resource_key":"role:c231d9da-0ca8-4fe4-bd81-0e3d6c20339a:merge_shepherd:0","tenant_id":"c231d9da-0ca8-4fe4-bd81-0e3d6c20339a","ttl_seconds":118}"#,
        )
        .unwrap();
        assert_eq!(
            parse_by_resource_body(&v),
            Ok(Some(h("3e7e4b04-75de-4efb-b718-c8ce8fcf7b17")))
        );
    }

    #[test]
    fn by_resource_body_with_a_session_carries_it() {
        let v: serde_json::Value = serde_json::from_str(
            r#"{"kind":"semantic_resource","machine_id":"m-1","session_id":"s-1","resource_key":"role:t:merge_shepherd:0","ttl_seconds":30}"#,
        )
        .unwrap();
        assert_eq!(
            parse_by_resource_body(&v),
            Ok(Some(SlotHolder {
                machine_id: "m-1".into(),
                session: Some("s-1".into()),
            }))
        );
    }

    #[test]
    fn by_resource_null_is_not_held() {
        assert_eq!(parse_by_resource_body(&serde_json::Value::Null), Ok(None));
    }

    #[test]
    fn by_resource_empty_machine_id_is_an_error_never_some_empty() {
        let v: serde_json::Value = serde_json::from_str(
            r#"{"kind":"semantic_resource","machine_id":"","resource_key":"k","ttl_seconds":5}"#,
        )
        .unwrap();
        assert!(parse_by_resource_body(&v).is_err());
        let v: serde_json::Value = serde_json::from_str(r#"{"kind":"semantic_resource"}"#).unwrap();
        assert!(parse_by_resource_body(&v).is_err());
    }

    #[test]
    fn by_resource_non_object_is_an_error() {
        for body in [r#""m-1""#, r#"[{"machine_id":"m-1"}]"#, "42", "true"] {
            let v: serde_json::Value = serde_json::from_str(body).unwrap();
            assert!(
                parse_by_resource_body(&v).is_err(),
                "{body} must be an error"
            );
        }
    }

    // -- URL builder -----------------------------------------------------------

    #[test]
    fn lookup_url_uses_the_coord_prefixed_read_path() {
        let url = by_resource_url("https://coord.example.com");
        assert_eq!(url, "https://coord.example.com/coord/claims/by-resource");
        // A trailing slash on the base does not produce `//coord`.
        assert_eq!(
            by_resource_url("http://127.0.0.1:8080/"),
            "http://127.0.0.1:8080/coord/claims/by-resource"
        );
    }

    // -- SlotHolderView / holder_changed (Phase 3) ---------------------------

    const KEY: &str = "role:c231d9da-0ca8-4fe4-bd81-0e3d6c20339a:merge_shepherd:0";
    const OWN: &str = "c79a07d5-0000-4000-8000-000000000001";
    const PEER: &str = "3e7e4b04-75de-4efb-b718-c8ce8fcf7b17";
    const OBSERVED_MS: i64 = 1_793_000_000_000;

    fn view_of(holder: Option<SlotHolder>, source: HolderSource) -> SlotHolderView {
        let obs = HolderObservation {
            holder,
            source,
            observed_at: Instant::now(),
        };
        SlotHolderView::from_observation(KEY, &obs, OWN, OBSERVED_MS)
    }

    fn with_session(machine_id: &str, session: &str) -> SlotHolder {
        SlotHolder {
            machine_id: machine_id.to_string(),
            session: Some(session.to_string()),
        }
    }

    /// A mutation making `holder_changed` always true turns this red.
    #[test]
    fn the_same_holder_twice_is_not_a_change() {
        let a = view_of(Some(h(PEER)), HolderSource::Acquire);
        let b = view_of(Some(h(PEER)), HolderSource::Acquire);
        assert!(!holder_changed(Some(&a), &b));
        // Re-observed through the other door: still the same holder.
        let c = view_of(Some(h(PEER)), HolderSource::ByResource);
        assert!(!holder_changed(Some(&a), &c));
    }

    #[test]
    fn a_first_observation_is_a_change() {
        assert!(holder_changed(
            None,
            &view_of(Some(h(PEER)), HolderSource::Acquire)
        ));
    }

    #[test]
    fn a_different_machine_is_a_change() {
        let a = view_of(Some(h(PEER)), HolderSource::Acquire);
        let b = view_of(Some(h("another-machine")), HolderSource::Acquire);
        assert!(holder_changed(Some(&a), &b));
    }

    #[test]
    fn the_same_machine_with_a_different_session_is_a_change() {
        let a = view_of(Some(with_session(PEER, "s-1")), HolderSource::Acquire);
        let b = view_of(Some(with_session(PEER, "s-2")), HolderSource::Acquire);
        assert!(holder_changed(Some(&a), &b));
        let c = view_of(Some(h(PEER)), HolderSource::Acquire);
        assert!(holder_changed(Some(&a), &c));
    }

    #[test]
    fn unknown_to_known_is_a_change() {
        let a = view_of(None, HolderSource::LookupFailed("coord status 503".into()));
        let b = view_of(Some(h(PEER)), HolderSource::ByResource);
        assert!(holder_changed(Some(&a), &b));
        assert!(holder_changed(Some(&b), &a), "known to unknown too");
    }

    #[test]
    fn the_same_unknown_source_twice_is_not_a_change() {
        let a = view_of(None, HolderSource::LookupFailed("coord status 503".into()));
        let b = view_of(None, HolderSource::LookupFailed("coord status 503".into()));
        assert!(!holder_changed(Some(&a), &b));
        let a = view_of(None, HolderSource::NotHeldAtLookup);
        let b = view_of(None, HolderSource::NotHeldAtLookup);
        assert!(!holder_changed(Some(&a), &b));
    }

    #[test]
    fn a_different_resource_key_is_a_change() {
        let a = view_of(Some(h(PEER)), HolderSource::Acquire);
        let mut b = a.clone();
        b.resource_key = "role:t:merge_shepherd:1".into();
        assert!(holder_changed(Some(&a), &b));
    }

    #[test]
    fn holder_label_names_the_machine_or_why_it_is_unknown() {
        assert_eq!(
            view_of(Some(h(PEER)), HolderSource::Acquire).holder_label(),
            PEER
        );
        assert_eq!(
            view_of(None, HolderSource::LookupFailed("coord status 503".into())).holder_label(),
            "unknown(lookup_failed: coord status 503)"
        );
        assert_eq!(
            view_of(None, HolderSource::NotHeldAtLookup).holder_label(),
            "unknown(not_held_at_lookup)"
        );
    }

    #[test]
    fn is_this_machine_flags_a_hold_by_our_own_machine_id() {
        assert_eq!(
            view_of(Some(h(PEER)), HolderSource::Acquire).is_this_machine,
            Some(false)
        );
        assert_eq!(
            view_of(Some(with_session(OWN, "s-other")), HolderSource::Acquire).is_this_machine,
            Some(true)
        );
        assert_eq!(
            view_of(None, HolderSource::NotHeldAtLookup).is_this_machine,
            None,
            "an unknown holder is neither this machine nor another"
        );
        assert_eq!(
            view_of(None, HolderSource::AcquireUnnamed).is_this_machine,
            None
        );
    }

    /// The vanish race: an acquire reply naming nobody after an earlier acquire
    /// named someone serves UNKNOWN — never the earlier name — records nothing
    /// (so the cached prior keeps the one-read-per-TTL clock), and costs no read.
    #[test]
    fn an_unnamed_acquire_after_a_named_one_serves_unknown_not_the_old_name() {
        let t0 = Instant::now();
        let named = HolderObservation::from_acquire(h(PEER), t0);
        let later = t0 + Duration::from_secs(5);
        assert_eq!(
            plan_observation(None, Some(&named), later, TTL),
            ObservationPlan::Serve(HolderObservation::unnamed_after_acquire(later))
        );
        let v = view_of(None, HolderSource::AcquireUnnamed);
        assert_eq!(v.holder_label(), "unknown(acquire_unnamed)");
        // Past the TTL the read is due again, as for any prior.
        assert_eq!(
            plan_observation(None, Some(&named), t0 + TTL, TTL),
            ObservationPlan::Lookup
        );
    }

    #[test]
    fn plan_observation_records_a_named_reply_and_looks_up_or_reuses_otherwise() {
        let t0 = Instant::now();
        let later = t0 + Duration::from_secs(5);
        // A named reply is recorded whatever the prior says.
        let looked = HolderObservation::from_lookup(Ok(Some(h("m"))), t0);
        assert_eq!(
            plan_observation(Some(&h(PEER)), Some(&looked), later, TTL),
            ObservationPlan::Record(HolderObservation::from_acquire(h(PEER), later))
        );
        // No prior: a read is due.
        assert_eq!(
            plan_observation(None, None, t0, TTL),
            ObservationPlan::Lookup
        );
        // A by-resource answer or a failed read inside the TTL is reused as is.
        assert_eq!(
            plan_observation(None, Some(&looked), later, TTL),
            ObservationPlan::Serve(looked.clone())
        );
        let failed = HolderObservation::from_lookup(Err("coord status 503".into()), t0);
        assert_eq!(
            plan_observation(None, Some(&failed), later, TTL),
            ObservationPlan::Serve(failed.clone())
        );
        // Past the TTL each is due again.
        assert_eq!(
            plan_observation(None, Some(&failed), t0 + TTL, TTL),
            ObservationPlan::Lookup
        );
    }

    /// `LoopingAgentStatus.slotHolder` serializes this view as-is; the keys are
    /// the operator-facing contract.
    #[test]
    fn slot_holder_view_serializes_camel_case() {
        let known = view_of(Some(with_session(PEER, "s-1")), HolderSource::Acquire);
        assert_eq!(
            serde_json::to_value(&known).unwrap(),
            serde_json::json!({
                "resourceKey": "role:c231d9da-0ca8-4fe4-bd81-0e3d6c20339a:merge_shepherd:0",
                "machineId": "3e7e4b04-75de-4efb-b718-c8ce8fcf7b17",
                "session": "s-1",
                "source": "acquire",
                "isThisMachine": false,
                "observedAtUnixMs": 1_793_000_000_000_i64
            })
        );
        let failed = view_of(None, HolderSource::LookupFailed("coord status 503".into()));
        assert_eq!(
            serde_json::to_value(&failed).unwrap(),
            serde_json::json!({
                "resourceKey": "role:c231d9da-0ca8-4fe4-bd81-0e3d6c20339a:merge_shepherd:0",
                "machineId": null,
                "session": null,
                "source": "lookup_failed",
                "isThisMachine": null,
                "lookupError": "coord status 503",
                "observedAtUnixMs": 1_793_000_000_000_i64
            })
        );
    }

    #[test]
    fn observation_time_is_not_part_of_the_change_key() {
        let a = view_of(Some(h(PEER)), HolderSource::Acquire);
        let mut b = a.clone();
        b.observed_at_unix_ms += 120_000;
        assert!(!holder_changed(Some(&a), &b));
    }

    /// Review round 1, MAJOR: with several held slots, the per-agent memo used
    /// to compare slot 1's holder against slot 0's and log a "change" every
    /// tick. Per-key edge detection: two held keys walked on two ticks log
    /// exactly one change each, on the first tick only.
    #[test]
    fn alternating_held_keys_across_ticks_change_only_once_per_key() {
        let key0 = "role:t:merge_shepherd:0";
        let key1 = "role:t:merge_shepherd:1";
        let view = |key: &str, machine: &str| {
            let obs = HolderObservation::from_acquire(h(machine), Instant::now());
            SlotHolderView::from_observation(key, &obs, OWN, OBSERVED_MS)
        };
        let mut last = HolderViewsByKey::new();
        // Tick 1: both keys are first observations.
        assert!(record_holder_view(&mut last, &view(key0, "machine-a")));
        assert!(record_holder_view(&mut last, &view(key1, "machine-b")));
        // Tick 2 (and 3): same holders, same walk order → no change, no INFO.
        for _ in 0..2 {
            assert!(!record_holder_view(&mut last, &view(key0, "machine-a")));
            assert!(!record_holder_view(&mut last, &view(key1, "machine-b")));
        }
        // A real change on one key is still an edge, and only for that key.
        assert!(record_holder_view(&mut last, &view(key1, "machine-c")));
        assert!(!record_holder_view(&mut last, &view(key0, "machine-a")));
    }
}
