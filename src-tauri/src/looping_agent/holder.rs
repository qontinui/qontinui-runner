//! Who holds a slot lease that is not ours — the PURE core of the supervisor's
//! holder observation (plan
//! `2026-10-30-looping-agent-slot-holder-is-never-named`, Phases 2-3).
//!
//! The acquire reply names the holder on every live `held` once
//! [`super::lease::parse_acquire_body`] reads the reserve door's `holder` key.
//! This module is the bounded guard for the shapes that parse cannot name: a
//! coord build that stops emitting the holder, or the empty-string vanish race.
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

/// How long one holder observation stays fresh: the lease TTL. A holder cannot
/// change more often than its lease turns over, so re-reading inside one TTL
/// buys nothing.
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
    /// looked (the lease turned over between the acquire and the read).
    NotHeldAtLookup,
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
        assert_eq!(holder_observation_ttl(), Duration::from_secs(120));
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
        assert!(url.ends_with("/coord/claims/by-resource"));
        // A trailing slash on the base does not produce `//coord`.
        assert_eq!(
            by_resource_url("http://127.0.0.1:8080/"),
            "http://127.0.0.1:8080/coord/claims/by-resource"
        );
    }
}
