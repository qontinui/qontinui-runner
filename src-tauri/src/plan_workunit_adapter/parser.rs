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
//!    sub-unit per declared phase — see [`detect_phases`]), which coord discarded.

use once_cell::sync::Lazy;
use regex::Regex;

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
    /// The phase number the plan declares (heading, bold line, list item or
    /// phase-table row — see [`detect_phases`]).
    pub index: u32,
    /// The phase's name: bold-span content, heading line, list-item text or
    /// the phase-table row's second cell, trimmed of markers.
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

/// `YYYY-MM-DD` from a date-prefixed stem, as an RFC-3339 UTC midnight.
///
/// The one place a plan's authoring date is derived, for BOTH sinks: the
/// plan-library artifact's `authored_at` (`body_push`) and the coord work-unit
/// upsert's `authored_at` (`push`). Two writers with one derivation converge on
/// one answer under coord's COALESCE; a second derivation is how they drift.
/// The shape is anchored — four digits, `-`, two, `-`, two, `-` — so
/// `feature-2026-01-01-x` is NOT dated, and Phase A's SQL backfill mirrors it
/// (`slug ~ '^\d{4}-\d{2}-\d{2}-'`).
///
/// Returns `None` for an undated stem (the three root `prompts/` files, and
/// any plan named without a date) rather than inventing a date — absent is
/// UNKNOWN, which each sink renders honestly; a fabricated one would not be.
///
/// The shape check alone is not enough: `2026-02-30-bogus` has the shape and
/// is not a date. coord deserializes the field as `Option<DateTime<Utc>>`, so
/// an impossible date would reject the WHOLE upsert — title, status and
/// metadata included — not just the date. The calendar check
/// (`NaiveDate::from_ymd_opt`) turns such a stem into `None`, which the wire
/// omits, so the rest of the upsert still lands.
pub fn authored_at_from_stem(stem: &str) -> Option<String> {
    let b = stem.as_bytes();
    if b.len() < 11 {
        return None;
    }
    let digit = |i: usize| b[i].is_ascii_digit();
    if !(digit(0) && digit(1) && digit(2) && digit(3)) || b[4] != b'-' {
        return None;
    }
    if !(digit(5) && digit(6)) || b[7] != b'-' {
        return None;
    }
    if !(digit(8) && digit(9)) || b[10] != b'-' {
        return None;
    }
    // Shape verified above, so these parses cannot fail; the calendar can.
    let y: i32 = stem[..4].parse().ok()?;
    let m: u32 = stem[5..7].parse().ok()?;
    let d: u32 = stem[8..10].parse().ok()?;
    chrono::NaiveDate::from_ymd_opt(y, m, d)?;
    Some(format!("{}T00:00:00Z", &stem[..10]))
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

/// A heading that opens a **phase-list section** (arm C of [`detect_phases`]):
/// after one leading enumerator (`4.`, `6.1`, `a.`, `§3`) is stripped, the
/// heading's first word or two are `Phases` / `Phasing` — `## 4. Phases`,
/// `## Proposed phases`, `### Phasing & value order`.
///
/// Anchored to the START of the heading on purpose: an unanchored `phases`
/// match fired on `## Named follow-ups (deliberately NOT phases)` in the
/// corpus census, turning a list of explicitly-NOT-phases into declarations.
static PHASE_LIST_HEADING: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)^(?:[\w-]+\s*(?:/|&|and)?\s*)?phas(?:es|ing)\b").expect("valid regex")
});

/// One leading section enumerator on a heading: `4.`, `6.1`, `4)`, `§3`, `a.`.
static HEADING_ENUMERATOR: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)^(?:§?\d+(?:\.\d+)*[.)]?|[a-z][.)])\s+").expect("valid regex"));

/// A heading that is itself a single phase (`Phase 2 — push client`), which
/// never opens a phase-LIST section.
static SINGLE_PHASE_HEADING: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)^\W*phase\s+\d").expect("valid regex"));

/// A table data row's first cell naming a phase: `1`, `**2**`, `Phase 3`, `P4`.
static PHASE_TABLE_CELL: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^\**(?:Phase\s+|P)?(\d+)").expect("valid regex"));

/// A table's separator-row cell: `---`, `:---`, `---:`, `:-:`.
static TABLE_SEPARATOR_CELL: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^:?-+:?$").expect("valid regex"));

/// `rest` begins with `Phase`, a whitespace boundary, then digits — the token
/// shape arms A and B key on. Returns the index.
fn phase_index_at(rest: &str) -> Option<u32> {
    let after = rest.strip_prefix("Phase")?;
    if !after.chars().next().is_some_and(char::is_whitespace) {
        return None;
    }
    let digits: String = after
        .trim_start()
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().ok()
}

/// The content of a bold span, given the text just AFTER its opening `**`:
/// everything up to the closing `**`, or the whole remainder when the span
/// does not close on this line.
fn bold_span(s: &str) -> String {
    match s.find("**") {
        Some(i) => s[..i].trim().to_string(),
        None => s.trim_end_matches(['*']).trim().to_string(),
    }
}

/// A list item's ordinal and content: `- x` / `* x` / `+ x` → `(None, "x")`,
/// `3. x` / `3) x` → `(Some(3), "x")`; `None` for a line that is not a list
/// item. `t` is already trimmed.
fn list_item(t: &str) -> Option<(Option<u32>, &str)> {
    for bullet in ["- ", "* ", "+ "] {
        if let Some(rest) = t.strip_prefix(bullet) {
            return Some((None, rest.trim_start()));
        }
    }
    let digits_end = t.find(|c: char| !c.is_ascii_digit())?;
    if digits_end == 0 {
        return None;
    }
    let rest = &t[digits_end..];
    let rest = rest.strip_prefix('.').or_else(|| rest.strip_prefix(')'))?;
    if !rest.chars().next().is_some_and(char::is_whitespace) {
        return None;
    }
    let n = t[..digits_end].parse().ok()?;
    Some((Some(n), rest.trim_start()))
}

/// Where the scan stands relative to a Markdown table.
#[derive(Clone, Copy, PartialEq, Eq)]
enum TableState {
    /// Not inside a table.
    Outside,
    /// Inside a table whose header's first cell is `Phase` — its rows declare.
    PhaseTable,
    /// Inside any other table — its rows are ignored.
    OtherTable,
}

/// Detect the phases a plan DECLARES and project each to a [`ParsedPhase`],
/// deduped by index (first occurrence in document order wins), in document
/// order. Fenced code blocks are skipped, and so is any list item or table
/// row whose content opens with `~~` — a struck-through phase was dropped,
/// and declaring it would hand coord a phase that can never be delivered.
///
/// Four arms, chosen from a census of how the plan corpus actually spells its
/// phases (plan
/// `2026-09-19-runner-detect-phases-misses-plans-that-list-phases-in-a-table-or-prose`):
///
/// - **A** — a `#`-heading or `**`-bold line: `Phase`, whitespace, digits.
/// - **B** — a column-0 list item (`-`/`*`/`+`/`N.`/`N)`) opening `**Phase N`.
///   Column 0 only: an indented sub-bullet is commentary about a phase.
/// - **C** — a column-0 ordered item `N.` / `N)` in the DIRECT body of a
///   phase-list section ([`PHASE_LIST_HEADING`]), i.e. before the next heading
///   of any level — so numbered STEPS under a `### Phase 1` inside `## Phases`
///   are not read as phases.
/// - **D** — a data row of a table whose header's first cell is `Phase`, with a
///   first cell of `N` / `**N**` / `Phase N` / `PN`.
///
/// Mid-line prose never declares, and neither does a blockquote line (status
/// narration such as `> **Phase 4 is HELD**`).
fn detect_phases(body: &str) -> Vec<ParsedPhase> {
    let mut out: Vec<ParsedPhase> = Vec::new();
    let mut push = |index: u32, name: String| {
        if !out.iter().any(|p| p.index == index) {
            out.push(ParsedPhase { index, name });
        }
    };
    let mut in_fence = false;
    let mut in_phase_list = false;
    let mut table = TableState::Outside;

    for line in body.lines() {
        let t = line.trim();
        if t.starts_with("```") || t.starts_with("~~~") {
            in_fence = !in_fence;
            table = TableState::Outside;
            continue;
        }
        if in_fence {
            continue;
        }
        if !t.starts_with('|') {
            table = TableState::Outside;
        }

        // Arm A, heading form. Every real heading (1-6 `#` then whitespace)
        // also re-decides whether the scan is inside a phase-list section.
        if t.starts_with('#') {
            let rest = t.trim_start_matches('#');
            let level = t.len() - rest.len();
            if level <= 6 && rest.starts_with(char::is_whitespace) {
                let text = HEADING_ENUMERATOR.replace(rest.trim(), "");
                in_phase_list =
                    PHASE_LIST_HEADING.is_match(&text) && !SINGLE_PHASE_HEADING.is_match(&text);
            }
            let rest = rest.trim_start();
            if let Some(index) = phase_index_at(rest) {
                push(index, rest.trim_end_matches(['#', '*']).trim().to_string());
            }
            continue;
        }

        // Arm A, bold form.
        if let Some(rest) = t.strip_prefix("**") {
            if let Some(index) = phase_index_at(rest) {
                push(index, bold_span(rest));
            }
            continue;
        }

        // Arms B and C: column-0 list items only.
        if !line.starts_with([' ', '\t']) {
            if let Some((ordinal, content)) = list_item(t) {
                if content.starts_with("~~") {
                    continue;
                }
                if let Some(rest) = content.strip_prefix("**") {
                    if let Some(index) = phase_index_at(rest) {
                        push(index, bold_span(rest));
                        continue;
                    }
                }
                if let (true, Some(index)) = (in_phase_list, ordinal) {
                    let name = match content.strip_prefix("**") {
                        Some(rest) => bold_span(rest),
                        None => content.to_string(),
                    };
                    push(index, name);
                }
                continue;
            }
        }

        // Arm D: tables.
        if t.starts_with('|') {
            let cells: Vec<&str> = t.trim_matches('|').split('|').map(str::trim).collect();
            let first = cells.first().copied().unwrap_or("");
            match table {
                TableState::Outside => {
                    let is_phase = first.trim_matches('*').trim().eq_ignore_ascii_case("phase");
                    table = if is_phase {
                        TableState::PhaseTable
                    } else {
                        TableState::OtherTable
                    };
                }
                TableState::PhaseTable
                    if !TABLE_SEPARATOR_CELL.is_match(first) && !first.starts_with("~~") =>
                {
                    let index = PHASE_TABLE_CELL
                        .captures(first)
                        .and_then(|caps| caps[1].parse().ok());
                    if let Some(index) = index {
                        let name = cells
                            .get(1)
                            .copied()
                            .filter(|c| !c.is_empty())
                            .unwrap_or(first)
                            .trim_matches('*')
                            .trim()
                            .to_string();
                        push(index, name);
                    }
                }
                _ => {}
            }
        }
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

    #[test]
    fn authored_at_from_a_dated_stem() {
        assert_eq!(
            authored_at_from_stem("2026-08-10-plan-and-prompt-library-in-web").as_deref(),
            Some("2026-08-10T00:00:00Z")
        );
        assert_eq!(authored_at_from_stem("merge-queue-report"), None);
        assert_eq!(
            authored_at_from_stem("2026-08-10"),
            None,
            "needs the trailing -"
        );
    }

    /// The undated root `prompts/` stems yield no date rather than a fabricated
    /// one (moved here from `body_push`'s kind-classification test).
    #[test]
    fn authored_at_from_an_undated_prompt_stem_is_none() {
        assert_eq!(
            authored_at_from_stem("feature-pr-failure-reasons-prompt"),
            None
        );
    }

    /// The date prefix is anchored at the stem's start — the same `^` as Phase
    /// A's SQL backfill — so an embedded date does not count, and a stem that
    /// is exactly the date plus the dash still needs a body after it to be
    /// eleven bytes wide.
    #[test]
    fn authored_at_prefix_is_anchored_and_shape_checked() {
        assert_eq!(authored_at_from_stem("feature-2026-01-01-x"), None);
        assert_eq!(authored_at_from_stem("2026-1-01-short-month"), None);
        assert_eq!(authored_at_from_stem("2026_01_01-underscores"), None);
        assert_eq!(
            authored_at_from_stem("2026-09-02-x").as_deref(),
            Some("2026-09-02T00:00:00Z")
        );
    }

    /// A stem with the right shape but an impossible calendar date yields
    /// `None`: coord parses the field as `Option<DateTime<Utc>>` and would
    /// reject the whole upsert on a bogus date, so it must never be sent.
    #[test]
    fn authored_at_rejects_impossible_calendar_dates() {
        assert_eq!(authored_at_from_stem("2026-02-30-bogus"), None);
        assert_eq!(authored_at_from_stem("2026-13-01-x"), None);
        assert_eq!(authored_at_from_stem("2026-00-10-x"), None);
        assert_eq!(authored_at_from_stem("2026-04-31-x"), None);
        assert_eq!(
            authored_at_from_stem("2024-02-29-leap").as_deref(),
            Some("2024-02-29T00:00:00Z"),
            "a real leap day is a real date"
        );
        assert_eq!(authored_at_from_stem("2023-02-29-x"), None);
        assert_eq!(
            authored_at_from_stem("2026-12-31-x").as_deref(),
            Some("2026-12-31T00:00:00Z")
        );
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

    fn indices(body: &str) -> Vec<u32> {
        parse(body).phases.iter().map(|p| p.index).collect()
    }

    /// Arm B: a column-0 bullet or ordered item opening `**Phase N`. The
    /// indented sub-bullet is commentary, not a declaration.
    #[test]
    fn phases_arm_b_list_item_opening_bold_phase() {
        let body = "# T\n\n- **Phase 1 — schema.** body\n1. **Phase 2 — wiring** (~40 LOC)\n  - **Phase 9 depends on this** indented\n";
        let p = parse(body);
        assert_eq!(indices(body), vec![1, 2]);
        assert_eq!(p.phases[0].name, "Phase 1 — schema.");
        assert_eq!(p.phases[1].name, "Phase 2 — wiring");
    }

    /// Arm C: ordered items in the DIRECT body of a phase-list section; the
    /// section ends at the next heading of any level, and a nested item is not
    /// column 0.
    #[test]
    fn phases_arm_c_numbered_list_under_a_phases_heading() {
        let body = "# T\n\n## 4. Phases\n\n1. **Schema freeze** — author the schemas\n2. Rubric semantics\n   3. nested step\n\n## 5. Risks\n\n7. not a phase\n";
        let p = parse(body);
        assert_eq!(indices(body), vec![1, 2]);
        assert_eq!(p.phases[0].name, "Schema freeze");
        assert_eq!(p.phases[1].name, "Rubric semantics");
    }

    /// Arm C's heading rule is anchored: a heading that merely MENTIONS phases
    /// (measured in the corpus) opens no phase list.
    #[test]
    fn phases_arm_c_ignores_a_heading_that_only_mentions_phases() {
        let body = "# T\n\n## Named follow-ups (deliberately NOT phases)\n\n1. one\n2. two\n";
        assert!(indices(body).is_empty());
    }

    /// Numbered STEPS under a `### Phase N` inside `## Phases` are steps, not
    /// phases: the sub-heading ends the phase list's direct body.
    #[test]
    fn phases_arm_c_steps_under_a_phase_subheading_are_not_phases() {
        let body =
            "# T\n\n## 5. Phases\n\n### Phase 1 — the guard\n\n1. hook\n2. prefilter\n3. message\n";
        assert_eq!(indices(body), vec![1]);
    }

    /// Arm D: a table whose header's first cell is `Phase`. The separator row
    /// and a struck-through row declare nothing; a table under any other
    /// header declares nothing.
    #[test]
    fn phases_arm_d_table_keyed_on_a_phase_header() {
        let body = "# T\n\n| Phase | Deliverable |\n|---|---|\n| 1 | re-export forms |\n| **2** | basis |\n| Phase 3 | label |\n| ~~4~~ | dropped |\n\n| # | Claim |\n|---|---|\n| 5 | not a phase |\n";
        let p = parse(body);
        assert_eq!(indices(body), vec![1, 2, 3]);
        assert_eq!(p.phases[0].name, "re-export forms");
    }

    /// A struck-through item is a DROPPED phase: declaring it would give coord a
    /// phase that can never be delivered.
    #[test]
    fn phases_struck_through_items_are_not_declared() {
        let body = "# T\n\n## Phases\n\n0. probe\n1. ~~relay~~ **DROPPED**\n2. migrate\n- ~~**Phase 7 — gone**~~\n";
        assert_eq!(indices(body), vec![0, 2]);
    }

    /// Fenced code and blockquotes never declare — a fenced example of the
    /// grammar, or status narration, is not a phase list.
    #[test]
    fn phases_fences_and_blockquotes_do_not_declare() {
        let body = "# T\n\n> **Phase 4 is HELD**, not forgotten.\n\n```\n## Phase 7\n**Phase 8 — example**\n| Phase | x |\n|---|---|\n| 9 | y |\n```\n\n~~~\n- **Phase 6 — example**\n~~~\n";
        assert!(indices(body).is_empty());
    }

    /// Regression for the measured unit (`cf9ae0b2`): the parser used to
    /// record only `{0}` from the bold `**Phase 0 is a gate, not a step.**`
    /// line. The plan's real declaration is the numbered list under
    /// `## 4. Phases` — `0.` … `5.`, with `1.` struck through as DROPPED —
    /// which is exactly what its own status block reports.
    #[test]
    fn golden_measured_unit_declares_phases_0_2_3_4_5() {
        let body = include_str!(
            "fixtures/2026-09-04-coord-jetstream-durable-consumers-redis-subscriber-migration.md"
        );
        let u = parse_work_unit(
            "2026-09-04-coord-jetstream-durable-consumers-redis-subscriber-migration",
            "plans/2026-09-04-coord-jetstream-durable-consumers-redis-subscriber-migration.md",
            body,
            &conv(),
        );
        let got: Vec<u32> = u.phases.iter().map(|p| p.index).collect();
        assert_eq!(got, vec![0, 2, 3, 4, 5], "phases: {:?}", u.phases);
    }

    /// Reproduces the plan-corpus census from the REAL parser (rather than a
    /// port): point `QONTINUI_PHASE_CENSUS_DIR` at a `plans/` directory and run
    /// `cargo test -p qontinui-runner --lib phase_census -- --ignored --nocapture`.
    #[test]
    #[ignore = "reads a plans directory named by QONTINUI_PHASE_CENSUS_DIR"]
    fn phase_census() {
        let dir = std::env::var("QONTINUI_PHASE_CENSUS_DIR")
            .expect("set QONTINUI_PHASE_CENSUS_DIR to a plans/ directory");
        let (mut plans, mut zero, mut one, mut many) = (0u32, 0u32, 0u32, 0u32);
        for entry in std::fs::read_dir(&dir)
            .expect("readable plans dir")
            .flatten()
        {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.ends_with(".md") || authored_at_from_stem(&name[..name.len() - 3]).is_none() {
                continue;
            }
            let Ok(body) = std::fs::read_to_string(entry.path()) else {
                continue;
            };
            plans += 1;
            match detect_phases(&body).len() {
                0 => zero += 1,
                1 => one += 1,
                _ => many += 1,
            }
        }
        println!("phase_census plans={plans} zero={zero} one={one} two_or_more={many}");
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
