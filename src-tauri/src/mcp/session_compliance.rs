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
//! not rebuilt. It fires **at most once per session**, and only when ALL of:
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
//! **`reopen` mode is NOT implemented.** Re-opening a closed session is a
//! spawn that outlives the request, and
//! `production-and-cost#agent-spawn-authorization` requires a standing
//! per-path opt-in that does not exist. When coord's config says `reopen`,
//! this module does nothing — see [`Applicability::nudge_allowed`].

use std::collections::HashSet;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::Value;
use tracing::{debug, info};

use super::continuation_verdict::{coord_client_parts, http_client};
use crate::session::session_lifecycle_store::SessionLifecycleStore;

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
            max_attempts: obj
                .get("max_attempts")
                .and_then(Value::as_u64)
                .unwrap_or(1)
                .min(u32::MAX as u64) as u32,
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
            prompt_document_version: obj
                .get("prompt_document_version")
                .and_then(Value::as_i64),
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
            footprint_commits: fp.map(|f| string_list(f.get("commits"))).unwrap_or_default(),
            footprint_claims: fp.map(|f| string_list(f.get("claims"))).unwrap_or_default(),
        }
    }

    /// Has coord independently observed ANY work for this session? §A4's
    /// condition 3 — the guard that keeps the nudge from nagging every session
    /// at its first pause.
    pub fn footprint_is_empty(&self) -> bool {
        self.footprint_prs.is_empty()
            && self.footprint_commits.is_empty()
            && self.footprint_claims.is_empty()
    }
}

/// Coerce a footprint array into display strings. Accepts plain strings and
/// objects (preferring the identifying field coord is most likely to carry).
fn string_list(v: Option<&Value>) -> Vec<String> {
    let Some(arr) = v.and_then(Value::as_array) else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|e| match e {
            Value::String(s) => Some(s.trim().to_string()),
            Value::Object(_) => ["ref", "pr", "sha", "key", "id", "resource_key"]
                .iter()
                .find_map(|k| e.get(*k).and_then(Value::as_str))
                .map(|s| s.trim().to_string()),
            _ => None,
        })
        .filter(|s| !s.is_empty())
        .collect()
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
    let body = serde_json::json!({
        "report": report.cloned().unwrap_or(Value::Null),
        "absent_reason": absent_reason.map(Value::from).unwrap_or(Value::Null),
    });
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

/// A corrective prompt the continuation-verdict handler may deliver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComplianceNudge {
    /// Claude session id — the key [`mark_nudged`] consumes on delivery.
    pub session_id: String,
    pub prompt: String,
}

/// Sessions already nudged, process-global and in-memory — the same posture
/// [`crate::mcp::continuation_verdict`]'s hourly cap registry takes. A runner
/// restart resets it, which can only re-grant a nudge that was never
/// delivered-and-answered; it can never produce a second nudge inside one run.
static NUDGED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

fn nudged() -> &'static Mutex<HashSet<String>> {
    NUDGED.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Has this session already been nudged? A poisoned lock reads as "yes"
/// (fail-safe: never nudge on a broken guard).
fn already_nudged(session_id: &str) -> bool {
    match nudged().lock() {
        Ok(g) => g.contains(session_id),
        Err(_) => true,
    }
}

/// Claim the session's single nudge. Returns `true` when THIS caller won the
/// claim, `false` when the session was already marked (or the guard is
/// poisoned). Called by the continuation-verdict handler at the moment it emits
/// the block, not when the candidate is computed — a candidate the cap
/// swallowed must stay eligible.
///
/// Compare-and-set rather than a separate check-then-set: two Stops racing for
/// the same session must not both deliver.
#[must_use]
pub fn mark_nudged(session_id: &str) -> bool {
    match nudged().lock() {
        Ok(mut g) => g.insert(session_id.to_string()),
        Err(_) => false,
    }
}

/// Refs coord could not reconcile, for the "contradicted" arm of the nudge.
///
/// Coord's `reconciliation` payload shape is defined by the parallel Phase 3
/// work, so this reads leniently: a top-level array, `{"unreconciled": […]}`,
/// or `{"items": […]}` with a per-item reconciled/status marker. Anything it
/// does not recognize yields an empty list and the nudge falls back to coord's
/// own `reason` string — never a fabricated claim about which item failed.
pub fn unreconciled_refs(reconciliation: &Value) -> Vec<String> {
    let arr = match reconciliation {
        Value::Array(a) => Some(a),
        Value::Object(_) => reconciliation
            .get("unreconciled")
            .and_then(Value::as_array)
            .or_else(|| reconciliation.get("items").and_then(Value::as_array)),
        _ => None,
    };
    let Some(arr) = arr else {
        return Vec::new();
    };
    arr.iter()
        .filter(|e| !item_reconciled(e))
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

/// Treat an entry as reconciled only when it explicitly says so. An entry with
/// no marker at all (e.g. a plain `unreconciled` list) counts as failed.
fn item_reconciled(e: &Value) -> bool {
    if let Some(b) = e
        .get("reconciled")
        .or_else(|| e.get("ok"))
        .and_then(Value::as_bool)
    {
        return b;
    }
    if let Some(s) = e
        .get("status")
        .or_else(|| e.get("state"))
        .or_else(|| e.get("verdict"))
        .and_then(Value::as_str)
    {
        return matches!(
            s.trim().to_ascii_lowercase().as_str(),
            "reconciled" | "verified" | "ok" | "confirmed"
        );
    }
    false
}

/// Build the corrective prompt, carrying coord's observations so the
/// correction is productive rather than a second guess.
pub fn nudge_text(v: &ComplianceVerdict) -> String {
    fn render(list: &[String]) -> String {
        if list.is_empty() {
            "none".to_string()
        } else {
            list.join(", ")
        }
    }
    let mut out = String::from(
        "You did not emit the POLICY_COMPLIANCE block required by \
         `policy/session-protocol` v4 Step 3.\n",
    );
    if v.reason.trim() != "absent" && !v.reason.trim().is_empty() {
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
    out.push_str("Produce the block now, reconciled against those observations.");
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
    let report = tokio::task::spawn_blocking(move || detect(&transcript))
        .await
        .ok()??;

    // 3. Nudge conditions that are knowable BEFORE coord answers (§A4). When
    //    none of them can hold, nothing in this turn's response depends on the
    //    verdict — so emit it DETACHED and return immediately, and the hook
    //    never waits on coord. That is the whole fleet under the report-only
    //    default, and every repeat turn of an already-nudged session.
    let nudge_possible = applicability.nudge_allowed(&config.mode)
        && !crate::mcp::continuation_verdict::stop_hook_active_from(hook_input)
        && !already_nudged(session_id);
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
    Some(ComplianceNudge {
        session_id: session_id.to_string(),
        prompt: nudge_text(&verdict),
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
    let Ok(Some(report)) = tokio::task::spawn_blocking(move || detect(&path)).await else {
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
        let text = "<!-- POLICY_COMPLIANCE v1 -->\n{\"schema\":\"policy-compliance/1\",\"items\":[]}";
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
        let text = "<!-- POLICY_COMPLIANCE v1 -->\n```json\n{ \"schema\": \"policy-compliance/1\", \
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

    // ── once-per-session guard ───────────────────────────────────────────

    #[test]
    fn mark_nudged_is_a_once_per_session_compare_and_set() {
        let a = "session-compliance-test-marker-a";
        let b = "session-compliance-test-marker-b";
        assert!(!already_nudged(a));
        assert!(mark_nudged(a), "first claim wins");
        assert!(!mark_nudged(a), "second claim must lose — one nudge per session");
        assert!(already_nudged(a));
        assert!(!already_nudged(b));
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
