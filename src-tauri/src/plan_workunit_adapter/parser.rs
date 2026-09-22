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
    /// The plan's `Area:` from the status blockquote, when it satisfies coord's
    /// kebab grammar — pushed as work-unit `metadata.area`. `None` when absent
    /// or rejected (a rejection is warned; see [`extract_area`]).
    pub area: Option<String>,
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

/// coord's own area grammar, `[a-z0-9]+(-[a-z0-9]+)*`: non-empty, lowercase
/// ASCII letters and digits in segments joined by single hyphens.
///
/// A byte-for-byte mirror of `is_kebab_case` in `qontinui-coord`
/// `crates/coord/src/work_unit_registry.rs`, which is what coord's
/// `area=` filter and its `metadata_area_warnings` judge a stored area by. A
/// looser local predicate (`^[a-z0-9][a-z0-9-]*$`) would admit `a--b` and
/// `a-`, which coord would then warn on and no canonical filter would match.
pub(crate) fn is_kebab_case(s: &str) -> bool {
    !s.is_empty()
        && s.split('-').all(|seg| {
            !seg.is_empty()
                && seg
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        })
}

/// What the status blockquote declares about the plan's area.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AreaDecl {
    /// No `Area:` key in the status blockquote (including a plan whose only
    /// `Area:` line sits outside it). Not a defect — nothing is warned.
    Absent,
    /// A key whose value satisfies [`is_kebab_case`].
    Accepted(String),
    /// A key whose value does not — kept verbatim so the warning can name it.
    Rejected(String),
}

/// Extract the plan's `Area:` from the status blockquote — the same block, and
/// the same case-sensitive key convention, as [`extract_depends_on`]. A
/// body-wide scan would pick up quoted `Area:` examples in plan prose.
///
/// **The key.** An occurrence of `Area:` counts only when it stands as a key:
/// after stripping one optional `**` (the bold opener of `**Area:**`), what
/// precedes it must be the line start, whitespace, or the blockquote `>`. So
/// `> Area: x`, `> **Area:** x` and `**Repo:** … **Area:** x` are keys, while
/// `` `Area:` `` (inline code, as in "scanner `Area:` parse") and `Sub-Area:`
/// are not. The FIRST key in the block decides; a later one is never consulted,
/// so a malformed declaration is reported rather than silently shadowed.
///
/// **The value.** After the key, one optional `**` (the bold closer) and any
/// whitespace are skipped. Then:
///
/// * if the next character is a backtick, the value is everything up to the
///   next backtick (`` `published-parity` (work-unit …) `` → `published-parity`;
///   an unclosed backtick takes the rest of the line, which then fails the
///   grammar);
/// * otherwise it is the first whitespace-delimited token with trailing
///   `.`, `,`, `;`, `:`, `)` and `*` removed (`Area: domain-cost-ledger.` →
///   `domain-cost-ledger`).
///
/// Nothing else is normalised — no lowercasing, no `_`→`-`. The value is then
/// judged by [`is_kebab_case`]: `` `agent_worktree` reclaim `` is
/// [`AreaDecl::Rejected`] with `agent_worktree`, never guessed into shape.
pub(crate) fn extract_area(body: &str) -> AreaDecl {
    const KEY: &str = "Area:";
    for line in status_blockquote_lines(body) {
        let mut search_from = 0usize;
        while let Some(rel) = line[search_from..].find(KEY) {
            let abs = search_from + rel;
            search_from = abs + KEY.len();
            let before = &line[..abs];
            let before = before.strip_suffix("**").unwrap_or(before);
            let boundary = before
                .chars()
                .next_back()
                .is_none_or(|c| c.is_whitespace() || c == '>');
            if !boundary {
                continue;
            }
            let rest = &line[abs + KEY.len()..];
            let rest = rest.strip_prefix("**").unwrap_or(rest).trim_start();
            let value = match rest.strip_prefix('`') {
                Some(code) => code.split('`').next().unwrap_or(code),
                None => rest
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
                    .trim_end_matches(['.', ',', ';', ':', ')', '*']),
            };
            return if is_kebab_case(value) {
                AreaDecl::Accepted(value.to_string())
            } else {
                AreaDecl::Rejected(value.to_string())
            };
        }
    }
    AreaDecl::Absent
}

/// A heading that opens a **phase-list section** (arms B and C of
/// [`detect_phases`]), tested after one leading enumerator
/// ([`HEADING_ENUMERATOR`]) is stripped: `Phases` / `Phasing`, optionally
/// after ONE qualifier from a closed list — `## 4. Phases`,
/// `## Proposed phases`, `## Rollout / phasing`.
///
/// A closed qualifier list, not "any first word": an open one admitted
/// `### Not phases`, `## Deferred phases` and `## Why Phases 6 and 7 have not
/// landed` from the real corpus, each of which would turn a list of
/// explicitly-NOT-phases into declarations.
static PHASE_LIST_HEADING: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?i)^(?:(?:proposed|implementation|implementing|execution|delivery|rollout|revised|planned|remaining|the)\s*(?:/|&|and)?\s*)?phas(?:es|ing)\b",
    )
    .expect("valid regex")
});

/// Refusal half of [`PHASE_LIST_HEADING`] (the `regex` crate has no
/// look-ahead): the same prefix followed by `4–5 …`, `A-B …` or `A and B …`,
/// i.e. a heading naming SPECIFIC phases (`Phases 4–5 remain DEFERRED`).
/// Case-sensitive on the capital, so `Phases and risks` still opens a list.
static PHASE_LIST_HEADING_NAMES_PHASES: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"^(?i:(?:proposed|implementation|implementing|execution|delivery|rollout|revised|planned|remaining|the)\s*(?:/|&|and)?\s*)?(?i:phas(?:es|ing))\s+(?:\d|[A-Z](?:[-–]|\s+and\b))",
    )
    .expect("valid regex")
});

/// Text in a phase-list section's introduction that says its list is not
/// this plan's phases (`Under Option B, Phases 1–4 do not exist`).
static NOT_A_PHASE_LIST: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)\bdo(?:es)? not exist\b|\bsuperseded\b").expect("valid regex"));

/// One leading section enumerator on a heading: `4.`, `6.1`, `4)`, `§3`, `a.`.
static HEADING_ENUMERATOR: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)^(?:§?\d+(?:\.\d+)*[.)]?|[a-z][.)])\s+").expect("valid regex"));

/// A phase TITLE — `Phase N` (optionally lettered, `2b`) followed by nothing,
/// a dash, a colon, an opening parenthesis or a sentence-ending period — as
/// opposed to a sentence ABOUT a phase (`Phase 3's check could …`, `Phase 0 is
/// a gate`, `Phase 0.3 — …`). Arm B declares only titles; a bold title line
/// ends a phase list (its numbered items are that phase's STEPS); and a title
/// outranks a sentence when both name the same index.
static PHASE_TITLE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"^Phase\s+(\d+)([a-z]?)(?:\s*$|\s*[—–:(\-]|\.\s|\.$)").expect("valid regex")
});

/// `Phase` then a LETTERED number (`Phase 1e`, `Phase 2b`).
static LETTERED_PHASE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^Phase\s+\d+[A-Za-z]").expect("valid regex"));

/// A phase title whose name is struck after its prefix:
/// `**Phase 1 — ~~make machine.json …~~ ALREADY EXECUTED**`.
static STRUCK_PHASE_TITLE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^Phase\s+\d+[a-z]?\s*[—–:\-]\s*~~").expect("valid regex"));

/// A list item or table row the plan itself marks as not delivered BY THIS
/// PLAN. Arms B-D are inferred from lists and tables, which routinely carry
/// such rows (`DELEGATED to their own plans`, `Not built`, `NOT NEEDED`,
/// `(optional, later)`); declaring one would leave coord a phase that can
/// never be covered. Arm A — an explicit `Phase N` heading — is NOT filtered:
/// that is the author naming a phase, and whether it was deferred is its
/// delivery state, not whether it exists (filtering headings too removes
/// currently-declared phases, because these words also occur in ordinary
/// phase titles; the census in the plan named on [`detect_phases`] measured it).
static NOT_DELIVERED_MARKER: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?i)not scheduled|not this plan|not built|not needed|\(defer|\bdeferred\b|\bdelegated\b|\bdropped\b|\bwithdrawn\b|\bsplit (?:out|to|into)\b|follow-?up plan|separate plan|\bits own plan\b|\bown plan, own vet\b|\bdischarged\b|\bmoot\b|\(optional\b|\bstretch\b|\bskip if\b|\bseparately\b|\(separate\)|\balready exists?\b",
    )
    .expect("valid regex")
});

/// A phase-table data row's first cell: `1`, `**2**`, `2b`, `Phase 3`, `P4`,
/// `1a — base theory doc`, `3 (coord)`. After the number only the end of the
/// cell or a title separator may follow, so `1.5`, `3(a)` and `33 of 52 rustc
/// units` do not declare their leading integer; ranges are refused separately
/// ([`PHASE_TABLE_RANGE`]).
static PHASE_TABLE_CELL: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)^\**(?:Phase\s+|P)?(\d+)([a-z]?)\**(?:$|\s*[—:]|\s+[–-]\s|\s+\()")
        .expect("valid regex")
});

/// A range in a phase-table cell — `4 – 6`, `4—6`, `Phase 4 - 6` — names
/// several phases at once and is not a declaration of its first.
static PHASE_TABLE_RANGE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)^\**(?:Phase\s+|P)?\d+\s*[–—-]\s*\d").expect("valid regex"));

/// A table's separator-row cell: `---`, `:---`, `---:`, `:-:`.
static TABLE_SEPARATOR_CELL: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^:?-+:?$").expect("valid regex"));

/// A fence line: three or more backticks or tildes, then an info string. A
/// backtick fence's info string may not contain a backtick (CommonMark), so
/// ```` ```x` ```` is inline code, not a fence.
fn fence_line(t: &str) -> Option<(char, usize, bool)> {
    static FENCE_LINE: Lazy<Regex> =
        Lazy::new(|| Regex::new(r"^(`{3,}|~{3,})(.*)$").expect("valid regex"));
    let caps = FENCE_LINE.captures(t)?;
    let ch = caps[1].chars().next()?;
    if ch == '`' && caps[2].contains('`') {
        return None;
    }
    Some((ch, caps[1].len(), caps[2].trim().is_empty()))
}

/// `rest` begins with `Phase`, a whitespace boundary, then digits — the token
/// shape arm A has always keyed on. Returns the index.
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
/// item. `t` is already trimmed. The marker must be followed by whitespace, so
/// `-x` and `1.5 x` are not items.
fn list_item(t: &str) -> Option<(Option<u32>, &str)> {
    let marker_end = if t.starts_with(['-', '*', '+']) {
        1
    } else {
        let digits_end = t.find(|c: char| !c.is_ascii_digit())?;
        if digits_end == 0 || !t[digits_end..].starts_with(['.', ')']) {
            return None;
        }
        digits_end + 1
    };
    let rest = &t[marker_end..];
    if !rest.starts_with(char::is_whitespace) {
        return None;
    }
    let ordinal = if marker_end == 1 {
        None
    } else {
        Some(t[..marker_end - 1].parse().ok()?)
    };
    Some((ordinal, rest.trim_start()))
}

/// Content that opens struck through — `~~x~~` or `**~~x~~**` — was dropped.
fn is_struck(content: &str) -> bool {
    content.trim_start_matches('*').starts_with("~~")
}

/// Which lines sit inside a fenced code block. A fence closes only on the SAME
/// character, at least as long, with no info string. A fence still open at the
/// end of the document is treated as NOT a fence: the only way a plan ends
/// inside one is a mis-nested fence, and reading the rest of the plan as code
/// would silently drop every phase after it (measured: one corpus plan lost its
/// `## Phase 3` that way).
fn fenced_lines(lines: &[&str]) -> Vec<bool> {
    let mut mask = vec![false; lines.len()];
    let mut open: Option<(char, usize, usize)> = None;
    for (n, line) in lines.iter().enumerate() {
        let fence = fence_line(line.trim());
        match (open, fence) {
            (None, Some((ch, len, _))) => {
                open = Some((ch, len, n));
                mask[n] = true;
            }
            (Some((ch, len, _)), fence) => {
                mask[n] = true;
                if fence.is_some_and(|(c, l, bare)| c == ch && l >= len && bare) {
                    open = None;
                }
            }
            (None, None) => {}
        }
    }
    if let Some((_, _, from)) = open {
        mask[from..].iter_mut().for_each(|m| *m = false);
    }
    mask
}

/// Where the scan stands relative to a phase list (arms B and C).
#[derive(Clone, Copy, PartialEq, Eq)]
enum PhaseList {
    /// Not in a phase-list section, or its list has ended.
    Off,
    /// In a phase-list section's direct body, before its first item.
    Armed,
    /// Inside the section's first column-0 list. `procedure` is set when the
    /// paragraph introducing the list ends in `:` and never mentions a phase
    /// (`For each allowlisted object:`) — its numbering is a procedure's steps,
    /// so only explicit `**Phase N — …**` titles declare from it.
    InList { procedure: bool },
}

/// Where the scan stands relative to a Markdown table.
#[derive(Clone, Copy, PartialEq, Eq)]
enum TableState {
    /// Not inside a table.
    Outside,
    /// Inside a table whose header's first cell is `Phase`; `name_col` is the
    /// column a row's name falls back to (column 2 when column 1 is a status).
    PhaseTable { name_col: usize },
    /// Inside any other table — its rows are ignored.
    OtherTable,
}

/// How authoritative a declared phase's NAME is; a higher rank renames a
/// lower. Separate from whether arm A declared the index at all
/// ([`Declared::by_heading`]), which is what protects an index from refusal.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum NameRank {
    /// Arm A, a heading or bold line that is a sentence about the phase
    /// (`Phase 4 execution record`, `Phase 1 replay — measured`): a report
    /// ABOUT the phase, the weakest name.
    HeadingSentence,
    /// Arms B-D: a list item or table row outside a phase-list section.
    Inferred,
    /// Arms B-D inside a phase-list section.
    InferredInSection,
    /// Arm A, a heading or bold line that is a phase TITLE.
    HeadingTitle,
}

impl NameRank {
    fn is_heading(self) -> bool {
        matches!(self, NameRank::HeadingSentence | NameRank::HeadingTitle)
    }
}

/// One declared phase, the rank of the name it carries, and whether any arm-A
/// heading declared it.
struct Declared {
    phase: ParsedPhase,
    rank: NameRank,
    by_heading: bool,
}

/// Record `index`, keeping the FIRST occurrence's position; a later occurrence
/// of a higher [`NameRank`] replaces the name, so a status table at the top of
/// a plan cannot rename its `### Phase N — x`, `Phase 4 — tests` wins over an
/// earlier `Phase 4 gap, verified`, and a phase list's own titles win over a
/// later report heading about the same phase.
fn declare(out: &mut Vec<Declared>, index: u32, name: String, rank: NameRank) {
    match out.iter_mut().find(|d| d.phase.index == index) {
        Some(d) => {
            if rank > d.rank {
                d.phase.name = name;
                d.rank = rank;
            }
            d.by_heading |= rank.is_heading();
        }
        None => out.push(Declared {
            phase: ParsedPhase { index, name },
            rank,
            by_heading: rank.is_heading(),
        }),
    }
}

/// The rank of an arm B-D name: higher inside a phase-list section, so the
/// `## Phases` table outranks one under `## Divergent design for B`.
fn inferred_rank(in_section: bool) -> NameRank {
    if in_section {
        NameRank::InferredInSection
    } else {
        NameRank::Inferred
    }
}

/// The rank of an arm-A name.
fn heading_rank(name: &str) -> NameRank {
    if PHASE_TITLE.is_match(name) {
        NameRank::HeadingTitle
    } else {
        NameRank::HeadingSentence
    }
}

/// Detect the phases a plan DECLARES and project each to a [`ParsedPhase`],
/// deduped by index, in the document order of each index's first occurrence.
///
/// Downstream, coord treats every declared index as a phase that must
/// eventually be delivered, so a false positive is worse than a miss; every
/// arm below is shaped by that. Four arms, chosen from a census of how the
/// plan corpus spells its phases and then narrowed by three independent
/// reviews against the same corpus (plan
/// `2026-09-19-runner-detect-phases-misses-plans-that-list-phases-in-a-table-or-prose`):
///
/// - **A** — a `#`-heading (after an optional section enumerator, so
///   `## 5. Phase 0 — x` counts, unless the number is lettered — `### A.
///   Phase 1e` is another plan's sub-phase) or a `**`-bold line: `Phase`,
///   whitespace, digits. The pre-existing arm; its titles name a phase ahead of
///   every other arm ([`NameRank`]).
/// - **B** — inside a phase list (below), a column-0 list item whose bold text
///   is a phase TITLE ([`PHASE_TITLE`]): `- **Phase 1 — schema**`, never
///   `- **Phase 3's check could ossify**`. Outside a phase-list section a
///   bullet like this is a reference to a phase (of this plan or another), so
///   it does not declare.
/// - **C** — inside a phase list, a column-0 ordered item `N.` / `N)`.
/// - **D** — a data row of a table whose header's first cell is `Phase`
///   ([`PHASE_TABLE_CELL`]).
///
/// A **phase list** is the FIRST column-0 list in the direct body of a
/// phase-list section ([`PHASE_LIST_HEADING`]). It ends at the next heading of
/// any level, at a column-0 bold line that is not a `**Phase N` sentence (a
/// bold pseudo-heading such as `**Definition of done**` or `**Phase 1 — x**`,
/// whose numbered items are something else), and at a column-0 paragraph after
/// its first item. A section whose introduction says its phases do not exist,
/// were superseded, or are not delivered by this plan opens no list and its
/// tables declare nothing ([`NOT_A_PHASE_LIST`]); a list introduced by a
/// procedure's `…:` declares only explicit phase titles.
///
/// Never declared: fenced code ([`fenced_lines`]); blockquotes (status
/// narration such as `> **Phase 4 is HELD**`); mid-line prose; and an arm B-D
/// entry struck through or marked as not delivered by this plan
/// ([`NOT_DELIVERED_MARKER`]). Such an entry also REFUSES its exact index: an
/// index refused anywhere is dropped unless an arm-A heading declares it, so a
/// later unrelated table mentioning the same number cannot re-declare it. These
/// are heuristics over free-form Markdown, not a grammar the corpus agreed to;
/// the census in the plan above is the evidence for each one, and a miss is the
/// intended failure mode.
fn detect_phases(body: &str) -> Vec<ParsedPhase> {
    let lines: Vec<&str> = body.lines().collect();
    let fenced = fenced_lines(&lines);
    let mut out: Vec<Declared> = Vec::new();
    let mut refused: Vec<u32> = Vec::new();
    let mut refuse = |index: Option<u32>, lettered: bool| {
        if let (Some(index), false) = (index, lettered) {
            refused.push(index);
        }
    };
    let mut list = PhaseList::Off;
    let mut in_section = false;
    let mut disowned = false;
    let mut intro = String::new();
    let mut table = TableState::Outside;

    for (line, in_fence) in lines.iter().zip(fenced) {
        let t = line.trim();
        if in_fence || !t.starts_with('|') {
            table = TableState::Outside;
        }
        if in_fence {
            continue;
        }
        let column0 = !line.starts_with([' ', '\t']);

        // Arm A, heading form. A real heading (1-6 `#` then whitespace) also
        // decides whether a phase-list section starts here, and has its
        // section enumerator stripped before the `Phase N` test.
        if t.starts_with('#') {
            let rest = t.trim_start_matches('#');
            let level = t.len() - rest.len();
            let (text, enumerated) = if level <= 6 && rest.starts_with(char::is_whitespace) {
                let text = HEADING_ENUMERATOR.replace(rest.trim(), "").into_owned();
                in_section = PHASE_LIST_HEADING.is_match(&text)
                    && !PHASE_LIST_HEADING_NAMES_PHASES.is_match(&text);
                list = if in_section {
                    PhaseList::Armed
                } else {
                    PhaseList::Off
                };
                intro.clear();
                disowned = false;
                let enumerated = text.len() != rest.trim().len();
                (text, enumerated)
            } else {
                (rest.trim_start().to_string(), false)
            };
            let lettered = LETTERED_PHASE.is_match(&text);
            if let (Some(index), false) = (phase_index_at(&text), enumerated && lettered) {
                let name = text.trim_end_matches(['#', '*']).trim().to_string();
                let rank = heading_rank(&name);
                declare(&mut out, index, name, rank);
            }
            continue;
        }

        // Arm A, bold form. A bold phase TITLE, or any column-0 bold line that
        // is not about a phase, ends the phase list; a bold SENTENCE about a
        // phase (`**Phase 0 is a gate, not a step.**`) does not.
        if let Some(rest) = t.strip_prefix("**") {
            match phase_index_at(rest) {
                Some(index) => {
                    let name = bold_span(rest);
                    let rank = heading_rank(&name);
                    if rank == NameRank::HeadingTitle {
                        list = PhaseList::Off;
                    }
                    declare(&mut out, index, name, rank);
                }
                None if column0 => list = PhaseList::Off,
                None => {}
            }
            continue;
        }

        // Arms B and C: column-0 items of a phase list.
        if column0 && !t.is_empty() {
            if let (Some((ordinal, content)), PhaseList::Armed | PhaseList::InList { .. }) =
                (list_item(t), list)
            {
                let procedure = match list {
                    PhaseList::InList { procedure } => procedure,
                    _ => intro.ends_with(':') && !intro.to_lowercase().contains("phase"),
                };
                list = PhaseList::InList { procedure };
                let bold = content.strip_prefix("**").map(bold_span);
                let title = bold.as_deref().and_then(|b| PHASE_TITLE.captures(b));
                let (index, lettered) = match &title {
                    Some(caps) => (caps[1].parse().ok(), !caps[2].is_empty()),
                    None => (ordinal, false),
                };
                let struck_title = bold
                    .as_deref()
                    .is_some_and(|b| STRUCK_PHASE_TITLE.is_match(b));
                if is_struck(content) || struck_title || NOT_DELIVERED_MARKER.is_match(content) {
                    refuse(index, lettered);
                    continue;
                }
                if title.is_some() || !procedure {
                    if let Some(index) = index {
                        let name = bold.unwrap_or_else(|| content.to_string());
                        declare(&mut out, index, name, inferred_rank(in_section));
                    }
                }
                continue;
            }
            if !t.starts_with('|') {
                match list {
                    PhaseList::Armed
                        if NOT_DELIVERED_MARKER.is_match(t) || NOT_A_PHASE_LIST.is_match(t) =>
                    {
                        list = PhaseList::Off;
                        in_section = false;
                        disowned = true;
                    }
                    PhaseList::Armed => intro = t.to_string(),
                    PhaseList::InList { .. } => list = PhaseList::Off,
                    PhaseList::Off => {}
                }
            }
        }

        // Arm D: tables.
        if t.starts_with('|') {
            let cells: Vec<&str> = t.trim_matches('|').split('|').map(str::trim).collect();
            let first = cells.first().copied().unwrap_or("");
            match table {
                TableState::Outside => {
                    let header = |i: usize| {
                        cells
                            .get(i)
                            .map(|c| c.replace('*', "").trim().to_ascii_lowercase())
                            .unwrap_or_default()
                    };
                    table = if header(0) == "phase" {
                        let status = matches!(header(1).as_str(), "status" | "state");
                        TableState::PhaseTable {
                            name_col: if status { 2 } else { 1 },
                        }
                    } else {
                        TableState::OtherTable
                    };
                }
                TableState::PhaseTable { name_col }
                    if !disowned && !TABLE_SEPARATOR_CELL.is_match(first) =>
                {
                    if PHASE_TABLE_RANGE.is_match(first) {
                        continue;
                    }
                    let Some(caps) = PHASE_TABLE_CELL.captures(first) else {
                        continue;
                    };
                    let index: Option<u32> = caps[1].parse().ok();
                    let name_cell = cells.get(name_col).copied().unwrap_or("");
                    if is_struck(first) || is_struck(name_cell) || NOT_DELIVERED_MARKER.is_match(t)
                    {
                        refuse(index, !caps[2].is_empty());
                        continue;
                    }
                    let Some(index) = index else {
                        continue;
                    };
                    let clean = |s: &str| s.replace('*', "").trim().to_string();
                    // A match that consumed ` (` gives the parenthesis back, so
                    // `3 (coord)` is named `coord`, not `coord)`.
                    let matched = caps[0].len() - usize::from(caps[0].ends_with('('));
                    let after = clean(&first[matched..]);
                    let after =
                        after.trim_matches(|c: char| c.is_whitespace() || "—–:-".contains(c));
                    let after = match after.strip_prefix('(').and_then(|a| a.strip_suffix(')')) {
                        Some(inner) if !inner.contains(['(', ')']) => inner.trim(),
                        _ => after,
                    };
                    let name = [after.to_string(), clean(name_cell), clean(first)]
                        .into_iter()
                        .find(|n| !n.is_empty())
                        .unwrap_or_default();
                    declare(&mut out, index, name, inferred_rank(in_section));
                }
                _ => {}
            }
        }
    }
    out.into_iter()
        .filter(|d| d.by_heading || !refused.contains(&d.phase.index))
        .map(|d| d.phase)
        .collect()
}

/// Parse a plan markdown body into a [`ParsedWorkUnit`]. Pure — no IO. `slug`
/// and `source_path` are caller-supplied (the slug comes from the filename, not
/// the body — see [`slug_from_filename`]).
///
/// Status defaults to `"draft"` when no `> **Status:` line is present; title
/// defaults to `None` when no `# ` H1 is present (both matching coord).
///
/// Not quite effect-free: a status-block `Area:` whose value fails coord's
/// kebab grammar is logged with `tracing::warn!` (naming `source_path` and the
/// rejected value) and yields `area: None`. The returned value stays a pure
/// function of the inputs.
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

    let area = match extract_area(body) {
        AreaDecl::Absent => None,
        AreaDecl::Accepted(area) => Some(area),
        AreaDecl::Rejected(value) => {
            // Per cycle, on purpose: a malformed declaration stays visible in
            // the log until the plan is fixed, rather than being guessed into
            // shape or dropped silently.
            tracing::warn!(
                path = %source_path,
                rejected_area = %value,
                "plan adapter: status-block `Area:` is not kebab-case \
                 ([a-z0-9]+(-[a-z0-9]+)*); metadata.area omitted for this plan"
            );
            None
        }
    };

    ParsedWorkUnit {
        slug: slug.to_string(),
        title,
        status: status.unwrap_or_else(|| "draft".to_string()),
        depends_on: extract_depends_on(body),
        area,
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

    /// Arm B: inside a phase list, a column-0 bullet or ordered item whose bold
    /// text is a phase title. The indented sub-bullet — a title too — is
    /// commentary, not a declaration.
    #[test]
    fn phases_arm_b_list_item_opening_bold_phase() {
        let body = "# T\n\n## Phases\n\n- **Phase 1 — schema.** body\n1. **Phase 2 — wiring** (~40 LOC)\n  - **Phase 9 — nested** indented\n";
        let p = parse(body);
        assert_eq!(indices(body), vec![1, 2]);
        assert_eq!(p.phases[0].name, "Phase 1 — schema.");
        assert_eq!(p.phases[1].name, "Phase 2 — wiring");
    }

    /// Arm B needs a phase list: the same bullets under any other section are
    /// references to phases (corpus: `## 9. What this unblocks`, `## Diagnosis`,
    /// `## 0. What is ALREADY shipped`), not declarations.
    #[test]
    fn phases_arm_b_outside_a_phase_list_does_not_declare() {
        let body = "# T\n\n## 9. What this unblocks\n\n- **Phase 2 — the reaper** (another plan)\n- **Phase 3 — the dashboard**\n";
        assert!(indices(body).is_empty());
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
        let body = "# T\n\n| Phase | Deliverable |\n|---|---|\n| 1 | re-export forms |\n| **2** | basis |\n| Phase 3 | label |\n| ~~4~~ | gone |\n\n| # | Claim |\n|---|---|\n| 5 | not a phase |\n";
        let p = parse(body);
        assert_eq!(indices(body), vec![1, 2, 3]);
        assert_eq!(p.phases[0].name, "re-export forms");
    }

    /// A struck-through item is a DROPPED phase: declaring it would give coord a
    /// phase that can never be delivered.
    #[test]
    fn phases_struck_through_items_are_not_declared() {
        let body = "# T\n\n## Phases\n\n0. probe\n1. ~~relay~~ gone\n2. migrate\n3. **~~relay two~~** gone\n";
        assert_eq!(indices(body), vec![0, 2]);
    }

    /// New-arm entries the plan itself marks as not delivered by it do not
    /// declare (corpus shapes: `(not scheduled)`, `(deferred)`, `SPLIT to a
    /// follow-up plan`, a `DELEGATED` row, a `Not built` status in a later
    /// column, a struck name cell). The marker is read over the whole item or
    /// row, not only its name.
    #[test]
    fn phases_marked_not_delivered_are_not_declared() {
        let body = "# T\n\n## Phases\n\n- **Phase 1 — schema**\n- **Phase 4 (not scheduled)** later\n- **Phase 6 — relay** (deferred)\n7. **Relay** — SPLIT to a follow-up plan\n\n| Phase | Work | State |\n|---|---|---|\n| 2 | wiring | open |\n| **3 — further instances (DELEGATED to their own plans)** | x | |\n| 5 | audit | Not built |\n| 8 | ~~gone~~ | |\n";
        assert_eq!(indices(body), vec![1, 2]);
    }

    /// Fenced code and blockquotes never declare — a fenced example of the
    /// grammar, or status narration, is not a phase list.
    #[test]
    fn phases_fences_and_blockquotes_do_not_declare() {
        let body = "# T\n\n> **Phase 4 is HELD**, not forgotten.\n\n```\n## Phase 7\n**Phase 8 — example**\n| Phase | x |\n|---|---|\n| 9 | y |\n```\n\n~~~\n- **Phase 6 — example**\n~~~\n";
        assert!(indices(body).is_empty());
    }

    /// A fence closes only on the SAME character, at least as long, with no
    /// info string — so a four-backtick block quoting a three-backtick
    /// example, or a `~~~` line inside a backtick block, does not end it early.
    #[test]
    fn phases_fences_close_only_on_a_matching_fence() {
        let body = "# T\n\n````md\n```\n## Phase 7\n```\n## Phase 8\n````\n\n```\n~~~\n## Phase 9\n```rust\n## Phase 10\n```\n\n## Phase 1\n";
        assert_eq!(indices(body), vec![1]);
    }

    /// A fence still open at the end of the plan is a mis-nesting, not code:
    /// reading the rest of the plan as fenced would drop every later phase.
    #[test]
    fn phases_an_unclosed_trailing_fence_is_not_a_fence() {
        let body = "# T\n\n```md\nexample\n\n## Phase 3 — after a stray fence\n";
        assert_eq!(indices(body), vec![3]);
    }

    /// Arm C's list ends at a bold phase TITLE line: the numbered items after
    /// `**Phase 1 — x**` are that phase's steps. A bold SENTENCE about a phase
    /// (`**Phase 0 is a gate**`) does not end it.
    #[test]
    fn phases_arm_c_steps_under_a_bold_phase_title_are_not_phases() {
        let body = "# T\n\n## Phases\n\n**Phase 1 — the guard**\n\n1. hook\n2. prefilter\n3. message\n4. register\n";
        assert_eq!(indices(body), vec![1]);
        let body = "# T\n\n## Phases\n\n**Phase 0 is a gate, not a step.**\n\n0. probe\n1. build\n";
        assert_eq!(indices(body), vec![0, 1]);
    }

    /// A phase list is the section's FIRST column-0 list: a column-0 bold
    /// pseudo-heading (`**Definition of done**`) or a column-0 paragraph after
    /// the first item ends it, so later numbered lists in the same section are
    /// not phases.
    #[test]
    fn phases_arm_c_list_ends_at_a_bold_line_or_a_paragraph() {
        let body =
            "# T\n\n## Phases\n\n1. build\n2. ship\n\nThen, once it lands:\n\n1. a\n2. b\n3. c\n";
        assert_eq!(indices(body), vec![1, 2]);
        let body = "# T\n\n## 6. Phases\n\n**Definition of done, corrected**\n\n1. a\n2. b\n3. c\n";
        assert!(indices(body).is_empty());
    }

    /// Headings that negate phases or name specific ones open no phase list.
    #[test]
    fn phases_arm_c_refuses_negating_and_specific_phase_headings() {
        for heading in [
            "### Not phases",
            "## Deferred phases",
            "## Why Phases 6 and 7 have not landed",
            "## Phases 4–5 remain DEFERRED",
            "## Phases A and B, if the operator picks A",
        ] {
            let body = format!("# T\n\n{heading}\n\n1. one\n2. two\n");
            assert!(indices(&body).is_empty(), "{heading} opened a phase list");
        }
    }

    /// Arm B declares only a phase TITLE, never a bold sentence about a phase.
    #[test]
    fn phases_arm_b_refuses_bold_sentences_about_a_phase() {
        let body = "# T\n\n## Phases\n\n- **Phase 3's check could ossify** x\n- **Phase 2.2 automatic re-stamping narrows** y\n- **Phase 1.1 may queue rather than land**\n";
        assert!(indices(body).is_empty());
    }

    /// Arm A strips a heading's section enumerator: `## 5. Phase 0 — x`.
    #[test]
    fn phases_heading_with_a_section_enumerator_declares() {
        let body = "# T\n\n## 5. Phase 0 — measure\n\n## 6. Phase 1 — build\n";
        let p = parse(body);
        assert_eq!(indices(body), vec![0, 1]);
        assert_eq!(p.phases[0].name, "Phase 0 — measure");
    }

    /// Arm D names: the first cell's own text wins, a Status column is skipped,
    /// and a later heading renames a phase a table row named first. `0.5` and
    /// `7(a)` are sub-items, `4 – 6` and `5—6` are ranges and `33 of 52 …` is a
    /// count, so none declares its leading integer; `8 (coord)` is named
    /// `coord`, parentheses balanced.
    #[test]
    fn phases_arm_d_names_and_heading_precedence() {
        let body = "# T\n\n| Phase | Status | Deliverable |\n|---|---|---|\n| 1 | SHIPPED | Foundation |\n| **2a — base theory doc** | open | x |\n| 3 | open | y |\n| 0.5 | open | sub-item |\n| 4 – 6 | open | range |\n| 5—6 | open | range |\n| 33 of 52 rustc units | open | timing |\n| 7(a) | open | sub-item |\n| 8 (coord) | open | z |\n\n### Phase 3 — the real name\n";
        let p = parse(body);
        let got: Vec<(u32, &str)> = p
            .phases
            .iter()
            .map(|x| (x.index, x.name.as_str()))
            .collect();
        assert_eq!(
            got,
            vec![
                (1, "Foundation"),
                (2, "base theory doc"),
                (3, "Phase 3 — the real name"),
                (8, "coord")
            ]
        );
    }

    /// An index an arm B-D entry refuses (struck, or marked not delivered) is
    /// not re-declared by another list or table mentioning the same number —
    /// only an explicit `Phase N` heading can declare it. A LETTERED refusal
    /// (`3b (deferred)`) refuses nothing: phase 3 itself stands.
    #[test]
    fn phases_a_refused_index_is_not_redeclared_elsewhere() {
        let body = "# T\n\n| Phase | Outcome |\n|---|---|\n| 1 | built |\n| 4 | deferred |\n| 3b | x (deferred) |\n\n## Security\n\n| Phase | Touches credentials? |\n|---|---|\n| 3 | no |\n| 4 | yes |\n";
        assert_eq!(indices(body), vec![1, 3]);
        let body = "# T\n\n| Phase | Outcome |\n|---|---|\n| 4 | deferred |\n\n## Phase 4 — the real heading\n";
        assert_eq!(indices(body), vec![4]);
    }

    /// A phase title struck after its prefix was dropped:
    /// `**Phase 1 — ~~make it~~ ALREADY EXECUTED**`.
    #[test]
    fn phases_a_title_struck_after_its_prefix_is_not_declared() {
        let body = "# T\n\n## Phases\n\n- **Phase 1 — ~~make it~~ ALREADY EXECUTED BY A PEER**\n- **Phase 2 — the remaining work**\n";
        assert_eq!(indices(body), vec![2]);
    }

    /// The not-delivered wording the corpus actually uses.
    #[test]
    fn phases_not_delivered_wording_from_the_corpus() {
        let body = "# T\n\n## Phases\n\n1. build\n2. frontend. **NOT NEEDED**; do not open a web PR\n3. polish (optional, later)\n4. rollout — likely its own plan\n5. migration — MOOT\n";
        assert_eq!(indices(body), vec![1]);
    }

    /// A list introduced by a procedure's `…:` is the procedure's steps: only
    /// an explicit phase title declares from it.
    #[test]
    fn phases_a_procedure_list_is_not_a_phase_list() {
        let body = "# T\n\n### Phasing (one PR per table)\n\nFor each allowlisted object:\n\n1. write the revision\n2. run it\n3. check\n";
        assert!(indices(body).is_empty());
        let body = "# T\n\n## Phases\n\nIn order:\n\n1. **Phase 1 — schema**\n2. wiring\n";
        assert_eq!(indices(body), vec![1]);
    }

    /// A section whose introduction says its phases do not exist, or were
    /// superseded, opens no phase list.
    #[test]
    fn phases_a_section_whose_intro_disowns_its_list() {
        let body = "# T\n\n## Phases\n\n> These phases describe Option A only. Under Option B, Phases 1-4 do not exist.\n\n- **Phase 1 — a**\n- **Phase 2 — b**\n";
        assert!(indices(body).is_empty());
    }

    /// A phase TITLE outranks a sentence for the same index, wherever each
    /// appears: `Phase 4 — tests` renames an earlier `Phase 4 gap, verified`.
    #[test]
    fn phases_a_title_outranks_a_sentence_for_the_name() {
        let body = "# T\n\n**Phase 4 gap, verified** in review.\n\n## Phase 4 — tests\n";
        let p = parse(body);
        assert_eq!(indices(body), vec![4]);
        assert_eq!(p.phases[0].name, "Phase 4 — tests");
    }

    /// A backtick fence's info string may not contain a backtick, so
    /// ```` ```x` ```` is not a fence and does not hide what follows.
    #[test]
    fn phases_a_backtick_info_string_with_a_backtick_is_not_a_fence() {
        let body = "# T\n\n```x` inline\n\n## Phase 1 — real\n\n```\n";
        assert_eq!(indices(body), vec![1]);
    }

    /// A phase-list section whose introduction marks it not delivered by this
    /// plan opens no list.
    #[test]
    fn phases_a_section_intro_marked_not_delivered_opens_no_list() {
        let body = "# T\n\n## Phases\n\nDeferred to a follow-up plan.\n\n1. a\n2. b\n";
        assert!(indices(body).is_empty());
        // Its tables declare nothing either, until the next heading.
        let body = "# T\n\n## Phases\n\nThese phases were superseded.\n\n| Phase | Work |\n|---|---|\n| 1 | a |\n\n## Now\n\n| Phase | Work |\n|---|---|\n| 2 | b |\n";
        assert_eq!(indices(body), vec![2]);
    }

    /// Corpus wording for work done elsewhere or not at all: `shipping
    /// separately`, `(separate)`, `already exist`.
    #[test]
    fn phases_separate_and_already_existing_work_is_not_declared() {
        let body = "# T\n\n## Phases\n\n1. **(Layer 1 — shipping separately, prerequisite)**\n2. build\n3. **(separate)** scope TBD\n4. **Gates already exist.**\n";
        assert_eq!(indices(body), vec![2]);
    }

    /// An enumerated heading naming a LETTERED phase (`### A. Phase 1e`) is
    /// another plan's sub-phase, not this plan's phase 1.
    #[test]
    fn phases_an_enumerated_lettered_heading_does_not_declare() {
        let body = "# T\n\n### A. Phase 1e: extend Phase 1d\n\n## 2. Phase 2 — real\n";
        assert_eq!(indices(body), vec![2]);
        // Scope: only an ENUMERATED heading is affected. An un-enumerated
        // `### Phase 1e` declares 1, as it always has; an enumerated lettered
        // phase of the plan's own is a miss, the accepted direction.
        assert_eq!(indices("# T\n\n### Phase 1e — x\n"), vec![1]);
        assert!(indices("# T\n\n## 3. Phase 2b — x\n").is_empty());
    }

    /// A phase list's own title outranks a later report heading ABOUT the
    /// phase, and a table in the phase section outranks one elsewhere — but a
    /// refused index a heading declares is still kept.
    #[test]
    fn phases_name_ranks_list_titles_over_report_sentences() {
        let body = "# T\n\n## Divergent design for B\n\n| Phase | Work |\n|---|---|\n| 1 | the design not taken |\n\n## Phases\n\n| Phase | Work |\n|---|---|\n| 1 | the real phase |\n\n- **Phase 2 — remove engineering**\n\n## Phase 2 execution record (2026-08-16)\n";
        let p = parse(body);
        let got: Vec<(u32, &str)> = p
            .phases
            .iter()
            .map(|x| (x.index, x.name.as_str()))
            .collect();
        assert_eq!(
            got,
            vec![(1, "the real phase"), (2, "Phase 2 — remove engineering")]
        );
        let body = "# T\n\n## Phase 4 execution record\n\n| Phase | Outcome |\n|---|---|\n| 4 | deferred |\n";
        assert_eq!(indices(body), vec![4]);
        // The rename does not cost the heading's protection: a section row
        // renames phase 4, a later table refuses it, and it stays — declared by
        // a heading, whatever name won (`by_heading`, not the name's rank).
        let body = "# T\n\n## Phase 4 execution record\n\n## Phases\n\n| Phase | Work |\n|---|---|\n| 4 | tests |\n\n## Later\n\n| Phase | Outcome |\n|---|---|\n| 4 | deferred |\n";
        let p = parse(body);
        let got: Vec<(u32, &str)> = p
            .phases
            .iter()
            .map(|x| (x.index, x.name.as_str()))
            .collect();
        assert_eq!(got, vec![(4, "tests")]);
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

    /// A measuring tool, not a check: prints, per dated plan in a directory,
    /// what the REAL parser declares — one
    /// `phase_census_row<TAB><file><TAB><i,j,...><TAB>[names]` line each — then a
    /// summary line counting plans, and separately every entry it could not
    /// read (a directory-entry error, or a file that is unreadable or not
    /// UTF-8). To measure a grammar change for gained AND lost indices, run it
    /// on both parsers (cherry-pick this test onto the old one) and diff the
    /// row lines. Point `QONTINUI_PHASE_CENSUS_DIR` at a `plans/` directory and
    /// run `cargo test -- phase_census --ignored --nocapture`.
    #[test]
    #[ignore = "measuring tool: reads QONTINUI_PHASE_CENSUS_DIR, prints and returns when it is unset"]
    fn phase_census() {
        let Ok(dir) = std::env::var("QONTINUI_PHASE_CENSUS_DIR") else {
            println!("phase_census: set QONTINUI_PHASE_CENSUS_DIR to a plans/ directory");
            return;
        };
        let mut skipped = 0;
        let mut names: Vec<String> = Vec::new();
        for entry in std::fs::read_dir(&dir).expect("readable plans dir") {
            let Ok(entry) = entry else {
                skipped += 1;
                continue;
            };
            let name = entry.file_name().to_string_lossy().to_string();
            if name
                .strip_suffix(".md")
                .is_some_and(|stem| authored_at_from_stem(stem).is_some())
            {
                names.push(name);
            }
        }
        names.sort();
        let (mut plans, mut zero, mut one, mut many) = (0, 0, 0, 0);
        for name in names {
            let Ok(body) = std::fs::read_to_string(std::path::Path::new(&dir).join(&name)) else {
                skipped += 1;
                continue;
            };
            plans += 1;
            let got = detect_phases(&body);
            match got.len() {
                0 => zero += 1,
                1 => one += 1,
                _ => many += 1,
            }
            let indices: Vec<String> = got.iter().map(|p| p.index.to_string()).collect();
            let phase_names: Vec<&str> = got.iter().map(|p| p.name.as_str()).collect();
            println!(
                "phase_census_row\t{name}\t{}\t{phase_names:?}",
                indices.join(",")
            );
        }
        println!(
            "phase_census plans={plans} skipped={skipped} zero={zero} one={one} two_or_more={many}"
        );
    }

    // --- area ------------------------------------------------------------------

    /// The shared fixture list pinning [`is_kebab_case`] to coord's grammar,
    /// `[a-z0-9]+(-[a-z0-9]+)*`. The rejected half is exactly what the looser
    /// `^[a-z0-9][a-z0-9-]*$` would have admitted (`a--b`, `a-`) plus the
    /// shapes the plan corpus actually produced (`agent_worktree`, prose).
    const KEBAB_ACCEPTED: &[&str] = &[
        "a",
        "0",
        "a1",
        "ci-runners",
        "coord-tests",
        "published-parity",
        "domain-cost-ledger",
        "new-project-initiation",
        "fleet-tooling",
        "v2-api-3",
    ];
    const KEBAB_REJECTED: &[&str] = &[
        "",
        "a--b",
        "a-",
        "-a",
        "-",
        "A",
        "Published-Parity",
        "agent_worktree",
        "a b",
        "a.b",
        "ümlaut",
        "`x`",
    ];

    #[test]
    fn kebab_predicate_matches_coords_grammar() {
        for ok in KEBAB_ACCEPTED {
            assert!(is_kebab_case(ok), "{ok:?} must be accepted");
        }
        for bad in KEBAB_REJECTED {
            assert!(!is_kebab_case(bad), "{bad:?} must be rejected");
        }
    }

    /// The same fixture list, driven through the extractor: every accepted
    /// value resolves whether bold, un-bolded or backticked, and every rejected
    /// non-empty value comes back verbatim as `Rejected` — never normalised.
    #[test]
    fn extract_area_applies_the_kebab_fixtures() {
        for ok in KEBAB_ACCEPTED {
            for line in [
                format!("> **Area:** `{ok}`"),
                format!("> Area: {ok}"),
                format!("> **Area:** {ok}."),
            ] {
                let body = format!("# T\n\n> **Status: DRAFT.**\n{line}\n");
                assert_eq!(
                    extract_area(&body),
                    AreaDecl::Accepted(ok.to_string()),
                    "{line:?}"
                );
            }
        }
        for bad in [
            "a--b",
            "a-",
            "-a",
            "A",
            "agent_worktree",
            "Published-Parity",
        ] {
            let body = format!("# T\n\n> **Status: DRAFT.**\n> **Area:** `{bad}`\n");
            assert_eq!(
                extract_area(&body),
                AreaDecl::Rejected(bad.to_string()),
                "{bad:?}"
            );
        }
    }

    /// Every status-blockquote `Area:` line on `qontinui-dev-notes`
    /// `origin/main` `86208321` (2026-09-22), verbatim, each under the status
    /// line it sits beneath in its plan. Re-measure with
    /// `git grep -n 'Area:' origin/main -- plans/` filtered to the status block.
    const STATUS_BLOCK_AREA_FIXTURES: &[(&str, &str, Option<&str>)] = &[
        (
            "2026-08-31-published-build-parity-check",
            "> **Area:** `published-parity` (work-unit `metadata.area`; added 2026-09-22 by plan `2026-09-20-published-runner-parity-count-comes-from-a-run-not-from-reports` Phase 3).",
            Some("published-parity"),
        ),
        (
            "2026-09-14-a-poison-ci-runner-takes-every-job-and-no-detector-sees-it",
            "> **Area:** `ci-runners` (work-unit `metadata.area`, exactly that key).",
            Some("ci-runners"),
        ),
        (
            "2026-09-15-two-closeout-scripts-fail-silently-on-a-windows-box",
            "> History: **Status: DRAFT 2026-09-15.** Authored at closeout by session `ff515a63` (spaceship) under `planning-and-scope` `finish-to-zero-includes-the-defect-underneath` (v6), from three defects met in one `/implement-plan` run (one of them, the allocator's CRLF path, was already fixed by qontinui-claude-config#961 while this session ran — finding `914bc31a` — so it is recorded here as prior art, not as a phase) of `2026-09-14-a-poison-ci-runner-takes-every-job-and-no-detector-sees-it`. Evidence: coord finding `7f5da040` (fleet-infra) and finding `b4bb2273` (landed-since CRLF). Not vetted; not implemented. **Repo:** `qontinui/qontinui-claude-config` (`scripts/`). **Area:** `fleet-tooling`.",
            Some("fleet-tooling"),
        ),
        (
            "2026-09-19-coord-tests-fail-on-a-windows-crlf-checkout",
            "> **Repo:** `qontinui-coord`. **Area:** `coord-tests`. Line references are to",
            Some("coord-tests"),
        ),
        (
            "2026-09-20-the-published-product-works-without-knowing-a-development-environment-exists",
            "> **Area:** `published-parity`.",
            Some("published-parity"),
        ),
        (
            "2026-09-20-the-second-ratchet-domain-is-operations-and-its-cost-is-compared-to-the-first",
            "> Area: domain-cost-ledger",
            Some("domain-cost-ledger"),
        ),
        (
            "2026-09-22-a-fan-out-skill-rediscovers-the-worktree-cap-once-per-subagent",
            "> **Repo:** `qontinui/qontinui-claude-config` (`.claude/commands/`). **Area:** `fleet-tooling`.",
            Some("fleet-tooling"),
        ),
        (
            "2026-09-22-new-project-initiation-pr-f-doctor-step-and-funnel-telemetry",
            "> **Area:** `new-project-initiation` (work-unit `metadata.area`, exactly that key).",
            Some("new-project-initiation"),
        ),
        // The measured free-text forms: rejected, with the value named.
        (
            "2026-09-19-orphan-target-reaper-cannot-remove-a-nested-windows-junction",
            "> **Repo:** `qontinui-runner`. **Area:** `agent_worktree` orphan target reaper.",
            None,
        ),
        (
            "2026-09-19-runner-reclaim-cannot-unlink-a-symlink-on-linux",
            "> **Repo:** `qontinui-runner`. **Area:** `agent_worktree` reclaim. Line references are to",
            None,
        ),
        (
            "2026-09-21-rejunction-on-linux-reports-success-without-creating-a-link",
            "> **Repo:** `qontinui-runner`. **Area:** `agent_worktree` reclaim, the `Rejunction` action.",
            None,
        ),
    ];

    #[test]
    fn status_block_area_fixtures_from_the_corpus() {
        for (stem, line, want) in STATUS_BLOCK_AREA_FIXTURES {
            let body = format!(
                "# {stem}\n\n> **Status: DRAFT 2026-09-22.** Summary.\n>\n{line}\n\nBody.\n"
            );
            let path = format!("plans/{stem}.md");
            let u = parse_work_unit(stem, &path, &body, &conv());
            assert_eq!(u.area.as_deref(), *want, "{stem}");
            let decl = extract_area(&body);
            match want {
                Some(a) => assert_eq!(decl, AreaDecl::Accepted(a.to_string()), "{stem}"),
                None => assert_eq!(
                    decl,
                    AreaDecl::Rejected("agent_worktree".to_string()),
                    "{stem}: rejected, and the warning names the value"
                ),
            }
        }
    }

    /// The parity plan's own head, verbatim: its `**Repo(s):**` line mentions
    /// "scanner `Area:` parse" INSIDE the status block and BEFORE the real key.
    /// That inline-code mention is not a key (a backtick precedes it), so the
    /// declaration two lines on wins.
    #[test]
    fn inline_code_area_mention_is_not_a_key() {
        let body = "# The published-runner parity count comes from a run, not from reports\n\
\n\
> **Status: IN PROGRESS 2026-09-22.** Implementation started by session\n\
> Depends-On: 2026-08-31-published-build-parity-check\n\
>\n\
> **Repo(s):** `qontinui-runner` (harness, workflow, scanner `Area:` parse, sweep),\n\
> `qontinui-claude-config` (`/create-plan` header convention, knowledge-base page,\n\
> capability-doctor cross-link), `qontinui-dev-notes` (plan stamps only). No\n\
> `qontinui-coord` or `qontinui-web` change; no `coord.*` schema change.\n\
> **Area:** `published-parity` (work-unit `metadata.area`, exactly that key — and see\n\
> Phase 3: today this line reaches no work unit).\n\
";
        assert_eq!(
            extract_area(body),
            AreaDecl::Accepted("published-parity".to_string())
        );
        assert_eq!(parse(body).area.as_deref(), Some("published-parity"));
    }

    /// `2026-09-02-coord-train-activity-idle-unserved-fires-on-a-moving-train`,
    /// verbatim: its `**Area:**` line sits OUTSIDE the status blockquote (after
    /// the blank line that ends it). Not read, and — being `Absent`, not
    /// `Rejected` — not warned either.
    #[test]
    fn area_outside_the_status_block_is_absent_not_rejected() {
        let body = "# coord's merge-status classifier calls a served PR \"orchestrator stalled\"\n\
\n\
> **Status: IN PROGRESS 2026-09-02.** Implementation started by session\n\
> **inherits** this defect rather than fixing it — see \"Interaction with\n\
> in-flight work\" below.\n\
\n\
**Repo:** `qontinui-coord`\n\
**Area:** `coord-merge-train`\n\
**Slug:** `2026-09-02-coord-train-activity-idle-unserved-fires-on-a-moving-train`\n\
";
        assert_eq!(extract_area(body), AreaDecl::Absent);
        assert_eq!(parse(body).area, None);
    }

    #[test]
    fn no_status_block_and_no_area_are_absent() {
        assert_eq!(extract_area("# T\n\n**Area:** `x`\n"), AreaDecl::Absent);
        assert_eq!(
            extract_area("# T\n\n> **Status: DRAFT.** no area here\n"),
            AreaDecl::Absent
        );
    }

    /// Key boundaries: `Sub-Area:` is not a key, and the FIRST real key decides
    /// even when a later one would have been valid — a malformed declaration is
    /// reported, not shadowed.
    #[test]
    fn area_key_boundary_and_first_key_decides() {
        let body = "> **Status: DRAFT.** Sub-Area: nope\n> **Area:** `ok-area`\n";
        assert_eq!(
            extract_area(body),
            AreaDecl::Accepted("ok-area".to_string())
        );
        let body = "> **Status: DRAFT.**\n> Area: Bad_Area\n> **Area:** `ok-area`\n";
        assert_eq!(
            extract_area(body),
            AreaDecl::Rejected("Bad_Area".to_string())
        );
    }

    /// Value delimiting: a backticked value ends at the closing backtick; an
    /// unclosed one takes the rest of the line (and fails); a bare value is the
    /// first whitespace token with trailing punctuation removed; an empty value
    /// is rejected as `""`.
    #[test]
    fn area_value_delimiting() {
        let one = |line: &str| extract_area(&format!("> **Status: DRAFT.**\n{line}\n"));
        assert_eq!(
            one("> **Area:** `a-b` (and more prose)"),
            AreaDecl::Accepted("a-b".to_string())
        );
        assert_eq!(
            one("> **Area:** `a-b and more"),
            AreaDecl::Rejected("a-b and more".to_string())
        );
        assert_eq!(
            one("> Area: a-b, then prose"),
            AreaDecl::Accepted("a-b".to_string())
        );
        assert_eq!(
            one("> **Area: a-b.**"),
            AreaDecl::Accepted("a-b".to_string())
        );
        assert_eq!(one("> **Area:**"), AreaDecl::Rejected(String::new()));
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
