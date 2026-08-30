//! Session POLICY_COMPLIANCE detection + verdict emit (plan
//! `2026-07-30-session-compliance-report-enforcement.md`, §A1 / §A1a / §A4).
//!
//! `policy/session-protocol` v4 Step 3 requires every session to close with a
//! structured compliance report. Nothing checked that it happened. This module
//! is the runner's half of the check, under the corrected architecture:
//! **the runner executes, coord configures, qontinui-web controls.**
//!
//! ## What runs, and when
//!
//! - **Every turn end.** The runner-bundled Claude `Stop` hook already POSTs
//!   to `/sessions/{id}/continuation-verdict`
//!   ([`crate::mcp::continuation_verdict`]). That handler calls
//!   [`observe_turn_end`], which reads the session's transcript
//!   (`transcript_path` straight off the hook payload), extracts the
//!   structured block, and POSTs the verdict to coord.
//! - **Session close.** [`finalize_on_close`] runs the same detection once
//!   more from the single-fire close observer on
//!   [`crate::session::session_lifecycle_store::SessionLifecycleStore`], so
//!   the last stored verdict reflects the whole session.
//!
//! **Turn end is the load-bearing key, not session end.** Session end is not
//! observable to an acceptable standard here (`pty-exit` is frontend-recorded
//! and absent on window teardown; the `poll-dead` backstop lags ~135 s). Turn
//! end is exact and already wired. Coord's store is last-write-wins per
//! session (`UNIQUE (tenant_id, claude_session_id)`), so firing N times per
//! session is correct and cheap — each firing overwrites with a
//! better-informed verdict.
//!
//! ## Detection is structured, never prose
//!
//! A session passes only by emitting
//!
//! ~~~~text
//! <!-- POLICY_COMPLIANCE v1 -->
//! ```json
//! { "schema": "policy-compliance/1", "items": [ … ], … }
//! ```
//! ~~~~
//!
//! [`extract_compliance_block`] requires the `<!-- POLICY_COMPLIANCE` opener at
//! the START of a line, then validates `schema == "policy-compliance/1"` **and**
//! that `items` is an array. Prose that merely says the words is not a pass —
//! that is what makes the check unfakeable by accident and machine-reconcilable
//! by design. Three parsing hazards, all handled:
//!
//! 1. The block can be split across several `content[]` text blocks (and even
//!    across assistant messages). [`scan_transcript`] joins every assistant
//!    text block with `\n` — the same join `terminal::transcript::
//!    parse_assistant_record` already does — before matching.
//! 2. Multiple blocks in one session: the **last** valid one wins.
//! 3. A stray `{` between the opener and the payload does not lose the block:
//!    [`first_valid_json_after`] retries at successive brace positions.
//!
//! Two honest bounds, both stated in [`coverage_bound`] rather than papered
//! over. (a) An assistant that QUOTES a valid block — pasting an example while
//! explaining the schema, or reviewing this very file — is indistinguishable
//! from one that emits it as its own attestation. The line-start anchor kills
//! inline prose mentions but not deliberate illustration. (b) An `items: []`
//! block is structurally valid here; the runner cannot decide the verdict, so
//! it forwards the report verbatim and coord's §A2 reconcile — which sees BOTH
//! the empty claim set and its own non-empty footprint — is what must refuse to
//! call that `verified`.
//!
//! ## Applicability — checked FIRST, before any parsing
//!
//! `GET /coord/session-compliance/config` decides whether this session is
//! subject to the check at all. The mapping from
//! `{enabled, applicable, applicability_reason}` onto runner behaviour is
//! [`Applicability`]:
//!
//! | coord says | runner does |
//! |---|---|
//! | `applicable` | detect, emit the verdict, nudge-eligible |
//! | `enforcement_disabled` | detect + emit, **never** nudge (report-only soak) |
//! | `clause_absent` / `document_missing` | nothing at all |
//! | route unreachable / 404 / undecodable | nothing at all |
//!
//! The `enforcement_disabled` arm is deliberate and is what makes Phase 1
//! useful before anyone trusts the nudge: coord's `enabled` defaults to
//! `false`, and the plan ships this report-only *on purpose* so the
//! false-positive rate is measured on real sessions first. It is also the only
//! way coord's `not_applicable` verdict can ever be recorded — that verdict
//! exists precisely because the runner still reports when enforcement is off.
//! There is deliberately **no second local flag**: coord's config is the
//! single switch.
//!
//! `clause_absent` / `document_missing`, by contrast, mean the operator's
//! prompt does not carry the clause being enforced, so the session is not
//! subject to it and manufacturing rows about it would be noise.
//!
//! ## Fail open, everywhere
//!
//! Unreachable coord, a 404 (these routes are NOT on production coord yet), a
//! 501, an undecodable body, a missing/unreadable transcript, a malformed
//! payload — every one of them does nothing, silently, and never disturbs the
//! session. Every coord call carries a short timeout. This is the same posture
//! `terminal::auto_response` and `claude_session::coord_register` established.
//!
//! ## The nudge (§A4)
//!
//! Delivered by returning `{"decision":"block","prompt":…}` from the existing
//! continuation-verdict path — no new spawn, and the two loop guards already
//! there (`stop_hook_active` short-circuit, rolling hourly cap) are inherited,
//! not rebuilt. It fires **at most `max_attempts` times per session PER
//! [`NudgeClass`]** (the served config's value, defaulting to 1 and clamped to
//! [`MAX_NUDGE_ATTEMPTS_CEILING`]), and only when ALL of:
//!
//! 1. enforcement is `enabled` AND `applicable`, and
//! 2. the stored verdict is `unverified`, and
//! 3. **coord's independently observed footprint is non-empty** — PRs
//!    authored, commits, or claims.
//!
//! Condition 3 is the whole reason this is safe. Turn end ≠ session end, so a
//! nudge on every turn end would nag every session at its first pause — a
//! false positive by construction. A session that has done no reportable work
//! has nothing to report and gets nothing.
//!
//! ## Two nudge classes, two budgets
//!
//! An `unverified` verdict can mean two independent things, and they need
//! opposite corrections: the REPORT arm (the block is missing or did not
//! reconcile) and the POLICY-PULL arm (coord observed no read of
//! `policy/session-protocol` at its current version — the session is being held
//! to policy it never pulled). [`NudgeClass`] names them, and the per-session
//! marker is keyed on `(session_id, class)` so one can never suppress the
//! other. Only coord's `absent` result is nudgeable: `unavailable` and `error`
//! mean coord could not check, and nudging on either would assert something
//! coord does not know.
//!
//! **`reopen` mode is NOT implemented.** Re-opening a closed session is a
//! spawn that outlives the request, and
//! `production-and-cost#agent-spawn-authorization` requires a standing
//! per-path opt-in that does not exist. When coord's config says `reopen`,
//! this module does nothing — see [`Applicability::nudge_allowed`].

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock, PoisonError, Weak};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::Value;
use tracing::{debug, info};

use super::continuation_verdict::{coord_client_parts, http_client};
use crate::session::session_lifecycle_store::SessionLifecycleStore;
use qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked;

// ===========================================================================
// Constants
// ===========================================================================

/// Opener for the structured block, required at the START of a line (leading
/// whitespace allowed). Anchoring to the HTML-comment opener rather than the
/// bare token is what stops an inline prose mention — "I emitted the
/// POLICY_COMPLIANCE footer" — from locating a block; the schema validation
/// below is what decides the pass.
const MARKER: &str = "<!-- POLICY_COMPLIANCE";

/// How many successive `{` positions [`first_valid_json_after`] will try before
/// giving up on one opener. Bounded so a marker followed by a long brace-heavy
/// prose tail cannot turn into a quadratic scan.
const MAX_BRACE_ATTEMPTS: usize = 8;

/// The only schema string this detector accepts.
pub const SCHEMA: &str = "policy-compliance/1";

/// `absent_reason` sent when a transcript was read successfully and carried no
/// valid block.
const ABSENT_NO_BLOCK: &str = "no_policy_compliance_block";

/// How much of the transcript tail to scan. The block, if present, is in the
/// session's most recent assistant output, and transcripts run to tens of MB
/// on long sessions — reading the whole file on every turn end would be a real
/// cost for no gain.
const TRANSCRIPT_TAIL_BYTES: u64 = 4 * 1024 * 1024;

/// TTL for the process-global enforcement-config cache. Mirrors
/// [`crate::mcp::continuation_verdict`]'s 45 s cache posture.
const CONFIG_TTL: Duration = Duration::from_secs(45);

/// Upper bound on the per-session nudge cap, whatever coord serves.
///
/// The nudge shares an hourly delivery budget with session-continuation, so an
/// unbounded cap does not merely nag one session — it can drain that budget
/// and starve the other feature. Ten is far above any plausible setting and
/// far below anything that could do that.
const MAX_NUDGE_ATTEMPTS_CEILING: u64 = 10;

// ===========================================================================
// Enforcement config (coord contract, §B2)
// ===========================================================================

/// `GET /coord/session-compliance/config`, parsed leniently. Absent fields
/// take conservative defaults (`enabled=false`, `applicable=false`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComplianceConfig {
    pub enabled: bool,
    /// `nudge` | `reopen`. `reopen` is deliberately unimplemented.
    pub mode: String,
    pub max_attempts: u32,
    pub enforced_clause_ref: Option<String>,
    pub applicable: bool,
    /// One of `applicable`, `enforcement_disabled`, `clause_absent`,
    /// `document_missing`.
    pub applicability_reason: String,
    pub clause_resolved_via: Option<String>,
    pub prompt_document_version: Option<i64>,
}

/// What the runner is allowed to do for this session, derived from the config.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applicability {
    /// Detect, emit the verdict, and (subject to §A4's conditions) nudge.
    Enforce,
    /// Detect and emit the verdict; never nudge. The report-only soak arm.
    ReportOnly,
    /// Do nothing at all.
    Inert,
}

impl Applicability {
    /// Should this session be parsed + reported at all?
    pub fn emits(self) -> bool {
        !matches!(self, Applicability::Inert)
    }

    /// Is a corrective nudge permitted? Only under [`Applicability::Enforce`]
    /// — and never in `reopen` mode, which this phase does not implement.
    pub fn nudge_allowed(self, mode: &str) -> bool {
        matches!(self, Applicability::Enforce) && mode.trim().eq_ignore_ascii_case("nudge")
    }
}

impl ComplianceConfig {
    /// Parse the config response body. `None` = undecodable ⇒ caller treats it
    /// as unreachable ⇒ inert.
    pub fn from_body(body: &Value) -> Option<Self> {
        let obj = body.as_object()?;
        let reason = obj
            .get("applicability_reason")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        Some(ComplianceConfig {
            enabled: obj.get("enabled").and_then(Value::as_bool).unwrap_or(false),
            mode: obj
                .get("mode")
                .and_then(Value::as_str)
                .unwrap_or("nudge")
                .trim()
                .to_string(),
            // Clamped to a sane ceiling, not just to `u32::MAX`. This value
            // now drives real delivery, so a fat-fingered `max_attempts:
            // 999999` would make the per-session cap meaningless and leave
            // only the shared hourly budget between the fleet and a nag loop.
            // Absent or unparseable reads as 1 — the pre-existing hard-coded
            // behaviour, so an untouched config changes nothing.
            max_attempts: obj
                .get("max_attempts")
                .and_then(Value::as_u64)
                .unwrap_or(1)
                .min(MAX_NUDGE_ATTEMPTS_CEILING) as u32,
            enforced_clause_ref: obj
                .get("enforced_clause_ref")
                .and_then(Value::as_str)
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty()),
            applicable: obj
                .get("applicable")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            applicability_reason: reason,
            clause_resolved_via: obj
                .get("clause_resolved_via")
                .and_then(Value::as_str)
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty()),
            prompt_document_version: obj.get("prompt_document_version").and_then(Value::as_i64),
        })
    }

    /// Map the config onto what the runner may do. See the module docs for why
    /// `enforcement_disabled` still emits.
    pub fn applicability(&self) -> Applicability {
        match self.applicability_reason.as_str() {
            "clause_absent" | "document_missing" => Applicability::Inert,
            "enforcement_disabled" => Applicability::ReportOnly,
            "applicable" => {
                if self.enabled && self.applicable {
                    Applicability::Enforce
                } else {
                    // Coord said `applicable` but the booleans disagree —
                    // trust the booleans and degrade, never escalate.
                    Applicability::ReportOnly
                }
            }
            // Unknown / absent reason: report only if coord clearly said this
            // session is subject to the check, otherwise stay inert. A future
            // reason word must never silently start blocking sessions.
            _ => {
                if self.enabled && self.applicable {
                    Applicability::ReportOnly
                } else {
                    Applicability::Inert
                }
            }
        }
    }
}

static CONFIG_CACHE: OnceLock<Mutex<Option<(Instant, Option<ComplianceConfig>)>>> = OnceLock::new();

fn config_cache() -> &'static Mutex<Option<(Instant, Option<ComplianceConfig>)>> {
    CONFIG_CACHE.get_or_init(|| Mutex::new(None))
}

/// TTL-cached enforcement-config read. `None` = coord could not be consulted
/// (no JWT, transport error, non-2xx including the 404/501 this route returns
/// until coord's Phase 3 lands, undecodable body) ⇒ the caller does nothing.
async fn fetch_config() -> Option<ComplianceConfig> {
    let now = Instant::now();
    if let Ok(g) = config_cache().lock() {
        if let Some((at, cached)) = g.as_ref() {
            if now.duration_since(*at) < CONFIG_TTL {
                return cached.clone();
            }
        }
    }
    let fresh = fetch_config_uncached().await;
    if let Ok(mut g) = config_cache().lock() {
        *g = Some((now, fresh.clone()));
    }
    fresh
}

async fn fetch_config_uncached() -> Option<ComplianceConfig> {
    let (base, jwt) = match coord_client_parts() {
        Ok(p) => p,
        Err(e) => {
            debug!("session-compliance: config skipped ({e})");
            return None;
        }
    };
    let client = http_client().ok()?;
    let url = format!("{base}/coord/session-compliance/config");
    let resp = client.get(&url).bearer_auth(&jwt).send().await.ok()?;
    if !resp.status().is_success() {
        debug!(
            status = resp.status().as_u16(),
            "session-compliance: config non-2xx — inert this cycle"
        );
        return None;
    }
    let body: Value = resp.json().await.ok()?;
    ComplianceConfig::from_body(&body)
}

// ===========================================================================
// The structured-block parser (pure — the unit-test surface)
// ===========================================================================

/// Extract the LAST valid `policy-compliance/1` block from a text buffer.
///
/// A block is valid when a line STARTS with the [`MARKER`] opener (leading
/// whitespace allowed) and the first JSON value after it — with or without a
/// surrounding ```` ```json ```` fence, on that line or a following one — is an
/// object whose `schema` is exactly [`SCHEMA`] and whose `items` is an array.
/// Prose alone never satisfies that; an inline mention, malformed JSON, a wrong
/// schema string, and a non-array `items` all fail closed.
pub fn extract_compliance_block(text: &str) -> Option<Value> {
    let mut last: Option<Value> = None;
    let mut offset = 0usize;
    // `split_inclusive` keeps the trailing `\n` in each item, so the running
    // byte offset stays exact and every index below is a real char boundary
    // (the marker and the newline are both ASCII).
    for line in text.split_inclusive('\n') {
        let line_start = offset;
        offset += line.len();
        let trimmed = line.trim_start();
        if !trimmed.starts_with(MARKER) {
            continue;
        }
        let after_marker = line_start + (line.len() - trimmed.len()) + MARKER.len();
        if let Some(v) = first_valid_json_after(&text[after_marker..]) {
            last = Some(v);
        }
    }
    last
}

/// Parse the first valid compliance report appearing after an opener, skipping
/// an optional opening code fence.
///
/// Retries at successive `{` positions (bounded by [`MAX_BRACE_ATTEMPTS`]) so a
/// stray brace in the opener's own comment — `<!-- POLICY_COMPLIANCE v1 {see
/// plan} -->` — does not make a genuine block undetectable.
fn first_valid_json_after(rest: &str) -> Option<Value> {
    // Skip an opening fence line (```json / ``` / ~~~json) if one is next.
    let after_fence = {
        let trimmed = rest.trim_start_matches([' ', '\t', '\r', '\n']);
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            // Drop the fence line entirely (it may carry an info string).
            let offset = rest.len() - trimmed.len();
            match trimmed.find('\n') {
                Some(nl) => &rest[offset + nl + 1..],
                None => return None,
            }
        } else {
            rest
        }
    };
    let mut from = 0usize;
    for _ in 0..MAX_BRACE_ATTEMPTS {
        let brace = from + after_fence[from..].find('{')?;
        // A streaming deserializer reads exactly one value and ignores whatever
        // trails it — so a closing fence, more prose, or a later block all
        // terminate cleanly without brace-counting through JSON string escapes.
        let mut stream =
            serde_json::Deserializer::from_str(&after_fence[brace..]).into_iter::<Value>();
        if let Some(Ok(value)) = stream.next() {
            if is_valid_report(&value) {
                return Some(value);
            }
        }
        from = brace + 1;
    }
    None
}

/// Structural validation: the schema string is exact and `items` is an array.
pub fn is_valid_report(v: &Value) -> bool {
    v.get("schema").and_then(Value::as_str) == Some(SCHEMA)
        && v.get("items").map(Value::is_array).unwrap_or(false)
}

/// Scan a transcript-JSONL slice for the session's compliance block.
///
/// Every `{type:"assistant"}` record's `content[]` **text** blocks are joined
/// with `\n` — the same join `parse_assistant_record` performs — across the
/// whole slice, so a block split across content blocks (or across messages)
/// still matches. Malformed lines are skipped, never fatal.
pub fn scan_transcript(slice: &str) -> Option<Value> {
    let mut joined = String::new();
    for line in slice.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(record) = serde_json::from_str::<Value>(trimmed) else {
            continue;
        };
        if record.get("type").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        let Some(content) = record
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(Value::as_array)
        else {
            continue;
        };
        for block in content {
            if block.get("type").and_then(Value::as_str) == Some("text") {
                if let Some(t) = block.get("text").and_then(Value::as_str) {
                    joined.push_str(t);
                    joined.push('\n');
                }
            }
        }
    }
    extract_compliance_block(&joined)
}

/// Read the last [`TRANSCRIPT_TAIL_BYTES`] of a transcript, dropping the
/// (probably truncated) first line when the file was larger than the window.
fn read_transcript_tail(path: &Path) -> Option<String> {
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    let truncated = len > TRANSCRIPT_TAIL_BYTES;
    let start = if truncated {
        len - TRANSCRIPT_TAIL_BYTES
    } else {
        0
    };
    f.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = Vec::with_capacity((len - start) as usize);
    f.read_to_end(&mut buf).ok()?;
    // Take the buffer by value on the (overwhelmingly common) valid-UTF-8 path
    // — `from_utf8_lossy(&buf).into_owned()` would copy the whole window a
    // second time, and the window is megabytes.
    let text = match String::from_utf8(buf) {
        Ok(s) => s,
        Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned(),
    };
    if !truncated {
        return Some(text);
    }
    Some(match text.find('\n') {
        Some(i) => text[i + 1..].to_string(),
        None => String::new(),
    })
}

// ===========================================================================
// Coord emit — POST /coord/sessions/:claude_session_id/compliance
// ===========================================================================

/// Coord's answer to the compliance POST, parsed leniently.
#[derive(Debug, Clone)]
pub struct ComplianceVerdict {
    /// `verified` | `unverified` | `not_applicable`.
    pub verdict: String,
    pub reason: String,
    pub reconciliation: Value,
    pub footprint_prs: Vec<String>,
    pub footprint_commits: Vec<String>,
    pub footprint_claims: Vec<String>,
}

impl ComplianceVerdict {
    pub fn from_body(body: &Value) -> Self {
        let fp = body.get("footprint");
        ComplianceVerdict {
            verdict: body
                .get("verdict")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string(),
            reason: body
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string(),
            reconciliation: body.get("reconciliation").cloned().unwrap_or(Value::Null),
            footprint_prs: fp.map(|f| string_list(f.get("prs"))).unwrap_or_default(),
            footprint_commits: fp
                .map(|f| string_list(f.get("commits")))
                .unwrap_or_default(),
            footprint_claims: fp.map(|f| string_list(f.get("claims"))).unwrap_or_default(),
        }
    }

    /// Has coord independently observed work that a report would be DUE for?
    /// §A4's condition 3 — the guard that keeps the nudge from nagging every
    /// session at its first pause.
    ///
    /// **Claims are deliberately excluded.** The plan originally specified
    /// "PRs, commits, or claims", which is wrong: coord builds `claims` from
    /// `claims_audit` rows with `claim_kind IN ('symbol','file_glob')`, and the
    /// fleet's `/preflight` protocol acquires file-glob claims BEFORE the first
    /// line of code is written. Claims therefore measure work *started*, not
    /// work *done* — they are non-empty from turn 1 of essentially every
    /// session. Combined with the other two conditions (`enabled && applicable`,
    /// and `verdict == "unverified"`, which is by definition what an unfinished
    /// session gets via `reason: "absent"`), including claims would fire the
    /// nudge at the FIRST pause of any session that ran preflight — telling it
    /// to "produce the block now" when it has barely started. That is precisely
    /// the per-turn nag condition 3 exists to prevent, and it would have gone
    /// live the moment an operator flipped the web toggle.
    ///
    /// A PR or a commit is evidence that something landed-or-is-landing and a
    /// report is genuinely owed.
    pub fn footprint_is_empty(&self) -> bool {
        self.footprint_prs.is_empty() && self.footprint_commits.is_empty()
    }
}

/// Coerce a footprint array into display strings. Accepts plain strings and
/// objects.
///
/// The `{repo, pr_number}` arm is NOT optional politeness — it is the shape
/// coord actually emits for `footprint.prs` (`session_footprint` in coord's
/// `session_compliance.rs`), and `pr_number` is a NUMBER. A key-hunt that only
/// accepts string-valued fields drops every PR, which silently defeats §A4's
/// condition 3: a session that authored PRs and skipped the footer — the exact
/// case this plan was commissioned for — reads as an empty footprint and is
/// permanently exempt from the nudge. It also makes the nudge text assert
/// "PRs `none`" about a session with PRs, which is the `ux-priorities#honesty`
/// failure this feature exists to catch.
fn string_list(v: Option<&Value>) -> Vec<String> {
    let Some(arr) = v.and_then(Value::as_array) else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|e| match e {
            Value::String(s) => Some(s.trim().to_string()),
            Value::Object(_) => {
                // A `sha` wins over `pr_number`. coord's `session_commits`
                // emits {sha, repo, branch, pr_number, recorded_at} — a commit
                // pushed on a PR branch carries a NON-NULL `pr_number`, which
                // is the normal case. Probing the PR arm first would render
                // every such commit as `owner/repo#N` in the COMMITS list:
                // false, and useless to the session being nudged, which needs
                // the sha to fill `evidence.sha` on its retry.
                let sha = e
                    .get("sha")
                    .and_then(Value::as_str)
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty());

                // coord's PR shape: `owner/repo#N`, tolerating a
                // number-or-string `pr_number`.
                let pr = sha.or_else(|| {
                    e.get("pr_number")
                        .and_then(|n| {
                            n.as_i64()
                                .map(|i| i.to_string())
                                .or_else(|| n.as_str().map(str::to_string))
                        })
                        .map(|num| match e.get("repo").and_then(Value::as_str) {
                            Some(repo) if !repo.trim().is_empty() => {
                                format!("{}#{}", repo.trim(), num)
                            }
                            _ => format!("#{num}"),
                        })
                });
                pr.or_else(|| {
                    ["ref", "pr", "sha", "key", "id", "resource_key"]
                        .iter()
                        .find_map(|k| e.get(*k).and_then(Value::as_str))
                        .map(|s| s.trim().to_string())
                })
            }
            _ => None,
        })
        .filter(|s| !s.is_empty())
        .collect()
}

/// The compliance POST body.
///
/// Extracted from `post_compliance` so the OMISSION rule is testable without a
/// live coord: the nudge fields are present only when this run actually has a
/// record for the session. Sending `nudge_attempts: 0` for a session this run
/// never saw would overwrite a real count in coord's last-write-wins store —
/// see [`nudge_state_for`]. Older coord builds ignore unknown fields, so
/// sending these before the coord side lands is inert rather than an error.
fn compliance_body(
    report: Option<&Value>,
    absent_reason: Option<&str>,
    nudges: Option<(u32, Option<String>)>,
) -> Value {
    let mut body = serde_json::json!({
        "report": report.cloned().unwrap_or(Value::Null),
        "absent_reason": absent_reason.map(Value::from).unwrap_or(Value::Null),
    });
    if let (Some((attempts, last_at)), Some(map)) = (nudges, body.as_object_mut()) {
        map.insert("nudge_attempts".into(), Value::from(attempts));
        // A recorded session with no timestamp is still a recorded COUNT, so
        // the count goes even when the stamp cannot.
        if let Some(at) = last_at {
            map.insert("last_nudged_at".into(), Value::from(at));
        }
    }
    body
}

/// POST the (possibly absent) report to coord and return its verdict. `None`
/// on every failure path — unreachable coord, 404/501 (the route does not
/// exist on production coord yet), non-2xx, undecodable body.
async fn post_compliance(
    claude_session_id: &str,
    report: Option<&Value>,
    absent_reason: Option<&str>,
) -> Option<ComplianceVerdict> {
    let (base, jwt) = coord_client_parts().ok()?;
    let client = http_client().ok()?;
    let url = format!("{base}/coord/sessions/{claude_session_id}/compliance");
    let body = compliance_body(report, absent_reason, nudge_state_for(claude_session_id));
    // coord-auth-exempt(device-jwt-required): `coord_client_parts` fails closed
    // when unpaired, so the turn-end hook goes inert rather than filing a
    // compliance verdict anonymously.
    let resp = client
        .post(&url)
        .bearer_auth(&jwt)
        .json(&body)
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        debug!(
            status = resp.status().as_u16(),
            session = %claude_session_id,
            "session-compliance: verdict POST non-2xx — no-op"
        );
        return None;
    }
    let parsed: Value = resp.json().await.ok()?;
    Some(ComplianceVerdict::from_body(&parsed))
}

// ===========================================================================
// The nudge (§A4)
// ===========================================================================

/// Which ARM of the compliance verdict a nudge is correcting.
///
/// ## Why this exists at all
///
/// The per-session marker used to be keyed on the session id ALONE. With one
/// nudge class that was indistinguishable from correct; with two it is a
/// silent-suppression bug — whichever class fired first would permanently
/// suppress the other, so a session that got a report nudge could never be told
/// it skipped the policy pull. A verification arm swallowed by an unrelated arm
/// is WORSE than no arm, because the dashboard reads clean while the check is
/// dead. Keying the marker on `(session_id, class)` is what keeps the arms
/// independent.
///
/// The variants are `&'static str`-backed rather than free strings so the key
/// space is closed: a typo cannot mint a third class that silently gets its own
/// budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NudgeClass {
    /// The POLICY_COMPLIANCE block is missing, malformed, or its claims did not
    /// reconcile against coord's own record.
    Report,
    /// Coord observed no read of `policy/session-protocol` at its current
    /// version for this session — the session is being held to policy it never
    /// pulled. Strictly a PRECONDITION of the report arm, which is why it is
    /// offered first when both are due.
    PolicyPull,
}

impl NudgeClass {
    /// Priority order, and the enumeration every "is any class still eligible?"
    /// check walks. [`PolicyPull`](Self::PolicyPull) leads deliberately:
    /// telling a session to fix its report while it has never read the document
    /// that defines the report is backwards.
    pub const ALL: [NudgeClass; 2] = [NudgeClass::PolicyPull, NudgeClass::Report];

    /// The stable label. Appears in the marker key and in operator-visible log
    /// lines, so it is part of the observable contract.
    pub fn as_str(self) -> &'static str {
        match self {
            NudgeClass::Report => "report",
            NudgeClass::PolicyPull => "policy_pull",
        }
    }
}

/// A corrective prompt the continuation-verdict handler may deliver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComplianceNudge {
    /// Claude session id — half of the key [`mark_nudged`] counts against.
    pub session_id: String,
    /// The other half. Carried on the candidate rather than re-derived at
    /// delivery for the same reason [`Self::max_attempts`] is: the delivery site
    /// must claim against exactly the budget the eligibility check consulted.
    pub class: NudgeClass,
    pub prompt: String,
    /// The cap this candidate was authorised against, carried so the delivery
    /// site's compare-and-set checks the SAME number the eligibility check
    /// used. The config is not in scope where delivery happens, and re-reading
    /// it there would let a config change between decision and delivery apply
    /// a different cap than the one that granted the nudge.
    pub max_attempts: u32,
}

/// What this run has actually delivered to one session.
///
/// `attempts` is the OBSERVATION the operator surface reports, distinct from
/// the `max_attempts` POLICY it is checked against. Keeping the two apart is
/// what makes it possible to notice the cap was exceeded rather than assume it
/// held — see the note on the restart window below.
#[derive(Debug, Clone, Default)]
struct NudgeState {
    attempts: u32,
    /// RFC3339, set on each delivery. `None` ⇔ `attempts == 0`; it exists so a
    /// nudge can be correlated with the turn that provoked it, which a bare
    /// counter cannot do.
    last_at: Option<String>,
}

/// Per-session nudge state, process-global and in-memory — the same posture
/// [`crate::mcp::continuation_verdict`]'s hourly cap registry takes.
///
/// This was a `HashSet<String>` of "sessions already nudged", which capped
/// delivery at exactly one per session and ignored the configured
/// `max_attempts` entirely: the setting was parsed and never read, so raising
/// it to 3 changed nothing. It is a `HashMap` now so the cap is the operator's
/// number rather than a hard-coded 1, and so the count can be reported to
/// coord — without it, an `unverified/absent` verdict cannot distinguish
/// "nudged and ignored" from "never nudged", which are opposite conclusions
/// about whether the mechanism works.
///
/// A runner restart still resets it. That can only re-grant a nudge that was
/// never delivered-and-answered, and within one run the cap holds. But a
/// session spanning a restart CAN exceed `max_attempts` overall, and the
/// honest consequence is that the count reported to coord is this run's, not
/// the session's lifetime total.
///
/// **Keyed on `(session_id, class)`, not on `session_id` alone.** The single-key
/// form made the nudge classes MUTUALLY EXCLUSIVE per session: whichever fired
/// first permanently suppressed the other, so a session that got a report nudge
/// could never be told it skipped the policy pull, and the dashboard would read
/// clean while a whole verification arm was dead. See [`NudgeClass`].
static NUDGED: OnceLock<Mutex<HashMap<(String, NudgeClass), NudgeState>>> = OnceLock::new();

fn nudged() -> &'static Mutex<HashMap<(String, NudgeClass), NudgeState>> {
    NUDGED.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Take the guard, recovering from poisoning rather than propagating it.
///
/// `NudgeState` is a counter plus a timestamp with no cross-field invariant a
/// panic could leave half-applied, so the data behind a poisoned guard is
/// still sound. Propagating instead would disable nudging for the whole
/// process lifetime after a single unrelated panic — silently, since every
/// caller's poison arm looks exactly like "this session was never nudged".
fn nudged_guard() -> std::sync::MutexGuard<'static, HashMap<(String, NudgeClass), NudgeState>> {
    nudged().lock().unwrap_or_else(PoisonError::into_inner)
}

/// How many nudges of THIS CLASS this run has delivered to the session.
fn nudge_attempts_for(session_id: &str, class: NudgeClass) -> u32 {
    nudged_guard()
        .get(&(session_id.to_string(), class))
        .map_or(0, |s| s.attempts)
}

/// Is any class still under the cap? The pre-coord eligibility screen, which
/// runs before there is a verdict to derive a class from.
///
/// Advisory, like the single-class check it replaces: [`mark_nudged`] re-checks
/// the specific class under the lock at delivery.
fn any_class_under_cap(session_id: &str, max_attempts: u32) -> bool {
    NudgeClass::ALL
        .iter()
        .any(|c| nudge_attempts_for(session_id, *c) < max_attempts)
}

/// The same count, for the delivery site's operator-visible reason string.
pub fn nudge_attempts_reported(session_id: &str, class: NudgeClass) -> u32 {
    nudge_attempts_for(session_id, class)
}

/// The state to report to coord, or `None` when THIS RUN has no record of the
/// session.
///
/// `None` is not `(0, None)`, and conflating them corrupts coord's store. The
/// map is rebuilt empty on every restart, so after one, `finalize_on_close`
/// re-posts every stale session — and coord's compliance store is
/// last-write-wins. Reporting a fabricated `0` there would OVERWRITE a correct
/// non-zero count coord recorded mid-session, erasing exactly the evidence
/// this field exists to carry, in exactly the scenario it exists to explain.
///
/// So callers must OMIT the fields on `None` rather than send a zero. Omission
/// is right in both directions: coord keeps `0` if it never had anything, and
/// keeps `3` if it did.
///
/// Coord stores ONE count per session, so the classes are SUMMED here — the
/// stored number means "how many corrective nudges did this session receive",
/// which is a fact about the session, not about any one arm. The per-class
/// budgets stay runner-side, where the cap is actually enforced.
fn nudge_state_for(session_id: &str) -> Option<(u32, Option<String>)> {
    let g = nudged_guard();
    let mut attempts = 0u32;
    let mut last_at: Option<String> = None;
    let mut seen = false;
    for class in NudgeClass::ALL {
        let Some(state) = g.get(&(session_id.to_string(), class)) else {
            continue;
        };
        seen = true;
        attempts = attempts.saturating_add(state.attempts);
        // RFC3339 with a fixed-width millisecond field and a `Z` suffix sorts
        // lexicographically in timestamp order, which `mark_nudged` guarantees
        // — so `max` is the most recent delivery across the classes.
        if state.last_at > last_at {
            last_at = state.last_at.clone();
        }
    }
    seen.then_some((attempts, last_at))
}

/// Claim one nudge of `class` against `max_attempts`. Returns `true` when THIS
/// caller won the claim, `false` when the session is already at the cap FOR THAT
/// CLASS (or the guard is poisoned). Called by the continuation-verdict handler
/// at the moment it emits the block, not when the candidate is computed — a
/// candidate the cap swallowed must stay eligible.
///
/// Compare-and-set rather than a separate check-then-set: two Stops racing for
/// the same session must not both deliver. `max_attempts: 0` therefore means
/// never nudge, which the old set-based cap could not express.
///
/// The claim is per `(session, class)`: one class exhausting its budget must
/// never consume another's, which is the whole point of keying the marker on the
/// pair.
#[must_use]
pub fn mark_nudged(session_id: &str, class: NudgeClass, max_attempts: u32) -> bool {
    let mut g = nudged_guard();
    let entry = g.entry((session_id.to_string(), class)).or_default();
    if entry.attempts >= max_attempts {
        return false;
    }
    entry.attempts += 1;
    // Fixed-width milliseconds with a `Z` suffix, not bare `to_rfc3339()`.
    // That helper uses `AutoSi`, which emits 0/3/6/9 fractional digits
    // depending on what the clock happened to read — so a nanosecond reading
    // produces a value some RFC3339 parsers reject (Python `%f` caps at 6) and
    // that Postgres rounds on insert, intermittently and only sometimes, which
    // is the worst shape a wire-format bug can take.
    entry.last_at = Some(chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true));
    true
}

/// Refs coord could not reconcile, for the "contradicted" arm of the nudge.
///
/// Coord's key is `unreconciled_refs` (Phase 3 `reconciliation_payload`);
/// `unreconciled` and `items` are kept as lenient fallbacks. Probing only the
/// latter two left the primary lookup dead and made the result depend on
/// coord's `items[]` carrying a per-item marker — which, combined with
/// a marker-is-missing-means-failed reading, would name every
/// item as "failed to reconcile" the moment coord emitted markerless items.
/// That is a fabricated claim about the session's own work, so the primary key
/// is probed first and the markerless case falls back to coord's `reason`.
pub fn unreconciled_refs(reconciliation: &Value) -> Vec<String> {
    // `explicit` = the array is ALREADY the failure list, so every entry counts
    // without needing a per-item marker. `items` is the full set, where only an
    // entry that explicitly says it failed may be named — a markerless entry
    // there is UNKNOWN, and calling it failed invents a claim about the
    // session's work.
    let (arr, explicit) = match reconciliation {
        Value::Array(a) => (Some(a), true),
        Value::Object(_) => {
            match reconciliation
                .get("unreconciled_refs")
                .and_then(Value::as_array)
                .or_else(|| reconciliation.get("unreconciled").and_then(Value::as_array))
            {
                Some(a) => (Some(a), true),
                None => (reconciliation.get("items").and_then(Value::as_array), false),
            }
        }
        _ => (None, true),
    };
    let Some(arr) = arr else {
        return Vec::new();
    };
    arr.iter()
        .filter(|e| if explicit { true } else { explicitly_failed(e) })
        .filter_map(|e| match e {
            Value::String(s) => Some(s.trim().to_string()),
            Value::Object(_) => ["ref", "item", "id", "pr"]
                .iter()
                .find_map(|k| e.get(*k).and_then(Value::as_str))
                .map(|s| s.trim().to_string()),
            _ => None,
        })
        .filter(|s| !s.is_empty())
        .collect()
}

/// Does this entry of a FULL `items[]` list explicitly say it failed to
/// reconcile? Used only for the non-explicit arm of [`unreconciled_refs`].
///
/// A missing marker returns `false` — deliberately. In a full item list,
/// absence of a marker means UNKNOWN, and naming an unknown item as failed
/// would put a fabricated claim about the session's own work into the nudge.
/// Coord's `reason` string carries the honest fallback in that case.
fn explicitly_failed(e: &Value) -> bool {
    if let Some(b) = e
        .get("reconciled")
        .or_else(|| e.get("ok"))
        .and_then(Value::as_bool)
    {
        return !b;
    }
    if let Some(s) = e
        .get("status")
        .or_else(|| e.get("state"))
        .or_else(|| e.get("verdict"))
        .and_then(Value::as_str)
    {
        return matches!(
            s.trim().to_ascii_lowercase().as_str(),
            "unreconciled" | "unverified" | "failed" | "contradicted" | "mismatch"
        );
    }
    false
}

/// Coord's session-level policy-pull signal, read out of the reconciliation
/// payload (`session_signals[]`, `Signal::json` shape).
///
/// Returns the signal's `detail` ONLY when the result is exactly `absent` — the
/// one arm that means "coord looked and this session had not pulled policy".
///
/// `unavailable` and `error` must NEVER reach here as a nudge. They mean coord
/// could not check — an unattributable transport, or the read table not
/// provisioned yet — and nudging a session over coord's own blind spot would
/// assert something coord does not know. The strict `== "absent"` match is what
/// enforces that: a future result word fails closed to "do not nudge".
pub fn policy_pull_absent(reconciliation: &Value) -> Option<&Value> {
    reconciliation
        .get("session_signals")
        .and_then(Value::as_array)?
        .iter()
        .find(|s| {
            s.get("signal").and_then(Value::as_str) == Some("policy_pull")
                && s.get("result").and_then(Value::as_str) == Some("absent")
        })
        .map(|s| s.get("detail").unwrap_or(&Value::Null))
}

/// Did the REPORT arm itself fail — as distinct from the session merely never
/// having pulled policy?
///
/// The two are independent failures with independent corrections, and coord's
/// single `unverified` verdict covers both. Without this split, a session whose
/// block reconciled perfectly but which never read the policy would be told to
/// "re-emit the block" — a false statement about its own work, and the
/// `ux-priorities#honesty` failure this feature was commissioned to catch.
fn report_arm_failed(v: &ComplianceVerdict) -> bool {
    let reason = v.reason.trim();
    // `absent` (or an empty reason) is the missing-block case.
    if reason == "absent" || reason.is_empty() {
        return true;
    }
    // A block that coord could not reconcile names refs, or says so in `reason`.
    if !unreconciled_refs(&v.reconciliation).is_empty() {
        return true;
    }
    if v.reconciliation
        .get("unreconciled_refs")
        .and_then(Value::as_array)
        .is_some_and(|a| !a.is_empty())
    {
        return true;
    }
    if v.reconciliation.get("schema_ok").and_then(Value::as_bool) == Some(false) {
        return true;
    }
    // Coord appends the policy-pull clause to `reason` with a `; ` separator
    // when BOTH failed, and emits it alone when only the pull did. A reason that
    // is ONLY the policy-pull clause is not a report failure.
    !reason.starts_with("policy pull not observed")
}

/// Which class of nudge this verdict warrants, in priority order.
///
/// Both can be due at once; the caller takes the first whose per-class budget is
/// not exhausted, so the other stays eligible for a later turn instead of being
/// swallowed.
fn due_nudge_classes(v: &ComplianceVerdict) -> Vec<NudgeClass> {
    let mut due = Vec::with_capacity(2);
    if policy_pull_absent(&v.reconciliation).is_some() {
        due.push(NudgeClass::PolicyPull);
    }
    if report_arm_failed(v) {
        due.push(NudgeClass::Report);
    }
    due
}

/// The corrective prompt for a session coord observed no policy pull from.
///
/// Deliberately NOT phrased as "you did not emit the block": the session may
/// have emitted a perfect one. The failure is upstream of the report — it is
/// being held to documents it never read — so the correction is Step 0, named
/// with the doors that actually work.
fn policy_pull_nudge_text(detail: &Value) -> String {
    let mut out = String::from(
        "Coord has no record of this session reading `policy/session-protocol` at its \
         current version.\n",
    );
    if let Some(why) = detail.get("why").and_then(Value::as_str) {
        out.push_str(why);
        out.push_str(".\n");
    }
    // The stale case is materially different from "never read", and saying so
    // is what makes the correction land: a session that read v5 believes it is
    // current and will not re-read unless told the number moved.
    let stale = detail
        .get("stale_version_reads")
        .and_then(Value::as_array)
        .filter(|a| !a.is_empty());
    if let (Some(stale), Some(current)) = (stale, detail.get("current_version")) {
        let versions: Vec<String> = stale
            .iter()
            .filter_map(|r| r.get("version").and_then(Value::as_i64))
            .map(|v| format!("v{v}"))
            .collect();
        if !versions.is_empty() {
            out.push_str(&format!(
                "You read {} while v{} is current.\n",
                versions.join(", "),
                current
            ));
        }
    }
    out.push_str(
        "`policy/session-protocol` Step 0: never work from memory of these documents; they \
         version frequently. Pull them now — `coord_list_prompt_documents` + \
         `coord_get_prompt_document`, or the equal-authority device door \
         `GET /coord/agent-prompt-documents{,/{kind}/{name}}` if the MCP tools are masked — \
         and apply them to the work you have already done this session before closing.",
    );
    out
}

/// Build the corrective prompt for the REPORT arm, carrying coord's
/// observations so the correction is productive rather than a second guess.
pub fn nudge_text(v: &ComplianceVerdict) -> String {
    fn render(list: &[String]) -> String {
        if list.is_empty() {
            "none".to_string()
        } else {
            list.join(", ")
        }
    }
    // The opening sentence MUST branch on why the verdict is unverified. A
    // block that was emitted but failed reconciliation is not a missing block,
    // and opening with "You did not emit..." there is a false statement about
    // the session's own work — two lines above this function then says "...in
    // the block you did emit", contradicting it. Asserting something untrue to
    // provoke a correction is exactly the `ux-priorities#honesty` failure this
    // feature was commissioned to catch.
    let block_absent = v.reason.trim() == "absent" || v.reason.trim().is_empty();
    let mut out = String::from(if block_absent {
        "You did not emit the POLICY_COMPLIANCE block required by \
         `policy/session-protocol` v4 Step 3.\n"
    } else {
        "Your POLICY_COMPLIANCE block (required by `policy/session-protocol` \
         v4 Step 3) did not reconcile against coord's own record.\n"
    });
    if !block_absent {
        let refs = unreconciled_refs(&v.reconciliation);
        if refs.is_empty() {
            out.push_str(&format!(
                "Coord could not reconcile every claim in the block you did emit ({}).\n",
                v.reason.trim()
            ));
        } else {
            out.push_str(&format!(
                "These claims failed to reconcile against coord's own record: {}.\n",
                refs.join(", ")
            ));
        }
    }
    out.push_str(&format!(
        "Coord observed this session: PRs `{}`, commits `{}`, claims `{}`.\n",
        render(&v.footprint_prs),
        render(&v.footprint_commits),
        render(&v.footprint_claims),
    ));
    out.push_str(if block_absent {
        "Produce the block now, reconciled against those observations."
    } else {
        "Re-emit the block, reconciled against those observations."
    });
    out
}

// ===========================================================================
// Turn-end entry point
// ===========================================================================

/// Hard budget for everything the turn-end hook WAITS on.
///
/// The bundled Stop hook allows the whole endpoint `--connect-timeout 2
/// --max-time 10`, and the continuation arm can already spend up to two 4 s
/// coord reads inside that. Compliance therefore caps its own awaited work so
/// it can never be the reason the hook aborts — and an aborted hook loses the
/// CONTINUATION verdict too, so this is not merely a latency nicety. Anything
/// slower than the budget fails open (allow), exactly like an unreachable
/// coord.
const TURN_END_BUDGET: Duration = Duration::from_millis(2500);

/// Observe one turn end: applicability gate → parse → emit → decide whether a
/// nudge is warranted.
///
/// `hook_input` is the raw Claude `Stop` payload, which carries both the
/// `session_id` (the Claude UUID coord keys compliance on — *not* the runner
/// terminal id the URL path may use) and the `transcript_path`.
///
/// Returns the nudge CANDIDATE. The caller
/// ([`crate::mcp::continuation_verdict`]) owns the inherited loop guards
/// (`stop_hook_active`, the rolling hourly cap) and calls [`mark_nudged`] only
/// if it actually delivers. Every failure path — including exceeding
/// [`TURN_END_BUDGET`] — returns `None`.
pub async fn observe_turn_end(hook_input: &Value) -> Option<ComplianceNudge> {
    match tokio::time::timeout(TURN_END_BUDGET, observe_turn_end_inner(hook_input)).await {
        Ok(nudge) => nudge,
        Err(_) => {
            debug!("session-compliance: turn-end budget exceeded — allowing the stop");
            None
        }
    }
}

async fn observe_turn_end_inner(hook_input: &Value) -> Option<ComplianceNudge> {
    // 1. Applicability FIRST — before touching the transcript.
    let config = fetch_config().await?;
    let applicability = config.applicability();
    if !applicability.emits() {
        debug!(
            reason = %config.applicability_reason,
            "session-compliance: inert for this tenant"
        );
        return None;
    }

    // 2. Identity + transcript, both straight off the hook payload.
    let session_id = hook_input
        .get("session_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())?;
    let transcript = hook_transcript_path(hook_input, session_id)?;

    // Reading and JSON-parsing a multi-MB transcript window is synchronous
    // work; running it on the async worker would stall every other request the
    // runner's API is serving.
    let report = spawn_blocking_tracked(move || detect(&transcript))
        .await
        .ok()??;

    // 3. Nudge conditions that are knowable BEFORE coord answers (§A4). When
    //    none of them can hold, nothing in this turn's response depends on the
    //    verdict — so emit it DETACHED and return immediately, and the hook
    //    never waits on coord. That is the whole fleet under the report-only
    //    default, and every repeat turn of an already-nudged session.
    // The cap is the operator's configured number, not a hard-coded 1, and it
    // applies PER CLASS — a session that has exhausted its report nudges may
    // still be owed a policy-pull one, which is exactly the suppression the
    // class key exists to prevent. This read is advisory; `mark_nudged`
    // re-checks the chosen class under the lock at delivery, which is the point
    // two racing Stops are actually serialised.
    let nudge_possible = applicability.nudge_allowed(&config.mode)
        && !crate::mcp::continuation_verdict::stop_hook_active_from(hook_input)
        && any_class_under_cap(session_id, config.max_attempts);
    if !nudge_possible {
        let sid = session_id.to_string();
        tauri::async_runtime::spawn(async move {
            let _ = emit(&sid, report.as_ref()).await;
        });
        return None;
    }

    // 4. Emit and wait — a nudge is possible, so the verdict is load-bearing.
    let verdict = emit(session_id, report.as_ref()).await?;

    if verdict.verdict != "unverified" {
        return None;
    }
    if verdict.footprint_is_empty() {
        debug!(
            session = %session_id,
            "session-compliance: unverified but coord observed no footprint — no nudge"
        );
        return None;
    }

    // Pick the first DUE class whose per-class budget is not exhausted. Taking
    // the first *due* class unconditionally would let an exhausted policy-pull
    // budget mask a fresh report failure, which is the same suppression in a
    // different costume.
    let class = due_nudge_classes(&verdict)
        .into_iter()
        .find(|c| nudge_attempts_for(session_id, *c) < config.max_attempts)?;
    let prompt = match class {
        // `policy_pull_absent` re-reads the payload rather than being threaded
        // down from `due_nudge_classes`, so the text is built from the same
        // detail the classification was made on and cannot describe a different
        // signal than the one that fired.
        NudgeClass::PolicyPull => {
            policy_pull_nudge_text(policy_pull_absent(&verdict.reconciliation)?)
        }
        NudgeClass::Report => nudge_text(&verdict),
    };
    Some(ComplianceNudge {
        session_id: session_id.to_string(),
        class,
        prompt,
        max_attempts: config.max_attempts,
    })
}

/// Locate the transcript for a turn-end hook payload.
///
/// Claude Code's Stop payload carries `transcript_path` directly, which is the
/// authoritative answer and the fast path. The fallback derives the path from
/// the payload's `cwd` the same way the restore listing does, so a payload
/// missing the field (or naming a file that has since moved between accounts)
/// still resolves instead of silently disabling detection.
fn hook_transcript_path(hook_input: &Value, session_id: &str) -> Option<PathBuf> {
    if let Some(p) = hook_input
        .get("transcript_path")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let path = PathBuf::from(p);
        if path.is_file() {
            return Some(path);
        }
    }
    let cwd = hook_input
        .get("cwd")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())?;
    resolve_transcript(None, Some(cwd), session_id)
}

/// Read + scan a transcript. `None` = the transcript could not be read at all
/// (fail open: do nothing). `Some(None)` = read fine, no valid block.
fn detect(transcript: &Path) -> Option<Option<Value>> {
    let tail = read_transcript_tail(transcript)?;
    Some(scan_transcript(&tail))
}

/// POST the detection result and log the outcome.
async fn emit(session_id: &str, report: Option<&Value>) -> Option<ComplianceVerdict> {
    let absent_reason = if report.is_none() {
        Some(ABSENT_NO_BLOCK)
    } else {
        None
    };
    let verdict = post_compliance(session_id, report, absent_reason).await?;
    info!(
        session = %session_id,
        block_present = report.is_some(),
        verdict = %verdict.verdict,
        reason = %verdict.reason,
        "session-compliance"
    );
    Some(verdict)
}

// ===========================================================================
// Session-close finalize
// ===========================================================================

/// Re-run detection once at session close so the last stored verdict reflects
/// the whole session, letting the UI tell a settled verdict from a mid-session
/// snapshot (coord's store is last-write-wins, and the session is closed by
/// the time this write lands).
///
/// Called from the single-fire close observer on
/// [`SessionLifecycleStore::record_close`], which only fires on a real
/// `open`→`closed` transition. Best-effort and off the caller's thread: the
/// observer runs inside the registry write path and must never block on I/O.
///
/// No nudge is ever computed here — the session is gone, and `reopen` mode is
/// deliberately unimplemented (see the module docs).
pub fn finalize_on_close(claude_session_id: &str, store: &Weak<SessionLifecycleStore>) {
    let Some(store) = store.upgrade() else {
        return;
    };
    let Some(record) = store.get(claude_session_id) else {
        return;
    };
    // Only Claude sessions have a Claude transcript to scan.
    if !record.provider.eq_ignore_ascii_case("claude") {
        return;
    }
    // A terminal that never ran a provider has no transcript to finalize, and
    // the boot liveness sweep closes these in bulk.
    if record
        .close_reason
        .as_deref()
        .is_some_and(|r| r.eq_ignore_ascii_case("never-started"))
    {
        return;
    }
    let session_id = claude_session_id.to_string();
    let config_dir = record.config_dir;
    let working_dir = record.working_dir;
    tauri::async_runtime::spawn(async move {
        // The liveness poll closes many stale records at once after a crash
        // restart, and every one of these does a config-dir walk, a multi-MB
        // read and a coord POST. Bound the burst rather than letting a boot
        // sweep fan out unthrottled.
        let _permit = match finalize_permits().acquire().await {
            Ok(p) => p,
            Err(_) => return,
        };
        finalize_impl(&session_id, config_dir.as_deref(), working_dir.as_deref()).await;
    });
}

/// Concurrency bound for close-time finalizes (see [`finalize_on_close`]).
const FINALIZE_CONCURRENCY: usize = 2;

static FINALIZE_PERMITS: OnceLock<tokio::sync::Semaphore> = OnceLock::new();

fn finalize_permits() -> &'static tokio::sync::Semaphore {
    FINALIZE_PERMITS.get_or_init(|| tokio::sync::Semaphore::new(FINALIZE_CONCURRENCY))
}

async fn finalize_impl(session_id: &str, config_dir: Option<&str>, working_dir: Option<&str>) {
    let Some(config) = fetch_config().await else {
        return;
    };
    if !config.applicability().emits() {
        return;
    }
    let Some(path) = resolve_transcript(config_dir, working_dir, session_id) else {
        debug!(session = %session_id, "session-compliance: no transcript at close — no-op");
        return;
    };
    // Same reason as the turn-end path: the read + per-line parse is
    // synchronous and must not run on the async worker.
    let Ok(Some(report)) = spawn_blocking_tracked(move || detect(&path)).await else {
        return;
    };
    let _ = emit(session_id, report.as_ref()).await;
}

/// Locate the on-disk transcript for a closed session. Prefers the recorded
/// config dir, then scans every known Claude config dir — the same resolution
/// `session::past_sessions` performs for the restore listing.
fn resolve_transcript(
    config_dir: Option<&str>,
    working_dir: Option<&str>,
    session_id: &str,
) -> Option<PathBuf> {
    crate::session::past_sessions::resolve_transcript_path(config_dir, working_dir, session_id)
}

// ===========================================================================
// §A1a — the coverage bound
// ===========================================================================

/// One scope this check does NOT cover, and why.
#[derive(Debug, Clone, Serialize)]
pub struct CoverageGap {
    pub scope: &'static str,
    pub why: &'static str,
}

/// An honest, STATIC statement of what session-compliance detection covers.
///
/// Deliberately not a computed number. The runner's own
/// `session::tracking_health` `liveUntracked` metric is computed over the
/// runner's own inclusive process subtree, so a hand-started session in a
/// foreign terminal is structurally invisible to it — presenting that as
/// coverage would be confidently wrong. This is a scope statement instead.
#[derive(Debug, Clone, Serialize)]
pub struct CoverageBound {
    pub schema: &'static str,
    /// Always `false`: nothing here is derived from a runner metric.
    pub derived: bool,
    pub covered: &'static str,
    pub not_covered: Vec<CoverageGap>,
    pub note: &'static str,
}

/// Build the coverage bound. Served by `GET /sessions/compliance-coverage` on
/// the runner API for the qontinui-web enforcement panel (§B3).
pub fn coverage_bound() -> CoverageBound {
    CoverageBound {
        schema: "session-compliance-coverage/1",
        derived: false,
        covered: "Claude Code sessions THIS runner instance spawned: PTY terminals that \
                  received the per-PTY identity shim, and therefore the runner-bundled Stop \
                  hook delivered via `claude --settings`.",
        not_covered: vec![
            CoverageGap {
                scope: "bare terminals the runner did not spawn",
                why: "the identity shim is materialized per-PTY into the child environment \
                      only, so a terminal started by hand never sees a shim — and without the \
                      shim there is no `--settings`, hence no Stop hook and no turn-end \
                      signal. The opt-in persistent Windows shim does not close this: without \
                      a pinned session id it delivers coord MCP identity only.",
            },
            CoverageGap {
                scope: "backend-initiated spawns with no capture hint",
                why: "sessions spawned with `capture_hint: None` are not registered in the \
                      runner's session lifecycle store, so neither the turn-end hook nor the \
                      close observer sees them.",
            },
            CoverageGap {
                scope: "other runner instances on this machine",
                why: "the session lifecycle store is instance-scoped; a secondary or temporary \
                      runner keeps its own store and reports only its own sessions.",
            },
            CoverageGap {
                scope: "sessions on other machines",
                why: "there is no cross-machine session federation — each machine's runner \
                      reports only what it spawned.",
            },
            CoverageGap {
                scope: "a block emitted far earlier in a very long session",
                why: "detection scans only the last 4 MiB of the transcript, so a report older \
                      than that window reads as absent. Reading tens of MB on every turn end \
                      would be a real cost for no gain.",
            },
            CoverageGap {
                scope: "a report that is not plain assistant text",
                why: "only `content[]` blocks of type `text` in `assistant` records are scanned. \
                      A report inside a tool call, a thinking block, or a user message does not \
                      count — the point is what the ASSISTANT attested.",
            },
            CoverageGap {
                scope: "the difference between emitting a block and quoting one",
                why: "an assistant that pastes a valid block as an EXAMPLE (explaining the \
                      schema, or reviewing code that contains one) is indistinguishable from one \
                      attesting its own work. Requiring the opener at the start of a line rules \
                      out inline prose mentions, not deliberate illustration. Reconciliation \
                      against coord's own observations, not this parse, is what makes a report \
                      evidence.",
            },
        ],
        note: "Static scope statement, NOT a computed coverage number. Do not derive this from \
               the runner's `liveUntracked` tracking-health metric: that counts only the \
               runner's own process subtree, so the largest blind spot here (a session started \
               in a foreign terminal) is invisible to it by construction.",
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const BLOCK: &str = r#"<!-- POLICY_COMPLIANCE v1 -->
```json
{ "schema": "policy-compliance/1",
  "items": [{"ref":"A1","state":"landed","evidence":{"pr":"qontinui/qontinui-runner#1"}}],
  "clauses_applied": [{"id":"finish-to-zero","for":"A1"}],
  "policy_gaps": [], "escalations": [], "declined": [] }
```
"#;

    fn assistant_line(texts: &[&str]) -> String {
        let content: Vec<Value> = texts
            .iter()
            .map(|t| json!({"type": "text", "text": t}))
            .collect();
        json!({
            "type": "assistant",
            "uuid": "u1",
            "timestamp": "2026-07-30T12:00:00Z",
            "message": {"model": "claude-opus-5", "content": content}
        })
        .to_string()
    }

    // ── extract_compliance_block ─────────────────────────────────────────

    #[test]
    fn extracts_a_fenced_block() {
        let v = extract_compliance_block(BLOCK).expect("block found");
        assert_eq!(v["schema"], SCHEMA);
        assert_eq!(v["items"][0]["ref"], "A1");
    }

    #[test]
    fn extracts_an_unfenced_block() {
        let text =
            "<!-- POLICY_COMPLIANCE v1 -->\n{\"schema\":\"policy-compliance/1\",\"items\":[]}";
        assert!(extract_compliance_block(text).is_some());
    }

    #[test]
    fn prose_alone_is_not_a_pass() {
        // The single most important negative: a session can print the words
        // having read nothing. That must not satisfy the check.
        let text = "I emitted the POLICY_COMPLIANCE footer and everything reconciled. \
                    All items landed. schema policy-compliance/1.";
        assert!(extract_compliance_block(text).is_none());
    }

    #[test]
    fn an_inline_mention_cannot_locate_a_later_block() {
        // The opener must START a line. Prose naming the marker mid-sentence
        // must not reach forward and adopt an unrelated JSON object.
        let text = "As promised I wrote the <!-- POLICY_COMPLIANCE v1 --> footer.\n\
                    Here is some config: {\"schema\":\"policy-compliance/1\",\"items\":[]}";
        assert!(extract_compliance_block(text).is_none());
    }

    #[test]
    fn an_indented_opener_still_matches() {
        let text = "   <!-- POLICY_COMPLIANCE v1 -->\n\
                    {\"schema\":\"policy-compliance/1\",\"items\":[{\"ref\":\"A\"}]}";
        assert!(extract_compliance_block(text).is_some());
    }

    #[test]
    fn a_stray_brace_in_the_opener_does_not_lose_the_block() {
        let text = "<!-- POLICY_COMPLIANCE v1 {see plan} -->\n```json\n\
                    {\"schema\":\"policy-compliance/1\",\"items\":[{\"ref\":\"A\"}]}\n```";
        let v = extract_compliance_block(text).expect("block found past the stray brace");
        assert_eq!(v["items"][0]["ref"], "A");
    }

    #[test]
    fn malformed_json_is_not_a_pass() {
        let text =
            "<!-- POLICY_COMPLIANCE v1 -->\n```json\n{ \"schema\": \"policy-compliance/1\", \
                    \"items\": [ {\"ref\": }\n```";
        assert!(extract_compliance_block(text).is_none());
    }

    #[test]
    fn wrong_schema_string_is_not_a_pass() {
        let text = "<!-- POLICY_COMPLIANCE v1 -->\n```json\n\
                    {\"schema\":\"policy-compliance/2\",\"items\":[]}\n```";
        assert!(extract_compliance_block(text).is_none());
        let text = "<!-- POLICY_COMPLIANCE v1 -->\n```json\n{\"items\":[]}\n```";
        assert!(extract_compliance_block(text).is_none());
    }

    #[test]
    fn items_must_be_an_array() {
        let text = "<!-- POLICY_COMPLIANCE v1 -->\n```json\n\
                    {\"schema\":\"policy-compliance/1\",\"items\":{}}\n```";
        assert!(extract_compliance_block(text).is_none());
        let text = "<!-- POLICY_COMPLIANCE v1 -->\n```json\n\
                    {\"schema\":\"policy-compliance/1\"}\n```";
        assert!(extract_compliance_block(text).is_none());
    }

    #[test]
    fn multiple_blocks_last_wins() {
        let first = "<!-- POLICY_COMPLIANCE v1 -->\n```json\n\
                     {\"schema\":\"policy-compliance/1\",\"items\":[{\"ref\":\"first\"}]}\n```";
        let second = "<!-- POLICY_COMPLIANCE v1 -->\n```json\n\
                      {\"schema\":\"policy-compliance/1\",\"items\":[{\"ref\":\"second\"}]}\n```";
        let text = format!("{first}\n\nsome prose\n\n{second}");
        let v = extract_compliance_block(&text).expect("block found");
        assert_eq!(v["items"][0]["ref"], "second");
    }

    #[test]
    fn a_later_prose_mention_does_not_invalidate_an_earlier_real_block() {
        let text = format!("{BLOCK}\n\nAs shown above, the POLICY_COMPLIANCE report is done.");
        let v = extract_compliance_block(&text).expect("block found");
        assert_eq!(v["items"][0]["ref"], "A1");
    }

    // ── scan_transcript ──────────────────────────────────────────────────

    #[test]
    fn scans_a_block_in_a_single_content_block() {
        let jsonl = format!(
            "{}\n{}\n",
            assistant_line(&["Working on it."]),
            assistant_line(&[BLOCK])
        );
        assert!(scan_transcript(&jsonl).is_some());
    }

    #[test]
    fn scans_a_block_split_across_content_blocks() {
        // The recon hazard: Claude can emit the marker + fence in one text
        // block and the payload in the next. Joining is what makes it match.
        let head = "<!-- POLICY_COMPLIANCE v1 -->\n```json";
        let tail = "{\"schema\":\"policy-compliance/1\",\"items\":[{\"ref\":\"split\"}]}\n```";
        let jsonl = format!("{}\n", assistant_line(&[head, tail]));
        let v = scan_transcript(&jsonl).expect("block found across content blocks");
        assert_eq!(v["items"][0]["ref"], "split");
    }

    #[test]
    fn scans_a_block_split_across_messages() {
        let head = "<!-- POLICY_COMPLIANCE v1 -->\n```json";
        let tail = "{\"schema\":\"policy-compliance/1\",\"items\":[]}\n```";
        let jsonl = format!("{}\n{}\n", assistant_line(&[head]), assistant_line(&[tail]));
        assert!(scan_transcript(&jsonl).is_some());
    }

    #[test]
    fn user_records_and_tool_blocks_are_ignored() {
        // A user PASTING the block must not count as the assistant emitting it.
        let user = json!({
            "type": "user",
            "message": {"content": [{"type": "text", "text": BLOCK}]}
        })
        .to_string();
        assert!(scan_transcript(&user).is_none());

        let tool_only = json!({
            "type": "assistant",
            "message": {"content": [{"type": "tool_use", "name": "Bash", "input": {"cmd": BLOCK}}]}
        })
        .to_string();
        assert!(scan_transcript(&tool_only).is_none());
    }

    #[test]
    fn malformed_and_partial_lines_are_skipped_not_fatal() {
        let jsonl = format!(
            "{{ this is not json\n\n{}\n{{\"type\":\"assistant\"}}\n",
            assistant_line(&[BLOCK])
        );
        assert!(scan_transcript(&jsonl).is_some());
    }

    #[test]
    fn an_empty_transcript_yields_no_block() {
        assert!(scan_transcript("").is_none());
        assert!(scan_transcript("\n\n  \n").is_none());
    }

    // ── ComplianceConfig / Applicability ─────────────────────────────────

    #[test]
    fn applicable_config_enforces() {
        let c = ComplianceConfig::from_body(&json!({
            "enabled": true, "mode": "nudge", "max_attempts": 1,
            "enforced_clause_ref": "policy/session-protocol#finish-to-zero",
            "applicable": true, "applicability_reason": "applicable",
            "clause_resolved_via": "clause_id", "prompt_document_version": 4
        }))
        .expect("parsed");
        assert_eq!(c.applicability(), Applicability::Enforce);
        assert!(c.applicability().emits());
        assert!(c.applicability().nudge_allowed(&c.mode));
    }

    #[test]
    fn enforcement_disabled_still_emits_but_never_nudges() {
        // Report-only by default is the whole point of Phase 1: the verdict
        // emit is how the false-positive rate gets measured.
        let c = ComplianceConfig::from_body(&json!({
            "enabled": false, "mode": "nudge", "applicable": false,
            "applicability_reason": "enforcement_disabled"
        }))
        .expect("parsed");
        assert_eq!(c.applicability(), Applicability::ReportOnly);
        assert!(c.applicability().emits());
        assert!(!c.applicability().nudge_allowed(&c.mode));
    }

    #[test]
    fn clause_absent_or_document_missing_is_fully_inert() {
        for reason in ["clause_absent", "document_missing"] {
            let c = ComplianceConfig::from_body(&json!({
                "enabled": true, "mode": "nudge", "applicable": false,
                "applicability_reason": reason
            }))
            .expect("parsed");
            assert_eq!(c.applicability(), Applicability::Inert, "reason {reason}");
            assert!(!c.applicability().emits(), "reason {reason}");
        }
    }

    #[test]
    fn reopen_mode_never_nudges() {
        // A spawn that outlives the request needs a standing per-path opt-in
        // that does not exist — so `reopen` is inert on the nudge arm.
        let c = ComplianceConfig::from_body(&json!({
            "enabled": true, "mode": "reopen", "applicable": true,
            "applicability_reason": "applicable"
        }))
        .expect("parsed");
        assert_eq!(c.applicability(), Applicability::Enforce);
        assert!(c.applicability().emits());
        assert!(!c.applicability().nudge_allowed(&c.mode));
    }

    #[test]
    fn an_unknown_reason_word_never_escalates_to_enforce() {
        let c = ComplianceConfig::from_body(&json!({
            "enabled": true, "mode": "nudge", "applicable": true,
            "applicability_reason": "some_future_reason"
        }))
        .expect("parsed");
        assert_eq!(c.applicability(), Applicability::ReportOnly);
        let c = ComplianceConfig::from_body(&json!({"applicability_reason": "some_future_reason"}))
            .expect("parsed");
        assert_eq!(c.applicability(), Applicability::Inert);
    }

    #[test]
    fn an_empty_config_body_is_inert() {
        let c = ComplianceConfig::from_body(&json!({})).expect("parsed");
        assert!(!c.enabled);
        assert_eq!(c.applicability(), Applicability::Inert);
        assert!(ComplianceConfig::from_body(&json!("nope")).is_none());
        assert!(ComplianceConfig::from_body(&Value::Null).is_none());
    }

    // ── ComplianceVerdict / footprint ────────────────────────────────────

    #[test]
    fn verdict_parses_footprint_strings_and_objects() {
        let v = ComplianceVerdict::from_body(&json!({
            "verdict": "unverified", "reason": "absent",
            "reconciliation": {},
            "footprint": {
                "prs": ["qontinui/qontinui-runner#42", {"ref": "qontinui/qontinui-web#7"}],
                "commits": [{"sha": "abc1234"}],
                "claims": []
            }
        }));
        assert_eq!(v.verdict, "unverified");
        assert_eq!(
            v.footprint_prs,
            vec!["qontinui/qontinui-runner#42", "qontinui/qontinui-web#7"]
        );
        assert_eq!(v.footprint_commits, vec!["abc1234"]);
        assert!(!v.footprint_is_empty());
    }

    #[test]
    fn an_absent_footprint_reads_empty() {
        let v = ComplianceVerdict::from_body(&json!({"verdict": "unverified"}));
        assert!(v.footprint_is_empty());
        let v = ComplianceVerdict::from_body(&json!({
            "verdict": "unverified",
            "footprint": {"prs": [], "commits": [], "claims": []}
        }));
        assert!(v.footprint_is_empty());
    }

    /// §A4 condition 3: CLAIMS ALONE MUST NOT ARM THE NUDGE.
    ///
    /// coord builds `footprint.claims` from `claims_audit` rows of kind
    /// `symbol`/`file_glob`, and the fleet's `/preflight` protocol acquires
    /// file-glob claims before the first line of code is written. If claims
    /// counted, this footprint would be non-empty at turn 1 of nearly every
    /// session — and since an unfinished session's verdict is `unverified`
    /// with `reason: "absent"` by construction, the nudge would fire at its
    /// FIRST pause. That is the per-turn nag condition 3 exists to prevent.
    #[test]
    fn claims_alone_do_not_arm_the_nudge() {
        let v = ComplianceVerdict::from_body(&json!({
            "verdict": "unverified", "reason": "absent",
            "footprint": {
                "prs": [], "commits": [],
                "claims": ["src/**/*.rs", "qontinui-runner:src/mcp/*"]
            }
        }));
        assert!(!v.footprint_claims.is_empty(), "claims still parsed");
        assert!(
            v.footprint_is_empty(),
            "claims measure work STARTED, not work DONE — they must not arm the nudge"
        );

        // A single PR or commit DOES arm it: a report is genuinely owed.
        let v = ComplianceVerdict::from_body(&json!({
            "verdict": "unverified", "reason": "absent",
            "footprint": {"prs": ["o/r#1"], "commits": [], "claims": []}
        }));
        assert!(!v.footprint_is_empty());
    }

    /// A commit object carrying a non-null `pr_number` — the normal case for a
    /// commit pushed on a PR branch — must render as its SHA, not as a PR ref.
    /// The session being nudged needs the sha to fill `evidence.sha`.
    #[test]
    fn a_commit_with_a_pr_number_renders_its_sha() {
        let v = ComplianceVerdict::from_body(&json!({
            "verdict": "unverified", "reason": "absent",
            "footprint": {
                "prs": [],
                "commits": [{
                    "sha": "abc1234", "repo": "qontinui/qontinui-runner",
                    "branch": "feat/x", "pr_number": 923,
                    "recorded_at": "2026-07-30T00:00:00Z"
                }],
                "claims": []
            }
        }));
        assert_eq!(
            v.footprint_commits,
            vec!["abc1234"],
            "a commit must not render as owner/repo#N"
        );
    }

    /// A block that WAS emitted but failed reconciliation must not be told it
    /// emitted nothing — the nudge would contradict itself two lines later.
    #[test]
    fn the_nudge_does_not_claim_an_emitted_block_is_missing() {
        let emitted = ComplianceVerdict::from_body(&json!({
            "verdict": "unverified",
            "reason": "2 of 5 item(s) could not be reconciled: A1, B2",
            "reconciliation": {},
            "footprint": {"prs": ["o/r#1"], "commits": [], "claims": []}
        }));
        let text = nudge_text(&emitted);
        assert!(
            !text.contains("You did not emit"),
            "must not assert absence about a block that was emitted: {text}"
        );
        assert!(text.contains("did not reconcile"));

        let absent = ComplianceVerdict::from_body(&json!({
            "verdict": "unverified", "reason": "absent",
            "footprint": {"prs": ["o/r#1"], "commits": [], "claims": []}
        }));
        assert!(nudge_text(&absent).contains("You did not emit"));
    }

    // ── nudge text ───────────────────────────────────────────────────────

    #[test]
    fn nudge_carries_the_observed_footprint() {
        let v = ComplianceVerdict::from_body(&json!({
            "verdict": "unverified", "reason": "absent",
            "footprint": {"prs": ["o/r#1"], "commits": ["deadbee"], "claims": []}
        }));
        let text = nudge_text(&v);
        assert!(text.contains("policy/session-protocol` v4 Step 3"));
        assert!(text.contains("PRs `o/r#1`"));
        assert!(text.contains("commits `deadbee`"));
        assert!(text.contains("claims `none`"));
        assert!(text.contains("reconciled against those observations"));
        // `absent` is the plain missing-block case — no contradiction sentence.
        assert!(!text.contains("failed to reconcile"));
    }

    #[test]
    fn nudge_names_contradicted_claims_when_coord_supplies_them() {
        let v = ComplianceVerdict::from_body(&json!({
            "verdict": "unverified", "reason": "contradicted",
            "reconciliation": {"unreconciled": [{"ref": "A1"}, "B2"]},
            "footprint": {"prs": ["o/r#1"], "commits": [], "claims": []}
        }));
        let text = nudge_text(&v);
        assert!(text.contains("failed to reconcile"));
        assert!(text.contains("A1"));
        assert!(text.contains("B2"));
    }

    #[test]
    fn nudge_falls_back_to_coords_reason_when_the_payload_is_unfamiliar() {
        let v = ComplianceVerdict::from_body(&json!({
            "verdict": "unverified", "reason": "contradicted",
            "reconciliation": {"some": "future shape"},
            "footprint": {"prs": ["o/r#1"], "commits": [], "claims": []}
        }));
        let text = nudge_text(&v);
        assert!(text.contains("could not reconcile every claim"));
        assert!(text.contains("contradicted"));
    }

    #[test]
    fn unreconciled_refs_reads_the_shapes_it_knows_and_no_others() {
        assert_eq!(
            unreconciled_refs(&json!({"unreconciled": ["A", {"ref": "B"}]})),
            vec!["A", "B"]
        );
        assert_eq!(
            unreconciled_refs(&json!({"items": [
                {"ref": "A", "reconciled": true},
                {"ref": "B", "reconciled": false},
                {"ref": "C", "status": "reconciled"},
                {"ref": "D", "status": "contradicted"}
            ]})),
            vec!["B", "D"]
        );
        assert!(unreconciled_refs(&json!({"unknown": 1})).is_empty());
        assert!(unreconciled_refs(&Value::Null).is_empty());
    }

    // ── counted per-session cap ──────────────────────────────────────────
    //
    // `NUDGED` is process-global across the whole test binary, so every test
    // below uses a session key unique to itself.

    #[test]
    fn mark_nudged_is_a_counted_compare_and_set() {
        let a = "session-compliance-test-marker-a";
        let b = "session-compliance-test-marker-b";
        let c = NudgeClass::Report;
        assert_eq!(nudge_attempts_for(a, c), 0);
        assert!(mark_nudged(a, c, 1), "first claim wins");
        assert!(
            !mark_nudged(a, c, 1),
            "second claim must lose at max_attempts=1"
        );
        assert_eq!(nudge_attempts_for(a, c), 1);
        assert_eq!(nudge_attempts_for(b, c), 0, "the cap is per session");
    }

    /// The headline behaviour of the change: the cap is the OPERATOR's number.
    /// Under the old set-based guard this test was unwritable — delivery
    /// stopped at one no matter what `max_attempts` said.
    #[test]
    fn a_higher_cap_allows_exactly_that_many() {
        let s = "session-compliance-test-cap-three";
        let c = NudgeClass::Report;
        for i in 1..=3 {
            assert!(mark_nudged(s, c, 3), "attempt {i} of 3 must be granted");
            assert_eq!(nudge_attempts_for(s, c), i);
        }
        assert!(!mark_nudged(s, c, 3), "the fourth is refused");
        assert_eq!(nudge_attempts_for(s, c), 3, "and does not increment");
    }

    /// `max_attempts: 0` means never nudge — a setting the old cap could not
    /// express at all.
    #[test]
    fn a_zero_cap_never_nudges() {
        let s = "session-compliance-test-cap-zero";
        for c in NudgeClass::ALL {
            assert!(!mark_nudged(s, c, 0));
            assert_eq!(nudge_attempts_for(s, c), 0);
        }
        assert!(
            !any_class_under_cap(s, 0),
            "a zero cap leaves no class eligible, so the pre-coord screen              short-circuits before the verdict is even fetched"
        );
    }

    /// The entire backward-compatibility argument for this change: an
    /// untouched config still nudges exactly once. Without this, flipping the
    /// `unwrap_or` would triple the fleet's nudge rate with a green suite.
    #[test]
    fn an_absent_max_attempts_defaults_to_one() {
        assert_eq!(
            ComplianceConfig::from_body(&json!({}))
                .expect("parsed")
                .max_attempts,
            1
        );
        assert_eq!(
            ComplianceConfig::from_body(&json!({"max_attempts": null}))
                .expect("parsed")
                .max_attempts,
            1
        );
        assert_eq!(
            ComplianceConfig::from_body(&json!({"max_attempts": "3"}))
                .expect("parsed")
                .max_attempts,
            1,
            "a non-numeric value is not a cap"
        );
    }

    #[test]
    fn an_absurd_max_attempts_is_clamped() {
        assert_eq!(
            ComplianceConfig::from_body(&json!({"max_attempts": 999_999}))
                .expect("parsed")
                .max_attempts,
            MAX_NUDGE_ATTEMPTS_CEILING as u32
        );
    }

    /// The timestamp goes on the wire, so its SHAPE is part of the contract.
    /// `to_rfc3339()` would emit 0/3/6/9 fractional digits depending on the
    /// clock reading; the 9-digit form is rejected by some RFC3339 parsers and
    /// rounded by Postgres.
    #[test]
    fn the_nudge_timestamp_is_fixed_width_utc() {
        let s = "session-compliance-test-stamp";
        assert!(mark_nudged(s, NudgeClass::Report, 1));
        let (attempts, last_at) = nudge_state_for(s).expect("a nudged session has state");
        assert_eq!(attempts, 1);
        let at = last_at.expect("a delivered nudge stamps its time");
        assert!(at.ends_with('Z'), "UTC as Z, not +00:00: {at}");
        assert_eq!(at.len(), 24, "fixed-width milliseconds: {at}");
        assert!(chrono::DateTime::parse_from_rfc3339(&at).is_ok());
    }

    /// The restart bug, pinned at the seam it actually broke.
    ///
    /// A session this run never saw reads `None`, and `None` must OMIT the
    /// fields — not send `0`. Coord's store is last-write-wins, so a
    /// fabricated zero at session close would erase a real count recorded
    /// before the restart.
    #[test]
    fn an_unknown_session_omits_the_nudge_fields_rather_than_sending_zero() {
        assert!(nudge_state_for("session-compliance-test-never-seen").is_none());

        let body = compliance_body(None, Some(ABSENT_NO_BLOCK), None);
        assert!(
            body.get("nudge_attempts").is_none(),
            "a fabricated 0 would overwrite coord's real count: {body}"
        );
        assert!(body.get("last_nudged_at").is_none());
        // The report half of the post is unaffected.
        assert_eq!(body["absent_reason"], json!(ABSENT_NO_BLOCK));

        let carried = compliance_body(
            None,
            None,
            Some((2, Some("2026-08-08T10:00:00.000Z".into()))),
        );
        assert_eq!(carried["nudge_attempts"], json!(2));
        assert_eq!(carried["last_nudged_at"], json!("2026-08-08T10:00:00.000Z"));

        // A recorded count with no stamp still carries the count.
        let stampless = compliance_body(None, None, Some((1, None)));
        assert_eq!(stampless["nudge_attempts"], json!(1));
        assert!(stampless.get("last_nudged_at").is_none());
    }

    // ── per-CLASS keying (plan `2026-08-08-runner-enforced-policy-pull`
    //    Phase 3) ─────────────────────────────────────────────────────────

    /// THE regression this whole sub-change exists to prevent.
    ///
    /// `NUDGED` used to be keyed on `session_id` ALONE. With a second nudge
    /// class that makes the two MUTUALLY EXCLUSIVE per session — whichever
    /// fires first permanently suppresses the other, so a session that got a
    /// report-enforcement nudge could never be told it skipped the policy pull.
    /// A verification arm silently swallowed by an unrelated arm is worse than
    /// no arm, because the dashboard reads clean.
    #[test]
    fn one_classs_marker_does_not_suppress_the_other() {
        let s = "session-compliance-test-class-independence";

        assert!(mark_nudged(s, NudgeClass::Report, 1), "report claim wins");
        assert!(
            !mark_nudged(s, NudgeClass::Report, 1),
            "the report class is now at its cap"
        );

        // ...and the policy-pull class is untouched. Under the old key this
        // assertion failed: the session was simply "already nudged".
        assert_eq!(nudge_attempts_for(s, NudgeClass::PolicyPull), 0);
        assert!(
            mark_nudged(s, NudgeClass::PolicyPull, 1),
            "an exhausted report budget must not consume the policy-pull one"
        );
        assert_eq!(nudge_attempts_for(s, NudgeClass::PolicyPull), 1);
        assert_eq!(
            nudge_attempts_for(s, NudgeClass::Report),
            1,
            "and the policy-pull delivery did not touch the report counter"
        );
    }

    /// The pre-coord screen must stay open while ANY class has budget left,
    /// or an exhausted class would suppress the other one turn earlier — the
    /// same bug moved upstream of the marker.
    #[test]
    fn the_pre_coord_screen_stays_open_while_any_class_has_budget() {
        let s = "session-compliance-test-any-class";
        assert!(any_class_under_cap(s, 1));
        assert!(mark_nudged(s, NudgeClass::PolicyPull, 1));
        assert!(
            any_class_under_cap(s, 1),
            "one class spent, the other still due"
        );
        assert!(mark_nudged(s, NudgeClass::Report, 1));
        assert!(!any_class_under_cap(s, 1), "both classes spent");
    }

    /// The configured cap applies PER CLASS, and `max_attempts: 0` still means
    /// never — for every class.
    #[test]
    fn the_configured_cap_applies_to_each_class_independently() {
        let s = "session-compliance-test-per-class-cap";
        for c in NudgeClass::ALL {
            for i in 1..=2 {
                assert!(mark_nudged(s, c, 2), "{} attempt {i} of 2", c.as_str());
            }
            assert!(!mark_nudged(s, c, 2), "{} is capped at 2", c.as_str());
            assert_eq!(nudge_attempts_for(s, c), 2);
        }
    }

    /// Coord stores ONE count per session, so the classes are SUMMED on the
    /// wire — and the `None`-vs-`0` distinction survives that summing. `None`
    /// still means "this run has no record of the session", which must OMIT the
    /// fields rather than send a fabricated `0` into a last-write-wins store.
    #[test]
    fn the_reported_count_sums_the_classes_without_collapsing_none_to_zero() {
        let s = "session-compliance-test-sum";
        assert!(
            nudge_state_for(s).is_none(),
            "no record for either class is None, not Some((0, None))"
        );

        assert!(mark_nudged(s, NudgeClass::Report, 1));
        assert_eq!(nudge_state_for(s).expect("recorded").0, 1);

        assert!(mark_nudged(s, NudgeClass::PolicyPull, 1));
        let (attempts, last_at) = nudge_state_for(s).expect("recorded");
        assert_eq!(attempts, 2, "coord's per-session count spans the classes");
        assert!(
            last_at.is_some(),
            "the most recent delivery across the classes stamps the report"
        );

        // And the omission rule still holds end to end.
        let body = compliance_body(
            None,
            Some(ABSENT_NO_BLOCK),
            nudge_state_for("never-seen-sum"),
        );
        assert!(body.get("nudge_attempts").is_none());
    }

    /// The class labels are part of the observable contract (marker key,
    /// operator-visible reason strings), and the policy-pull one must match
    /// coord's signal name so the two sides read as one thing.
    #[test]
    fn the_class_labels_are_stable_and_distinct() {
        assert_eq!(NudgeClass::Report.as_str(), "report");
        assert_eq!(NudgeClass::PolicyPull.as_str(), "policy_pull");
        assert_eq!(NudgeClass::ALL.len(), 2);
        assert_ne!(NudgeClass::ALL[0], NudgeClass::ALL[1]);
        assert_eq!(
            NudgeClass::ALL[0],
            NudgeClass::PolicyPull,
            "the precondition arm is offered first: telling a session to fix its report \
             while it has never read the document defining the report is backwards"
        );
    }

    // ── the policy-pull arm ──────────────────────────────────────────────

    fn verdict_with(reconciliation: Value, reason: &str) -> ComplianceVerdict {
        ComplianceVerdict::from_body(&json!({
            "verdict": "unverified",
            "reason": reason,
            "reconciliation": reconciliation,
            "footprint": {"prs": ["qontinui/qontinui-coord#1"], "commits": [], "claims": []},
        }))
    }

    fn policy_pull_signal(result: &str, detail: Value) -> Value {
        json!({"session_signals": [{
            "signal": "policy_pull",
            "result": result,
            "detail": detail,
        }]})
    }

    /// `unavailable` and `error` mean coord could not CHECK. Nudging on either
    /// would assert something coord does not know — and it is exactly the
    /// population (unattributable transports, a coord ahead of its migration)
    /// that the `Unavailable` arm exists to protect.
    #[test]
    fn only_an_absent_policy_pull_signal_is_nudgeable() {
        let detail = json!({"why": "no read"});
        assert!(policy_pull_absent(&policy_pull_signal("absent", detail.clone())).is_some());
        for benign in ["found", "unavailable", "error", "", "ABSENT", "future_word"] {
            assert!(
                policy_pull_absent(&policy_pull_signal(benign, detail.clone())).is_none(),
                "`{benign}` must not produce a nudge"
            );
        }
        // A payload from a coord that does not emit the signal at all.
        assert!(policy_pull_absent(&json!({"items": []})).is_none());
        assert!(policy_pull_absent(&Value::Null).is_none());
        // A different session-level signal must not be mistaken for this one.
        assert!(policy_pull_absent(&json!({"session_signals": [
            {"signal": "something_else", "result": "absent", "detail": {}}
        ]}))
        .is_none());
    }

    /// The two arms are independent: a session whose block reconciled perfectly
    /// but which never pulled policy gets the POLICY-PULL nudge, not "re-emit
    /// the block" — which would be a false statement about its own work.
    #[test]
    fn a_clean_report_with_no_policy_pull_nudges_only_the_policy_arm() {
        let v = verdict_with(
            {
                let mut r = policy_pull_signal("absent", json!({"why": "no read"}));
                r["schema_ok"] = json!(true);
                r["unreconciled_refs"] = json!([]);
                r
            },
            "policy pull not observed: no read",
        );
        assert_eq!(due_nudge_classes(&v), vec![NudgeClass::PolicyPull]);
        assert!(
            !report_arm_failed(&v),
            "a reason that is ONLY the policy-pull clause is not a report failure"
        );
    }

    /// Both failing yields both classes, policy-pull first — so a single turn
    /// delivers one and the other stays eligible instead of being swallowed.
    #[test]
    fn both_arms_failing_yields_both_classes_in_priority_order() {
        let mut r = policy_pull_signal("absent", json!({"why": "no read"}));
        r["schema_ok"] = json!(true);
        r["unreconciled_refs"] = json!(["#1"]);
        let v = verdict_with(
            r,
            "1 of 1 item(s) could not be reconciled: #1; policy pull not observed: no read",
        );
        assert_eq!(
            due_nudge_classes(&v),
            vec![NudgeClass::PolicyPull, NudgeClass::Report]
        );
    }

    /// A missing block with the policy pull FOUND is the pre-existing report
    /// arm, untouched.
    #[test]
    fn a_missing_block_with_a_found_pull_is_the_report_arm_alone() {
        let v = verdict_with(policy_pull_signal("found", json!({})), "absent");
        assert_eq!(due_nudge_classes(&v), vec![NudgeClass::Report]);
    }

    /// The policy-pull prompt must correct STEP 0, name the doors that work,
    /// and — when the read was merely stale — say so, because a session that
    /// read v5 believes it is current and will not re-read unless told the
    /// number moved.
    #[test]
    fn the_policy_pull_prompt_states_the_stale_case_distinctly() {
        let never = policy_pull_nudge_text(&json!({
            "current_version": 6,
            "why": "this session has attributed policy reads but never read \
                    policy/session-protocol",
            "stale_version_reads": [],
        }));
        assert!(never.contains("policy/session-protocol"));
        assert!(never.contains("Step 0"));
        assert!(
            never.contains("/coord/agent-prompt-documents"),
            "the masked-tools escape hatch must be named: {never}"
        );
        assert!(!never.contains("while v6 is current"));
        assert!(
            !never.contains("did not emit"),
            "this arm must never assert the block was missing: {never}"
        );

        let stale = policy_pull_nudge_text(&json!({
            "current_version": 6,
            "why": "this session read policy/session-protocol at a SUPERSEDED version",
            "stale_version_reads": [{"version": 5, "source": "mcp"}],
        }));
        assert!(
            stale.contains("You read v5 while v6 is current"),
            "the stale case must name both numbers: {stale}"
        );
    }

    /// Two Stops racing the same session must not both deliver — the stated
    /// reason `mark_nudged` is a compare-and-set rather than check-then-set.
    #[test]
    fn concurrent_claims_yield_exactly_one_winner() {
        let s = "session-compliance-test-race";
        let winners: usize = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..8)
                .map(|_| scope.spawn(move || usize::from(mark_nudged(s, NudgeClass::Report, 1))))
                .collect();
            handles.into_iter().map(|h| h.join().unwrap_or(0)).sum()
        });
        assert_eq!(winners, 1, "exactly one of eight racing claims may win");
        assert_eq!(nudge_attempts_for(s, NudgeClass::Report), 1);
    }

    // ── coverage bound ───────────────────────────────────────────────────

    #[test]
    fn coverage_bound_is_static_and_names_the_bare_terminal_gap() {
        let c = coverage_bound();
        assert!(!c.derived, "must never be derived from a runner metric");
        assert_eq!(c.schema, "session-compliance-coverage/1");
        assert!(c.not_covered.len() >= 4);
        assert!(c
            .not_covered
            .iter()
            .any(|g| g.scope.contains("bare terminals")));
        assert!(c.note.contains("liveUntracked"));
    }

    // ── transcript tail reader ───────────────────────────────────────────

    #[test]
    fn tail_reader_drops_the_truncated_first_line() {
        // Unique per process AND per nanosecond: several test runs share this
        // box, and a collision here would be a confusing cross-run failure.
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        let dir = std::env::temp_dir().join(format!(
            "qontinui-compliance-tail-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("tmp dir");
        let path = dir.join("t.jsonl");
        // One oversized junk line, then the real record. The junk line pushes
        // the file past the window, so it must be dropped as partial.
        let filler = "x".repeat((TRANSCRIPT_TAIL_BYTES + 1024) as usize);
        std::fs::write(&path, format!("{filler}\n{}\n", assistant_line(&[BLOCK]))).expect("write");
        let tail = read_transcript_tail(&path).expect("tail");
        assert!(!tail.contains("xxxx"), "partial first line must be dropped");
        assert!(scan_transcript(&tail).is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_transcript_reads_none_and_never_panics() {
        assert!(read_transcript_tail(Path::new("/definitely/not/here.jsonl")).is_none());
        assert!(detect(Path::new("/definitely/not/here.jsonl")).is_none());
    }
}
