//! One classifier for the only question a retry loop actually asks: **did the
//! server evaluate this request and refuse it, or did the transport blip?**
//!
//! > A rejection a server issues for a *structural* reason is terminal for that
//! > exact request, not retryable. Re-sending a byte-identical request against
//! > unchanged server state cannot produce a different verdict, so sending it
//! > again is not a retry — it is a duplicate.
//!
//! ## Why this module exists in the LIB crate
//!
//! [`PostDisposition`] and [`result_post_disposition`] were written for, and
//! were sole-sited in, the BIN crate's `ci_node::reporting` (the CI
//! dispatch-result POST). `plan_workunit_adapter` needs exactly the same
//! judgement on its own coord writes — its `Err` arm formatted the HTTP status
//! into a string and threw it away, so a `422` structural rejection and a `502`
//! transport failure were indistinguishable downstream — and it lives in the
//! LIB crate, which cannot import from the runner bin's module tree.
//!
//! So the classifier **moved here**; it was not copied. There is exactly ONE
//! definition, for the same reason `fs_atomic`, `instance_env` and
//! `machine_identity` are lib modules: two consumers in two crates must not be
//! able to drift on the same judgement. The bin reaches it as
//! `qontinui_runner_lib::http_disposition`.
//!
//! ## The carve-out, which is not negotiable
//!
//! **"Structural" is NOT "4xx".** On 2026-08-29 a 40-minute burst produced
//! **4,268 consecutive `401 {"error":"invalid token"}` failures** across every
//! work unit on one device (~6,400/hour, 4.3× the steady-state failure rate),
//! which **self-cleared** the moment the runner's device JWT was minted. A naive
//! "a 4xx is terminal" rule would have permanently tombstoned all 424 units for
//! the whole process lifetime — converting a self-healing credential warm-up
//! into a silent, total, indefinite sync outage, i.e. a failure mode
//! categorically worse than the one being fixed.
//!
//! `401 | 408 | 429` are therefore [`PostDisposition::Retry`], and
//! [`carve_out_401_408_429_stay_retryable_even_with_a_denial_body`] pins it.
//! `agent_pusher`'s `push_401_is_transient_because_token_refreshes_per_tick`
//! says the same thing about the same bearer; this module must not contradict
//! it.
//!
//! ## Denial tags — read off the answer, never guessed from a local table
//!
//! coord's write-denial sites emit `{"error": "<code>", "message": "…"}`. This
//! module reads that code off the **response** ([`DenialTag`]) rather than
//! vendoring coord's status vocabulary client-side. A vendored copy was
//! rejected with evidence: the proposed `Derived = shipped` table was already
//! wrong on the day it was written (coord's `transition_class()` is
//! `Ready | Shipped`, and 58 live ``status `ready` is derived`` rejections were
//! measured in one 3.73 h window), and coord's crate is binary-only so no copy
//! could ever be a re-export. Reading the answer also covers denial codes this
//! build has never seen — including any coord adds tomorrow — which a status
//! table cannot represent at all.
//!
//! An unrecognised code is carried **verbatim** as [`DenialTag::Unrecognized`]
//! and is still `GiveUp`. It is never silently folded into one of the four
//! known codes, because a caller keying a terminal store on the tag must be able
//! to tell "coord refused this for a reason I understand" from "coord refused
//! this for a reason this build has never heard of".
//!
//! ## `terminality` — the retry semantics, straight from the answer
//!
//! Beside that code coord also sends `{"terminality": "<wire>"}`
//! ([`DenialTerminality`]), naming what would have to CHANGE before a retry
//! could work. It is a closed three-element vocabulary and it exists for this
//! exact consumer: coord's own doc block says the class it prevents is *"a new
//! denial arriving with no retry semantics, which is exactly how the adapter's
//! futile-retry loop became possible in the first place."*
//!
//! Only `permanent` licenses retiring a key — [`Verdict::is_permanently_denied`]
//! — and **an absent, unparseable or unrecognised hint is UNKNOWN, which
//! retries**. That default is what keeps a runner talking to a coord older than
//! the hint on its pre-change behaviour instead of silently freezing every unit,
//! and it is the same fail-soft direction as the 401 carve-out above.

/// What to do after one HTTP write attempt. Pure over the observed status so
/// the policy is unit-testable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PostDisposition {
    /// 2xx — the server recorded it.
    Done,
    /// `409` — the server already holds a terminal/conflicting state for this
    /// key (in `ci_node`, a `dispatch_terminal`: a sweep marked the dispatch
    /// `lost`, or a duplicate arrived with a different conclusion). The
    /// server's ledger stands; retrying can never succeed.
    TerminalConflict,
    /// A non-retryable client error (`400` bad request, `403` denied, `404`
    /// unknown key, `422` structurally unprocessable) — **re-sending the same
    /// body cannot heal it.** Where the server named its denial, the tag is
    /// carried in [`Verdict::denial`].
    GiveUp,
    /// Network failure, 5xx, or an auth-refresh-shaped status
    /// (`401`/`408`/`429`) — something OUTSIDE the request will change on its
    /// own, so retry on the schedule. See the module header's carve-out.
    Retry,
}

/// A machine-readable denial code the server put in its response body, for the
/// [`PostDisposition::GiveUp`] arm.
///
/// The four named variants are coord's `TransitionDenied::error_code()` values
/// (`crates/coord/src/work_unit_registry.rs`). They differ in **what would
/// invalidate them**, which is why they are not collapsed into one:
///
/// | tag | terminal on | invalidated by |
/// |---|---|---|
/// | `status_is_derived` | `(slug, status)` | the file's status changing — `shipped`/`ready` never become settable |
/// | `self_attestation_forbidden` | `(slug, status, actor)` | a graduation flip, or a token-shape change |
/// | `owner_unresolved` | `(slug, status)` | *any* actor writing a Free status to that unit (out-of-band) |
/// | `attester_unresolved` | `(slug, status, actor)` | a device-scoped token |
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DenialTag {
    /// The target status is coord-computed from a predicate and settable by no
    /// identity.
    StatusIsDerived,
    /// Separation of duties: the attester identity equals the unit's recorded
    /// owner.
    SelfAttestationForbidden,
    /// The unit has no recorded owner, so separation of duties cannot be
    /// evaluated.
    OwnerUnresolved,
    /// The attester identity could not be resolved from the presented token.
    AttesterUnresolved,
    /// A denial code this build does not recognise, carried **verbatim**.
    ///
    /// Reaching this variant is the normal, expected outcome for any code coord
    /// adds after this build shipped. It stays `GiveUp` — the server evaluated
    /// the request and said no — but a caller keying a terminal store must
    /// treat it as "reason unknown to me" rather than guessing which of the
    /// four known classes it resembles.
    Unrecognized(String),
}

impl DenialTag {
    /// Map a wire code onto a tag. Anything unknown is preserved, never
    /// bucketed.
    pub fn from_code(code: &str) -> Self {
        match code {
            "status_is_derived" => Self::StatusIsDerived,
            "self_attestation_forbidden" => Self::SelfAttestationForbidden,
            "owner_unresolved" => Self::OwnerUnresolved,
            "attester_unresolved" => Self::AttesterUnresolved,
            other => Self::Unrecognized(other.to_string()),
        }
    }

    /// The wire code, round-tripping [`DenialTag::from_code`].
    pub fn as_code(&self) -> &str {
        match self {
            Self::StatusIsDerived => "status_is_derived",
            Self::SelfAttestationForbidden => "self_attestation_forbidden",
            Self::OwnerUnresolved => "owner_unresolved",
            Self::AttesterUnresolved => "attester_unresolved",
            Self::Unrecognized(code) => code,
        }
    }

    /// False for a code this build has never heard of. A caller that keys
    /// behaviour on the *specific* denial must consult this first.
    pub fn is_recognized(&self) -> bool {
        !matches!(self, Self::Unrecognized(_))
    }
}

impl std::fmt::Display for DenialTag {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_code())
    }
}

/// **What would have to change before a retry could work** — coord's own
/// `terminality` hint, read off the denial body beside the [`DenialTag`].
///
/// The wire vocabulary is a CLOSED three-element set
/// (`crates/coord/src/work_unit_registry.rs`, `DenialTerminality::as_wire`,
/// `origin/main`) and coord emits it on every work-unit write denial. Its doc
/// block names the motive outright: *"the class this signal exists to prevent is
/// a new denial arriving with no retry semantics, which is exactly how the
/// adapter's futile-retry loop became possible in the first place."*
///
/// coord deliberately ships no `from_wire` of its own — *"coord is the SERVER
/// and never parses its own hint back … The parse belongs in whichever client
/// acts on it."* This is that client, and this is that parse.
///
/// **Only [`DenialTerminality::Permanent`] is unconditionally terminal.** The
/// other two are terminal for a NARROWER key and clear out of band: an
/// `actor_dependent` denial is invalidated by a different actor, a token-shape
/// change, or a graduation flip; a `state_dependent` one by any actor writing a
/// Free status to the unit. Retiring on either would suppress a denial a later
/// cycle could legitimately clear.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DenialTerminality {
    /// Permanently unsatisfiable for this `(slug, status)` — nothing
    /// invalidates it. The only value a caller may retire on.
    Permanent,
    /// Terminal for this `(slug, status, actor)`; a different actor or token
    /// shape can clear it. **Retryable.**
    ActorDependent,
    /// Terminal for this `(slug, status)` until the unit's own recorded state
    /// changes, which happens out of band. **Retryable.**
    StateDependent,
}

impl DenialTerminality {
    /// Parse the wire string. **Byte-exact against the closed set**; anything
    /// else — including a value a future coord adds — is `None`, i.e. UNKNOWN,
    /// which every caller must treat as retryable. That is both the
    /// pre-`terminality` behaviour and the correct default for this runner
    /// talking to an older coord that sends no hint at all.
    pub fn from_wire(wire: &str) -> Option<Self> {
        match wire {
            "permanent" => Some(Self::Permanent),
            "actor_dependent" => Some(Self::ActorDependent),
            "state_dependent" => Some(Self::StateDependent),
            _ => None,
        }
    }

    /// The wire string, round-tripping [`DenialTerminality::from_wire`].
    pub fn as_wire(self) -> &'static str {
        match self {
            Self::Permanent => "permanent",
            Self::ActorDependent => "actor_dependent",
            Self::StateDependent => "state_dependent",
        }
    }
}

impl std::fmt::Display for DenialTerminality {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_wire())
    }
}

/// The full classification of one response: what to do, and — when the server
/// denied it structurally — which denial it was.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    /// What the retry loop should do.
    pub disposition: PostDisposition,
    /// The server's own denial code, present only on the
    /// [`PostDisposition::GiveUp`] arm. `None` on every other arm **by
    /// construction** — a `401 {"error":"invalid token"}` carries an `error`
    /// field too, and tagging it would invite a caller to treat the 401 burst
    /// as a structural denial.
    pub denial: Option<DenialTag>,
    /// coord's `terminality` hint for this denial, read on the
    /// [`PostDisposition::GiveUp`] arm only — for exactly the reason `denial`
    /// is: a retryable status must never come back carrying a terminal-looking
    /// field.
    ///
    /// `None` means UNKNOWN — coord sent no hint (an older build), sent an
    /// unparseable body, or sent a word this build does not recognise. **UNKNOWN
    /// is retryable**; see [`Verdict::is_permanently_denied`].
    pub terminality: Option<DenialTerminality>,
}

impl Verdict {
    /// True iff re-sending this exact request later could plausibly succeed.
    pub fn is_retryable(&self) -> bool {
        matches!(self.disposition, PostDisposition::Retry)
    }

    /// True iff the server evaluated the request and refused it — the class a
    /// byte-identical retry can never satisfy.
    pub fn is_structural(&self) -> bool {
        matches!(
            self.disposition,
            PostDisposition::GiveUp | PostDisposition::TerminalConflict
        )
    }

    /// True **only** when the server said `terminality: permanent` — the one
    /// value a caller may retire a key on for the life of the process.
    ///
    /// Deliberately narrower than [`Verdict::is_structural`]: a `422` is
    /// structural for the request as sent, but only `permanent` says nothing at
    /// all can ever make it succeed. Absent, unrecognised, `actor_dependent` and
    /// `state_dependent` all answer `false` — UNKNOWN retries.
    pub fn is_permanently_denied(&self) -> bool {
        matches!(self.terminality, Some(DenialTerminality::Permanent))
    }
}

/// Classify one HTTP response. `status` is `None` when there was no HTTP status
/// at all (a transport/network failure), which is always retryable.
///
/// `body` is the response body verbatim; an empty string is fine and simply
/// yields no [`DenialTag`]. Shaped after
/// [`crate::cognito::classify_token_error`], the tree's established
/// `(status, body)` classifier idiom.
pub fn classify(status: Option<u16>, body: &str) -> Verdict {
    let disposition = disposition_of(status);
    // The tag is read ONLY on the GiveUp arm. A retryable status that happens
    // to carry an `error` field — the 401 burst's `{"error":"invalid token"}`
    // is exactly that shape — must not come back looking like a structural
    // denial.
    let (denial, terminality) = match disposition {
        PostDisposition::GiveUp => (denial_tag(body), terminality_of(body)),
        _ => (None, None),
    };
    Verdict {
        disposition,
        denial,
        terminality,
    }
}

/// The status-only half of [`classify`], unchanged from the shipped
/// `ci_node::reporting` original.
fn disposition_of(status: Option<u16>) -> PostDisposition {
    match status {
        Some(s) if (200..300).contains(&s) => PostDisposition::Done,
        Some(409) => PostDisposition::TerminalConflict,
        // 401 can be a token freshly reminted mid-flight; 408/429 are
        // explicitly transient. Everything else in 4xx is a contract error
        // that a byte-identical retry cannot fix.
        Some(401) | Some(408) | Some(429) => PostDisposition::Retry,
        Some(s) if (400..500).contains(&s) => PostDisposition::GiveUp,
        _ => PostDisposition::Retry,
    }
}

/// Status-only classification, for callers whose route carries no
/// machine-readable denial code (`ci_node`'s dispatch-result POST).
///
/// Exactly [`classify`] with an empty body — NOT a second classifier.
pub fn result_post_disposition(status: Option<u16>) -> PostDisposition {
    classify(status, "").disposition
}

/// Pull `{"error": "<code>"}` out of a denial body. `None` when the body is not
/// JSON, carries no `error` string, or carries an empty one — all of which mean
/// "the server denied it but named no code", which is still `GiveUp`.
fn denial_tag(body: &str) -> Option<DenialTag> {
    let parsed: serde_json::Value = serde_json::from_str(body).ok()?;
    let code = parsed.get("error")?.as_str()?.trim();
    if code.is_empty() {
        return None;
    }
    Some(DenialTag::from_code(code))
}

/// Pull `{"terminality": "<wire>"}` out of a denial body.
///
/// `None` — UNKNOWN, therefore retryable — when the body is not JSON, carries no
/// `terminality` string, or carries a word outside coord's closed set. **Fail
/// soft is the whole contract here:** a body that does not parse is not
/// `permanent`, and a runner talking to a coord that predates the hint must keep
/// its pre-change retry behaviour rather than silently freezing every unit.
fn terminality_of(body: &str) -> Option<DenialTerminality> {
    let parsed: serde_json::Value = serde_json::from_str(body).ok()?;
    let wire = parsed.get("terminality")?.as_str()?;
    DenialTerminality::from_wire(wire)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The status-only policy, byte-for-byte the shipped `ci_node` behaviour.
    /// `ci_node::reporting`'s own `result_disposition_policy` test asserts the
    /// same thing through the moved function and is NOT edited by the lift.
    #[test]
    fn status_only_policy_is_unchanged_by_the_lift() {
        assert_eq!(result_post_disposition(Some(200)), PostDisposition::Done);
        assert_eq!(
            result_post_disposition(Some(409)),
            PostDisposition::TerminalConflict
        );
        for s in [400, 403, 404, 422] {
            assert_eq!(result_post_disposition(Some(s)), PostDisposition::GiveUp);
        }
        for s in [401, 408, 429, 500, 502, 503, 504] {
            assert_eq!(result_post_disposition(Some(s)), PostDisposition::Retry);
        }
        assert_eq!(result_post_disposition(None), PostDisposition::Retry);
    }

    /// **The single most important assertion in this module.** A 40-minute
    /// burst of 4,268 `401 {"error":"invalid token"}` failures self-cleared when
    /// the device JWT was minted. Marking any of these terminal would have
    /// tombstoned every work unit on the device for the whole process lifetime.
    #[test]
    fn carve_out_401_408_429_stay_retryable_even_with_a_denial_body() {
        // The exact body coord returned throughout the 2026-08-29 burst.
        let burst = classify(Some(401), r#"{"error":"invalid token"}"#);
        assert_eq!(burst.disposition, PostDisposition::Retry);
        assert!(burst.is_retryable());
        assert!(!burst.is_structural());
        // And no tag: an `error` field on a retryable status must not come back
        // looking like a structural denial.
        assert_eq!(burst.denial, None);

        for s in [408, 429] {
            let v = classify(Some(s), r#"{"error":"status_is_derived"}"#);
            assert_eq!(
                v.disposition,
                PostDisposition::Retry,
                "{s} must stay retryable regardless of body"
            );
            assert_eq!(v.denial, None, "{s} must carry no denial tag");
        }

        // Transport failure and 5xx: nothing in the request changed, so retry.
        assert!(classify(None, "").is_retryable());
        assert!(classify(Some(502), "<html>bad gateway</html>").is_retryable());
    }

    /// coord's four `TransitionDenied::error_code()` values, verbatim from the
    /// measured log lines.
    #[test]
    fn coord_denial_tags_layer_onto_giveup() {
        let derived = classify(
            Some(422),
            r#"{"error":"status_is_derived","message":"status `shipped` is derived (coord-computed from a predicate), not directly settable"}"#,
        );
        assert_eq!(derived.disposition, PostDisposition::GiveUp);
        assert_eq!(derived.denial, Some(DenialTag::StatusIsDerived));
        assert!(derived.is_structural());

        let self_attest = classify(
            Some(403),
            r#"{"error":"self_attestation_forbidden","message":"an actor may not attest its own work-unit (separation of duties)"}"#,
        );
        assert_eq!(
            self_attest.denial,
            Some(DenialTag::SelfAttestationForbidden)
        );

        let owner = classify(Some(403), r#"{"error":"owner_unresolved","message":"…"}"#);
        assert_eq!(owner.denial, Some(DenialTag::OwnerUnresolved));

        // Never observed on this device; covered by the rule, not by a table.
        let attester = classify(Some(403), r#"{"error":"attester_unresolved"}"#);
        assert_eq!(attester.denial, Some(DenialTag::AttesterUnresolved));

        for v in [derived, self_attest, owner, attester] {
            assert!(v.denial.is_some_and(|d| d.is_recognized()));
        }
    }

    /// A denial code coord adds after this build shipped must be carried
    /// verbatim, not bucketed into one of the four we happen to know.
    #[test]
    fn an_unknown_denial_code_is_giveup_but_never_mis_bucketed() {
        let v = classify(Some(403), r#"{"error":"tenant_quota_exhausted"}"#);
        assert_eq!(v.disposition, PostDisposition::GiveUp);
        assert_eq!(
            v.denial,
            Some(DenialTag::Unrecognized("tenant_quota_exhausted".into()))
        );
        let tag = v.denial.unwrap();
        assert!(!tag.is_recognized());
        assert_eq!(tag.as_code(), "tenant_quota_exhausted");
        // Explicitly NOT any of the four known tags.
        for known in [
            DenialTag::StatusIsDerived,
            DenialTag::SelfAttestationForbidden,
            DenialTag::OwnerUnresolved,
            DenialTag::AttesterUnresolved,
        ] {
            assert_ne!(tag, known);
        }
    }

    /// A denial with no machine-readable code is still a denial.
    #[test]
    fn a_body_with_no_error_code_is_still_giveup_with_no_tag() {
        for body in [
            "",
            "not json at all",
            r#"{"message":"nope"}"#,
            r#"{"error":""}"#,
            r#"{"error":"   "}"#,
            r#"{"error":404}"#,
        ] {
            let v = classify(Some(400), body);
            assert_eq!(v.disposition, PostDisposition::GiveUp, "body={body:?}");
            assert_eq!(v.denial, None, "body={body:?}");
        }
    }

    #[test]
    fn denial_tags_round_trip_through_their_wire_codes() {
        for code in [
            "status_is_derived",
            "self_attestation_forbidden",
            "owner_unresolved",
            "attester_unresolved",
            "something_new",
        ] {
            let tag = DenialTag::from_code(code);
            assert_eq!(tag.as_code(), code);
            assert_eq!(tag.to_string(), code);
        }
    }

    /// 409 is its own arm and carries no tag — the server's ledger stands, and
    /// the caller drops rather than degrading.
    #[test]
    fn terminal_conflict_is_structural_but_untagged() {
        let v = classify(Some(409), r#"{"error":"dispatch_terminal"}"#);
        assert_eq!(v.disposition, PostDisposition::TerminalConflict);
        assert_eq!(v.denial, None);
        assert!(v.is_structural());
        assert!(!v.is_retryable());
    }

    /// coord's `terminality` hint, verbatim from the measured 422 body — and
    /// the discrimination that makes it worth reading: only `permanent`
    /// licenses a retirement.
    #[test]
    fn terminality_is_read_off_the_denial_body_and_only_permanent_retires() {
        let permanent = classify(
            Some(422),
            r#"{"error":"status_is_derived","message":"status `shipped` is derived (coord-computed from a predicate), not directly settable","terminality":"permanent"}"#,
        );
        assert_eq!(permanent.disposition, PostDisposition::GiveUp);
        assert_eq!(permanent.denial, Some(DenialTag::StatusIsDerived));
        assert_eq!(
            permanent.terminality,
            Some(DenialTerminality::Permanent),
            "coord said permanent"
        );
        assert!(permanent.is_permanently_denied());

        // The other two members of the closed set are PARSED but must NOT
        // license a retirement: each is invalidated by something that happens
        // out of band, so a later cycle can legitimately clear it.
        for (wire, expected) in [
            ("actor_dependent", DenialTerminality::ActorDependent),
            ("state_dependent", DenialTerminality::StateDependent),
        ] {
            let v = classify(
                Some(403),
                &format!(r#"{{"error":"self_attestation_forbidden","terminality":"{wire}"}}"#),
            );
            assert_eq!(v.terminality, Some(expected), "{wire}");
            assert!(
                !v.is_permanently_denied(),
                "{wire} must stay retryable — it clears out of band"
            );
        }
    }

    /// UNKNOWN is not `permanent`. An older coord sends no hint at all, a
    /// future one may send a word this build has never heard of, and a body may
    /// not parse — all three must keep the pre-change retry behaviour rather
    /// than freezing the unit.
    #[test]
    fn an_absent_or_unrecognized_terminality_is_unknown_and_never_retires() {
        for body in [
            // The pre-hint coord: a denial with no `terminality` key at all.
            r#"{"error":"status_is_derived","message":"…"}"#,
            "",
            "not json at all",
            r#"{"terminality":""}"#,
            r#"{"terminality":"Permanent"}"#,
            r#"{"terminality":" permanent "}"#,
            r#"{"terminality":"permanently"}"#,
            r#"{"terminality":"a_word_coord_adds_in_2027"}"#,
            r#"{"terminality":true}"#,
            r#"{"terminality":null}"#,
        ] {
            let v = classify(Some(422), body);
            assert_eq!(v.disposition, PostDisposition::GiveUp, "body={body:?}");
            assert_eq!(v.terminality, None, "body={body:?} must be UNKNOWN");
            assert!(
                !v.is_permanently_denied(),
                "body={body:?} must not license a retirement"
            );
        }
    }

    /// The 401-burst carve-out extends to `terminality`: a retryable status must
    /// never come back carrying a terminal-looking field, however the body is
    /// spelled. Without this, the 2026-08-29 burst — had coord tagged it —
    /// would have tombstoned every unit on the device.
    #[test]
    fn a_retryable_status_never_carries_a_terminality() {
        for s in [401, 408, 429, 500, 502, 503] {
            let v = classify(
                Some(s),
                r#"{"error":"invalid token","terminality":"permanent"}"#,
            );
            assert_eq!(v.disposition, PostDisposition::Retry, "{s}");
            assert_eq!(v.terminality, None, "{s} must carry no terminality");
            assert!(!v.is_permanently_denied(), "{s}");
        }
        // Transport failure, and the 2xx/409 arms, likewise.
        assert_eq!(
            classify(None, r#"{"terminality":"permanent"}"#).terminality,
            None
        );
        assert_eq!(
            classify(Some(200), r#"{"terminality":"permanent"}"#).terminality,
            None
        );
        assert_eq!(
            classify(Some(409), r#"{"terminality":"permanent"}"#).terminality,
            None
        );
    }

    #[test]
    fn terminality_round_trips_through_its_wire_words() {
        for wire in ["permanent", "actor_dependent", "state_dependent"] {
            let t = DenialTerminality::from_wire(wire).expect("closed set member");
            assert_eq!(t.as_wire(), wire);
            assert_eq!(t.to_string(), wire);
        }
        assert_eq!(DenialTerminality::from_wire("nope"), None);
    }
}
