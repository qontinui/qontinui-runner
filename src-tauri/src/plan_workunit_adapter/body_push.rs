//! Artifact **body** push client — plan `2026-08-10-plan-and-prompt-library-in-web`
//! Phase 2 (deterministic backfill + sync).
//!
//! Sibling of [`super::push`], and deliberately a *separate* sink rather than an
//! extra method on [`super::push::WorkUnitSink`]: the two speak to different
//! services with different identity models.
//!
//! | | [`super::push`] | this module |
//! |---|---|---|
//! | target | **coord** `/coord/work-units/*` | **qontinui-web** `/api/v1/plan-library` |
//! | carries | slug + opaque status + metadata | the whole markdown **body** |
//! | identity | `slug` | `(org, kind, slug, source_repo)` (D7) |
//!
//! What it shares with `push.rs` is the thing that matters most — the
//! **transport template**: every call is authenticated with the *runner-resolved*
//! device JWT via [`crate::auth::attach_device_auth`], exactly as
//! [`super::push::HttpWorkUnitSink`] does. It is emphatically **not** modelled on
//! `mcp::skills::sync_push`, whose `auth_token` is a caller-supplied request
//! field; on a loopback surface with no auth layer that would let any local
//! process choose the bearer. `skills.rs` is cited only for the
//! `{backend_url}/api/v1/…` URL convention.
//!
//! ## The organization is never in the request
//!
//! `WorkArtifactUpsert` on the web side has **no `organization_id` field** — the
//! org is derived server-side from the authenticated principal. This client
//! mirrors that: [`ArtifactUpsert`] has nowhere to put one.
//!
//! ## Skip on unchanged sha
//!
//! Two layers, both keyed on the sha256 of the exact body bytes sent:
//!
//! 1. **Client-side** ([`ArtifactSyncState`]): a re-scan of an unchanged file
//!    makes no HTTP call at all. This is what makes a 1,100-file reconcile tick
//!    nearly free.
//! 2. **Server-side**: an unchanged digest is a no-op that answers `200` with
//!    `changed: false` and an `X-Artifact-Unchanged: true` header (**not** a
//!    literal `304` — the caller still wants the row back to assert
//!    `current_version` did not move).
//!
//! The digest we send is computed over *exactly* the bytes we send: the endpoint
//! re-computes `sha256(body)` and **422s a disagreeing digest**, so a stale or
//! independently-derived hash is a hard error rather than a silent divergence.
//!
//! ## Kind stickiness
//!
//! Scan-originated upserts carry `captured_by: "runner_scan"` **and
//! `kind_is_heuristic: true`**. The flag makes the web side resolve the target
//! row by `(org, slug, source_repo)` *ignoring kind*, so a kind a human or an
//! agent corrected is left alone. Without it every corrected artifact forks into
//! a second row on the next scan — the classifier here only ever proposes an
//! **initial** kind.

use super::parser::{parse_work_unit, slug_from_filename, ParsedWorkUnit, PlanConvention};
use anyhow::{Context, Result};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// `captured_by` stamped on every scan-originated upsert. One of the web side's
/// three `CapturedBy` literals (`runner_scan | agent | operator`).
pub const CAPTURED_BY_RUNNER_SCAN: &str = "runner_scan";

/// The `depends_on` provenance relation on `agent.work_artifact_edges`.
pub const RELATION_DEPENDS_ON: &str = "depends_on";

// ============================================================================
// Kinds
// ============================================================================

/// The artifact kinds the web store's `ck_work_artifacts_kind` CHECK allows.
/// Mirrors `WorkArtifactKind` in `qontinui-web/backend/app/schemas/plan_library.py`
/// — a value outside this set is a 422, so the set is closed here too.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ArtifactKind {
    InvestigationPrompt,
    PlanAuthoringPrompt,
    ImplementationPrompt,
    InvestigationReport,
    Handoff,
    Plan,
}

impl ArtifactKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            ArtifactKind::InvestigationPrompt => "investigation_prompt",
            ArtifactKind::PlanAuthoringPrompt => "plan_authoring_prompt",
            ArtifactKind::ImplementationPrompt => "implementation_prompt",
            ArtifactKind::InvestigationReport => "investigation_report",
            ArtifactKind::Handoff => "handoff",
            ArtifactKind::Plan => "plan",
        }
    }

    /// Every kind, for a report that must show a zero rather than omit a row.
    pub const ALL: [ArtifactKind; 6] = [
        ArtifactKind::InvestigationPrompt,
        ArtifactKind::PlanAuthoringPrompt,
        ArtifactKind::ImplementationPrompt,
        ArtifactKind::InvestigationReport,
        ArtifactKind::Handoff,
        ArtifactKind::Plan,
    ];
}

impl std::fmt::Display for ArtifactKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Which corpus a scan root holds. The directory decides the *starting point*
/// of classification, never the answer on its own — `prompts/merge-queue-report.md`
/// is a report filed in a prompts dir, so a directory-only rule would mislabel it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanRootKind {
    /// A plans directory (active or archive). Everything in it is a `plan`.
    Plans,
    /// A prompts directory. The per-file heuristics below decide the kind.
    Prompts,
}

// ============================================================================
// Pure classification + extraction
// ============================================================================

/// Body marker for the operator's investigation-prompt convention.
const MARKER_INVESTIGATE_THEN_PLAN: &str = "investigate, then author a plan";
/// Body marker for the operator's plan-authoring-prompt convention.
const MARKER_AUTHOR_A_PLAN_FROM: &str = "author a plan from";

/// Classify one scanned file to an **initial** kind.
///
/// `stem` is the filename without directory or `.md`; `body` is the raw file
/// content. Pure, so every corpus fact below is a unit test rather than a
/// claim.
///
/// Order is load-bearing:
///
/// 1. A **plans root** is unconditionally `plan`. The operator's plan dirs are
///    single-purpose, and a plan's body legitimately quotes prompt phrases
///    (a plan authored *from* a prompt often pastes it), so running the prompt
///    heuristics over plan bodies would misfile plans, not rescue prompts.
/// 2. Explicit **body markers** beat filenames — a file that literally says
///    "INVESTIGATE, THEN AUTHOR A PLAN" is that prompt whatever it is called.
/// 3. A `-report` stem (or an H1 ending in "Report") is a produced artifact,
///    not an instruction — this is the `merge-queue-report.md` case, and it is
///    checked before the filename investigation rules so
///    `investigate-x-report.md` reads as the report it is.
/// 4. `investigate-*` / `*-investigation` filenames.
/// 5. `*handoff*` filenames.
/// 6. Otherwise `implementation_prompt` — the documented default.
///
/// Note the date-prefix assumption every other stem rule in this tree relies on
/// does **not** hold here: the three files in the operator's root `prompts/`
/// carry no `YYYY-MM-DD-` prefix, so nothing below may require one.
pub fn classify_kind(root_kind: ScanRootKind, stem: &str, body: &str) -> ArtifactKind {
    if root_kind == ScanRootKind::Plans {
        return ArtifactKind::Plan;
    }

    // Scanned case-insensitively IN PLACE rather than over a `body.to_lowercase()`
    // copy: this runs on every file of a ~1,100-file corpus every 60s, and the
    // copy allocated (and immediately dropped) the entire corpus — megabytes of
    // pure churn per cycle — to serve two `contains` checks.
    if contains_ignore_ascii_case(body, MARKER_INVESTIGATE_THEN_PLAN) {
        return ArtifactKind::InvestigationPrompt;
    }
    if contains_ignore_ascii_case(body, MARKER_AUTHOR_A_PLAN_FROM) {
        return ArtifactKind::PlanAuthoringPrompt;
    }

    let stem_lower = stem.to_lowercase();
    if stem_lower.ends_with("-report")
        || stem_lower.ends_with("_report")
        || stem_lower == "report"
        || first_h1(body).is_some_and(|t| t.trim_end_matches(['.', '*']).ends_with("Report"))
    {
        return ArtifactKind::InvestigationReport;
    }

    if stem_lower.starts_with("investigate-")
        || stem_lower.ends_with("-investigation")
        || stem_lower.contains("-investigation-")
    {
        return ArtifactKind::InvestigationPrompt;
    }

    if stem_lower.contains("handoff") {
        return ArtifactKind::Handoff;
    }

    ArtifactKind::ImplementationPrompt
}

/// ASCII-case-insensitive substring search that allocates nothing.
///
/// `needle` must already be ASCII lowercase (every caller here passes a `const`
/// marker, so that is a compile-time-visible property, not a runtime promise).
/// Only the ASCII case folding matters: the markers are plain English, and a
/// full Unicode fold would need the allocation this exists to avoid.
fn contains_ignore_ascii_case(haystack: &str, needle: &str) -> bool {
    debug_assert!(
        needle.bytes().all(|b| !b.is_ascii_uppercase()),
        "needle must be ASCII lowercase"
    );
    let (h, n) = (haystack.as_bytes(), needle.as_bytes());
    if n.is_empty() {
        return true;
    }
    if h.len() < n.len() {
        return false;
    }
    h.windows(n.len()).any(|w| w.eq_ignore_ascii_case(n))
}

/// The first `# ` H1 of a body, trimmed. Shared by [`classify_kind`] and the
/// structure check; deliberately the same "first H1 wins" rule the work-unit
/// parser uses.
fn first_h1(body: &str) -> Option<&str> {
    body.lines().find_map(|line| {
        line.trim_start()
            .strip_prefix("# ")
            .map(str::trim)
            .filter(|t| !t.is_empty())
    })
}

/// True when the file has enough markdown structure to be a real artifact.
///
/// The exclusion this exists for is `D:/qontinui-root/plans/plans to do.md`:
/// a bare newline-separated scratch list with **zero markdown headings and no
/// `> **Status:` stamp**, which parses to a junk artifact (empty title,
/// invented `draft` status) that then sits in the corpus forever. Requiring
/// *either* one heading *or* a status stamp is the narrowest rule that excludes
/// it while keeping every genuine file — including the three undated root
/// prompts, which all open with an H1.
pub fn has_document_structure(body: &str) -> bool {
    body.lines().any(|line| {
        let t = line.trim_start();
        t.starts_with('#') || t.starts_with("> **Status:")
    })
}

/// True when the body actually carries a `> **Status:` stamp.
///
/// [`parse_work_unit`] *invents* `"draft"` when there is none, which is right
/// for plans (coord's work-unit model has no empty status) and wrong for
/// prompts — a prompt with no lifecycle stamp has no status, and the artifact
/// store's `status` is opaque text defaulting to `""`. So the two are
/// distinguished here rather than shipping a fictional `draft` on 45 prompts.
pub fn has_status_stamp(body: &str) -> bool {
    body.lines()
        .any(|line| line.trim_start().starts_with("> **Status:"))
}

/// Tokens that survive `Repo(s):` chunking but are English, not repositories.
const REPO_STOPWORDS: [&str; 10] = [
    "and", "plus", "the", "for", "only", "none", "n/a", "primary", "also", "with",
];

/// Extract the repos a plan/prompt declares from its `> **Repo(s):**` line.
///
/// The corpus writes this line half a dozen ways — inside a blockquote or not,
/// backticked or bare, comma- or `+`-separated, with parenthetical roles, with a
/// trailing period, and sometimes wrapped onto a continuation line:
///
/// ```text
/// > **Repo(s):** qontinui-coord (primary), qontinui-web (Phase 4 alembic migration).
/// **Repo(s):** `ui-bridge`
/// > **Repo(s):** qontinui-coord (progress PATCH added by #749) + qontinui-runner (progress
/// > outbox).
/// ```
///
/// So: take the first marker line, plus its blockquote continuation lines (a
/// `>` line carrying no new `**bold**` marker), strip parenthetical spans, split
/// on `,` `+` `;`, and keep the first bare token of each chunk. Order-preserving
/// and deduped. Returns empty when the line is absent — most of the corpus has
/// no `Repo(s):` line at all, which is a real empty, not a parse failure.
pub fn extract_repos(body: &str) -> Vec<String> {
    const MARKER: &str = "**Repo(s):**";
    let mut lines = body.lines();
    let mut joined = String::new();
    let mut in_blockquote = false;
    let mut found = false;

    for line in lines.by_ref() {
        if let Some(idx) = line.find(MARKER) {
            found = true;
            in_blockquote = line.trim_start().starts_with('>');
            joined.push_str(&line[idx + MARKER.len()..]);
            break;
        }
    }
    if !found {
        // Most of the corpus has no `Repo(s):` line at all — a real empty.
        return Vec::new();
    }
    if in_blockquote {
        for line in lines {
            let t = line.trim_start();
            let Some(rest) = t.strip_prefix('>') else {
                break;
            };
            if rest.contains("**") || rest.trim().is_empty() {
                break;
            }
            joined.push(' ');
            joined.push_str(rest);
        }
    }

    let cleaned = strip_parenthetical(&joined);
    let mut out: Vec<String> = Vec::new();
    for chunk in cleaned.split([',', '+', ';', '/']) {
        let token = chunk
            .split_whitespace()
            .next()
            .unwrap_or("")
            .trim_matches(|c: char| !(c.is_alphanumeric() || c == '-' || c == '_' || c == '.'));
        if token.is_empty() || token.len() > 64 {
            continue;
        }
        let lowered = token.to_lowercase();
        if REPO_STOPWORDS.contains(&lowered.as_str()) {
            continue;
        }
        if !out.iter().any(|e| e == &lowered) {
            out.push(lowered);
        }
    }
    out
}

/// Drop `(...)` spans, which carry roles ("(primary)", "(Phase 4 alembic
/// migration)") rather than repo names. Unbalanced parens simply run to the end.
fn strip_parenthetical(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut depth = 0usize;
    for c in s.chars() {
        match c {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out
}

/// Hex sha256 of the body's UTF-8 bytes — byte-identical to the web side's
/// `crud.compute_content_sha256`. Computed over *exactly* the bytes sent, since
/// the endpoint 422s a digest that disagrees with its own.
pub fn content_sha256(body: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(body.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// `YYYY-MM-DD` from a date-prefixed stem, as an RFC-3339 UTC midnight.
///
/// Feeds the artifact's `authored_at`, which the Phase 6 candidate read turns
/// into `age_days`. Returns `None` for the undated stems (the three root
/// `prompts/` files) rather than inventing a date — an absent `authored_at`
/// falls back to `created_at` server-side, which is honest; a fabricated one
/// would not be.
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
    Some(format!("{}T00:00:00Z", &stem[..10]))
}

/// Derive the `source_repo` identity component for a scan root — the D7
/// component that keeps both copies of a duplicated stem alive.
///
/// The value is a **two-component scan-root identity**, not a bare repo name:
///
/// | scan root | `source_repo` |
/// |---|---|
/// | `D:/qontinui-root/plans` | `qontinui-root/plans` |
/// | `D:/qontinui-root/plans/archive` | `plans/archive` |
/// | `D:/qontinui-root/qontinui-dev-notes/plans` | `qontinui-dev-notes/plans` |
/// | `D:/qontinui-root/qontinui-dev-notes/prompts` | `qontinui-dev-notes/prompts` |
///
/// Inside a git checkout it is `<repo>/<path-relative-to-the-repo-root>`;
/// outside one (the workspace root is not a repo) it is
/// `<parent-dir>/<dir>`. A worktree's `.git` is a *file*, not a directory, so
/// both forms are accepted.
///
/// **Why not the bare repo basename**, which is what the plan's D7 sketch
/// implies. The 2026-08-15 backfill dry run over the real 1,102-file corpus
/// found `2026-08-04-sccache-daemon-wedge-and-s3-regression` in **both**
/// `qontinui-dev-notes/plans` and `qontinui-dev-notes/prompts` — same repo,
/// same stem, different kind, different content. A bare-basename
/// `source_repo` gives those two copies the *same* identity key, and because a
/// `kind_is_heuristic` upsert resolves by `(org, slug, source_repo)` **ignoring
/// kind**, they would land on one row: each scan cycle would overwrite the
/// other's body and flip the kind back, appending two version rows a minute
/// forever. Qualifying by the scan root makes the two copies two rows, which is
/// what D7 asks for ("collapsing on stem alone would silently destroy one
/// copy") — the same argument, one directory level deeper.
///
/// Consequence worth knowing: the web list route's `?repo=` filter matches
/// `repos[] OR source_repo`, so `?repo=qontinui-dev-notes` no longer matches on
/// the `source_repo` arm. That arm was always the weaker one — `source_repo` is
/// *where the file lives*, while `repos[]` (parsed from the `> **Repo(s):**`
/// line) is *what the work targets*, and only the latter is what a repo filter
/// means to ask.
pub fn derive_source_repo(dir: &Path) -> Option<String> {
    let two_component = |parent: Option<&Path>, leaf: &Path| -> Option<String> {
        let leaf = leaf.file_name()?.to_string_lossy().to_string();
        if leaf.is_empty() {
            return None;
        }
        match parent.and_then(|p| p.file_name()) {
            Some(p) => Some(format!("{}/{}", p.to_string_lossy(), leaf)),
            None => Some(leaf),
        }
    };

    let mut cur = Some(dir);
    while let Some(d) = cur {
        if d.join(".git").exists() {
            let repo = d.file_name()?.to_string_lossy().to_string();
            let rel = dir
                .strip_prefix(d)
                .ok()?
                .to_string_lossy()
                .replace('\\', "/");
            return Some(if rel.is_empty() {
                repo
            } else {
                format!("{repo}/{rel}")
            });
        }
        cur = d.parent();
    }
    two_component(dir.parent(), dir)
}

// ============================================================================
// The wire payload
// ============================================================================

/// `POST {backend}/api/v1/plan-library` body — mirrors the web side's
/// `WorkArtifactUpsert`.
///
/// **There is deliberately no `organization_id`.** The web model omits it so a
/// caller has nowhere to put one; this struct keeps that property on the client
/// side too, so a future edit cannot accidentally reintroduce a caller-supplied
/// scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ArtifactUpsert {
    pub kind: String,
    pub slug: String,
    pub title: String,
    /// Opaque. Never validated, never a 422.
    pub status: String,
    pub body: String,
    /// Always `Some(sha256(body))` — see the module docs on the 422.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_repo: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub work_unit_slug: Option<String>,
    pub repos: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authored_at: Option<String>,
    pub captured_by: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub change_description: Option<String>,
    /// `true` for every scan-originated write — see the module docs.
    pub kind_is_heuristic: bool,
}

impl ArtifactUpsert {
    /// The client-side identity key, matching the server's
    /// `(org, kind, slug, source_repo)` minus the server-derived org.
    pub fn identity(&self) -> (String, String, String) {
        (
            self.kind.clone(),
            self.slug.clone(),
            self.source_repo.clone().unwrap_or_default(),
        )
    }

    /// The kind-agnostic key the *server* resolves a heuristic upsert by
    /// (`kind_is_heuristic: true` ignores kind). Used for the client-side
    /// unchanged-sha memory so a re-classification does not read as a new row.
    pub fn scan_identity(&self) -> (String, String) {
        (
            self.slug.clone(),
            self.source_repo.clone().unwrap_or_default(),
        )
    }
}

/// One scanned file, ready to push, plus what the scan learned about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScannedArtifact {
    pub upsert: ArtifactUpsert,
    pub kind: ArtifactKind,
    /// Canonical `Depends-On:` stems from the status blockquote. Pushed as
    /// `depends_on` edges in a second pass, once every artifact has an id.
    pub depends_on: Vec<String>,
    /// `sha256(body)`, hoisted out of the payload for the skip check.
    pub sha: String,
}

/// Build the wire payload for one parsed file.
///
/// `parsed` comes from the existing [`parse_work_unit`], so plan parsing is
/// shared with [`super::push`] byte for byte — there is exactly one markdown
/// convention in this tree, not two.
pub fn build_artifact(
    parsed: &ParsedWorkUnit,
    kind: ArtifactKind,
    source_repo: Option<String>,
) -> ScannedArtifact {
    let body = parsed.content.clone();
    let sha = content_sha256(&body);
    // A prompt with no `> **Status:` stamp has no status; only the plan parser's
    // invented `draft` default is suppressed here (see `has_status_stamp`).
    let status = if kind == ArtifactKind::Plan || has_status_stamp(&body) {
        parsed.status.clone()
    } else {
        String::new()
    };
    ScannedArtifact {
        upsert: ArtifactUpsert {
            kind: kind.as_str().to_string(),
            slug: parsed.slug.clone(),
            title: parsed.title.clone().unwrap_or_default(),
            status,
            content_sha256: Some(sha.clone()),
            source_path: Some(parsed.source_path.clone()),
            source_repo,
            // The plan adapter pushes plan slugs to coord as work-unit slugs, so
            // for a plan the two identifiers are the same string. Soft link: it
            // may dangle and is never resolved.
            work_unit_slug: (kind == ArtifactKind::Plan).then(|| parsed.slug.clone()),
            repos: extract_repos(&body),
            authored_at: authored_at_from_stem(&parsed.slug),
            captured_by: CAPTURED_BY_RUNNER_SCAN.to_string(),
            change_description: Some("runner scan".to_string()),
            kind_is_heuristic: true,
            body,
        },
        kind,
        depends_on: parsed.depends_on.clone(),
        sha,
    }
}

// ============================================================================
// Scan roots
// ============================================================================

/// One directory the backfill reads, with the classification context it implies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanRoot {
    pub dir: PathBuf,
    pub kind: ScanRootKind,
    /// D7 identity component, normally from [`derive_source_repo`].
    pub source_repo: Option<String>,
    /// Human label for the dry-run report (`"plans"`, `"plans/archive"`, …).
    pub label: String,
}

impl ScanRoot {
    pub fn new(dir: impl Into<PathBuf>, kind: ScanRootKind, label: impl Into<String>) -> Self {
        let dir = dir.into();
        let source_repo = derive_source_repo(&dir);
        Self {
            dir,
            kind,
            source_repo,
            label: label.into(),
        }
    }
}

/// Assemble the scan roots from the runner's configured directories.
///
/// The active plans dir arrives already resolved through
/// [`super::trigger::resolve_plans_dir`] (env override → setting), so the
/// backfill and the reconcile loop can never disagree about which directory is
/// "the plans dir". The archive dir and the prompts dir are plain settings —
/// blank counts as unset at every layer.
pub fn scan_roots(
    plans_dir: Option<String>,
    archive_dir: Option<String>,
    prompts_dir: Option<String>,
) -> Vec<ScanRoot> {
    let mut out: Vec<ScanRoot> = Vec::new();
    let mut push = |dir: Option<String>, kind: ScanRootKind, label: &str| {
        if let Some(d) = dir.filter(|s| !s.trim().is_empty()) {
            let root = ScanRoot::new(d.trim(), kind, label);
            // Two roots pointing at the SAME directory would produce two
            // artifacts per file sharing one `(slug, source_repo)` identity —
            // they would overwrite each other every cycle. That is a
            // misconfiguration (archive_dir set to plans_dir, prompts_dir set
            // to plans_dir), and the first configured role wins.
            if out.iter().any(|r| r.dir == root.dir) {
                tracing::warn!(
                    dir = %root.dir.display(),
                    ignored_as = label,
                    "plan library: scan root already configured under another role; \
                     ignoring the duplicate (two roots on one directory would make \
                     each file overwrite itself)"
                );
                return;
            }
            out.push(root);
        }
    };
    push(plans_dir, ScanRootKind::Plans, "plans");
    push(archive_dir, ScanRootKind::Plans, "plans/archive");
    push(prompts_dir, ScanRootKind::Prompts, "prompts");
    out
}

/// Why a scanned file produced no artifact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedFile {
    pub path: String,
    pub reason: &'static str,
}

/// Read + classify every `*.md` in one root (non-recursive, matching the flat
/// layout [`super::trigger::read_plan_dir`] already assumes). A missing dir
/// yields nothing; per-file IO errors are collected, never fatal.
pub fn scan_one_root(
    root: &ScanRoot,
    conv: &PlanConvention,
    skipped: &mut Vec<SkippedFile>,
) -> Vec<ScannedArtifact> {
    let entries = match std::fs::read_dir(&root.dir) {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!(
                dir = %root.dir.display(),
                error = %e,
                "plan library: cannot read scan root"
            );
            skipped.push(SkippedFile {
                path: root.dir.to_string_lossy().to_string(),
                reason: "unreadable_dir",
            });
            return Vec::new();
        }
    };
    let mut out = Vec::new();
    // `read_dir` order is filesystem-dependent; sort so a dry-run report is
    // reproducible across runs and machines.
    let mut paths: Vec<PathBuf> = Vec::new();
    // Subdirectories are NOT descended into (the roots are flat, matching
    // `super::trigger::read_plan_dir`) — but they are RECORDED, because a
    // dry-run that silently omits them reads as "there is nothing there" when
    // the truth is "this was never looked at". Absence of a report is not a
    // report of absence.
    let mut subdirs: Vec<PathBuf> = Vec::new();
    for path in entries.flatten().map(|e| e.path()) {
        if path.is_dir() {
            subdirs.push(path);
        } else if path.extension().and_then(|e| e.to_str()) == Some("md") && path.is_file() {
            paths.push(path);
        }
    }
    paths.sort();
    subdirs.sort();
    for dir in subdirs {
        skipped.push(SkippedFile {
            path: dir.to_string_lossy().to_string(),
            reason: "subdirectory_not_scanned",
        });
    }

    for path in paths {
        let path_str = path.to_string_lossy().to_string();
        let body = match std::fs::read_to_string(&path) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(path = %path_str, error = %e, "plan library: cannot read file");
                skipped.push(SkippedFile {
                    path: path_str,
                    reason: "unreadable_file",
                });
                continue;
            }
        };
        if !has_document_structure(&body) {
            skipped.push(SkippedFile {
                path: path_str,
                reason: "no_markdown_structure",
            });
            continue;
        }
        let slug = slug_from_filename(&path_str);
        let kind = classify_kind(root.kind, &slug, &body);
        let parsed = parse_work_unit(&slug, &path_str, &body, conv);
        out.push(build_artifact(&parsed, kind, root.source_repo.clone()));
    }
    out
}

/// Read + classify every root, in order.
pub fn scan_all_roots(
    roots: &[ScanRoot],
    conv: &PlanConvention,
) -> (Vec<ScannedArtifact>, Vec<SkippedFile>) {
    let mut skipped = Vec::new();
    let mut out = Vec::new();
    for root in roots {
        out.extend(scan_one_root(root, conv, &mut skipped));
    }
    (out, skipped)
}

// ============================================================================
// Dry-run report
// ============================================================================

/// A stem that exists in more than one scan root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DivergentStem {
    pub slug: String,
    /// One `(source_repo, sha256)` per copy, in scan order.
    pub copies: Vec<(String, String)>,
    /// `true` when the copies' bodies actually differ. A duplicated stem whose
    /// copies are byte-identical is not a divergence — it is a mirror.
    pub differs: bool,
}

/// What a backfill dry run found. Reports counts per kind (including zeros) and
/// the duplicated-stem list, which is the falsifiable output Phase 2 exists to
/// produce.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BackfillReport {
    pub scanned: usize,
    pub per_kind: Vec<(ArtifactKind, usize)>,
    pub per_root: Vec<(String, usize)>,
    pub skipped: Vec<SkippedFile>,
    /// Stems present in more than one root, divergent copies first.
    pub duplicate_stems: Vec<DivergentStem>,
    pub with_repos: usize,
    pub with_depends_on: usize,
}

impl BackfillReport {
    pub fn divergent_count(&self) -> usize {
        self.duplicate_stems.iter().filter(|d| d.differs).count()
    }
}

/// Build the dry-run report from a scan. Pure — no HTTP, no clock.
pub fn build_report(
    artifacts: &[ScannedArtifact],
    skipped: Vec<SkippedFile>,
    per_root: Vec<(String, usize)>,
) -> BackfillReport {
    let mut per_kind: Vec<(ArtifactKind, usize)> = ArtifactKind::ALL
        .iter()
        .map(|k| (*k, artifacts.iter().filter(|a| a.kind == *k).count()))
        .collect();
    per_kind.sort_by_key(|(k, _)| *k);

    // Group by stem; a stem with >1 copy is a duplicate, and it "differs" when
    // the copies' digests are not all equal.
    let mut by_slug: Vec<(String, Vec<(String, String)>)> = Vec::new();
    for a in artifacts {
        let slug = a.upsert.slug.clone();
        let copy = (
            a.upsert.source_repo.clone().unwrap_or_default(),
            a.sha.clone(),
        );
        match by_slug.iter_mut().find(|(s, _)| *s == slug) {
            Some((_, copies)) => copies.push(copy),
            None => by_slug.push((slug, vec![copy])),
        }
    }
    let mut duplicate_stems: Vec<DivergentStem> = by_slug
        .into_iter()
        .filter(|(_, copies)| copies.len() > 1)
        .map(|(slug, copies)| {
            let differs = copies.iter().any(|(_, sha)| sha != &copies[0].1);
            DivergentStem {
                slug,
                copies,
                differs,
            }
        })
        .collect();
    // Divergent first, then alphabetical — the divergent ones are the finding.
    duplicate_stems.sort_by(|a, b| b.differs.cmp(&a.differs).then(a.slug.cmp(&b.slug)));

    BackfillReport {
        scanned: artifacts.len(),
        per_kind,
        per_root,
        skipped,
        duplicate_stems,
        with_repos: artifacts
            .iter()
            .filter(|a| !a.upsert.repos.is_empty())
            .count(),
        with_depends_on: artifacts
            .iter()
            .filter(|a| !a.depends_on.is_empty())
            .count(),
    }
}

/// Render the report as the operator-facing text the backfill subcommand prints.
pub fn render_report(report: &BackfillReport) -> String {
    use std::fmt::Write as _;
    let mut s = String::new();
    let _ = writeln!(s, "scanned: {}", report.scanned);
    let _ = writeln!(s, "\nby root:");
    for (label, n) in &report.per_root {
        let _ = writeln!(s, "  {label:<16} {n}");
    }
    let _ = writeln!(s, "\nby kind:");
    for (kind, n) in &report.per_kind {
        let _ = writeln!(s, "  {:<22} {}", kind.as_str(), n);
    }
    let _ = writeln!(
        s,
        "\nwith Repo(s): {}   with Depends-On: {}",
        report.with_repos, report.with_depends_on
    );
    let _ = writeln!(
        s,
        "\nduplicate stems: {} ({} DIVERGENT by content)",
        report.duplicate_stems.len(),
        report.divergent_count()
    );
    for d in &report.duplicate_stems {
        let marker = if d.differs { "DIVERGENT" } else { "identical" };
        let _ = writeln!(s, "  [{marker}] {}", d.slug);
        for (repo, sha) in &d.copies {
            let repo = if repo.is_empty() { "(no repo)" } else { repo };
            let _ = writeln!(s, "      {repo:<24} {}", &sha[..12.min(sha.len())]);
        }
    }
    if !report.skipped.is_empty() {
        let _ = writeln!(s, "\nskipped: {}", report.skipped.len());
        for sk in &report.skipped {
            let _ = writeln!(s, "  [{}] {}", sk.reason, sk.path);
        }
    }
    s
}

// ============================================================================
// The sink
// ============================================================================

/// Outcome of one upsert, as the web side reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpsertResult {
    /// The stored artifact's id — needed to hang edges off it. `None` only on
    /// the [`UpsertResult::ambiguous`] path, where nothing was written.
    pub id: Option<String>,
    /// `false` on the no-op path (unchanged digest, `X-Artifact-Unchanged: true`).
    pub changed: bool,
    pub created: bool,
    /// The web side answered `409 ambiguous_kind_resolution`: this
    /// `(org, slug, source_repo)` already has several rows with different kinds
    /// and no single `kind_locked` winner, so a heuristic scan is refused rather
    /// than allowed to pick one. **Nothing was written.**
    ///
    /// This is an expected state of the real corpus, not a client bug — it is
    /// the fork the operator resolves with `PATCH .../kind`. Counting it as an
    /// error would bury it in the noise; counting it separately makes it the
    /// actionable line of the report. The unchanged-sha memory is deliberately
    /// NOT updated for it, so the next tick retries and the fork heals itself
    /// the moment the operator locks a kind.
    pub ambiguous: bool,
}

/// The web side of a body push, abstracted so the push logic is testable
/// without live HTTP.
#[async_trait::async_trait]
pub trait ArtifactSink: Send + Sync {
    async fn upsert(&self, body: &ArtifactUpsert) -> Result<UpsertResult>;
    /// `POST /api/v1/plan-library/{from_id}/edges` with `{to_id, relation}` —
    /// an OUTGOING edge from `from_id`. Idempotent server-side.
    async fn link(
        &self,
        from_id: &str,
        to_id: &str,
        relation: &str,
        note: Option<&str>,
    ) -> Result<()>;
}

/// One provenance edge, as the server identifies it: the route is idempotent on
/// `(from_id, to_id, relation)`, so that triple is exactly the right memo key.
type EdgeKey = (String, String, String);

/// How many times one edge may fail before this process stops retrying it.
///
/// Bounds the wasted traffic from a systemically broken edges route: five
/// attempts spread over five cycles (~5 minutes at the default tick) is ample
/// for a transient, and after that the triple is abandoned for the process
/// rather than re-POSTed forever. Deliberately NOT wired into
/// [`super::trigger::BodySync`]'s breaker: edges are additive best-effort, and
/// a broken edge route must not stop bodies from being captured.
const MAX_EDGE_ATTEMPTS: u32 = 5;

/// Client-side memory: the digest last successfully pushed per
/// `(slug, source_repo)`, plus the artifact ids the edge pass needs and the
/// edges already applied.
///
/// Keyed kind-agnostically because that is exactly how the server resolves a
/// `kind_is_heuristic` upsert — if the classifier changes its mind about a
/// file, that is an *update* of one row, not a new one, and the memory must
/// agree or the skip check would miss.
#[derive(Debug, Clone, Default)]
pub struct ArtifactSyncState {
    last_sha: HashMap<(String, String), String>,
    ids: HashMap<(String, String), String>,
    /// Edges this process has already applied. Without it the edge pass is
    /// unconditional, so ~200 `Depends-On:` stems become ~200 POSTs every tick
    /// forever — flatly contradicting the "steady state costs one directory
    /// walk and zero HTTP calls" contract the digest memory exists to provide.
    ///
    /// Memoizing the **applied triple** rather than skipping the edge pass for
    /// unchanged artifacts is the stronger of the two shapes. It is keyed on
    /// what the server is idempotent on, so it is correct whatever made the
    /// artifact skip (a local digest hit, a server-side no-op, or nothing at
    /// all); and because only *successes* are recorded here, an edge whose POST
    /// failed is naturally retried next cycle — which the artifact-level skip
    /// would have suppressed forever, since the artifact stays unchanged.
    applied_edges: HashSet<EdgeKey>,
    /// Consecutive failures per edge, so a permanently broken edge route is
    /// abandoned after [`MAX_EDGE_ATTEMPTS`] rather than retried every tick.
    edge_failures: HashMap<EdgeKey, u32>,
}

impl ArtifactSyncState {
    pub fn new() -> Self {
        Self::default()
    }
    /// The artifact id last seen for this `(slug, source_repo)`, if any.
    pub fn id_for(&self, slug: &str, source_repo: Option<&str>) -> Option<&String> {
        self.ids.get(&(
            slug.to_string(),
            source_repo.unwrap_or_default().to_string(),
        ))
    }
    pub fn len(&self) -> usize {
        self.last_sha.len()
    }
    pub fn is_empty(&self) -> bool {
        self.last_sha.is_empty()
    }
    /// How many edges this process has successfully applied.
    pub fn applied_edge_count(&self) -> usize {
        self.applied_edges.len()
    }
}

/// What one [`push_artifact`] call did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyPushOutcome {
    /// The client-side digest memory matched — no HTTP call was made at all.
    SkippedUnchangedLocally,
    /// The server reported `changed: false` (its own no-op path). Note the web
    /// side deliberately does NOT rewrite metadata on this path — only a real
    /// body change persists `title`/`status`/`repos`/…
    UnchangedRemotely,
    Created,
    Updated,
    /// `409 ambiguous_kind_resolution` — see [`UpsertResult::ambiguous`].
    AmbiguousKind,
}

/// Push one scanned artifact, skipping entirely when the digest is unchanged
/// since our last successful push of the same `(slug, source_repo)`.
pub async fn push_artifact<S: ArtifactSink + ?Sized>(
    sink: &S,
    artifact: &ScannedArtifact,
    state: &mut ArtifactSyncState,
) -> Result<BodyPushOutcome> {
    let key = artifact.upsert.scan_identity();
    if state.last_sha.get(&key) == Some(&artifact.sha) {
        return Ok(BodyPushOutcome::SkippedUnchangedLocally);
    }
    let result = sink.upsert(&artifact.upsert).await?;
    if result.ambiguous {
        // Nothing was written: do NOT remember the sha, so the next tick retries
        // once the operator has locked a kind on one of the forked rows.
        return Ok(BodyPushOutcome::AmbiguousKind);
    }
    state.last_sha.insert(key.clone(), artifact.sha.clone());
    if let Some(id) = result.id {
        state.ids.insert(key, id);
    }
    Ok(match (result.created, result.changed) {
        (true, _) => BodyPushOutcome::Created,
        (false, true) => BodyPushOutcome::Updated,
        (false, false) => BodyPushOutcome::UnchangedRemotely,
    })
}

/// Summary of a full backfill pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BackfillSummary {
    pub created: u64,
    pub updated: u64,
    pub unchanged_remote: u64,
    pub skipped_local: u64,
    /// `409 ambiguous_kind_resolution` refusals — an operator-actionable kind
    /// fork, counted apart from `errors` so it does not read as a client fault.
    pub ambiguous_kind: u64,
    pub errors: u64,
    pub edges_set: u64,
    pub edges_unresolved: u64,
    pub edge_errors: u64,
    /// Edges this process already applied, so no HTTP call was made. In steady
    /// state this is the whole `Depends-On:` corpus and `edges_set` is 0.
    pub edges_skipped_applied: u64,
    /// Edges abandoned after [`MAX_EDGE_ATTEMPTS`] consecutive failures.
    pub edges_given_up: u64,
}

/// Push every scanned artifact, then wire the `depends_on` edges.
///
/// Two passes, because an edge needs both endpoints' ids and the web edge route
/// takes UUIDs, not slugs. The edge pass is **best effort**: a `Depends-On:`
/// stem naming a plan that is not in any scanned root cannot be resolved to an
/// id, and that is a normal result (the operator's plans reference stems that
/// were archived elsewhere or never written) — it is counted, not an error.
///
/// Resolution prefers a target in the SAME `source_repo`, so a dep declared in a
/// dev-notes plan binds to the dev-notes copy of a duplicated stem rather than
/// crossing repos arbitrarily.
///
/// The edge pass is **memoized** in [`ArtifactSyncState::applied_edges`], so a
/// second cycle over an unchanged corpus makes zero edge calls — without that,
/// the digest memory short-circuits pass 1 only and pass 2 re-POSTs every
/// `depends_on` edge on every tick forever.
pub async fn backfill_once<S: ArtifactSink + ?Sized>(
    sink: &S,
    artifacts: &[ScannedArtifact],
    state: &mut ArtifactSyncState,
) -> BackfillSummary {
    let mut summary = BackfillSummary::default();
    for a in artifacts {
        match push_artifact(sink, a, state).await {
            Ok(BodyPushOutcome::Created) => summary.created += 1,
            Ok(BodyPushOutcome::Updated) => summary.updated += 1,
            Ok(BodyPushOutcome::UnchangedRemotely) => summary.unchanged_remote += 1,
            Ok(BodyPushOutcome::SkippedUnchangedLocally) => summary.skipped_local += 1,
            Ok(BodyPushOutcome::AmbiguousKind) => {
                summary.ambiguous_kind += 1;
                tracing::warn!(
                    slug = %a.upsert.slug,
                    source_repo = ?a.upsert.source_repo,
                    "plan library: kind fork — several rows share (slug, source_repo) \
                     with no locked winner; nothing written. Resolve with \
                     PATCH /api/v1/plan-library/<id>/kind"
                );
            }
            Err(e) => {
                summary.errors += 1;
                tracing::warn!(
                    slug = %a.upsert.slug,
                    error = %format!("{e:#}"),
                    "plan library: artifact upsert failed"
                );
            }
        }
    }

    for a in artifacts {
        if a.depends_on.is_empty() {
            continue;
        }
        let own_repo = a.upsert.source_repo.clone().unwrap_or_default();
        let Some(from_id) = state
            .id_for(&a.upsert.slug, a.upsert.source_repo.as_deref())
            .cloned()
        else {
            // Its own upsert failed; the edge has no anchor.
            summary.edge_errors += a.depends_on.len() as u64;
            continue;
        };
        for dep in &a.depends_on {
            // Same-source_repo first. Failing that, fall back to the copy with
            // the lexicographically smallest source_repo — `HashMap` iteration
            // order is unspecified, so picking "the first match found" would
            // make a cross-root dep edge land on a DIFFERENT copy of a
            // duplicated stem from run to run.
            let to_id = match state.ids.get(&(dep.clone(), own_repo.clone())) {
                Some(id) => Some(id.clone()),
                None => state
                    .ids
                    .iter()
                    .filter(|((slug, _), _)| slug == dep)
                    .min_by(|((_, a), _), ((_, b), _)| a.cmp(b))
                    .map(|(_, id)| id.clone()),
            };
            let Some(to_id) = to_id else {
                summary.edges_unresolved += 1;
                continue;
            };
            // The server is idempotent on this triple, so re-POSTing one it
            // already accepted buys nothing and costs a request per artifact
            // per tick.
            let key: EdgeKey = (
                from_id.clone(),
                to_id.clone(),
                RELATION_DEPENDS_ON.to_string(),
            );
            if state.applied_edges.contains(&key) {
                summary.edges_skipped_applied += 1;
                continue;
            }
            if state.edge_failures.get(&key).copied().unwrap_or(0) >= MAX_EDGE_ATTEMPTS {
                summary.edges_given_up += 1;
                continue;
            }
            match sink
                .link(&from_id, &to_id, RELATION_DEPENDS_ON, Some("Depends-On:"))
                .await
            {
                Ok(()) => {
                    summary.edges_set += 1;
                    state.edge_failures.remove(&key);
                    state.applied_edges.insert(key);
                }
                Err(e) => {
                    summary.edge_errors += 1;
                    let attempts = state.edge_failures.entry(key).or_insert(0);
                    *attempts += 1;
                    let giving_up = *attempts >= MAX_EDGE_ATTEMPTS;
                    tracing::warn!(
                        slug = %a.upsert.slug,
                        dep = %dep,
                        attempt = *attempts,
                        giving_up,
                        error = %format!("{e:#}"),
                        "plan library: depends_on edge failed (non-fatal)"
                    );
                }
            }
        }
    }
    summary
}

// ============================================================================
// HTTP sink
// ============================================================================

/// Env override for the qontinui-web backend base URL, checked before
/// [`WEB_BACKEND_URL_ENV_ALT`]. Same two vars `api_config::get_api_base_url`
/// honours in the binary; this module lives in the lib crate, which cannot see
/// the binary's settings store, so the env layers are resolved here and the
/// persisted setting is passed in by the caller.
pub const WEB_BACKEND_URL_ENV: &str = "QONTINUI_WEB_BACKEND_URL";
/// Second env layer, matching `api_config`'s precedence.
pub const WEB_BACKEND_URL_ENV_ALT: &str = "QONTINUI_API_URL";

/// Resolve the web backend base: env → `configured` → `None`.
///
/// Pure over its inputs so the precedence is a unit test. Returns `None` rather
/// than guessing `localhost` — a runner with no configured backend must no-op,
/// never blind-post a 1,100-file corpus at whatever happens to be on :8000.
/// A trailing slash is trimmed so callers can `format!("{base}/api/v1/…")`.
pub fn resolve_backend_base(
    env_primary: Option<String>,
    env_alt: Option<String>,
    configured: Option<String>,
) -> Option<String> {
    env_primary
        .filter(|s| !s.trim().is_empty())
        .or_else(|| env_alt.filter(|s| !s.trim().is_empty()))
        .or_else(|| configured.filter(|s| !s.trim().is_empty()))
        .map(|s| s.trim().trim_end_matches('/').to_string())
}

/// [`resolve_backend_base`] reading the two env vars itself.
pub fn backend_base_from_env(configured: Option<String>) -> Option<String> {
    resolve_backend_base(
        std::env::var(WEB_BACKEND_URL_ENV).ok(),
        std::env::var(WEB_BACKEND_URL_ENV_ALT).ok(),
        configured,
    )
}

/// Resolve the backend base for a caller carrying an **explicit override** —
/// the `qontinui-pr plan-library-backfill --backend <url>` flag.
///
/// Precedence is `flag → env_primary → env_alt`, i.e. the flag WINS. That is
/// the opposite of [`backend_base_from_env`]'s `configured` slot, and the
/// difference is not cosmetic: a runner-provisioned terminal exports
/// [`WEB_BACKEND_URL_ENV_ALT`] into every session, so threading `--backend`
/// into the lowest-precedence slot means the flag is silently ignored exactly
/// where an operator is most likely to reach for it — and a
/// `--backend http://127.0.0.1:8000` intended to keep a 1,100-artifact backfill
/// local would instead go wherever the ambient env points, plausibly
/// production. A flag the caller typed always outranks ambient environment.
///
/// Pure over its inputs; [`backend_base_from_flag_or_env`] supplies the env.
pub fn resolve_backend_base_with_flag(
    flag: Option<String>,
    env_primary: Option<String>,
    env_alt: Option<String>,
) -> Option<String> {
    // The three slots of `resolve_backend_base` are a plain precedence chain,
    // so reusing it here also reuses the blank-is-unset and trailing-slash
    // rules rather than re-deriving them.
    resolve_backend_base(flag, env_primary, env_alt)
}

/// [`resolve_backend_base_with_flag`] reading the two env vars itself.
pub fn backend_base_from_flag_or_env(flag: Option<String>) -> Option<String> {
    resolve_backend_base_with_flag(
        flag,
        std::env::var(WEB_BACKEND_URL_ENV).ok(),
        std::env::var(WEB_BACKEND_URL_ENV_ALT).ok(),
    )
}

/// Production [`ArtifactSink`]: HTTP against qontinui-web with the runner's
/// device-JWT bearer.
///
/// The bearer comes from [`crate::auth::attach_device_auth`] — the runner
/// resolves it from its own credential store; it is never taken from a request.
#[derive(Clone)]
pub struct HttpArtifactSink {
    base: String,
    client: reqwest::Client,
}

/// Per-request ceiling on a plan-library call.
///
/// A body push carries up to a couple of megabytes of markdown, so this is
/// looser than `fleet_policy_poller`'s 10s — but it must exist: the sync runs
/// inside the reconcile tick, so a black-holing backend with no timeout parks
/// the whole plan-adapter loop indefinitely rather than failing one cycle.
const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

impl HttpArtifactSink {
    pub fn new(base: impl Into<String>) -> Self {
        Self {
            base: base.into().trim_end_matches('/').to_string(),
            // One long-lived client per sink: `reqwest::Client` owns the
            // connection pool and the TLS config, so building one per call
            // would defeat connection reuse entirely.
            client: reqwest::Client::builder()
                .timeout(REQUEST_TIMEOUT)
                .build()
                // `build()` only fails on a broken TLS backend, which a default
                // client would hit too — fall back rather than panic on the
                // runner's boot path.
                .unwrap_or_else(|e| {
                    tracing::warn!(
                        error = %e,
                        "plan library: could not build a timeout-bearing HTTP client; \
                         falling back to the default (no timeout)"
                    );
                    reqwest::Client::new()
                }),
        }
    }

    /// `None` when no backend is configured — the caller then no-ops.
    pub fn from_env(configured: Option<String>) -> Option<Self> {
        backend_base_from_env(configured).map(Self::new)
    }

    pub fn base(&self) -> &str {
        &self.base
    }
}

/// Turn a non-2xx web response into an error that carries the upstream body
/// verbatim, plus the one diagnosis a bare 401 does not give you.
fn upstream_error(what: &str, status: reqwest::StatusCode, text: String) -> anyhow::Error {
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return anyhow::anyhow!(
            "{what} -> {status} {text}\n\
             note: this client presents the runner's coord-issued DEVICE JWT. The \
             qontinui-web plan-library routes must therefore accept a device bearer \
             (app.api.deps.get_authenticated_device_user / a user-or-device dependency); \
             a route wired to `current_active_user` alone is Cognito-only and rejects it."
        );
    }
    anyhow::anyhow!("{what} -> {status} {text}")
}

#[async_trait::async_trait]
impl ArtifactSink for HttpArtifactSink {
    async fn upsert(&self, body: &ArtifactUpsert) -> Result<UpsertResult> {
        let url = format!("{}/api/v1/plan-library", self.base);
        // coord-tenant-scope(work-owed): the sink holds only base+client with no session id -- and this targets qontinui-web, NOT coord: its axis is organization_id derived from the authenticated principal, never the body. E3: whether a coord tenant maps onto a web org is unresolved. Phase 6.
        let resp = crate::auth::attach_device_auth(self.client.post(&url).json(body))
            .send()
            .await
            .context("POST /api/v1/plan-library")?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            // A heuristic scan that matched several differently-kinded rows with
            // no locked winner is REFUSED, not failed — the web side answers
            // `409 {"error":"ambiguous_kind_resolution"}` and writes nothing.
            if status == reqwest::StatusCode::CONFLICT && text.contains("ambiguous_kind_resolution")
            {
                return Ok(UpsertResult {
                    id: None,
                    changed: false,
                    created: false,
                    ambiguous: true,
                });
            }
            return Err(upstream_error(
                &format!("upsert artifact {}", body.slug),
                status,
                text,
            ));
        }
        let parsed: serde_json::Value = resp
            .json()
            .await
            .context("parse plan-library upsert response")?;
        let id = parsed
            .get("artifact")
            .and_then(|a| a.get("id"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("plan-library upsert response has no artifact.id"))?
            .to_string();
        Ok(UpsertResult {
            id: Some(id),
            changed: parsed
                .get("changed")
                .and_then(|v| v.as_bool())
                .unwrap_or(true),
            created: parsed
                .get("created")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            ambiguous: false,
        })
    }

    async fn link(
        &self,
        from_id: &str,
        to_id: &str,
        relation: &str,
        note: Option<&str>,
    ) -> Result<()> {
        let url = format!("{}/api/v1/plan-library/{}/edges", self.base, from_id);
        let body = serde_json::json!({
            "to_id": to_id,
            "relation": relation,
            "note": note,
        });
        // coord-tenant-scope(work-owed): same session-less web sink; qontinui-web derives organization_id from the principal, not from this body. E3 applies. Phase 6.
        let resp = crate::auth::attach_device_auth(self.client.post(&url).json(&body))
            .send()
            .await
            .context("POST /api/v1/plan-library/:id/edges")?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(upstream_error(
                &format!("link {from_id} -{relation}-> {to_id}"),
                status,
                text,
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // ---- kind classification -------------------------------------------

    #[test]
    fn a_plans_root_is_always_a_plan() {
        // Even when the plan's body quotes a prompt phrase verbatim — plans
        // authored FROM a prompt routinely paste it in.
        let body = "# P\n\nauthor a plan from the following investigation\n";
        assert_eq!(
            classify_kind(ScanRootKind::Plans, "2026-01-01-x", body),
            ArtifactKind::Plan
        );
    }

    #[test]
    fn investigate_then_author_marker_beats_the_filename() {
        let body = "# Whatever\n\nINVESTIGATE, THEN AUTHOR A PLAN from the findings.\n";
        assert_eq!(
            classify_kind(ScanRootKind::Prompts, "some-unrelated-name", body),
            ArtifactKind::InvestigationPrompt
        );
    }

    #[test]
    fn author_a_plan_from_marker_yields_plan_authoring_prompt() {
        let body = "# X\n\nPlease author a plan from the attached report.\n";
        assert_eq!(
            classify_kind(ScanRootKind::Prompts, "x", body),
            ArtifactKind::PlanAuthoringPrompt
        );
    }

    /// The corpus fact from vetting: `prompts/merge-queue-report.md` is a
    /// REPORT sitting in a prompts dir, so the directory alone cannot decide.
    #[test]
    fn a_report_in_a_prompts_dir_is_not_a_prompt() {
        let body = "# Qontinui Merge Queue & PR Status Report\n\n**Generated**: live\n";
        assert_eq!(
            classify_kind(ScanRootKind::Prompts, "merge-queue-report", body),
            ArtifactKind::InvestigationReport
        );
    }

    #[test]
    fn report_suffix_wins_over_investigate_prefix() {
        let body = "# Findings\n";
        assert_eq!(
            classify_kind(ScanRootKind::Prompts, "investigate-x-report", body),
            ArtifactKind::InvestigationReport
        );
    }

    #[test]
    fn investigate_prefix_and_investigation_suffix() {
        let body = "# Go look\n";
        assert_eq!(
            classify_kind(
                ScanRootKind::Prompts,
                "investigate-merge-train-anomalies-2026-07-31",
                body
            ),
            ArtifactKind::InvestigationPrompt
        );
        assert_eq!(
            classify_kind(
                ScanRootKind::Prompts,
                "2026-07-28-paktis-missing-transcripts-investigation",
                body
            ),
            ArtifactKind::InvestigationPrompt
        );
    }

    #[test]
    fn handoff_filenames_classify_as_handoff() {
        assert_eq!(
            classify_kind(
                ScanRootKind::Prompts,
                "2026-06-13-terminal-orchestration-handoff-brief",
                "# Brief\n"
            ),
            ArtifactKind::Handoff
        );
    }

    #[test]
    fn otherwise_implementation_prompt() {
        assert_eq!(
            classify_kind(
                ScanRootKind::Prompts,
                "coord-push-to-branch-tool",
                "# Tool\n"
            ),
            ArtifactKind::ImplementationPrompt
        );
    }

    /// The three files in the operator's root `prompts/` carry NO `YYYY-MM-DD-`
    /// prefix, so no rule may require one.
    #[test]
    fn undated_stems_classify_fine() {
        assert_eq!(
            classify_kind(
                ScanRootKind::Prompts,
                "feature-pr-failure-reasons-prompt",
                "# Feature Prompt: Extract Failing CI Check Details\n"
            ),
            ArtifactKind::ImplementationPrompt
        );
        assert_eq!(
            classify_kind(
                ScanRootKind::Prompts,
                "verify-state-machine-correctness",
                "# Verify State Machine Correctness\n\n## Prompt\n"
            ),
            ArtifactKind::ImplementationPrompt
        );
        assert_eq!(
            authored_at_from_stem("feature-pr-failure-reasons-prompt"),
            None
        );
    }

    // ---- junk exclusion -------------------------------------------------

    /// `D:/qontinui-root/plans/plans to do.md`: a newline-separated scratch
    /// list, zero headings, no Status stamp. It must be excluded or it parses
    /// to a junk artifact.
    #[test]
    fn structureless_scratch_list_is_excluded() {
        let body = "778,1068 shepherd dep-wait suppression\n\nshepherd-loop\n\nspeed-up-CI\n";
        assert!(!has_document_structure(body));
    }

    #[test]
    fn one_heading_or_a_status_stamp_is_enough() {
        assert!(has_document_structure("# Title\n"));
        assert!(has_document_structure("## Sub only\n"));
        assert!(has_document_structure("> **Status: vetted**\n"));
        assert!(!has_document_structure("just prose\nmore prose\n"));
    }

    // ---- status ---------------------------------------------------------

    #[test]
    fn a_prompt_without_a_status_stamp_gets_an_empty_status_not_draft() {
        let body = "# A prompt\n\nDo the thing.\n";
        let parsed = parse_work_unit(
            "p",
            "prompts/p.md",
            body,
            &PlanConvention::operator_default(),
        );
        assert_eq!(parsed.status, "draft", "the plan parser invents draft");
        let a = build_artifact(&parsed, ArtifactKind::ImplementationPrompt, None);
        assert_eq!(a.upsert.status, "", "but a prompt with no stamp has none");
    }

    #[test]
    fn a_plan_without_a_status_stamp_keeps_the_draft_default() {
        let body = "# A plan\n\nbody\n";
        let parsed = parse_work_unit("p", "plans/p.md", body, &PlanConvention::operator_default());
        let a = build_artifact(&parsed, ArtifactKind::Plan, None);
        assert_eq!(a.upsert.status, "draft");
    }

    #[test]
    fn a_stamped_prompt_keeps_its_opaque_status() {
        let body = "# A prompt\n\n> **Status: IN PROGRESS 2026-05-19.**\n";
        let parsed = parse_work_unit(
            "p",
            "prompts/p.md",
            body,
            &PlanConvention::operator_default(),
        );
        let a = build_artifact(&parsed, ArtifactKind::ImplementationPrompt, None);
        assert_eq!(a.upsert.status, "in_progress");
    }

    // ---- repos ----------------------------------------------------------

    #[test]
    fn repos_from_a_blockquote_line_with_roles_and_a_period() {
        let body = "# T\n> **Repo(s):** qontinui-coord (primary), qontinui-web (Phase 4 alembic migration).\n";
        assert_eq!(extract_repos(body), vec!["qontinui-coord", "qontinui-web"]);
    }

    #[test]
    fn repos_from_a_bare_backticked_line() {
        let body = "**Repo(s):** `ui-bridge`\n";
        assert_eq!(extract_repos(body), vec!["ui-bridge"]);
    }

    #[test]
    fn repos_split_on_plus_and_survive_a_wrapped_blockquote() {
        let body = "> **Repo(s):** qontinui-coord (progress PATCH already added by #749) + qontinui-runner (progress outbox\n> writer).\n";
        assert_eq!(
            extract_repos(body),
            vec!["qontinui-coord", "qontinui-runner"]
        );
    }

    #[test]
    fn repos_are_deduped_and_lowercased() {
        let body = "**Repo(s):** Qontinui-Web, qontinui-web, qontinui-runner\n";
        assert_eq!(extract_repos(body), vec!["qontinui-web", "qontinui-runner"]);
    }

    #[test]
    fn no_repo_line_is_a_real_empty() {
        assert!(extract_repos("# T\n\nbody\n").is_empty());
    }

    /// The continuation walk must stop at the next bold marker, or a
    /// `> **Status:` line directly under the repos line would leak in.
    #[test]
    fn continuation_stops_at_the_next_bold_marker() {
        let body = "> **Repo(s):** qontinui-runner\n> **Status: vetted**\n";
        assert_eq!(extract_repos(body), vec!["qontinui-runner"]);
    }

    // ---- digest + authored_at -------------------------------------------

    #[test]
    fn sha256_matches_the_server_algorithm() {
        // hex sha256 of the UTF-8 bytes, per crud.compute_content_sha256.
        assert_eq!(
            content_sha256("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            content_sha256(""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn the_digest_we_send_is_over_the_bytes_we_send() {
        // The endpoint 422s a digest that disagrees with sha256(body), so this
        // invariant is the difference between a working push and a 100% 422 rate.
        let parsed = parse_work_unit(
            "s",
            "plans/s.md",
            "# T\n\nbody\n",
            &PlanConvention::default(),
        );
        let a = build_artifact(&parsed, ArtifactKind::Plan, None);
        assert_eq!(
            a.upsert.content_sha256.as_deref(),
            Some(content_sha256(&a.upsert.body).as_str())
        );
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

    // ---- the payload ----------------------------------------------------

    /// D5/security: a caller-supplied organization is a scope-escalation bug.
    /// The server model has no field for one; assert the client cannot emit one.
    #[test]
    fn the_payload_never_carries_an_organization() {
        let parsed = parse_work_unit("s", "plans/s.md", "# T\n", &PlanConvention::default());
        let a = build_artifact(
            &parsed,
            ArtifactKind::Plan,
            Some("qontinui-dev-notes".into()),
        );
        let j = serde_json::to_value(&a.upsert).unwrap();
        let obj = j.as_object().unwrap();
        assert!(!obj.contains_key("organization_id"));
        assert!(!obj.contains_key("org_id"));
        assert!(!obj.contains_key("organization"));
    }

    /// Every scan-originated write must carry the kind-stickiness flag, or a
    /// human-corrected kind forks into a second row on the next tick.
    #[test]
    fn scan_writes_are_heuristic_and_stamped_runner_scan() {
        let parsed = parse_work_unit("s", "prompts/s.md", "# T\n", &PlanConvention::default());
        let a = build_artifact(&parsed, ArtifactKind::ImplementationPrompt, None);
        assert!(a.upsert.kind_is_heuristic);
        assert_eq!(a.upsert.captured_by, CAPTURED_BY_RUNNER_SCAN);
    }

    #[test]
    fn only_plans_carry_the_work_unit_soft_link() {
        let parsed = parse_work_unit("s", "plans/s.md", "# T\n", &PlanConvention::default());
        assert_eq!(
            build_artifact(&parsed, ArtifactKind::Plan, None)
                .upsert
                .work_unit_slug
                .as_deref(),
            Some("s")
        );
        assert_eq!(
            build_artifact(&parsed, ArtifactKind::ImplementationPrompt, None)
                .upsert
                .work_unit_slug,
            None
        );
    }

    // ---- scan roots ------------------------------------------------------

    #[test]
    fn scan_roots_skip_blank_and_unset_dirs() {
        let roots = scan_roots(Some("/w/plans".into()), Some("   ".into()), None);
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].label, "plans");
        assert_eq!(roots[0].kind, ScanRootKind::Plans);
        assert!(scan_roots(None, None, None).is_empty());
    }

    /// A misconfiguration guard: two roles pointing at one directory would give
    /// every file two artifacts sharing one identity, so each would overwrite
    /// the other on every cycle. First role configured wins.
    #[test]
    fn duplicate_scan_root_directories_are_ignored() {
        let roots = scan_roots(
            Some("/w/plans".into()),
            Some("/w/plans".into()),
            Some("/w/plans".into()),
        );
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].label, "plans");
        assert_eq!(roots[0].kind, ScanRootKind::Plans);
    }

    #[test]
    fn the_archive_root_is_a_plans_root_and_the_prompts_root_is_not() {
        let roots = scan_roots(
            Some("/w/plans".into()),
            Some("/w/dn/plans".into()),
            Some("/w/prompts".into()),
        );
        assert_eq!(roots.len(), 3);
        assert_eq!(roots[1].kind, ScanRootKind::Plans);
        assert_eq!(roots[2].kind, ScanRootKind::Prompts);
    }

    #[test]
    fn source_repo_outside_a_git_tree_is_the_parent_and_leaf_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("qontinui-root");
        let plans = root.join("plans");
        std::fs::create_dir_all(&plans).unwrap();
        // The no-git branch is only exercised when TMP itself is not inside a
        // checkout; on a machine where it is, the git branch above applies and
        // there is nothing here to assert.
        let mut ancestor = Some(dir.path());
        let mut in_repo = false;
        while let Some(a) = ancestor {
            if a.join(".git").exists() {
                in_repo = true;
                break;
            }
            ancestor = a.parent();
        }
        if !in_repo {
            assert_eq!(
                derive_source_repo(&plans).as_deref(),
                Some("qontinui-root/plans")
            );
        }
    }

    #[test]
    fn source_repo_inside_a_git_tree_is_repo_plus_relative_dir() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("qontinui-dev-notes");
        let plans = repo.join("plans");
        let prompts = repo.join("prompts");
        std::fs::create_dir_all(&plans).unwrap();
        std::fs::create_dir_all(&prompts).unwrap();
        // A worktree's `.git` is a FILE; assert the file form is accepted.
        std::fs::write(repo.join(".git"), "gitdir: /elsewhere\n").unwrap();
        assert_eq!(
            derive_source_repo(&plans).as_deref(),
            Some("qontinui-dev-notes/plans")
        );
        // The regression this shape exists for: two roots in the SAME repo must
        // NOT share an identity, or their same-stem copies collide on one row.
        assert_eq!(
            derive_source_repo(&prompts).as_deref(),
            Some("qontinui-dev-notes/prompts")
        );
        assert_ne!(derive_source_repo(&plans), derive_source_repo(&prompts));
        // The repo root itself is just the repo name.
        assert_eq!(
            derive_source_repo(&repo).as_deref(),
            Some("qontinui-dev-notes")
        );
    }

    /// The corpus case, end to end: one stem, two roots in one repo, different
    /// bodies — two artifacts with distinct identities, not one flip-flopping row.
    #[test]
    fn a_same_stem_plan_and_prompt_in_one_repo_do_not_collide() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("qontinui-dev-notes");
        let plans = repo.join("plans");
        let prompts = repo.join("prompts");
        std::fs::create_dir_all(&plans).unwrap();
        std::fs::create_dir_all(&prompts).unwrap();
        std::fs::write(repo.join(".git"), "gitdir: x").unwrap();
        write(&plans, "2026-08-04-sccache-wedge.md", "# The plan\n");
        write(&prompts, "2026-08-04-sccache-wedge.md", "# The prompt\n");

        let roots = vec![
            ScanRoot::new(&plans, ScanRootKind::Plans, "plans"),
            ScanRoot::new(&prompts, ScanRootKind::Prompts, "prompts"),
        ];
        let (artifacts, _) = scan_all_roots(&roots, &PlanConvention::operator_default());
        assert_eq!(artifacts.len(), 2);
        assert_ne!(
            artifacts[0].upsert.scan_identity(),
            artifacts[1].upsert.scan_identity(),
            "the kind-agnostic key the SERVER resolves by must differ, or these \
             two files overwrite each other every cycle"
        );
    }

    // ---- scanning + report ----------------------------------------------

    fn write(dir: &Path, name: &str, body: &str) {
        std::fs::write(dir.join(name), body).unwrap();
    }

    #[test]
    fn a_scan_excludes_the_structureless_file_and_classifies_the_rest() {
        let tmp = tempfile::tempdir().unwrap();
        let plans = tmp.path().join("plans");
        let prompts = tmp.path().join("prompts");
        std::fs::create_dir_all(&plans).unwrap();
        std::fs::create_dir_all(&prompts).unwrap();
        write(
            &plans,
            "2026-01-01-alpha.md",
            "# Alpha\n\n> **Status: vetted** Depends-On: 2026-01-01-beta\n",
        );
        write(
            &plans,
            "2026-01-01-beta.md",
            "# Beta\n\n> **Status: shipped**\n",
        );
        write(&plans, "plans to do.md", "alpha\n\nbeta\n\ngamma\n");
        write(&prompts, "merge-queue-report.md", "# Merge Queue Report\n");
        write(&prompts, "investigate-x.md", "# Look\n");

        let roots = vec![
            ScanRoot::new(&plans, ScanRootKind::Plans, "plans"),
            ScanRoot::new(&prompts, ScanRootKind::Prompts, "prompts"),
        ];
        let (artifacts, skipped) = scan_all_roots(&roots, &PlanConvention::operator_default());

        assert_eq!(artifacts.len(), 4, "the scratch list is excluded");
        assert_eq!(skipped.len(), 1);
        assert_eq!(skipped[0].reason, "no_markdown_structure");
        assert!(skipped[0].path.ends_with("plans to do.md"));

        let kinds: Vec<&str> = artifacts.iter().map(|a| a.kind.as_str()).collect();
        assert_eq!(
            kinds,
            vec![
                "plan",
                "plan",
                "investigation_prompt",
                "investigation_report"
            ],
            "sorted by path within each root"
        );
        let alpha = &artifacts[0];
        assert_eq!(alpha.depends_on, vec!["2026-01-01-beta".to_string()]);
    }

    /// The scan is non-recursive, and a subdirectory must be RECORDED as
    /// not-looked-at rather than silently omitted. Omitting it makes the
    /// dry-run read "there is nothing there" when the truth is "this was never
    /// scanned" — the silent-empty-is-unknown shape.
    #[test]
    fn a_subdirectory_is_recorded_as_skipped_not_silently_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        let plans = tmp.path().join("plans");
        let nested = plans.join("2026-q3");
        std::fs::create_dir_all(&nested).unwrap();
        write(&plans, "2026-01-01-top.md", "# Top\n");
        write(&nested, "2026-01-02-buried.md", "# Buried\n");

        let root = ScanRoot::new(&plans, ScanRootKind::Plans, "plans");
        let mut skipped = Vec::new();
        let found = scan_one_root(&root, &PlanConvention::operator_default(), &mut skipped);

        assert_eq!(found.len(), 1, "the nested file is NOT scanned");
        let subdir: Vec<&SkippedFile> = skipped
            .iter()
            .filter(|s| s.reason == "subdirectory_not_scanned")
            .collect();
        assert_eq!(
            subdir.len(),
            1,
            "but the directory IS reported: {skipped:?}"
        );
        assert!(subdir[0].path.ends_with("2026-q3"));

        // And it reaches the operator-facing report.
        let report = build_report(&found, skipped, vec![("plans".into(), 1)]);
        assert!(render_report(&report).contains("subdirectory_not_scanned"));
    }

    /// The marker match is case-insensitive without allocating a lowercase copy
    /// of every body on every tick.
    #[test]
    fn the_body_markers_match_in_any_case() {
        for body in [
            "# X\n\nINVESTIGATE, THEN AUTHOR A PLAN\n",
            "# X\n\ninvestigate, then author a plan\n",
            "# X\n\nInVeStIgAtE, ThEn AuThOr A pLaN\n",
        ] {
            assert_eq!(
                classify_kind(ScanRootKind::Prompts, "x", body),
                ArtifactKind::InvestigationPrompt,
                "failed for {body:?}"
            );
        }
        assert!(contains_ignore_ascii_case("AbC", "abc"));
        assert!(!contains_ignore_ascii_case("ab", "abc"));
        assert!(contains_ignore_ascii_case("", ""));
        // Non-ASCII bytes must not confuse the window scan.
        assert!(contains_ignore_ascii_case("héllo WORLD", "world"));
    }

    #[test]
    fn the_report_flags_a_duplicated_stem_only_when_the_bodies_differ() {
        let tmp = tempfile::tempdir().unwrap();
        let a_dir = tmp.path().join("repo-a").join("plans");
        let b_dir = tmp.path().join("repo-b").join("plans");
        std::fs::create_dir_all(&a_dir).unwrap();
        std::fs::create_dir_all(&b_dir).unwrap();
        std::fs::write(tmp.path().join("repo-a").join(".git"), "gitdir: x").unwrap();
        std::fs::write(tmp.path().join("repo-b").join(".git"), "gitdir: y").unwrap();
        write(&a_dir, "2026-01-01-same.md", "# Same\n");
        write(&b_dir, "2026-01-01-same.md", "# Same\n");
        write(&a_dir, "2026-01-01-differs.md", "# One\n");
        write(
            &b_dir,
            "2026-01-01-differs.md",
            "# Two — genuinely different\n",
        );

        let roots = vec![
            ScanRoot::new(&a_dir, ScanRootKind::Plans, "a"),
            ScanRoot::new(&b_dir, ScanRootKind::Plans, "b"),
        ];
        let (artifacts, skipped) = scan_all_roots(&roots, &PlanConvention::operator_default());
        let report = build_report(&artifacts, skipped, vec![("a".into(), 2), ("b".into(), 2)]);

        assert_eq!(report.scanned, 4);
        assert_eq!(report.duplicate_stems.len(), 2);
        assert_eq!(report.divergent_count(), 1);
        // Divergent sorts first.
        assert_eq!(report.duplicate_stems[0].slug, "2026-01-01-differs");
        assert!(report.duplicate_stems[0].differs);
        assert!(!report.duplicate_stems[1].differs);
        // Both copies survive: the source_repo component of the identity key is
        // what keeps them apart.
        assert_eq!(report.duplicate_stems[0].copies.len(), 2);
        assert_ne!(
            report.duplicate_stems[0].copies[0].0,
            report.duplicate_stems[0].copies[1].0
        );

        let rendered = render_report(&report);
        assert!(rendered.contains("DIVERGENT"));
        assert!(rendered.contains("2026-01-01-differs"));
        // Every kind gets a row, including the zeros.
        assert!(rendered.contains("handoff"));
    }

    // ---- backend base resolution ----------------------------------------

    #[test]
    fn backend_base_precedence_and_trailing_slash() {
        assert_eq!(
            resolve_backend_base(Some("http://a/".into()), Some("http://b".into()), None)
                .as_deref(),
            Some("http://a")
        );
        assert_eq!(
            resolve_backend_base(Some("  ".into()), Some("http://b".into()), None).as_deref(),
            Some("http://b")
        );
        assert_eq!(
            resolve_backend_base(None, None, Some("http://c".into())).as_deref(),
            Some("http://c")
        );
        assert_eq!(resolve_backend_base(None, None, None), None);
        assert_eq!(
            resolve_backend_base(None, None, Some("   ".into())),
            None,
            "a blank setting must not become a directory-less base"
        );
    }

    /// The `--backend` flag OUTRANKS the environment.
    ///
    /// The shipped wiring threaded the flag into the lowest-precedence slot, so
    /// inside a runner terminal — where `QONTINUI_API_URL` is exported into
    /// every session — `--backend http://127.0.0.1:8000` was silently ignored
    /// and the whole 1,100-artifact corpus went wherever the env pointed,
    /// plausibly production. A flag the operator typed always wins.
    #[test]
    fn an_explicit_backend_flag_beats_both_env_layers() {
        assert_eq!(
            resolve_backend_base_with_flag(
                Some("http://127.0.0.1:8000".into()),
                Some("https://api.qontinui.io".into()),
                Some("https://also-not-this".into()),
            )
            .as_deref(),
            Some("http://127.0.0.1:8000")
        );
        // No flag ⇒ the env layers keep their own order.
        assert_eq!(
            resolve_backend_base_with_flag(
                None,
                Some("https://primary".into()),
                Some("https://alt".into()),
            )
            .as_deref(),
            Some("https://primary")
        );
        // A blank flag is unset, not an empty base — it must not shadow the env.
        assert_eq!(
            resolve_backend_base_with_flag(Some("  ".into()), Some("https://primary".into()), None)
                .as_deref(),
            Some("https://primary")
        );
        assert_eq!(resolve_backend_base_with_flag(None, None, None), None);
    }

    /// The same precedence through the wrapper that reads the real env vars —
    /// the shape the CLI actually calls.
    #[test]
    fn the_flag_beats_the_env_through_the_env_reading_wrapper() {
        let _guard = crate::test_env::env_lock();
        let _restore = crate::test_env::EnvVarRestore::capture(&[
            WEB_BACKEND_URL_ENV,
            WEB_BACKEND_URL_ENV_ALT,
        ]);
        std::env::remove_var(WEB_BACKEND_URL_ENV);
        std::env::set_var(WEB_BACKEND_URL_ENV_ALT, "https://api.qontinui.io");

        assert_eq!(
            backend_base_from_flag_or_env(Some("http://127.0.0.1:8000".into())).as_deref(),
            Some("http://127.0.0.1:8000"),
            "a typed --backend must not be overridden by the terminal's exported env"
        );
        assert_eq!(
            backend_base_from_flag_or_env(None).as_deref(),
            Some("https://api.qontinui.io"),
            "with no flag the env is still the default"
        );
    }

    // ---- push behaviour --------------------------------------------------

    #[derive(Default)]
    struct FakeSink {
        upserts: Mutex<Vec<ArtifactUpsert>>,
        links: Mutex<Vec<(String, String, String)>>,
        /// Slug -> id handed back, so the edge pass has something to resolve.
        next_id: Mutex<u32>,
        /// When true, `changed` comes back false (the server no-op path).
        unchanged: bool,
        /// When true, every upsert answers the 409 kind-fork refusal.
        ambiguous: bool,
        /// When true, every `link` call errors — a broken edges route.
        fail_links: bool,
    }

    #[async_trait::async_trait]
    impl ArtifactSink for FakeSink {
        async fn upsert(&self, body: &ArtifactUpsert) -> Result<UpsertResult> {
            self.upserts.lock().unwrap().push(body.clone());
            let mut n = self.next_id.lock().unwrap();
            *n += 1;
            if self.ambiguous {
                return Ok(UpsertResult {
                    id: None,
                    changed: false,
                    created: false,
                    ambiguous: true,
                });
            }
            // The id encodes BOTH identity components, so a test can tell WHICH
            // copy of a duplicated stem an edge resolved to.
            Ok(UpsertResult {
                id: Some(format!(
                    "id-{}@{}",
                    body.slug,
                    body.source_repo.as_deref().unwrap_or("-")
                )),
                changed: !self.unchanged,
                created: !self.unchanged,
                ambiguous: false,
            })
        }
        async fn link(
            &self,
            from_id: &str,
            to_id: &str,
            relation: &str,
            _note: Option<&str>,
        ) -> Result<()> {
            self.links.lock().unwrap().push((
                from_id.to_string(),
                to_id.to_string(),
                relation.to_string(),
            ));
            if self.fail_links {
                anyhow::bail!("simulated edges-route failure");
            }
            Ok(())
        }
    }

    fn artifact(slug: &str, repo: Option<&str>, body: &str, deps: &[&str]) -> ScannedArtifact {
        let mut parsed = parse_work_unit(
            slug,
            &format!("plans/{slug}.md"),
            body,
            &PlanConvention::operator_default(),
        );
        parsed.depends_on = deps.iter().map(|s| s.to_string()).collect();
        build_artifact(&parsed, ArtifactKind::Plan, repo.map(String::from))
    }

    #[tokio::test]
    async fn an_unchanged_sha_makes_no_http_call_at_all() {
        let sink = FakeSink::default();
        let mut state = ArtifactSyncState::new();
        let a = artifact("s", None, "# T\n", &[]);

        assert_eq!(
            push_artifact(&sink, &a, &mut state).await.unwrap(),
            BodyPushOutcome::Created
        );
        assert_eq!(
            push_artifact(&sink, &a, &mut state).await.unwrap(),
            BodyPushOutcome::SkippedUnchangedLocally
        );
        assert_eq!(
            sink.upserts.lock().unwrap().len(),
            1,
            "the second pass never reached the network"
        );
    }

    #[tokio::test]
    async fn a_changed_body_pushes_again() {
        let sink = FakeSink::default();
        let mut state = ArtifactSyncState::new();
        push_artifact(&sink, &artifact("s", None, "# T\n", &[]), &mut state)
            .await
            .unwrap();
        push_artifact(
            &sink,
            &artifact("s", None, "# T\n\nmore\n", &[]),
            &mut state,
        )
        .await
        .unwrap();
        assert_eq!(sink.upserts.lock().unwrap().len(), 2);
    }

    /// The client memory is kind-agnostic because the SERVER resolves a
    /// heuristic upsert kind-agnostically. If the classifier changes its mind,
    /// that is one row updating, not a new row — and the skip check must agree.
    #[tokio::test]
    async fn the_skip_memory_is_keyed_ignoring_kind() {
        let sink = FakeSink::default();
        let mut state = ArtifactSyncState::new();
        let mut a = artifact("s", None, "# T\n", &[]);
        push_artifact(&sink, &a, &mut state).await.unwrap();
        a.upsert.kind = ArtifactKind::Handoff.as_str().to_string();
        assert_eq!(
            push_artifact(&sink, &a, &mut state).await.unwrap(),
            BodyPushOutcome::SkippedUnchangedLocally
        );
    }

    #[tokio::test]
    async fn the_server_no_op_path_is_reported_as_unchanged_remotely() {
        let sink = FakeSink {
            unchanged: true,
            ..Default::default()
        };
        let mut state = ArtifactSyncState::new();
        assert_eq!(
            push_artifact(&sink, &artifact("s", None, "# T\n", &[]), &mut state)
                .await
                .unwrap(),
            BodyPushOutcome::UnchangedRemotely
        );
    }

    /// A kind fork is REFUSED, not failed — and the refusal must not poison the
    /// unchanged-sha memory, or the scan would stop retrying and the fork would
    /// never heal after the operator locks a kind.
    #[tokio::test]
    async fn an_ambiguous_kind_refusal_is_not_an_error_and_is_retried() {
        let sink = FakeSink {
            ambiguous: true,
            ..Default::default()
        };
        let mut state = ArtifactSyncState::new();
        let a = artifact("s", None, "# T\n", &[]);
        assert_eq!(
            push_artifact(&sink, &a, &mut state).await.unwrap(),
            BodyPushOutcome::AmbiguousKind
        );
        assert!(
            state.is_empty(),
            "nothing was written, so nothing is remembered"
        );
        // Next tick tries again rather than skipping.
        assert_eq!(
            push_artifact(&sink, &a, &mut state).await.unwrap(),
            BodyPushOutcome::AmbiguousKind
        );
        assert_eq!(sink.upserts.lock().unwrap().len(), 2);

        let summary = backfill_once(&sink, &[a], &mut state).await;
        assert_eq!(summary.ambiguous_kind, 1);
        assert_eq!(summary.errors, 0, "a kind fork is not a client error");
    }

    #[tokio::test]
    async fn depends_on_becomes_an_edge_preferring_the_same_source_repo() {
        let sink = FakeSink::default();
        let mut state = ArtifactSyncState::new();
        let artifacts = vec![
            artifact("dep", Some("repo-a"), "# Dep A\n", &[]),
            artifact("dep", Some("repo-b"), "# Dep B\n", &[]),
            artifact("child", Some("repo-b"), "# Child\n", &["dep"]),
        ];
        let summary = backfill_once(&sink, &artifacts, &mut state).await;

        assert_eq!(summary.created, 3);
        assert_eq!(summary.edges_set, 1);
        assert_eq!(summary.edges_unresolved, 0);
        let links = sink.links.lock().unwrap();
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].2, RELATION_DEPENDS_ON);
        assert_eq!(links[0].0, "id-child@repo-b");
        assert_eq!(
            links[0].1, "id-dep@repo-b",
            "the dep must bind to the SAME-repo copy, not the other one"
        );
    }

    /// A cross-root dep must land on the SAME copy every run — `HashMap`
    /// iteration order is unspecified, so "first match wins" would silently
    /// flip which copy of a duplicated stem an edge points at.
    #[tokio::test]
    async fn a_cross_repo_dep_fallback_is_deterministic() {
        let artifacts = vec![
            artifact("dep", Some("zzz-repo"), "# Z\n", &[]),
            artifact("dep", Some("aaa-repo"), "# A\n", &[]),
            artifact("dep", Some("mmm-repo"), "# M\n", &[]),
            // `child` is in a root that holds no copy of `dep`.
            artifact("child", Some("other-repo"), "# C\n", &["dep"]),
        ];
        for _ in 0..5 {
            let sink = FakeSink::default();
            let mut state = ArtifactSyncState::new();
            let summary = backfill_once(&sink, &artifacts, &mut state).await;
            assert_eq!(summary.edges_set, 1);
            let links = sink.links.lock().unwrap();
            assert_eq!(links[0].0, "id-child@other-repo");
            assert_eq!(
                links[0].1, "id-dep@aaa-repo",
                "the fallback must be the lexicographically smallest source_repo \
                 on EVERY run, not whichever HashMap iteration reached first"
            );
        }
    }

    /// The contract `trigger.rs` advertises — "steady state costs one directory
    /// walk and zero HTTP calls" — applied to pass 2, which the digest memory
    /// does NOT cover. Before the edge memo the second cycle re-POSTed every
    /// `depends_on` edge, so ~200 stems became ~200 POSTs every 60s forever.
    #[tokio::test]
    async fn a_second_cycle_over_unchanged_artifacts_makes_zero_edge_calls() {
        let sink = FakeSink::default();
        let mut state = ArtifactSyncState::new();
        let artifacts = vec![
            artifact("dep", Some("repo-a"), "# Dep\n", &[]),
            artifact("child", Some("repo-a"), "# Child\n", &["dep"]),
            artifact("other", Some("repo-a"), "# Other\n", &["dep"]),
        ];

        let first = backfill_once(&sink, &artifacts, &mut state).await;
        assert_eq!(first.created, 3);
        assert_eq!(first.edges_set, 2);
        assert_eq!(first.edges_skipped_applied, 0);
        assert_eq!(sink.links.lock().unwrap().len(), 2);

        let second = backfill_once(&sink, &artifacts, &mut state).await;
        assert_eq!(
            second.skipped_local, 3,
            "pass 1 is short-circuited by the digest memory"
        );
        assert_eq!(
            second.edges_set, 0,
            "pass 2 must not re-POST an edge the server already accepted"
        );
        assert_eq!(second.edges_skipped_applied, 2);
        assert_eq!(
            sink.links.lock().unwrap().len(),
            2,
            "ZERO edge calls on the second cycle — the whole point of the memo"
        );

        // And a third, to show it is memoized rather than merely alternating.
        backfill_once(&sink, &artifacts, &mut state).await;
        assert_eq!(sink.links.lock().unwrap().len(), 2);
        assert_eq!(state.applied_edge_count(), 2);
    }

    /// A FAILED edge is not memoized, so it retries — but only up to
    /// [`MAX_EDGE_ATTEMPTS`], after which this process abandons it instead of
    /// re-POSTing a doomed request every tick forever.
    #[tokio::test]
    async fn a_failing_edge_retries_then_is_given_up_on() {
        let sink = FakeSink {
            fail_links: true,
            ..Default::default()
        };
        let mut state = ArtifactSyncState::new();
        let artifacts = vec![
            artifact("dep", Some("r"), "# Dep\n", &[]),
            artifact("child", Some("r"), "# Child\n", &["dep"]),
        ];

        for cycle in 1..=MAX_EDGE_ATTEMPTS {
            let s = backfill_once(&sink, &artifacts, &mut state).await;
            assert_eq!(s.edge_errors, 1, "cycle {cycle} must still be trying");
            assert_eq!(s.edges_given_up, 0);
        }
        assert_eq!(
            sink.links.lock().unwrap().len(),
            MAX_EDGE_ATTEMPTS as usize,
            "exactly the retry budget, no more"
        );

        // Budget exhausted: no further HTTP, counted as given up.
        let after = backfill_once(&sink, &artifacts, &mut state).await;
        assert_eq!(after.edge_errors, 0);
        assert_eq!(after.edges_given_up, 1);
        assert_eq!(
            sink.links.lock().unwrap().len(),
            MAX_EDGE_ATTEMPTS as usize,
            "a permanently broken edge route stops costing a request per tick"
        );
    }

    /// A dep whose target only appears in a LATER cycle still links — the memo
    /// records successes, so it cannot suppress an edge that was never applied.
    #[tokio::test]
    async fn an_edge_that_becomes_resolvable_later_is_still_applied() {
        let sink = FakeSink::default();
        let mut state = ArtifactSyncState::new();
        let child = artifact("child", Some("r"), "# Child\n", &["dep"]);

        let first = backfill_once(&sink, std::slice::from_ref(&child), &mut state).await;
        assert_eq!(first.edges_unresolved, 1);
        assert!(sink.links.lock().unwrap().is_empty());

        let both = vec![artifact("dep", Some("r"), "# Dep\n", &[]), child];
        let second = backfill_once(&sink, &both, &mut state).await;
        assert_eq!(second.edges_set, 1);
        assert_eq!(sink.links.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn an_unresolvable_dep_is_counted_not_an_error() {
        let sink = FakeSink::default();
        let mut state = ArtifactSyncState::new();
        let artifacts = vec![artifact("child", None, "# C\n", &["2026-01-01-nowhere"])];
        let summary = backfill_once(&sink, &artifacts, &mut state).await;
        assert_eq!(summary.edges_unresolved, 1);
        assert_eq!(summary.edge_errors, 0);
        assert!(sink.links.lock().unwrap().is_empty());
    }
}
