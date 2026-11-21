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
//!
//! ## Whose tenant — the session's
//!
//! Plan `2026-11-13-runner-stop-hook-continuation-rules-and-compliance-use-the-device-default-tenant`.
//! Coord's compliance config and its compliance rows are per tenant. Both the
//! config read and the POST present the credential of the SESSION's tenant:
//! at turn end the one decision the continuation verdict made for this Stop,
//! at close one resolved from the lifecycle record
//! ([`crate::mcp::policy_tenant::closed_session_policy_target`]). They used
//! to present the device DEFAULT credential, so on a multi-bound device a
//! session working another tenant's repo was gated by the default tenant's
//! config and its rows landed in the default tenant. When the session's tenant
//! is UNKNOWN nothing is read and nothing is filed — the turn or close is
//! WITHHELD and counted ([`COMPLIANCE_WITHHELD_TENANT_UNKNOWN`]), never filed
//! under the default tenant.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock, PoisonError, Weak};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::Value;
use tracing::{debug, info};
use uuid::Uuid;

use super::continuation_verdict::{coord_base, http_client};
use crate::mcp::policy_tenant::{self, PolicyTarget};
use crate::session::session_lifecycle_store::{SessionLifecycleStore, TerminalSessionRecord};
use qontinui_runner_lib::file_tail::read_tail_lossy;
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

/// One config-cache slot: the coord base and the tenant whose credential read
/// the config ([`PolicyTarget::tenant`]).
///
/// It used to be ONE slot keyed by nothing, so whichever tenant's config was
/// read last gated every session on the box for the TTL. An UNKNOWN tenant
/// reads and writes no entry (it reads nothing at all).
type ConfigKey = (String, Option<Uuid>);

type ConfigSlot = (Instant, Option<ComplianceConfig>);

static CONFIG_CACHE: OnceLock<Mutex<HashMap<ConfigKey, ConfigSlot>>> = OnceLock::new();

fn config_cache() -> &'static Mutex<HashMap<ConfigKey, ConfigSlot>> {
    CONFIG_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The config cached for `(base, tenant)` while within [`CONFIG_TTL`] of
/// `now`. The outer `None` is a miss; `Some(None)` is a cached "coord could not
/// be consulted".
fn config_cache_lookup(
    base: &str,
    tenant: Option<Uuid>,
    now: Instant,
) -> Option<Option<ComplianceConfig>> {
    let g = config_cache().lock().ok()?;
    let (at, cached) = g.get(&(base.to_string(), tenant))?;
    (now.duration_since(*at) < CONFIG_TTL).then(|| cached.clone())
}

/// Cache `config` as `(base, tenant)`'s, read at `at`.
fn config_cache_store(
    base: &str,
    tenant: Option<Uuid>,
    at: Instant,
    config: Option<ComplianceConfig>,
) {
    if let Ok(mut g) = config_cache().lock() {
        g.insert((base.to_string(), tenant), (at, config));
    }
}

/// TTL-cached enforcement-config read under `target`'s credential. `None` =
/// coord could not be consulted (no usable credential for the target tenant,
/// transport error, non-2xx including the 404/501 this route returns until
/// coord's Phase 3 lands, undecodable body) ⇒ the caller does nothing.
async fn fetch_config(target: PolicyTarget) -> Option<ComplianceConfig> {
    let base = coord_base();
    let now = Instant::now();
    if let Some(cached) = config_cache_lookup(&base, target.tenant, now) {
        return cached;
    }
    let fresh = fetch_config_uncached(&base, target).await;
    config_cache_store(&base, target.tenant, now, fresh.clone());
    fresh
}

/// The config GET under `target`'s credential (`coord_http::coord_get_for`),
/// refused before it can be sent unless its bearer claims `target.tenant`.
fn build_config_get(
    client: &reqwest::Client,
    base: &str,
    target: PolicyTarget,
) -> Result<reqwest::Request, String> {
    policy_tenant::build_checked(
        crate::coord_http::coord_get_for(
            client,
            format!("{base}/coord/session-compliance/config"),
            target.scope,
        ),
        target,
    )
    .map(|(request, _presented)| request)
}

async fn fetch_config_uncached(base: &str, target: PolicyTarget) -> Option<ComplianceConfig> {
    let client = http_client().ok()?;
    let request = match build_config_get(&client, base, target) {
        Ok(request) => request,
        Err(reason) => {
            debug!(reason = %reason, "session-compliance: config read not sent");
            return None;
        }
    };
    let resp = client.execute(request).await.ok()?;
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
// The session's tenant (Design §3)
// ===========================================================================

/// Process-lifetime count of compliance turns and closes WITHHELD because the
/// session's tenant was UNKNOWN: no config read, no transcript read, no POST.
/// Each withhold logs the running total as `withheld_total`; Phase 2's gate is
/// that it stays 0 for sessions whose tenant resolves.
static COMPLIANCE_WITHHELD_TENANT_UNKNOWN: AtomicU64 = AtomicU64::new(0);

/// A compliance turn or close WITHHELD because the session's tenant was
/// UNKNOWN; `total` is [`COMPLIANCE_WITHHELD_TENANT_UNKNOWN`] after counting it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Withheld {
    total: u64,
}

/// The target compliance acts under — or [`Withheld`], counted and logged,
/// when the session's tenant is UNKNOWN. Never the default tenant: a row filed
/// there would be a record about a session that tenant does not govern
/// (served `verification-and-evidence` `unknown-must-not-render-as-a-default`).
fn compliance_target(
    tenant: &Result<PolicyTarget, String>,
    at: &'static str,
    session: &str,
) -> Result<PolicyTarget, Withheld> {
    match tenant {
        Ok(target) => Ok(*target),
        Err(reason) => {
            let withheld_total =
                COMPLIANCE_WITHHELD_TENANT_UNKNOWN.fetch_add(1, Ordering::Relaxed) + 1;
            info!(
                session = %session,
                at,
                withheld_total,
                reason = %reason,
                "session-compliance: compliance_withheld_tenant_unknown — the session's tenant is \
                 unknown, so nothing was read or filed under any tenant"
            );
            Err(Withheld {
                total: withheld_total,
            })
        }
    }
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
#[expect(
    clippy::string_slice,
    reason = "legacy str byte slice — migrate to str::get / char_indices / str_utils::truncate_str; plan 2026-09-14-runner-str-byte-slice-class-has-no-lint-gate"
)]
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
#[expect(
    clippy::string_slice,
    reason = "legacy str byte slice — migrate to str::get / char_indices / str_utils::truncate_str; plan 2026-09-14-runner-str-byte-slice-class-has-no-lint-gate"
)]
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
/// (probably truncated) first line when the file was larger than the window —
/// the runner's one bounded tail reader, [`read_tail_lossy`].
fn read_transcript_tail(path: &Path) -> Option<String> {
    read_tail_lossy(path, TRANSCRIPT_TAIL_BYTES)
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

/// The compliance POST under `target`'s credential
/// (`coord_http::coord_post_for`), refused before it can be sent unless its
/// bearer claims `target.tenant` — so an unpaired runner, or a tenant with no
/// usable slot, files nothing rather than filing anonymously.
fn build_compliance_post(
    client: &reqwest::Client,
    base: &str,
    claude_session_id: &str,
    body: &Value,
    target: PolicyTarget,
) -> Result<reqwest::Request, String> {
    policy_tenant::build_checked(
        crate::coord_http::coord_post_for(
            client,
            format!("{base}/coord/sessions/{claude_session_id}/compliance"),
            target.scope,
        )
        .json(body),
        target,
    )
    .map(|(request, _presented)| request)
}

/// POST the (possibly absent) report to coord under `target`'s credential and
/// return its verdict. `None` on every failure path — no usable credential,
/// unreachable coord, 404/501 (the route does not exist on production coord
/// yet), non-2xx, undecodable body.
async fn post_compliance(
    target: PolicyTarget,
    claude_session_id: &str,
    report: Option<&Value>,
    absent_reason: Option<&str>,
) -> Option<ComplianceVerdict> {
    let client = http_client().ok()?;
    let body = compliance_body(report, absent_reason, nudge_state_for(claude_session_id));
    let request =
        match build_compliance_post(&client, &coord_base(), claude_session_id, &body, target) {
            Ok(request) => request,
            Err(reason) => {
                debug!(
                    session = %claude_session_id,
                    reason = %reason,
                    "session-compliance: verdict POST not sent"
                );
                return None;
            }
        };
    let resp = client.execute(request).await.ok()?;
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
pub(crate) const TURN_END_BUDGET: Duration = Duration::from_millis(2500);

/// Observe one turn end: applicability gate → parse → emit → decide whether a
/// nudge is warranted.
///
/// `hook_input` is the raw Claude `Stop` payload, which carries both the
/// `session_id` (the Claude UUID coord keys compliance on — *not* the runner
/// terminal id the URL path may use) and the `transcript_path`.
///
/// `tenant` is the ONE tenant decision the continuation verdict made for this
/// Stop: the config read and the POST present that tenant's credential, and an
/// UNKNOWN tenant withholds the whole turn ([`compliance_target`]).
///
/// Returns the nudge CANDIDATE. The caller
/// ([`crate::mcp::continuation_verdict`]) owns the inherited loop guards
/// (`stop_hook_active`, the rolling hourly cap) and calls [`mark_nudged`] only
/// if it actually delivers. Every failure path — including exceeding
/// [`TURN_END_BUDGET`] — returns `None`.
pub async fn observe_turn_end(
    hook_input: &Value,
    tenant: &Result<PolicyTarget, String>,
) -> Option<ComplianceNudge> {
    let session = hook_input
        .get("session_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default();
    let target = compliance_target(tenant, "turn_end", session).ok()?;
    match tokio::time::timeout(TURN_END_BUDGET, observe_turn_end_inner(hook_input, target)).await {
        Ok(nudge) => nudge,
        Err(_) => {
            debug!("session-compliance: turn-end budget exceeded — allowing the stop");
            None
        }
    }
}

async fn observe_turn_end_inner(
    hook_input: &Value,
    target: PolicyTarget,
) -> Option<ComplianceNudge> {
    // 1. Applicability FIRST — before touching the transcript.
    let config = fetch_config(target).await?;
    let applicability = config.applicability();
    if !applicability.emits() {
        debug!(
            reason = %config.applicability_reason,
            tenant = %policy_tenant::target_label(&Ok(target)).0,
            "session-compliance: inert for this tenant"
        );
        return None;
    }
    let config_version = config.prompt_document_version;

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
            let _ = emit(target, &sid, report.as_ref(), config_version).await;
        });
        return None;
    }

    // 4. Emit and wait — a nudge is possible, so the verdict is load-bearing.
    let verdict = emit(target, session_id, report.as_ref(), config_version).await?;

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

/// POST the detection result under `target`'s credential and log the
/// outcome, naming the tenant it was filed under and the version of that
/// tenant's prompt document its config was read against.
async fn emit(
    target: PolicyTarget,
    session_id: &str,
    report: Option<&Value>,
    config_version: Option<i64>,
) -> Option<ComplianceVerdict> {
    let absent_reason = if report.is_none() {
        Some(ABSENT_NO_BLOCK)
    } else {
        None
    };
    let verdict = post_compliance(target, session_id, report, absent_reason).await?;
    info!(
        session = %session_id,
        block_present = report.is_some(),
        verdict = %verdict.verdict,
        reason = %verdict.reason,
        tenant = %policy_tenant::target_label(&Ok(target)).0,
        config_prompt_document_version =
            %crate::mcp::continuation_verdict::version_label(config_version),
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
    let inputs = CloseTenantInputs::of(&record);
    let config_dir = record.config_dir;
    tauri::async_runtime::spawn(async move {
        // Whose compliance this is: the session's tenant, resolved from the
        // record (Design §5). The nonce may already be released, so the
        // registry leg — or the record's durable tenant — usually decides.
        // Decided BEFORE taking a permit: the decision may wait its whole
        // budget, and a permit held through it would stall the other closes.
        let tenant = policy_tenant::closed_session_policy_target(
            &inputs.terminal_id,
            &session_id,
            inputs.working_dir.as_deref(),
            inputs.recorded_row,
            inputs.recorded_tenant,
            CLOSE_TENANT_BUDGET,
        )
        .await;
        // The liveness poll closes many stale records at once after a crash
        // restart, and every one of these does a config-dir walk, a multi-MB
        // read and a coord POST. Bound the burst rather than letting a boot
        // sweep fan out unthrottled.
        let _permit = match finalize_permits().acquire().await {
            Ok(p) => p,
            Err(_) => return,
        };
        let _ = finalize_impl(
            &session_id,
            config_dir.as_deref(),
            inputs.working_dir.as_deref(),
            &tenant,
        )
        .await;
    });
}

/// How long the close-time finalize's tenant decision may take. No hook waits
/// on it — the finalize is detached, and the decision runs before a finalize
/// permit is taken — so it may take what SessionStart takes; a read that has
/// not settled is `Unnamed`, which the record's durable tenant may still
/// decide, and which is otherwise UNKNOWN on a multi-bound device (withheld).
const CLOSE_TENANT_BUDGET: Duration = policy_tenant::ROUTE_TENANT_BUDGET;

/// What a closed lifecycle record names for its close-time tenant decision
/// (Design §5): the terminal that hosted it, its working dir, the coord row it
/// records (`finish_coord_session_id`) and its DURABLE tenant (`tenant_id`,
/// stamped at spawn and kept across a restart for exactly this). Pure.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CloseTenantInputs {
    terminal_id: String,
    working_dir: Option<String>,
    recorded_row: Option<Uuid>,
    recorded_tenant: Option<Uuid>,
}

impl CloseTenantInputs {
    fn of(record: &TerminalSessionRecord) -> Self {
        CloseTenantInputs {
            terminal_id: record.terminal_id.clone(),
            working_dir: record.working_dir.clone(),
            recorded_row: record.finish_coord_session_id,
            recorded_tenant: record
                .tenant_id
                .as_deref()
                .and_then(|t| Uuid::parse_str(t.trim()).ok()),
        }
    }
}

/// Concurrency bound for close-time finalizes (see [`finalize_on_close`]).
const FINALIZE_CONCURRENCY: usize = 2;

static FINALIZE_PERMITS: OnceLock<tokio::sync::Semaphore> = OnceLock::new();

fn finalize_permits() -> &'static tokio::sync::Semaphore {
    FINALIZE_PERMITS.get_or_init(|| tokio::sync::Semaphore::new(FINALIZE_CONCURRENCY))
}

/// The close-time detection + emit. `Err` is the withhold of an UNKNOWN
/// tenant; every other path — including the fail-open ones — is `Ok`.
async fn finalize_impl(
    session_id: &str,
    config_dir: Option<&str>,
    working_dir: Option<&str>,
    tenant: &Result<PolicyTarget, String>,
) -> Result<(), Withheld> {
    let target = compliance_target(tenant, "session_close", session_id)?;
    let Some(config) = fetch_config(target).await else {
        return Ok(());
    };
    if !config.applicability().emits() {
        return Ok(());
    }
    let config_version = config.prompt_document_version;
    let Some(path) = resolve_transcript(config_dir, working_dir, session_id) else {
        debug!(session = %session_id, "session-compliance: no transcript at close — no-op");
        return Ok(());
    };
    // Same reason as the turn-end path: the read + per-line parse is
    // synchronous and must not run on the async worker.
    let Ok(Some(report)) = spawn_blocking_tracked(move || detect(&path)).await else {
        return Ok(());
    };
    let _ = emit(target, session_id, report.as_ref(), config_version).await;
    Ok(())
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
mod tests;
