//! Pure markdown -> work-unit parser (Phase 1).
//!
//! IO-free. Turns one plan markdown file's `(slug, source_path, body)` into a
//! [`ParsedWorkUnit`]: slug, title, an **opaque** status, the declared
//! dependency edges, and the phase structure as sub-units.
//!
//! ## Relationship to coord's old parser
//!
//! This is a port + enrichment of coord's `plan_ingest_worker::parse_plan`
//! (`qontinui-coord/src/plan_ingest_worker.rs`). Two deliberate differences:
//!
//! 1. **Status is opaque.** coord normalized the parsed status against a fixed
//!    `PLAN_STATUS_VOCAB` and could drop an unknown stamp to a fallback token.
//!    Here the convention's phrase list is used ONLY to *tokenize* a multi-word
//!    stamp (so `IN PROGRESS 2026-06-19.` resolves to `in_progress`, not the
//!    first token `in`); an unrecognized stamp is kept verbatim (lowercased +
//!    underscore-joined) rather than rejected, matching the opaque-status model
//!    of the coord work-unit API this adapter pushes to. Seeding
//!    [`PlanConvention::operator_default`] with exactly coord's vocabulary
//!    yields byte-identical status output to coord's parser for every known
//!    status — the property the future parity proof relies on.
//! 2. **Richer projection.** We also extract [`ParsedWorkUnit::depends_on`]
//!    (the canonical `Depends-On:` edges) and [`ParsedWorkUnit::phases`] (one
//!    sub-unit per `Phase N` heading), which coord discarded.

/// The operator's markdown convention, supplied as configuration rather than
/// baked into fleet semantics. Today it carries the set of known lifecycle
/// status phrases used to *tokenize* (never reject) a status stamp.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanConvention {
    /// Lowercase status phrases, longest-match-wins. Multi-word phrases (e.g.
    /// `"in progress"`) MUST be present so a stamp like `IN PROGRESS 2026-...`
    /// tokenizes to the whole phrase instead of the first word.
    pub status_phrases: Vec<String>,
}

impl PlanConvention {
    /// The operator's current plan-markdown convention. The phrase set mirrors
    /// coord's `PLAN_STATUS_VOCAB` (plus `implemented`, which the operator uses
    /// for "built, PRs open, not yet on main") so known statuses parse
    /// identically to coord's ingest.
    pub fn operator_default() -> Self {
        Self {
            status_phrases: [
                "draft",
                "vetted",
                "in progress",
                "shipped",
                "partial",
                "not started",
                "superseded",
                "obsolete",
                "implemented",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
        }
    }
}

impl Default for PlanConvention {
    fn default() -> Self {
        Self::operator_default()
    }
}

/// One phase of a plan, projected to a work sub-unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedPhase {
    /// The phase number parsed from the `Phase N` heading.
    pub index: u32,
    /// The heading text (bold-span content or heading line), trimmed of markers.
    pub name: String,
}

/// The pure result of parsing a plan markdown file. No IO, no clock, no env.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedWorkUnit {
    /// Slug — supplied by the caller (derived from the filename, not the body).
    pub slug: String,
    /// First `# ` H1, if any.
    pub title: Option<String>,
    /// Opaque status: lowercased + underscore-joined. Defaults to `"draft"`
    /// when no `> **Status:` stamp is present. Never rejected against a vocab.
    pub status: String,
    /// Canonical `Depends-On:` plan stems from the status blockquote, deduped
    /// and order-preserving.
    pub depends_on: Vec<String>,
    /// Phase structure -> sub-units, in document order, deduped by index.
    pub phases: Vec<ParsedPhase>,
    /// Provenance back-link: the source file path the caller supplied.
    pub source_path: String,
    /// Raw body, retained like coord's `ParsedPlan.content`.
    pub content: String,
}

/// Derive the plan slug from a file path: strip directory and a trailing
/// `.md` extension. Mirrors coord's filename->slug derivation.
pub fn slug_from_filename(path: &str) -> String {
    let normalized = path.replace('\\', "/");
    let base = normalized.rsplit('/').next().unwrap_or(&normalized);
    base.strip_suffix(".md").unwrap_or(base).to_string()
}

/// Match the longest convention status phrase at the start of `s`
/// (case-insensitive, word-boundary terminated so `drafty` does NOT match
/// `draft`). Returns the underscore-normalized canonical status on a match.
/// Ported verbatim from coord's `match_known_status`, with the vocabulary
/// supplied by [`PlanConvention`] instead of a hardcoded constant.
fn match_known_status(s: &str, conv: &PlanConvention) -> Option<String> {
    let lower = s.to_lowercase();
    let mut best: Option<&str> = None;
    for phrase in &conv.status_phrases {
        let phrase = phrase.as_str();
        if !lower.starts_with(phrase) {
            continue;
        }
        // Boundary check: the phrase must be the whole remainder or be
        // followed by a non-word character (whitespace, `.`, `,`, ...).
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

/// Normalize a raw status token (`IN_PROGRESS`, `In Progress`, `shipped`) to
/// the canonical lowercased + underscore-joined form. Ported from coord's
/// `normalize_status`.
fn normalize_status(raw: &str) -> String {
    let cleaned: String = raw
        .trim()
        .trim_end_matches([',', '.', ':', '*'])
        .chars()
        .map(|c| if c.is_whitespace() { '_' } else { c })
        .collect();
    cleaned.to_lowercase()
}

/// Parse the remainder of a `> **Status:` stamp into an opaque status string.
/// Uses the convention's phrases for multi-word tokenization, but NEVER rejects
/// an unknown stamp — an unrecognized status flows through verbatim
/// (normalized), unlike coord which gated against its fixed vocabulary.
fn parse_status_value(stamp_remainder: &str, conv: &PlanConvention) -> String {
    let mut s = stamp_remainder.trim().to_string();
    // Drop a trailing `**` if the stamp closes the bold span on this line.
    if let Some(stripped) = s.strip_suffix("**") {
        s = stripped.trim().to_string();
    }
    if let Some(known) = match_known_status(&s, conv) {
        return known;
    }
    // Unknown status: keep it (opaque). Take the first whitespace/`.`/`,`
    // terminated token so a trailing date/prose doesn't bleed into the status.
    let token = s
        .split(|c: char| c.is_whitespace() || c == '.' || c == ',')
        .next()
        .unwrap_or("")
        .trim();
    if !token.is_empty() {
        normalize_status(token)
    } else {
        s.split_whitespace()
            .next()
            .map(normalize_status)
            .unwrap_or_default()
    }
}

/// Collect the lines of the first `> **Status:` blockquote (the contiguous run
/// of `>`-prefixed lines starting at the status line). Used for `Depends-On:`
/// extraction, which the canonical rule confines to this block.
fn status_blockquote_lines(body: &str) -> Vec<&str> {
    let mut lines = body.lines();
    // Advance to the first `> **Status:` line.
    let mut collecting = false;
    let mut out: Vec<&str> = Vec::new();
    for line in lines.by_ref() {
        let t = line.trim_start();
        if !collecting {
            if t.starts_with("> **Status:") {
                collecting = true;
                out.push(line);
            }
            continue;
        }
        // Contiguous blockquote: stop at the first non-`>` line.
        if t.starts_with('>') {
            out.push(line);
        } else {
            break;
        }
    }
    out
}

/// True iff `tok` is a date-prefixed plan stem (`YYYY-MM-DD-<kebab>`), the same
/// token shape the canonical `resolve-plan-deps.py` keeps.
fn is_plan_stem(tok: &str) -> bool {
    let b = tok.as_bytes();
    if b.len() < 12 {
        return false;
    }
    let digit = |i: usize| b[i].is_ascii_digit();
    if !(digit(0) && digit(1) && digit(2) && digit(3)) || b[4] != b'-' {
        return false;
    }
    if !(digit(5) && digit(6)) || b[7] != b'-' {
        return false;
    }
    if !(digit(8) && digit(9)) || b[10] != b'-' {
        return false;
    }
    let rest = &tok[11..];
    !rest.is_empty()
        && rest
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Extract canonical `Depends-On:` stems from the status blockquote.
///
/// Mirrors `resolve-plan-deps.py`: every case-sensitive `Depends-On:`
/// occurrence contributes the date-prefixed stem tokens on the REMAINDER of
/// that physical line; union deduped, order-preserving.
fn extract_depends_on(body: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in status_blockquote_lines(body) {
        let mut search_from = 0usize;
        while let Some(rel) = line[search_from..].find("Depends-On:") {
            let abs = search_from + rel;
            let remainder = &line[abs + "Depends-On:".len()..];
            for raw in remainder.split([' ', '\t', ',']) {
                let tok = raw.trim_matches(|c: char| !(c.is_alphanumeric() || c == '-'));
                if is_plan_stem(tok) && !out.iter().any(|e| e == tok) {
                    out.push(tok.to_string());
                }
            }
            search_from = abs + "Depends-On:".len();
        }
    }
    out
}

/// Detect `Phase N` headings (markdown `#`-heading or `**`-bold forms only —
/// never mid-line prose) and project each to a [`ParsedPhase`], deduped by
/// index (first occurrence wins), in document order.
fn detect_phases(body: &str) -> Vec<ParsedPhase> {
    let mut out: Vec<ParsedPhase> = Vec::new();
    for line in body.lines() {
        let t = line.trim();
        let started_bold = t.starts_with("**");
        // The heading text after the leading marker (`#...` or `**`).
        let rest = if t.starts_with('#') {
            t.trim_start_matches('#').trim_start()
        } else if started_bold {
            &t[2..]
        } else {
            continue;
        };
        // Must be `Phase` followed by a whitespace boundary, then digits.
        let after_phase = match rest.strip_prefix("Phase") {
            Some(a) => a,
            None => continue,
        };
        match after_phase.chars().next() {
            Some(c) if c.is_whitespace() => {}
            _ => continue,
        }
        let digits: String = after_phase
            .trim_start()
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        let index: u32 = match digits.parse() {
            Ok(n) => n,
            Err(_) => continue,
        };
        if out.iter().any(|p| p.index == index) {
            continue;
        }
        // Name: for a bold heading, the bold-span content (up to the closing
        // `**`); for a `#` heading, the whole line minus trailing markers.
        let name = if started_bold {
            match rest.find("**") {
                Some(i) => rest[..i].trim().to_string(),
                None => rest.trim_end_matches(['*']).trim().to_string(),
            }
        } else {
            rest.trim_end_matches(['#', '*']).trim().to_string()
        };
        out.push(ParsedPhase { index, name });
    }
    out
}

/// Parse a plan markdown body into a [`ParsedWorkUnit`]. Pure — no IO. `slug`
/// and `source_path` are caller-supplied (the slug comes from the filename, not
/// the body — see [`slug_from_filename`]).
///
/// Status defaults to `"draft"` when no `> **Status:` line is present; title
/// defaults to `None` when no `# ` H1 is present (both matching coord).
pub fn parse_work_unit(
    slug: &str,
    source_path: &str,
    body: &str,
    conv: &PlanConvention,
) -> ParsedWorkUnit {
    let mut title: Option<String> = None;
    let mut status: Option<String> = None;

    for line in body.lines() {
        let trimmed = line.trim_start();
        // First H1 wins.
        if title.is_none() {
            if let Some(rest) = trimmed.strip_prefix("# ") {
                let t = rest.trim();
                if !t.is_empty() {
                    title = Some(t.to_string());
                }
            }
        }
        // First `> **Status: ...` blockquote wins.
        if status.is_none() {
            if let Some(after_quote) = trimmed.strip_prefix("> ") {
                if let Some(after_bold) = after_quote.strip_prefix("**Status:") {
                    status = Some(parse_status_value(after_bold, conv));
                }
            }
        }
        if title.is_some() && status.is_some() {
            break;
        }
    }

    ParsedWorkUnit {
        slug: slug.to_string(),
        title,
        status: status.unwrap_or_else(|| "draft".to_string()),
        depends_on: extract_depends_on(body),
        phases: detect_phases(body),
        source_path: source_path.to_string(),
        content: body.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conv() -> PlanConvention {
        PlanConvention::operator_default()
    }

    fn parse(body: &str) -> ParsedWorkUnit {
        parse_work_unit("test-slug", "plans/test-slug.md", body, &conv())
    }

    // --- slug derivation -----------------------------------------------------

    #[test]
    fn slug_strips_dir_and_extension() {
        assert_eq!(
            slug_from_filename("D:/qontinui-root/plans/2026-06-18-foo-bar.md"),
            "2026-06-18-foo-bar"
        );
        assert_eq!(
            slug_from_filename("plans\\2026-06-18-foo.md"),
            "2026-06-18-foo"
        );
        assert_eq!(slug_from_filename("2026-06-18-foo"), "2026-06-18-foo");
    }

    // --- status tokenization (ported coord edge cases, opaque variant) -------

    #[test]
    fn multiword_in_progress_with_trailing_date() {
        let u = parse("# T\n\n> **Status: IN PROGRESS 2026-05-19.** more prose here\n");
        assert_eq!(u.status, "in_progress");
    }

    #[test]
    fn single_word_status_with_trailing_bold_and_punct() {
        assert_eq!(parse("> **Status: shipped**\n").status, "shipped");
        assert_eq!(parse("> **Status: VETTED.**\n").status, "vetted");
    }

    #[test]
    fn unknown_status_survives_verbatim_not_rejected() {
        // coord would fall back to a token too, but the point here is the
        // status is KEPT (opaque), not dropped/rejected.
        let u = parse("> **Status: IMPLEMENTED 2026-06-19 — PRs open.** body\n");
        assert_eq!(u.status, "implemented");
    }

    #[test]
    fn shipped_decoy_later_in_block_does_not_win() {
        // The lifecycle word is at the START of the stamp; a "Flip to SHIPPED"
        // phrase later in the same block must not override it.
        let body = "# T\n\n> **Status: IMPLEMENTED 2026-06-19.** Flip to SHIPPED once it lands.\n";
        assert_eq!(parse(body).status, "implemented");
    }

    #[test]
    fn absent_status_defaults_to_draft() {
        assert_eq!(parse("# Title only\n\nbody\n").status, "draft");
    }

    #[test]
    fn first_h1_wins_and_status_block_wins() {
        let u = parse("# First\n# Second\n\n> **Status: vetted**\n");
        assert_eq!(u.title.as_deref(), Some("First"));
        assert_eq!(u.status, "vetted");
    }

    #[test]
    fn drafty_does_not_match_draft_boundary() {
        // Boundary check: `drafty` is not `draft`; falls back to the token.
        assert_eq!(parse("> **Status: drafty**\n").status, "drafty");
    }

    // --- depends_on ----------------------------------------------------------

    #[test]
    fn depends_on_extracts_stem_from_status_block() {
        let body = "# T\n\n> **Status: VETTED 2026-06-19.** summary. Depends-On: 2026-06-18-coord-generic-work-unit-primitive.\n";
        assert_eq!(
            parse(body).depends_on,
            vec!["2026-06-18-coord-generic-work-unit-primitive".to_string()]
        );
    }

    #[test]
    fn depends_on_ignores_prose_and_bare_dates_outside_block() {
        // A stem mentioned in the body (not the status block) is NOT a dep.
        let body = "# T\n\n> **Status: vetted**\n\nSee 2026-01-01-some-other-plan in the body.\n";
        assert!(parse(body).depends_on.is_empty());
    }

    #[test]
    fn depends_on_dedupes_and_preserves_order() {
        let body = "> **Status: vetted. Depends-On: 2026-01-01-a, 2026-01-02-b\n> trailing. Depends-On: 2026-01-01-a, 2026-01-03-c\n\nbody";
        assert_eq!(
            parse(body).depends_on,
            vec![
                "2026-01-01-a".to_string(),
                "2026-01-02-b".to_string(),
                "2026-01-03-c".to_string()
            ]
        );
    }

    // --- phases --------------------------------------------------------------

    #[test]
    fn phases_detect_bold_and_heading_forms_not_prose() {
        let body = "# T\n\n> **Status: in progress 2026-06-19.** Implementing Phase 1 only; gate Phases 2-4.\n\n**Phase 1 — parser.** body\n\n## Phase 2: push client\n\nProse mentioning Phase 1 again should not count.\n";
        let p = parse(body);
        assert_eq!(p.phases.len(), 2);
        assert_eq!(
            p.phases[0],
            ParsedPhase {
                index: 1,
                name: "Phase 1 — parser.".to_string()
            }
        );
        assert_eq!(p.phases[1].index, 2);
        assert_eq!(p.phases[1].name, "Phase 2: push client");
    }

    // --- golden-file tests over the real plans corpus ------------------------

    #[test]
    fn golden_p1_primitive_opaque_implemented_no_deps() {
        let body = include_str!("fixtures/2026-06-18-coord-generic-work-unit-primitive.md");
        let u = parse_work_unit(
            "2026-06-18-coord-generic-work-unit-primitive",
            "plans/2026-06-18-coord-generic-work-unit-primitive.md",
            body,
            &conv(),
        );
        assert_eq!(
            u.status, "implemented",
            "decoy 'Flip to SHIPPED' must not win"
        );
        assert!(u.depends_on.is_empty(), "foundation plan has no Depends-On");
        assert!(u.title.is_some());
    }

    #[test]
    fn golden_adapter_in_progress_with_dep_and_four_phases() {
        let body = include_str!("fixtures/2026-06-18-harness-markdown-to-workunit-adapter.md");
        let u = parse_work_unit(
            "2026-06-18-harness-markdown-to-workunit-adapter",
            "plans/2026-06-18-harness-markdown-to-workunit-adapter.md",
            body,
            &conv(),
        );
        assert_eq!(u.status, "in_progress");
        assert_eq!(
            u.depends_on,
            vec!["2026-06-18-coord-generic-work-unit-primitive".to_string()]
        );
        assert_eq!(u.phases.len(), 4, "phases: {:?}", u.phases);
        assert_eq!(u.phases.first().map(|p| p.index), Some(1));
        assert_eq!(u.phases.last().map(|p| p.index), Some(4));
        assert!(u.phases[0].name.contains("parser"));
        assert!(u.phases[3].name.contains("parity"));
    }

    #[test]
    fn golden_draft_vetted_in_progress_status_and_deps() {
        let cases: &[(&str, &str, &str, &[&str])] = &[
            (
                "fixtures/2026-06-18-coord-decommission-markdown-plan-ingest.md",
                include_str!("fixtures/2026-06-18-coord-decommission-markdown-plan-ingest.md"),
                "draft",
                &[],
            ),
            (
                "fixtures/2026-06-18-coord-orchestration-generalize-off-plans.md",
                include_str!("fixtures/2026-06-18-coord-orchestration-generalize-off-plans.md"),
                "vetted",
                &["2026-06-18-coord-generic-work-unit-primitive"],
            ),
            (
                "fixtures/2026-06-18-coord-merge-scheduler-db-test-harness.md",
                include_str!("fixtures/2026-06-18-coord-merge-scheduler-db-test-harness.md"),
                "in_progress",
                &["2026-06-17-coord-land-push-failure-wedge-and-terminal-classification"],
            ),
        ];
        for (path, body, want_status, want_deps) in cases {
            let u = parse_work_unit(&slug_from_filename(path), path, body, &conv());
            assert_eq!(&u.status, want_status, "status mismatch for {path}");
            let want: Vec<String> = want_deps.iter().map(|s| s.to_string()).collect();
            assert_eq!(u.depends_on, want, "deps mismatch for {path}");
        }
    }
}
