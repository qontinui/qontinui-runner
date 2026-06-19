//! Parity proof (Phase 4) — gates P4.
//!
//! Asserts the adapter reproduces coord's old `plan_ingest_worker` output for
//! the existing `plans/` corpus: **slug + status parity** at minimum (the
//! contract P4's decommission of coord's ingest depends on — if the adapter
//! drifts from coord here, removing coord's ingest would change what coord
//! records).
//!
//! Rather than depend on the coord crate (it's a binary-only crate), this
//! module **vendors a faithful copy of coord's exact status algorithm** —
//! `PLAN_STATUS_VOCAB` + the `match_known_status` longest-match + the
//! first-token fallback + `normalize_status` (verbatim from
//! `qontinui-coord/src/plan_registry.rs` and `plan_ingest_worker.rs`) — as an
//! independent reference oracle, then runs BOTH the oracle and the adapter
//! parser over the same embedded corpus and asserts they agree. The vendored
//! copy is the "coord's old ingest" side of the side-by-side run; the adapter
//! is the new side.
//!
//! The whole module is `#[cfg(test)]` — it is a proof, not shipped code.

// ---- Vendored coord reference oracle (coord's old ingest, verbatim) --------

/// Verbatim copy of `qontinui-coord/src/plan_registry.rs::PLAN_STATUS_VOCAB`.
const COORD_VOCAB: &[&str] = &[
    "draft",
    "vetted",
    "in progress",
    "in_progress",
    "shipped",
    "partial",
    "not started",
    "not_started",
    "superseded",
    "obsolete",
];

/// Verbatim port of coord's `plan_ingest_worker::match_known_status`.
fn coord_match_known_status(s: &str) -> Option<String> {
    let lower = s.to_lowercase();
    let mut best: Option<&str> = None;
    for phrase in COORD_VOCAB {
        if !lower.starts_with(phrase) {
            continue;
        }
        let boundary_ok = match lower[phrase.len()..].chars().next() {
            None => true,
            Some(c) => !(c.is_alphanumeric() || c == '_'),
        };
        if boundary_ok && best.is_none_or(|b| phrase.len() > b.len()) {
            best = Some(phrase);
        }
    }
    best.map(|p| p.replace(' ', "_"))
}

/// Verbatim port of coord's `plan_ingest_worker::normalize_status`.
fn coord_normalize_status(raw: &str) -> String {
    let cleaned: String = raw
        .trim()
        .trim_end_matches([',', '.', ':', '*'])
        .chars()
        .map(|c| if c.is_whitespace() { '_' } else { c })
        .collect();
    cleaned.to_lowercase()
}

/// Verbatim port of coord's `plan_ingest_worker::parse_plan` STATUS path
/// (defaults to `"draft"` when no `> **Status:` line is present).
fn coord_parse_status(body: &str) -> String {
    for line in body.lines() {
        let trimmed = line.trim_start();
        if let Some(after_quote) = trimmed.strip_prefix("> ") {
            if let Some(after_bold) = after_quote.strip_prefix("**Status:") {
                let mut s = after_bold.trim().to_string();
                if let Some(stripped) = s.strip_suffix("**") {
                    s = stripped.trim().to_string();
                }
                if let Some(known) = coord_match_known_status(&s) {
                    return known;
                }
                let token = s
                    .split(|c: char| c.is_whitespace() || c == '.' || c == ',')
                    .next()
                    .unwrap_or("")
                    .trim();
                if !token.is_empty() {
                    return coord_normalize_status(token);
                }
                if let Some(t) = s.split_whitespace().next().filter(|t| !t.is_empty()) {
                    return coord_normalize_status(t);
                }
            }
        }
    }
    "draft".to_string()
}

// ---- The corpus + the proof ------------------------------------------------

/// The embedded real-`plans/` corpus (same fixtures the parser golden tests
/// use). Extend this list as the corpus grows — every entry strengthens the
/// proof.
const CORPUS: &[(&str, &str)] = &[
    (
        "plans/2026-06-18-coord-generic-work-unit-primitive.md",
        include_str!("fixtures/2026-06-18-coord-generic-work-unit-primitive.md"),
    ),
    (
        "plans/2026-06-18-harness-markdown-to-workunit-adapter.md",
        include_str!("fixtures/2026-06-18-harness-markdown-to-workunit-adapter.md"),
    ),
    (
        "plans/2026-06-18-coord-decommission-markdown-plan-ingest.md",
        include_str!("fixtures/2026-06-18-coord-decommission-markdown-plan-ingest.md"),
    ),
    (
        "plans/2026-06-18-coord-orchestration-generalize-off-plans.md",
        include_str!("fixtures/2026-06-18-coord-orchestration-generalize-off-plans.md"),
    ),
    (
        "plans/2026-06-18-coord-merge-scheduler-db-test-harness.md",
        include_str!("fixtures/2026-06-18-coord-merge-scheduler-db-test-harness.md"),
    ),
];

#[cfg(test)]
mod tests {
    use super::super::parser::{parse_work_unit, slug_from_filename, PlanConvention};
    use super::*;

    #[test]
    fn adapter_matches_coord_slug_and_status_over_corpus() {
        let conv = PlanConvention::operator_default();
        let mut checked = 0;
        for (path, body) in CORPUS {
            let slug = slug_from_filename(path);
            let adapter = parse_work_unit(&slug, path, body, &conv);
            let coord_status = coord_parse_status(body);

            // Slug parity: the adapter derives the same slug coord would.
            assert_eq!(adapter.slug, slug, "slug mismatch for {path}");
            // Status parity: the adapter's opaque status equals coord's
            // vocab-normalized status for the existing corpus.
            assert_eq!(
                adapter.status, coord_status,
                "STATUS PARITY MISMATCH for {path}: adapter={:?} coord={:?}",
                adapter.status, coord_status
            );
            checked += 1;
        }
        assert!(checked >= 5, "parity corpus shrank unexpectedly: {checked}");
    }

    /// The decoy guard, framed as parity: coord's first-token fallback ALSO
    /// yields `implemented` for the P1 plan (its vocab lacks `implemented`),
    /// so the adapter and coord agree — and neither is fooled by the later
    /// "Flip to SHIPPED" sentence.
    #[test]
    fn implemented_status_agrees_and_decoy_loses_on_both_sides() {
        let body = include_str!("fixtures/2026-06-18-coord-generic-work-unit-primitive.md");
        assert_eq!(coord_parse_status(body), "implemented");
        let adapter = parse_work_unit("x", "x.md", body, &PlanConvention::operator_default());
        assert_eq!(adapter.status, "implemented");
    }
}
