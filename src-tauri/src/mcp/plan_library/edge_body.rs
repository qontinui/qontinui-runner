//! The ONE validator and renderer for a provenance edge's upstream body (plan
//! `2026-11-03-runner-link-door-refuses-an-unowned-follow-up` D1).
//!
//! Both edge write doors call it: `POST /plan-library/links` and every inline
//! `edges` entry of `POST /plan-library/artifacts`. Before it, the two doors
//! held two code paths for one wire shape, and neither could carry a one-ended
//! `spawned_followup`: the link body required `to_id`, and the inline renderer
//! refused an edge with neither end.
//!
//! **Pure.** It does no IO. The handlers keep every upstream call, so this
//! module adds no forward to the web backend.
//!
//! ## Exactly five rules, all of them SHAPE (D2)
//!
//! - **R1, ends.** Exactly one of `to_id` / `from_id`, except that a relation in
//!   [`OPEN_TARGET_RELATIONS`] may carry neither. Such a relation never carries
//!   `from_id`: it is always outgoing.
//! - **R2, ids.** A present `to_id` / `from_id` parses as a UUID and is rendered
//!   canonical (lower-case, hyphenated).
//! - **R3, note.** An [`OPEN_TARGET_RELATIONS`] edge needs a non-blank note,
//!   judged on the note the CALLER sent. `POST /plan-library/links` folds an
//!   attribution suffix into every note it forwards, and that fold is never
//!   blank, so judging the forwarded note would let a blank one through as
//!   `(agent write by …)` alone: an unactionable row in the open-follow-ups
//!   queue (D6).
//! - **R4, facts placement.** `anchors`, `size_class` and `unanchored_reason`
//!   appear only on an [`OPEN_TARGET_RELATIONS`] edge.
//! - **R5, reason.** A present `unanchored_reason` is non-blank and never sits
//!   beside a non-empty `anchors`.
//!
//! These are the rules that protect what the RUNNER owns: a path segment it
//! sends under its own device credential ([`link_anchor`]), no half-written
//! artifact for a malformed inline edge (the inline door validates every edge
//! BEFORE its upsert, which is not transactional with the edges), and a fold
//! that cannot invent content. Every VALUE rule is the web edge route's: an
//! anchor's `repo` / `path` grammar, the `size_class` vocabulary, the cap on
//! `anchors`, a self-edge, and whatever the web adds next (the refusal of a
//! note-only follow-up on OPEN qontinui-web#2065). Its 422 reaches the caller
//! verbatim through the door's `upstream_failure`. Retyping those rules here
//! would refuse at the loopback what the web has since started to accept,
//! which is the stale-refusal class this module exists to remove.
//!
//! Each refusal names its rule, in the web route's own wording where one exists
//! (`create_work_artifact_edge`, qontinui-web
//! `backend/app/api/v1/endpoints/plan_library.py`).

use serde_json::{Map, Value};

use super::EdgeRelation;

/// The relations whose edge may omit BOTH ends: a one-ended edge recording work
/// an artifact surfaced and did not do, which has no artifact of its own yet.
///
/// Mirrors the web's `RELATIONS_ALLOWING_OPEN_TARGET` (qontinui-web
/// `backend/app/models/work_artifact.py:198`), which the edge route checks
/// before `ck_work_artifact_edges_open_target` would refuse the row.
///
/// The ONE source on the loopback: R1, R3 and R4 read it, the refusals render
/// it, and the ungated reads advertise it as
/// `writeVocabulary.open_target_relation`, so the advert cannot drift from what
/// this door enforces (D3).
pub(super) const OPEN_TARGET_RELATIONS: [EdgeRelation; 1] = [EdgeRelation::SpawnedFollowup];

/// Whether `relation` may omit both ends ([`OPEN_TARGET_RELATIONS`]).
fn is_open_target(relation: EdgeRelation) -> bool {
    OPEN_TARGET_RELATIONS.contains(&relation)
}

/// [`OPEN_TARGET_RELATIONS`] as refusal text, rendered from the constant rather
/// than retyped, so a refusal names the set the rule actually used.
fn open_target_set() -> String {
    OPEN_TARGET_RELATIONS
        .iter()
        .map(|r| format!("`{}`", r.as_str()))
        .collect::<Vec<_>>()
        .join(" | ")
}

/// One edge as a write door received it, borrowed from that door's request
/// struct (`EdgeSpec::draft`, `LinkRequest::draft`).
#[derive(Debug, Clone, Copy)]
pub(super) struct EdgeDraft<'a> {
    pub relation: EdgeRelation,
    /// The OUTGOING far end.
    pub to_id: Option<&'a str>,
    /// The INCOMING far end. `POST /plan-library/links` never sets it: that
    /// door's `from_id` is the ANCHOR, the upstream path segment
    /// ([`link_anchor`]), not an end of the body.
    pub from_id: Option<&'a str>,
    /// The note the CALLER sent. R3 judges this one (D6).
    pub caller_note: Option<&'a str>,
    /// The note the body carries: on an inline edge the caller's own, unfolded;
    /// on `POST /plan-library/links` its attribution fold.
    pub forwarded_note: Option<&'a str>,
    /// Forwarded as given, `[]` included. Each anchor's shape is the web's.
    pub anchors: Option<&'a [Value]>,
    /// Forwarded as given. Its vocabulary is the web's.
    pub size_class: Option<&'a str>,
    pub unanchored_reason: Option<&'a str>,
}

/// Validate one edge against R1 to R5 and render the web edge route's
/// anchor-relative body, or the refusal naming the rule it broke.
///
/// A key is present only when supplied: `to_id`, `from_id` or neither, then
/// `relation`, `note`, `anchors` (`[]` included), `size_class` and
/// `unanchored_reason`. Nothing is rendered as `null`.
///
/// The relation needs no check here: it is an [`EdgeRelation`], so a blank or
/// unknown one was refused when the request body was parsed.
pub(super) fn render(draft: &EdgeDraft<'_>) -> Result<Value, String> {
    let relation = draft.relation.as_str();
    let open_target = is_open_target(draft.relation);

    // R1, ends. Checked in the web route's own order: neither, both, then an
    // incoming end on a relation that is always outgoing.
    match (draft.to_id, draft.from_id) {
        (None, None) if !open_target => {
            return Err(format!(
                "Relation `{relation}` requires a target: supply exactly one of `to_id` \
                 (outgoing) or `from_id` (incoming); on POST /plan-library/links, whose \
                 `from_id` is the artifact the edge leaves from, that is `to_id`. Only {} may be \
                 recorded with no far end, because the work it names has no artifact yet.",
                open_target_set()
            ));
        }
        (Some(_), Some(_)) => {
            return Err(
                "Supply exactly one of `to_id` (outgoing) or `from_id` (incoming), not both."
                    .to_string(),
            );
        }
        (None, Some(_)) if open_target => {
            return Err(format!(
                "`{relation}` is always OUTGOING: this artifact surfaced that work. Omit \
                 `from_id`; use `to_id` only to record a follow-up that already has an owning \
                 artifact."
            ));
        }
        _ => {}
    }

    // R2, ids.
    let to_id = draft
        .to_id
        .map(|raw| canonical_id("to_id", raw))
        .transpose()?;
    let from_id = draft
        .from_id
        .map(|raw| canonical_id("from_id", raw))
        .transpose()?;

    // R3, note: the CALLER's note, never the forwarded one (D6).
    if open_target && draft.caller_note.is_none_or(|n| n.trim().is_empty()) {
        return Err(format!(
            "`{relation}` requires a non-empty `note`. The follow-up has no artifact at the far \
             end, so the note is the entire record of what was surfaced; a blank one occupies \
             the open-follow-ups queue with nothing anyone can act on. This is judged on the \
             note you sent, before any attribution this door appends."
        ));
    }

    // R4, facts placement.
    let facts_supplied: Vec<&str> = [
        ("anchors", draft.anchors.is_some()),
        ("size_class", draft.size_class.is_some()),
        ("unanchored_reason", draft.unanchored_reason.is_some()),
    ]
    .into_iter()
    .filter_map(|(name, supplied)| supplied.then_some(name))
    .collect();
    if !open_target && !facts_supplied.is_empty() {
        let facts = facts_supplied
            .iter()
            .map(|name| format!("`{name}`"))
            .collect::<Vec<_>>()
            .join(", ");
        return Err(format!(
            "{facts} only apply to {}: relation `{relation}` carries no anchors or size class.",
            open_target_set()
        ));
    }

    // R5, reason.
    if let Some(reason) = draft.unanchored_reason {
        if reason.trim().is_empty() {
            return Err("`unanchored_reason` is blank; omit it or say why.".to_string());
        }
        if draft.anchors.is_some_and(|anchors| !anchors.is_empty()) {
            return Err(
                "`unanchored_reason` contradicts a non-empty `anchors` list: a follow-up either \
                 names its files or says why it cannot."
                    .to_string(),
            );
        }
    }

    let mut body = Map::new();
    if let Some(id) = to_id {
        body.insert("to_id".to_string(), Value::String(id));
    }
    if let Some(id) = from_id {
        body.insert("from_id".to_string(), Value::String(id));
    }
    body.insert("relation".to_string(), Value::String(relation.to_string()));
    if let Some(note) = draft.forwarded_note {
        body.insert("note".to_string(), Value::String(note.to_string()));
    }
    if let Some(anchors) = draft.anchors {
        body.insert("anchors".to_string(), Value::Array(anchors.to_vec()));
    }
    if let Some(size_class) = draft.size_class {
        body.insert(
            "size_class".to_string(),
            Value::String(size_class.to_string()),
        );
    }
    if let Some(reason) = draft.unanchored_reason {
        body.insert(
            "unanchored_reason".to_string(),
            Value::String(reason.to_string()),
        );
    }
    Ok(Value::Object(body))
}

/// `POST /plan-library/links`' `from_id` as its canonical UUID, or the refusal.
///
/// That door anchors the edge on `from_id`, which makes it a PATH SEGMENT of the
/// upstream URL, sent under the runner's own device credential. So it is
/// required, non-blank and parsed here, before any upstream call, and only its
/// canonical form reaches the wire: no `..`, `/` or `?` can. This is the guard
/// `edge_upstream_path` and `artifact_upstream_path` already give every other
/// id-in-path forward on this door (security finding `80cde6f7`).
pub(super) fn link_anchor(raw: &str) -> Result<String, String> {
    if raw.trim().is_empty() {
        return Err(
            "`from_id` is required: the artifact the edge leaves from, as an artifact UUID (as \
             returned by GET /plan-library/search). It becomes the upstream path segment."
                .to_string(),
        );
    }
    canonical_id("from_id", raw)
}

/// R2: a present id parses as a UUID and renders canonical, the lower-case
/// hyphenated spelling the door's path guards normalise to. Braced, simple, URN
/// and padded spellings all normalise; anything else is refused, naming the
/// field and echoing a bounded prefix of what was sent.
fn canonical_id(field: &str, raw: &str) -> Result<String, String> {
    if raw.trim().is_empty() {
        return Err(format!(
            "`{field}` is blank: send an artifact UUID (as returned by GET \
             /plan-library/search), or leave the key out (`null` reads the same as absent)."
        ));
    }
    uuid::Uuid::parse_str(raw.trim())
        .map(|id| id.hyphenated().to_string())
        .map_err(|e| {
            let got = bounded_echo(raw);
            format!(
                "`{field}` must be an artifact UUID (as returned by GET /plan-library/search), \
                 got {got:?}: {e}"
            )
        })
}

/// A caller-supplied value as a refusal may echo it: a bounded prefix, marked
/// when cut. The value ends up in a response body and in logs, so an unbounded
/// echo would let a caller size both.
pub(super) fn bounded_echo(got: &str) -> String {
    const ECHO_MAX_CHARS: usize = 64;
    let mut echoed: String = got.chars().take(ECHO_MAX_CHARS).collect();
    if got.chars().nth(ECHO_MAX_CHARS).is_some() {
        echoed.push('…');
    }
    echoed
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// An inline OUTGOING edge renders only its far end, canonical, beside the
    /// relation and the caller's note forwarded unfolded. The end used to be a
    /// free string (`"peer"`); R2 now refuses anything that is not a UUID.
    #[test]
    fn an_inline_outgoing_edge_spec_renders_only_its_far_end() {
        let spec: super::super::EdgeSpec = serde_json::from_value(serde_json::json!({
            "to_id": "{0D2F2A6E-7C1B-4F38-9A3E-1B2C3D4E5F60}",
            "relation": "authored_plan",
            "note": "chain",
        }))
        .unwrap();
        assert_eq!(
            render(&spec.draft()).unwrap(),
            serde_json::json!({
                "to_id": "0d2f2a6e-7c1b-4f38-9a3e-1b2c3d4e5f60",
                "relation": "authored_plan",
                "note": "chain",
            })
        );
    }

    /// An inline INCOMING edge renders only its far end, and no `note` key at
    /// all when none was sent (no `null` placeholder).
    #[test]
    fn an_inline_incoming_edge_spec_renders_only_its_far_end() {
        let spec: super::super::EdgeSpec = serde_json::from_value(serde_json::json!({
            "from_id": "3F2504E0-4F89-11D3-9A0C-0305E82C3301",
            "relation": "produced_report",
        }))
        .unwrap();
        assert_eq!(
            render(&spec.draft()).unwrap(),
            serde_json::json!({
                "from_id": "3f2504e0-4f89-11d3-9a0c-0305e82c3301",
                "relation": "produced_report",
            })
        );
    }

    /// A canonical artifact id, and an equivalent spelling of it per form the
    /// renderer must normalise.
    const A: &str = "0d2f2a6e-7c1b-4f38-9a3e-1b2c3d4e5f60";
    const A_SPELLINGS: [&str; 5] = [
        "0D2F2A6E-7C1B-4F38-9A3E-1B2C3D4E5F60",
        "{0d2f2a6e-7c1b-4f38-9a3e-1b2c3d4e5f60}",
        "0d2f2a6e7c1b4f389a3e1b2c3d4e5f60",
        " 0d2f2a6e-7c1b-4f38-9a3e-1b2c3d4e5f60 ",
        "urn:uuid:0d2f2a6e-7c1b-4f38-9a3e-1b2c3d4e5f60",
    ];
    const B: &str = "3f2504e0-4f89-11d3-9a0c-0305e82c3301";

    /// An edge with the given relation and ends and a non-blank note, so R3
    /// holds wherever it applies and the ends are the only variable.
    fn ends<'a>(
        relation: EdgeRelation,
        to_id: Option<&'a str>,
        from_id: Option<&'a str>,
    ) -> EdgeDraft<'a> {
        EdgeDraft {
            relation,
            to_id,
            from_id,
            caller_note: Some("n"),
            forwarded_note: Some("n"),
            anchors: None,
            size_class: None,
            unanchored_reason: None,
        }
    }

    /// A one-ended follow-up with a note and no facts.
    fn followup() -> EdgeDraft<'static> {
        ends(EdgeRelation::SpawnedFollowup, None, None)
    }

    #[derive(Debug, Clone, Copy)]
    enum Ends {
        ToOnly,
        FromOnly,
        Both,
        Neither,
    }

    /// R1, every relation by every shape of ends. The expectation is written
    /// against `SpawnedFollowup` LITERALLY, not derived from
    /// [`OPEN_TARGET_RELATIONS`]: derived, it would move with the constant and
    /// could not catch a constant that lost its one member.
    #[test]
    fn an_edge_needs_exactly_one_end_unless_its_relation_may_omit_both() {
        for relation in EdgeRelation::ALL {
            for shape in [Ends::ToOnly, Ends::FromOnly, Ends::Both, Ends::Neither] {
                let (to_id, from_id) = match shape {
                    Ends::ToOnly => (Some(A), None),
                    Ends::FromOnly => (None, Some(B)),
                    Ends::Both => (Some(A), Some(B)),
                    Ends::Neither => (None, None),
                };
                let followup = relation == EdgeRelation::SpawnedFollowup;
                let accepted = match shape {
                    Ends::ToOnly => true,
                    Ends::FromOnly => !followup,
                    Ends::Both => false,
                    Ends::Neither => followup,
                };
                let got = render(&ends(relation, to_id, from_id));
                assert_eq!(got.is_ok(), accepted, "{relation:?} x {shape:?}: {got:?}");
                match got {
                    Ok(body) => {
                        // Only the far ends that were sent, never a `null` one.
                        assert_eq!(
                            body.get("to_id").cloned(),
                            to_id.map(|_| json!(A)),
                            "{relation:?} x {shape:?}: {body}"
                        );
                        assert_eq!(
                            body.get("from_id").cloned(),
                            from_id.map(|_| json!(B)),
                            "{relation:?} x {shape:?}: {body}"
                        );
                    }
                    Err(e) => {
                        let names_the_rule = match shape {
                            Ends::Both => e.contains("exactly one of `to_id`"),
                            Ends::FromOnly => e.contains("is always OUTGOING"),
                            Ends::Neither => e.contains("requires a target"),
                            Ends::ToOnly => false,
                        };
                        assert!(names_the_rule, "{relation:?} x {shape:?}: {e}");
                    }
                }
            }
        }
    }

    /// The `neither` refusal names the set that MAY omit both ends, rendered
    /// from the constant, and says which field the far end is on `/links`.
    #[test]
    fn the_missing_target_refusal_names_the_open_target_set() {
        let err = render(&ends(EdgeRelation::Feeds, None, None)).unwrap_err();
        assert!(err.contains("Relation `feeds` requires a target"), "{err}");
        assert!(err.contains("Only `spawned_followup` may be"), "{err}");
        assert!(err.contains("POST /plan-library/links"), "{err}");
    }

    /// R2: a non-UUID end is refused naming its field, on either end; every
    /// accepted spelling of a UUID is rendered canonical.
    #[test]
    fn a_non_uuid_end_is_refused_and_a_uuid_end_is_rendered_canonical() {
        for bad in [
            "peer",
            "producer",
            "../candidates",
            "abc?include_coord=true",
            "0d2f2a6e-7c1b-4f38-9a3e-1b2c3d4e5f6",
            "🦀",
        ] {
            for (field, draft) in [
                ("to_id", ends(EdgeRelation::Feeds, Some(bad), None)),
                ("from_id", ends(EdgeRelation::Feeds, None, Some(bad))),
            ] {
                let err = render(&draft).unwrap_err();
                assert!(
                    err.contains(&format!("`{field}` must be an artifact UUID")),
                    "{field}={bad:?}: {err}"
                );
                assert!(err.contains(&format!("{bad:?}")), "{field}={bad:?}: {err}");
            }
        }
        for blank in ["", "   "] {
            let err = render(&ends(EdgeRelation::Feeds, Some(blank), None)).unwrap_err();
            assert!(err.contains("`to_id` is blank"), "{blank:?}: {err}");
        }
        for spelling in A_SPELLINGS {
            assert_eq!(
                render(&ends(EdgeRelation::Feeds, Some(spelling), None)).unwrap(),
                json!({"to_id": A, "relation": "feeds", "note": "n"}),
                "{spelling}"
            );
            assert_eq!(
                render(&ends(EdgeRelation::Feeds, None, Some(spelling))).unwrap(),
                json!({"from_id": A, "relation": "feeds", "note": "n"}),
                "{spelling}"
            );
        }
    }

    /// A refused end is echoed only as a bounded prefix: it is caller-supplied
    /// and lands in a response body and in logs.
    #[test]
    fn a_refused_end_is_echoed_only_as_a_bounded_prefix() {
        let long = "x".repeat(10_000);
        let err = render(&ends(EdgeRelation::Feeds, Some(&long), None)).unwrap_err();
        assert!(err.contains(&"x".repeat(64)), "{err}");
        assert!(!err.contains(&"x".repeat(65)), "the echo is not capped");
        assert!(err.contains('…'), "a cut echo is marked: {err}");
        assert_eq!(bounded_echo("short"), "short", "an uncut echo is unmarked");
    }

    /// R3: absent, empty and whitespace-only notes are refused on a follow-up,
    /// one-ended AND owned, even when the forwarded note is the never-blank
    /// attribution fold `/links` sends: the CALLER's note is the one judged.
    #[test]
    fn a_followup_needs_a_non_blank_note_of_its_own_one_ended_or_owned() {
        for caller_note in [None, Some(""), Some("   "), Some("\n\t")] {
            for to_id in [None, Some(A)] {
                let draft = EdgeDraft {
                    to_id,
                    caller_note,
                    forwarded_note: Some("(agent write by device)"),
                    ..followup()
                };
                let err = render(&draft).unwrap_err();
                assert!(
                    err.contains("`spawned_followup` requires a non-empty `note`"),
                    "note={caller_note:?} to_id={to_id:?}: {err}"
                );
                assert!(err.contains("before any attribution"), "{err}");
            }
        }
        // Every other relation may go without a note, and then sends no `note`.
        for relation in EdgeRelation::ALL {
            if relation == EdgeRelation::SpawnedFollowup {
                continue;
            }
            let draft = EdgeDraft {
                caller_note: None,
                forwarded_note: None,
                ..ends(relation, Some(A), None)
            };
            assert_eq!(
                render(&draft).unwrap(),
                json!({"to_id": A, "relation": relation.as_str()}),
                "{relation:?}"
            );
        }
    }

    /// R4: each follow-up fact on any other relation is refused, naming the
    /// fact; `anchors: []` counts as supplied, as it does on the web route.
    #[test]
    fn a_followup_fact_on_any_other_relation_is_refused_naming_it() {
        let anchors = [json!({"repo": "o/r", "path": "a.rs"})];
        for relation in EdgeRelation::ALL {
            if relation == EdgeRelation::SpawnedFollowup {
                continue;
            }
            let base = ends(relation, Some(A), None);
            for (fact, draft) in [
                (
                    "anchors",
                    EdgeDraft {
                        anchors: Some(&anchors),
                        ..base
                    },
                ),
                (
                    "anchors",
                    EdgeDraft {
                        anchors: Some(&[]),
                        ..base
                    },
                ),
                (
                    "size_class",
                    EdgeDraft {
                        size_class: Some("small"),
                        ..base
                    },
                ),
                (
                    "unanchored_reason",
                    EdgeDraft {
                        unanchored_reason: Some("no file"),
                        ..base
                    },
                ),
            ] {
                let err = render(&draft).unwrap_err();
                assert_eq!(
                    err,
                    format!(
                        "`{fact}` only apply to `spawned_followup`: relation `{}` carries no \
                         anchors or size class.",
                        relation.as_str()
                    ),
                    "{relation:?}"
                );
            }
            let all_three = EdgeDraft {
                anchors: Some(&anchors),
                size_class: Some("small"),
                unanchored_reason: Some("no file"),
                ..base
            };
            let err = render(&all_three).unwrap_err();
            assert!(
                err.starts_with("`anchors`, `size_class`, `unanchored_reason` only apply to"),
                "{relation:?}: {err}"
            );
        }
    }

    /// R5: a blank reason is refused, a reason beside a non-empty `anchors` is
    /// refused, and a reason beside `[]` is accepted with the `[]` forwarded.
    #[test]
    fn an_unanchored_reason_says_something_and_never_contradicts_anchors() {
        let anchors = [json!({"repo": "o/r", "path": "a.rs"})];
        for blank in ["", "   ", "\n"] {
            let err = render(&EdgeDraft {
                unanchored_reason: Some(blank),
                ..followup()
            })
            .unwrap_err();
            assert_eq!(err, "`unanchored_reason` is blank; omit it or say why.");
        }
        let err = render(&EdgeDraft {
            unanchored_reason: Some("no file yet"),
            anchors: Some(&anchors),
            ..followup()
        })
        .unwrap_err();
        assert!(
            err.contains("contradicts a non-empty `anchors` list"),
            "{err}"
        );
        assert_eq!(
            render(&EdgeDraft {
                unanchored_reason: Some("no file yet"),
                anchors: Some(&[]),
                ..followup()
            })
            .unwrap(),
            json!({
                "relation": "spawned_followup",
                "note": "n",
                "anchors": [],
                "unanchored_reason": "no file yet",
            })
        );
    }

    /// Rendering, as LITERAL bodies: a key appears only when it was supplied,
    /// nothing renders as `null`, the forwarded note is the one that travels,
    /// and values ride through unjudged (D2: the web owns value rules).
    #[test]
    fn the_rendered_body_carries_exactly_the_supplied_keys() {
        let anchors = [json!({"repo": "o/r", "path": "a.rs"})];
        // The plan's example: no `to_id` or `from_id` key at all.
        assert_eq!(
            render(&EdgeDraft {
                anchors: Some(&anchors),
                size_class: Some("small"),
                ..followup()
            })
            .unwrap(),
            json!({
                "relation": "spawned_followup",
                "note": "n",
                "anchors": [{"repo": "o/r", "path": "a.rs"}],
                "size_class": "small",
            })
        );
        // Facts absent: their keys absent.
        assert_eq!(
            render(&followup()).unwrap(),
            json!({"relation": "spawned_followup", "note": "n"})
        );
        // A reason instead of anchors.
        assert_eq!(
            render(&EdgeDraft {
                size_class: Some("trivial"),
                unanchored_reason: Some("a process gap, not a file"),
                ..followup()
            })
            .unwrap(),
            json!({
                "relation": "spawned_followup",
                "note": "n",
                "size_class": "trivial",
                "unanchored_reason": "a process gap, not a file",
            })
        );
        // An OWNED follow-up keeps its far end.
        assert_eq!(
            render(&EdgeDraft {
                to_id: Some(A_SPELLINGS[0]),
                size_class: Some("plan_sized"),
                ..followup()
            })
            .unwrap(),
            json!({
                "to_id": A,
                "relation": "spawned_followup",
                "note": "n",
                "size_class": "plan_sized",
            })
        );
        // The FORWARDED note travels; the caller's is only judged.
        assert_eq!(
            render(&EdgeDraft {
                forwarded_note: Some("n (agent write by device)"),
                ..followup()
            })
            .unwrap(),
            json!({"relation": "spawned_followup", "note": "n (agent write by device)"})
        );
        // Values the web will judge are forwarded unjudged.
        assert_eq!(
            render(&EdgeDraft {
                anchors: Some(&[json!({"repo": "O/R", "path": "../a.rs", "line_start": 0})]),
                size_class: Some("enormous"),
                ..followup()
            })
            .unwrap(),
            json!({
                "relation": "spawned_followup",
                "note": "n",
                "anchors": [{"repo": "O/R", "path": "../a.rs", "line_start": 0}],
                "size_class": "enormous",
            })
        );
    }

    /// `/links`' anchor is required, non-blank and a UUID, and only its
    /// canonical spelling is returned: nothing path-like survives.
    #[test]
    fn the_link_anchor_is_a_required_canonical_uuid() {
        for blank in ["", "   "] {
            let err = link_anchor(blank).unwrap_err();
            assert!(err.contains("`from_id` is required"), "{blank:?}: {err}");
        }
        for bad in [
            "not-a-uuid",
            "../../etc/passwd",
            "0d2f2a6e-7c1b-4f38-9a3e-1b2c3d4e5f60/../secrets",
            "?include_coord=true",
            "a/b",
        ] {
            let err = link_anchor(bad).unwrap_err();
            assert!(
                err.contains("`from_id` must be an artifact UUID"),
                "{bad:?}: {err}"
            );
        }
        for spelling in A_SPELLINGS {
            assert_eq!(link_anchor(spelling).unwrap(), A, "{spelling}");
        }
    }
}
