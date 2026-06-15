//! Approach-D Conductor/Engine — Phase 6 coord-gated phase boundaries + the
//! Digital-Twin `DriftVerdict` verify.
//!
//! Contract refs: `D:/qontinui-root/qontinui-dev-notes/plans/2026-06-13-terminal-orchestration-handoff-contract.md`
//! §6 (coord durable checkpoints).
//!
//! ## What Phase 6 adds
//!
//! Earlier phases drive the subtask DAG purely off the durable ledger. Phase 6
//! lets a subtask's dispatch be GATED on an **observable external condition**
//! that coord already watches first-class (CI is green, a PR is merged, a deploy
//! is healthy), and lets a VERIFY subtask surface the Digital-Twin `DriftVerdict`.
//!
//! The control surface is the `expected_output` string a planner/elaborator
//! wrote on the subtask. [`classify_gate`] is a PURE function mapping that string
//! to an optional [`GatePredicateSpec`]; when it matches, the reconciler registers
//! a coord gate (via [`CoordGateClient`]) and HOLDS the subtask `Submitted` —
//! annotated as blocked-on-gate via the durable `gate_id`/`gate_status` columns —
//! until the gate clears (dispatch) or fails (fail the subtask). The gate ROW is
//! the durable record, so a restart re-attaches by re-reading the columns; no
//! re-registration.
//!
//! ## Coord client reuse (no hand-rolled auth)
//!
//! [`LiveCoordGateClient`] reuses [`crate::coord_mcp`] for EVERY coord call:
//! - register → `POST <coord>/mcp` JSON-RPC `tools/call coord_register_gate`
//!   authenticated with the runner's live **device** JWT (the same EdDSA token
//!   the coord `/mcp` relay presents). The device-authed `coord_register_gate`
//!   tool resolves the tenant + `registered_by` server-side from the JWT claims —
//!   we NEVER pass a tenant.
//! - poll → `GET <coord>/coord/gates` (filtered, device-bearer attached), matched
//!   client-side on `gate_id`. Coord's `resolve_operator_optional` resolves the
//!   tenant from the bearer context. Any failure → [`GateStatus::Unknown`] so the
//!   subtask simply stays blocked (best-effort, never wedges the run).
//! - drift verdict → `POST <coord>/mcp` JSON-RPC `tools/call <twin tool>`, the
//!   same tool the Digital-Twin Explorer's `GET /coord/twin/:subspace/verdict`
//!   dispatches; we read the raw [`crate::twin_verdict`]-shaped `DriftVerdict`
//!   envelope's `drift_class` token.
//!
//! Every live call is best-effort with short timeouts: coord may be unreachable,
//! and a coord outage must never crash or wedge a run (contract §6 — coord is a
//! durable checkpoint, not a hard dependency).

use async_trait::async_trait;
use serde_json::json;
use std::time::Duration;
use tracing::{info, warn};

// ============================================================================
// Gate predicate spec (the classifier's output — a runner-local shape that maps
// 1:1 onto coord's `GatePredicate` JSON `{kind, ...}`)
// ============================================================================

/// The kinds of observable external condition a subtask's `expected_output` can
/// gate dispatch on. Mirrors the relevant subset of coord's `gates::GatePredicate`
/// (`ci_green` / `pr_merged` / `deploy_healthy`). DriftVerdict is NOT here — it is
/// a verify-phase READ ([`DriftClass`]), not a dispatch gate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GatePredicateSpec {
    /// Gate clears when CI is green for `head_sha` on `repo`. Maps to coord
    /// `{"kind":"ci_green","repo":..,"head_sha":..}`.
    CiGreen { repo: String, head_sha: String },
    /// Gate clears when `pr_number` is merged on `repo`. Maps to coord
    /// `{"kind":"pr_merged","repo":..,"pr_number":..}`.
    PrMerged { repo: String, pr_number: i64 },
    /// Gate clears when `service` is healthy AND serving `expected_rev`. Maps to
    /// coord `{"kind":"deploy_healthy","service":..,"expected_rev":..}`.
    DeployHealthy {
        service: String,
        expected_rev: String,
    },
}

impl GatePredicateSpec {
    /// The coord `GatePredicate` JSON (`#[serde(tag="kind", rename_all="snake_case")]`)
    /// this spec maps to — the exact `predicate` argument `coord_register_gate`
    /// expects. The runner does NOT depend on the coord crate, so the predicate
    /// is built as untyped JSON here (coord re-validates it on register).
    ///
    /// `repo`/`resource_key` anchoring: coord's `register_gate_core` requires
    /// EXACTLY ONE anchor — `(claim_kind+resource_key)` or `(plan_id+phase_name)`.
    /// A conductor gate has neither a plan row nor a coord claim, so we anchor it
    /// on a `file_glob`-style claim whose `resource_key` is a synthetic, unique
    /// run/task key (it pins the gate to this subtask without colliding with any
    /// real claim). See [`Self::anchor_resource_key`].
    pub fn predicate_json(&self) -> serde_json::Value {
        match self {
            GatePredicateSpec::CiGreen { repo, head_sha } => json!({
                "kind": "ci_green",
                "repo": repo,
                "head_sha": head_sha,
            }),
            GatePredicateSpec::PrMerged { repo, pr_number } => json!({
                "kind": "pr_merged",
                "repo": repo,
                // coord's PrMerged.pr_number is i32; classify caps to i32 range.
                "pr_number": *pr_number,
            }),
            GatePredicateSpec::DeployHealthy {
                service,
                expected_rev,
            } => json!({
                "kind": "deploy_healthy",
                "service": service,
                "expected_rev": expected_rev,
            }),
        }
    }

    /// A short, stable label for logs.
    pub fn kind_label(&self) -> &'static str {
        match self {
            GatePredicateSpec::CiGreen { .. } => "ci_green",
            GatePredicateSpec::PrMerged { .. } => "pr_merged",
            GatePredicateSpec::DeployHealthy { .. } => "deploy_healthy",
        }
    }
}

/// The synthetic claim anchor `resource_key` for a conductor gate. Unique per
/// `(run_id, task_id)` so coord pins the gate to exactly this subtask and a
/// re-register (idempotent retry) maps to the same anchor.
pub fn anchor_resource_key(run_id: uuid::Uuid, task_id: &str) -> String {
    format!("orchestration:{run_id}:{task_id}")
}

/// The coord claim kind a conductor gate anchors on. `file_glob` is a real coord
/// claim kind, and our synthetic `resource_key` namespace (`orchestration:…`)
/// cannot collide with a repo file-glob, so the anchor is well-formed without
/// inventing a new coord claim kind.
pub const GATE_CLAIM_KIND: &str = "file_glob";

// ============================================================================
// Gate status (coord verdict, normalized) + drift class
// ============================================================================

/// A coord gate's verdict as the reconciler treats it. `Unknown` collapses every
/// best-effort failure mode (coord unreachable, parse failure, gate not yet
/// found) into "keep waiting" so a coord outage NEVER fails a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateStatus {
    /// Gate is registered and its condition has not yet been met → stay blocked.
    Open,
    /// Gate's condition is met → unblock + dispatch.
    Cleared,
    /// Gate's condition failed terminally (e.g. CI red, PR closed-unmerged) →
    /// fail the subtask.
    Failed,
    /// Could not determine the status (best-effort failure) → treat as `Open`.
    Unknown,
}

impl GateStatus {
    /// Parse coord's `verdict` token (`open`/`cleared`/`failed`).
    pub fn from_verdict(s: &str) -> GateStatus {
        match s.trim().to_ascii_lowercase().as_str() {
            "open" => GateStatus::Open,
            "cleared" => GateStatus::Cleared,
            "failed" => GateStatus::Failed,
            _ => GateStatus::Unknown,
        }
    }

    /// The durable `gate_status` column token to persist. `Unknown` persists as
    /// `open` (the conservative "keep waiting" shape) so a restart re-attaches
    /// and resumes polling rather than seeing an unrecognized value.
    pub fn as_column(self) -> &'static str {
        match self {
            GateStatus::Open | GateStatus::Unknown => "open",
            GateStatus::Cleared => "cleared",
            GateStatus::Failed => "failed",
        }
    }
}

/// The Digital-Twin `DriftVerdict.drift_class` token, normalized to the only
/// distinction the verify phase needs: drift vs no-drift. (`none` ⇒ no drift.)
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DriftClass {
    /// `drift_class == "none"` → Φ holds, no drift.
    NoDrift,
    /// Any non-`none` class (`benign_add`/`pending`/`in_place`/`active_negation`/
    /// `divergent`/`unknown`) → drift present, remediation needed.
    Drift(String),
}

impl DriftClass {
    /// Map a coord `drift_class` wire token to the verify distinction.
    pub fn from_drift_class_token(token: &str) -> DriftClass {
        if token.trim().eq_ignore_ascii_case("none") {
            DriftClass::NoDrift
        } else {
            DriftClass::Drift(token.trim().to_string())
        }
    }
}

// ============================================================================
// expected_output → gate-predicate classifier (PURE — unit-tested)
// ============================================================================

/// Classify a subtask's `expected_output` into an optional dispatch gate.
///
/// Pattern rules (case-insensitive substring matching on the natural-language
/// `expected_output` a planner/elaborator writes):
/// - mentions **CI being green** (`ci` + `green`, or `ci green`/`ci is green`) →
///   [`GatePredicateSpec::CiGreen`].
/// - mentions a **PR being merged** (`pr` + `merge`, or `pull request` + `merge`)
///   → [`GatePredicateSpec::PrMerged`].
/// - mentions a **deploy being healthy** (`deploy` + `health`) →
///   [`GatePredicateSpec::DeployHealthy`].
/// - otherwise → `None` (dispatch normally; the Phase-3 behavior).
///
/// The DriftVerdict verify pattern is handled separately by [`classify_drift_verify`]
/// — it is a verify-phase READ, not a dispatch gate, so it is NOT returned here.
///
/// ### How the predicate's concrete fields are filled
///
/// `expected_output` is free text, so the classifier extracts what it can and
/// fills the rest with a per-subtask default so the gate is well-formed:
/// - `repo` is taken from the subtask `repo` (passed in) when set, else parsed
///   from a `repo=<name>` / `repo:<name>` token in the text, else the literal
///   `"unknown"` (coord will keep the gate open — a wrong repo never spuriously
///   clears; CI-green requires matching check rows).
/// - `head_sha` / `pr_number` / `expected_rev` are parsed from `sha=`, `#<n>` /
///   `pr=<n>`, `rev=` tokens when present, else conservative defaults
///   (`"HEAD"` / `0` / `"HEAD"`) that coord cannot match → the gate stays open
///   until the real condition is observed against the right anchor. This is the
///   FAIL-CLOSED posture: a vaguely-specified gate never clears prematurely.
///
/// A real planner emits structured tokens (the contract recommends
/// `expected_output: "CI green on repo=qontinui-web sha=<sha>"`); the parser
/// reads those. The point of the classifier is the trigger + predicate KIND; the
/// concrete anchor is best-effort over the text + the subtask's own `repo`.
pub fn classify_gate(expected_output: &str, repo: Option<&str>) -> Option<GatePredicateSpec> {
    let lower = expected_output.to_ascii_lowercase();

    // DriftVerdict is a verify READ, never a dispatch gate — exclude it here even
    // if the text also mentions a gate keyword (the verify path owns it).
    if is_drift_verify(&lower) {
        return None;
    }

    let resolved_repo = repo
        .map(|r| r.to_string())
        .or_else(|| parse_kv_token(expected_output, "repo"))
        .unwrap_or_else(|| "unknown".to_string());

    // CI green.
    if (lower.contains("ci") && lower.contains("green"))
        || lower.contains("ci green")
        || lower.contains("ci is green")
    {
        let head_sha = parse_kv_token(expected_output, "sha")
            .or_else(|| parse_kv_token(expected_output, "head_sha"))
            .unwrap_or_else(|| "HEAD".to_string());
        return Some(GatePredicateSpec::CiGreen {
            repo: resolved_repo,
            head_sha,
        });
    }

    // PR merged.
    let mentions_pr = lower.contains("pr") || lower.contains("pull request");
    if mentions_pr && lower.contains("merge") {
        let pr_number = parse_pr_number(expected_output).unwrap_or(0);
        return Some(GatePredicateSpec::PrMerged {
            repo: resolved_repo,
            pr_number,
        });
    }

    // Deploy healthy.
    if lower.contains("deploy") && (lower.contains("health") || lower.contains("healthy")) {
        let service = parse_kv_token(expected_output, "service").unwrap_or(resolved_repo);
        let expected_rev = parse_kv_token(expected_output, "rev")
            .or_else(|| parse_kv_token(expected_output, "expected_rev"))
            .or_else(|| parse_kv_token(expected_output, "sha"))
            .unwrap_or_else(|| "HEAD".to_string());
        return Some(GatePredicateSpec::DeployHealthy {
            service,
            expected_rev,
        });
    }

    None
}

/// `true` when `expected_output` describes the Digital-Twin DriftVerdict verify
/// (`DriftVerdict shows no drift`, `drift verdict`, `no drift`). Used by the
/// reconciler's verify path; case-insensitive.
pub fn classify_drift_verify(expected_output: &str) -> bool {
    is_drift_verify(&expected_output.to_ascii_lowercase())
}

fn is_drift_verify(lower: &str) -> bool {
    lower.contains("driftverdict")
        || lower.contains("drift verdict")
        || (lower.contains("drift") && lower.contains("no drift"))
        || lower.contains("shows no drift")
        || lower.contains("digital twin")
}

/// Parse a `key=value` or `key:value` token (whitespace-delimited) out of free
/// text, case-insensitive on the key. Returns the value with surrounding
/// quotes/punctuation trimmed. `None` when the key is absent.
fn parse_kv_token(text: &str, key: &str) -> Option<String> {
    let key_l = key.to_ascii_lowercase();
    for tok in text.split_whitespace() {
        let (k, sep_rest) = if let Some(rest) = tok.split_once('=') {
            (rest.0, rest.1)
        } else if let Some(rest) = tok.split_once(':') {
            (rest.0, rest.1)
        } else {
            continue;
        };
        if k.to_ascii_lowercase() == key_l {
            let v = sep_rest.trim_matches(|c: char| c == '"' || c == '\'' || c == ',' || c == ';');
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    None
}

/// Parse a PR number from free text: a `#<n>`, `pr=<n>`, `pr_number=<n>`, or
/// `pr #<n>` token. Returns `None` when none present. Caps to `i32::MAX` (coord's
/// `PrMerged.pr_number` is i32).
fn parse_pr_number(text: &str) -> Option<i64> {
    if let Some(v) = parse_kv_token(text, "pr_number").or_else(|| parse_kv_token(text, "pr")) {
        if let Ok(n) = v.trim_start_matches('#').parse::<i64>() {
            return Some(n.min(i32::MAX as i64));
        }
    }
    // `#123` token anywhere.
    for tok in text.split_whitespace() {
        if let Some(num) = tok.strip_prefix('#') {
            if let Ok(n) = num
                .trim_matches(|c: char| !c.is_ascii_digit())
                .parse::<i64>()
            {
                return Some(n.min(i32::MAX as i64));
            }
        }
    }
    None
}

// ============================================================================
// Coord gate client (injected — live = coord_mcp, test = scripted fake)
// ============================================================================

/// The conductor's coord interface, factored behind a trait so tests inject a
/// fake that returns scripted gate statuses / drift verdicts WITHOUT a live coord.
///
/// All three methods are best-effort: the live impl swallows transport errors and
/// returns `Err` only for a genuinely actionable failure (the reconciler treats
/// ANY error as "keep waiting" / "no verdict yet", never as a hard stop).
#[async_trait]
pub trait CoordGateClient: Send + Sync {
    /// Register a gate for `(run_id, task_id)` with the given predicate. Returns
    /// the coord `gate_id`. Tenant + `registered_by` derive server-side from the
    /// runner's device JWT — never passed.
    async fn register_gate(
        &self,
        run_id: uuid::Uuid,
        task_id: &str,
        predicate: &GatePredicateSpec,
    ) -> Result<String, String>;

    /// Poll a registered gate's current status by `gate_id`.
    async fn poll_gate(&self, gate_id: &str) -> Result<GateStatus, String>;

    /// Read the Digital-Twin `DriftVerdict` for `subspace` (e.g. `health`,
    /// `schema`, `release`) and return its normalized [`DriftClass`].
    async fn drift_verdict(&self, subspace: &str) -> Result<DriftClass, String>;
}

/// The twin sub-space a DriftVerdict verify reads when the `expected_output`
/// doesn't name one. `health` is the broadest fleet-wide snapshot — a meaningful
/// whole-system "no drift" check for a generic verify subtask.
pub const DEFAULT_DRIFT_SUBSPACE: &str = "health";

/// Map a twin sub-space id to the coord MCP snapshot tool that answers it (the
/// same mapping `twin_routes::subspace_tool` uses). `None` for an unmapped /
/// parameterized sub-space.
fn drift_subspace_tool(subspace: &str) -> Option<&'static str> {
    Some(match subspace {
        "release" => "coord_query_release_state",
        "schema" => "coord_query_migration_state",
        "infra" => "coord_query_infra_drift",
        "infra_health" => "coord_query_infra_health_drift",
        "config" => "coord_query_config_state",
        "health" => "coord_query_health",
        "deps" => "coord_query_dependency_state",
        "route_serving" => "coord_query_route_serving_drift",
        "auth" => "coord_query_auth_config",
        "client_telemetry" => "coord_query_client_telemetry_drift",
        "worktree" => "coord_list_worktrees",
        _ => return None,
    })
}

/// Live coord client: reuses [`crate::coord_mcp`] for the base URL + device JWT
/// and `reqwest` for the HTTP calls (the same pattern `agent_runtime`'s coord
/// REST calls use). Construct once per run and thread it into the reconciler.
pub struct LiveCoordGateClient {
    /// HTTP request timeout for every coord call (best-effort).
    timeout: Duration,
}

impl Default for LiveCoordGateClient {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(8),
        }
    }
}

impl LiveCoordGateClient {
    pub fn new() -> Self {
        Self::default()
    }

    /// Read the runner's live device JWT from the encrypted `AuthManager` slot —
    /// the SAME credential `coord_mcp::provision_coord_mcp_for_session` writes
    /// into spawned sessions' `.mcp.json`. `None` when the runner is unpaired (no
    /// token) — the caller degrades to a no-op (coord calls skipped).
    fn device_jwt() -> Option<String> {
        match crate::auth::AuthManager::new().get_access_token() {
            Ok(t) if !t.trim().is_empty() => Some(t),
            _ => None,
        }
    }

    fn http_client(&self) -> Result<reqwest::Client, String> {
        reqwest::Client::builder()
            .timeout(self.timeout)
            .build()
            .map_err(|e| format!("coord_gate: http client build: {e:#}"))
    }

    /// Issue a coord `/mcp` JSON-RPC `tools/call` with the device JWT and return
    /// the tool's structured result `Value`. Reuses `coord_mcp::coord_mcp_url`.
    async fn mcp_tools_call(
        &self,
        tool_name: &str,
        arguments: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let jwt = Self::device_jwt()
            .ok_or_else(|| "coord_gate: runner has no device JWT (unpaired)".to_string())?;
        let url = crate::coord_mcp::coord_mcp_url();
        let client = self.http_client()?;
        let body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": tool_name, "arguments": arguments },
        });
        let resp = client
            .post(&url)
            .bearer_auth(&jwt)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("coord_gate: tools/call {tool_name} POST: {e:#}"))?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(format!(
                "coord_gate: tools/call {tool_name} → {status}: {}",
                first_line(&text)
            ));
        }
        let env: serde_json::Value = serde_json::from_str(&text)
            .map_err(|e| format!("coord_gate: tools/call {tool_name} parse: {e}"))?;
        if let Some(err) = env.get("error") {
            return Err(format!(
                "coord_gate: tools/call {tool_name} JSON-RPC error: {err}"
            ));
        }
        // The MCP envelope wraps the tool result under `result`. A coord tool's
        // structured payload arrives either directly as `result` or under
        // `result.structuredContent` / the first `result.content[].text` JSON.
        let result = env
            .get("result")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        Ok(extract_tool_payload(result))
    }
}

/// Best-effort extraction of a coord MCP tool's structured payload from the
/// `result` envelope. Accepts (in order): `structuredContent`, the parsed JSON of
/// the first `content[].text`, or the raw `result` itself.
fn extract_tool_payload(result: serde_json::Value) -> serde_json::Value {
    if let Some(sc) = result.get("structuredContent") {
        return sc.clone();
    }
    if let Some(arr) = result.get("content").and_then(|c| c.as_array()) {
        for part in arr {
            if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(text) {
                    return v;
                }
            }
        }
    }
    result
}

fn first_line(s: &str) -> String {
    s.lines()
        .next()
        .unwrap_or("")
        .trim()
        .chars()
        .take(300)
        .collect()
}

#[async_trait]
impl CoordGateClient for LiveCoordGateClient {
    async fn register_gate(
        &self,
        run_id: uuid::Uuid,
        task_id: &str,
        predicate: &GatePredicateSpec,
    ) -> Result<String, String> {
        let args = json!({
            "predicate": predicate.predicate_json(),
            // Claim anchor (coord requires exactly one anchor): a synthetic,
            // unique-per-subtask file_glob resource_key. tenant + registered_by
            // come from the device JWT server-side — never passed.
            "claim_kind": GATE_CLAIM_KIND,
            "resource_key": anchor_resource_key(run_id, task_id),
            // 'agent' clearance: this is an agent-fact gate the conductor watches,
            // not a human decision (no operator "Mark met" affordance needed).
            "clearance_audience": "agent",
        });
        let payload = self.mcp_tools_call("coord_register_gate", args).await?;
        let gate_id = payload
            .get("gate_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| format!("coord_gate: register returned no gate_id: {payload}"))?;
        info!(
            "coord_gate: registered {} gate {gate_id} for {run_id}/{task_id}",
            predicate.kind_label()
        );
        Ok(gate_id)
    }

    async fn poll_gate(&self, gate_id: &str) -> Result<GateStatus, String> {
        // Reuse the coord base resolver; poll the list route filtered nothing and
        // match gate_id client-side (coord has no per-gate-id MCP read tool; the
        // REST GET resolves the tenant from the device-bearer context).
        let jwt = Self::device_jwt()
            .ok_or_else(|| "coord_gate: runner has no device JWT (unpaired)".to_string())?;
        let base = crate::coord_mcp::coord_base_url();
        let url = format!("{base}/coord/gates?limit=500");
        let client = self.http_client()?;
        let resp = client
            .get(&url)
            .bearer_auth(&jwt)
            .send()
            .await
            .map_err(|e| format!("coord_gate: poll GET: {e:#}"))?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            // Best-effort: a poll that can't read coord → Unknown (keep waiting).
            warn!(
                "coord_gate: poll {gate_id} → {status}: {}",
                first_line(&text)
            );
            return Ok(GateStatus::Unknown);
        }
        let gates: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| format!("coord_gate: poll parse: {e}"))?;
        let arr = gates.as_array().cloned().unwrap_or_default();
        for g in &arr {
            if g.get("gate_id").and_then(|v| v.as_str()) == Some(gate_id) {
                let verdict = g.get("verdict").and_then(|v| v.as_str()).unwrap_or("open");
                return Ok(GateStatus::from_verdict(verdict));
            }
        }
        // Gate not present in the listing yet (eventual consistency / pagination)
        // → Unknown, keep waiting.
        Ok(GateStatus::Unknown)
    }

    async fn drift_verdict(&self, subspace: &str) -> Result<DriftClass, String> {
        let tool = drift_subspace_tool(subspace).ok_or_else(|| {
            format!("coord_gate: no snapshot twin tool for drift sub-space {subspace:?}")
        })?;
        let payload = self.mcp_tools_call(tool, json!({})).await?;
        // The DriftVerdict envelope's top-level `drift_class` is the wire token.
        let token = payload
            .get("drift_class")
            .and_then(|v| v.as_str())
            .ok_or_else(|| format!("coord_gate: drift verdict has no drift_class: {payload}"))?;
        Ok(DriftClass::from_drift_class_token(token))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- classifier: gate predicates ---------------------------------------

    #[test]
    fn classify_ci_green() {
        let p = classify_gate("a green PR with CI green", Some("qontinui-web")).unwrap();
        match p {
            GatePredicateSpec::CiGreen { repo, head_sha } => {
                assert_eq!(repo, "qontinui-web");
                assert_eq!(head_sha, "HEAD", "no sha token → conservative default");
            }
            other => panic!("expected CiGreen, got {other:?}"),
        }
    }

    #[test]
    fn classify_ci_green_with_sha_token() {
        let p = classify_gate("CI green sha=abc123 repo=qontinui-runner", None).unwrap();
        assert_eq!(
            p,
            GatePredicateSpec::CiGreen {
                repo: "qontinui-runner".to_string(),
                head_sha: "abc123".to_string()
            }
        );
    }

    #[test]
    fn classify_pr_merged() {
        let p = classify_gate("the PR is merged to main", Some("qontinui-coord")).unwrap();
        match p {
            GatePredicateSpec::PrMerged { repo, pr_number } => {
                assert_eq!(repo, "qontinui-coord");
                assert_eq!(
                    pr_number, 0,
                    "no #n token → 0 (coord cannot match → stays open)"
                );
            }
            other => panic!("expected PrMerged, got {other:?}"),
        }
    }

    #[test]
    fn classify_pr_merged_with_number() {
        let p = classify_gate("pull request #581 merged", Some("qontinui-web")).unwrap();
        assert_eq!(
            p,
            GatePredicateSpec::PrMerged {
                repo: "qontinui-web".to_string(),
                pr_number: 581
            }
        );
        // pr=<n> token form too.
        let p2 = classify_gate("merge the pr=42", Some("r")).unwrap();
        assert_eq!(
            p2,
            GatePredicateSpec::PrMerged {
                repo: "r".to_string(),
                pr_number: 42
            }
        );
    }

    #[test]
    fn classify_deploy_healthy() {
        let p = classify_gate("deploy is healthy on prod", Some("qontinui-coord")).unwrap();
        match p {
            GatePredicateSpec::DeployHealthy {
                service,
                expected_rev,
            } => {
                assert_eq!(service, "qontinui-coord");
                assert_eq!(expected_rev, "HEAD");
            }
            other => panic!("expected DeployHealthy, got {other:?}"),
        }
        // service= + rev= tokens override.
        let p2 = classify_gate("deploy healthy service=coord rev=deadbeef", None).unwrap();
        assert_eq!(
            p2,
            GatePredicateSpec::DeployHealthy {
                service: "coord".to_string(),
                expected_rev: "deadbeef".to_string()
            }
        );
    }

    #[test]
    fn classify_non_gated_is_none() {
        assert!(classify_gate("a functional spec document", Some("r")).is_none());
        assert!(classify_gate("the login screen is implemented", None).is_none());
        assert!(classify_gate("", None).is_none());
    }

    #[test]
    fn classify_drift_verify_is_not_a_dispatch_gate() {
        // A DriftVerdict verify is a READ, never a dispatch gate — even though it
        // contains no gate keyword, and even if it mentioned one.
        assert!(classify_drift_verify("DriftVerdict shows no drift"));
        assert!(classify_gate("DriftVerdict shows no drift", None).is_none());
        // A drift verify that ALSO says "CI green" must still not be a CI gate.
        assert!(classify_gate("digital twin DriftVerdict no drift, CI green", None).is_none());
        // Non-drift text is not a verify.
        assert!(!classify_drift_verify("a green PR"));
    }

    // --- predicate JSON shape (matches coord's tagged GatePredicate) --------

    #[test]
    fn predicate_json_uses_snake_case_kind_tag() {
        let ci = GatePredicateSpec::CiGreen {
            repo: "r".to_string(),
            head_sha: "s".to_string(),
        };
        assert_eq!(
            ci.predicate_json(),
            json!({"kind": "ci_green", "repo": "r", "head_sha": "s"})
        );
        let pr = GatePredicateSpec::PrMerged {
            repo: "r".to_string(),
            pr_number: 7,
        };
        assert_eq!(
            pr.predicate_json(),
            json!({"kind": "pr_merged", "repo": "r", "pr_number": 7})
        );
        let dh = GatePredicateSpec::DeployHealthy {
            service: "svc".to_string(),
            expected_rev: "rev".to_string(),
        };
        assert_eq!(
            dh.predicate_json(),
            json!({"kind": "deploy_healthy", "service": "svc", "expected_rev": "rev"})
        );
    }

    // --- status / drift parsing --------------------------------------------

    #[test]
    fn gate_status_from_verdict() {
        assert_eq!(GateStatus::from_verdict("open"), GateStatus::Open);
        assert_eq!(GateStatus::from_verdict("CLEARED"), GateStatus::Cleared);
        assert_eq!(GateStatus::from_verdict(" failed "), GateStatus::Failed);
        assert_eq!(GateStatus::from_verdict("weird"), GateStatus::Unknown);
        // Unknown persists as the conservative "open" so a restart keeps waiting.
        assert_eq!(GateStatus::Unknown.as_column(), "open");
        assert_eq!(GateStatus::Cleared.as_column(), "cleared");
        assert_eq!(GateStatus::Failed.as_column(), "failed");
    }

    #[test]
    fn drift_class_none_is_no_drift() {
        assert_eq!(
            DriftClass::from_drift_class_token("none"),
            DriftClass::NoDrift
        );
        assert_eq!(
            DriftClass::from_drift_class_token("NONE"),
            DriftClass::NoDrift
        );
        match DriftClass::from_drift_class_token("active_negation") {
            DriftClass::Drift(c) => assert_eq!(c, "active_negation"),
            other => panic!("expected Drift, got {other:?}"),
        }
    }

    #[test]
    fn anchor_resource_key_is_unique_per_subtask() {
        let r = uuid::Uuid::nil();
        assert_eq!(
            anchor_resource_key(r, "spec"),
            "orchestration:00000000-0000-0000-0000-000000000000:spec"
        );
        assert_ne!(anchor_resource_key(r, "a"), anchor_resource_key(r, "b"));
    }

    #[test]
    fn drift_subspace_tool_maps_health() {
        assert_eq!(drift_subspace_tool("health"), Some("coord_query_health"));
        assert_eq!(
            drift_subspace_tool("schema"),
            Some("coord_query_migration_state")
        );
        assert_eq!(drift_subspace_tool("definitely-unknown"), None);
    }

    #[test]
    fn extract_tool_payload_unwraps_content_text_json() {
        // structuredContent wins.
        let r = json!({"structuredContent": {"drift_class": "none"}});
        assert_eq!(extract_tool_payload(r), json!({"drift_class": "none"}));
        // else first content[].text parsed as JSON.
        let r2 = json!({"content": [{"type": "text", "text": "{\"drift_class\":\"pending\"}"}]});
        assert_eq!(extract_tool_payload(r2), json!({"drift_class": "pending"}));
        // else raw.
        let r3 = json!({"gate_id": "g1"});
        assert_eq!(extract_tool_payload(r3.clone()), r3);
    }
}
