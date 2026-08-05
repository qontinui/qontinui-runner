//! Pull the canonical dev-environment config and compute a local apply plan.
//!
//! The read half of the **pull model** (plan
//! `2026-07-02-devenv-copy-canonical-config-phase2-agent-apply`, P2): one
//! developer designates a canonical machine in qontinui-web; every other box
//! pulls that machine's (secret-free) config and decides **locally** whether to
//! reconcile toward it. Nothing is ever pushed onto a box from the server, and
//! nothing in this module mutates the host — it fetches, diffs, and prints.
//!
//! The apply *policy* per section is **server-authoritative**: it arrives in the
//! pull payload (`section_policy`) rather than being hardcoded here, so the
//! rules can change without reshipping every runner. An unrecognized policy
//! string degrades to [`SectionPolicy::ReportOnly`] — a runner must never invent
//! permission to mutate from a policy it doesn't understand.
//!
//! Secret safety holds by construction on both sides: the stored envelope has
//! already had `env_contract` values coerced to `present`/`absent` by the
//! backend, so the canonical payload carries secret *names* and presence only —
//! never a value. The plan can therefore report "this box is missing secret X"
//! and can never copy X.

use std::collections::BTreeSet;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Map, Value};

use self::SectionPolicy::*;
use super::config::EnvAgentConfig;

/// Wire shape of `GET /api/v1/devenv/agent/environments/{id}/canonical-config`.
///
/// Deserialize-only and deliberately tolerant: unknown fields are ignored so a
/// backend that grows the payload doesn't break older runners.
#[derive(Debug, Deserialize)]
pub struct CanonicalConfig {
    /// Nullable in the served contract (`UUID | None`) even though the endpoint
    /// 422s the no-canonical case before it can emit null. Parsed as optional so
    /// a contract-legal payload can never surface as a "malformed payload" parse
    /// error; [`pull_and_plan`] turns the absent case into a readable message.
    /// Note `serde(default)` alone would NOT cover this — it fills a *missing*
    /// key, not an explicit `null`.
    #[serde(default)]
    pub canonical_machine_id: Option<String>,
    #[serde(default)]
    pub canonical_machine_name: Option<String>,
    /// Also nullable in the contract (`int | None`) — see above.
    #[serde(default)]
    pub schema_version: Option<u32>,
    #[serde(default)]
    pub captured_at: Option<String>,
    /// Section name → section map (each value a string), same shape as the
    /// capture envelope's `sections`.
    #[serde(default)]
    pub sections: Map<String, Value>,
    /// Section name → policy string. Server-authoritative; see [`SectionPolicy`].
    #[serde(default)]
    pub section_policy: Map<String, Value>,
    /// Section name → list of key names whose value is **derived from the repo**
    /// (a crate version, a package.json version, …). Such a key converges by
    /// pulling the repo, never by an apply, so it is reported but is never an
    /// action — see [`SectionPlan::actionable`].
    ///
    /// The contract gives **every section in the response an entry** (an empty
    /// list when nothing in it is derived), so "classified, none derived" is
    /// distinguishable from "section absent". An absent field entirely means an
    /// older backend, and yields exactly the pre-`derived_keys` behavior.
    ///
    /// `serde(default)` alone would NOT make this safe: it fills a *missing* key,
    /// not an explicit `null`. [`map_or_default`] covers the null case too.
    #[serde(default, deserialize_with = "map_or_default")]
    pub derived_keys: Map<String, Value>,
}

/// Deserialize a JSON object, treating an explicit `null` as an empty map.
///
/// The `serde(default)` trap: `#[serde(default)]` only fires on a *missing* key.
/// A contract-legal `"derived_keys": null` would otherwise be a hard parse error
/// and take down the whole pull.
fn map_or_default<'de, D>(d: D) -> Result<Map<String, Value>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<Map<String, Value>>::deserialize(d)?.unwrap_or_default())
}

/// The section whose capture is **process-scoped**: its values come from the
/// capturing process's own environment, so a runner-supervisor capture and a
/// plain-shell capture legitimately disagree. Only affects preview accuracy —
/// the section is `secret_report_only` and is never applied.
const ENV_CONTRACT_SECTION: &str = "env_contract";

/// What this runner is permitted to do with a section, as decided by the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SectionPolicy {
    /// Safe to reconcile toward canonical (`versions`, `services`).
    Applyable,
    /// Presence-only secrets — report gaps, never copy a value (`env_contract`).
    SecretReportOnly,
    /// Destructive; stop and defer to a human on this box (`db_schema`).
    DestructiveConfirm,
    /// Surface the drift, change nothing. Also the fallback for any policy this
    /// runner does not recognize.
    ReportOnly,
}

impl SectionPolicy {
    /// Parse a wire policy string. **Unknown → [`ReportOnly`]**: a newer backend
    /// policy must never be read as permission to mutate.
    pub fn from_wire(s: &str) -> Self {
        match s {
            "applyable" => Applyable,
            "secret_report_only" => SecretReportOnly,
            "destructive_confirm" => DestructiveConfirm,
            _ => ReportOnly,
        }
    }

    /// Short human label for the text plan.
    pub fn label(self) -> &'static str {
        match self {
            Applyable => "applyable",
            SecretReportOnly => "secrets: report-only",
            DestructiveConfirm => "destructive: needs human confirm",
            ReportOnly => "report-only",
        }
    }

    /// The canonical wire string. Round-trips with [`from_wire`](Self::from_wire)
    /// — `--json` consumers get the server's vocabulary, not the human label.
    pub fn wire(self) -> &'static str {
        match self {
            Applyable => "applyable",
            SecretReportOnly => "secret_report_only",
            DestructiveConfirm => "destructive_confirm",
            ReportOnly => "report_only",
        }
    }
}

/// A single key-level difference between canonical and this box.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// Canonical has the key; this box lacks it.
    Missing { key: String, canonical: String },
    /// Both have the key, values differ.
    Differs {
        key: String,
        local: String,
        canonical: String,
    },
    /// This box has the key; canonical lacks it. Informational — an extra local
    /// key is not automatically wrong, so it is never an apply action.
    Extra { key: String, local: String },
}

impl Change {
    /// The section key this change is about. Public because the section apply
    /// modules match on specific keys — `apply_versions` reads the capture
    /// provenance key straight off the diff.
    pub fn key(&self) -> &str {
        match self {
            Change::Missing { key, .. }
            | Change::Differs { key, .. }
            | Change::Extra { key, .. } => key,
        }
    }
}

/// The diff for one section, plus the policy that governs it.
#[derive(Debug, Clone)]
pub struct SectionPlan {
    pub section: String,
    pub policy: SectionPolicy,
    pub changes: Vec<Change>,
    /// True when canonical carries this section but this box produced nothing
    /// for it (collector unavailable — e.g. `db_schema` with no reachable PG).
    /// Distinguishes "no local data" from "in sync", which an empty `changes`
    /// list alone cannot express.
    pub local_section_absent: bool,
    /// Keys in this section whose value is repo-derived (from the server's
    /// `derived_keys`). Reported, never acted on.
    pub derived_keys: BTreeSet<String>,
}

impl SectionPlan {
    /// True when this section has nothing to report.
    pub fn is_clean(&self) -> bool {
        self.changes.is_empty() && !self.local_section_absent
    }

    /// True when this key's value is derived from the repo — it converges by
    /// pulling the repo, not by anything this runner could apply.
    pub fn is_derived(&self, key: &str) -> bool {
        self.derived_keys.contains(key)
    }

    /// Changes this runner would actually act on, given the policy. Only
    /// `Applyable` sections yield actions; `Extra` keys never do, and neither do
    /// repo-derived keys **regardless of policy** — applying one would fight the
    /// repo rather than reconcile the box.
    pub fn actionable(&self) -> Vec<&Change> {
        if self.policy != Applyable {
            return Vec::new();
        }
        self.changes
            .iter()
            .filter(|c| !matches!(c, Change::Extra { .. }))
            .filter(|c| !self.is_derived(c.key()))
            .collect()
    }
}

/// The full plan-first preview: what this box would change to match canonical.
#[derive(Debug, Clone)]
pub struct ApplyPlan {
    pub environment_id: String,
    pub canonical_machine_id: String,
    pub canonical_machine_name: Option<String>,
    pub captured_at: Option<String>,
    /// True when THIS box is the canonical machine — there is nothing to pull.
    pub is_canonical_self: bool,
    /// Set when canonical was captured under a different envelope schema than
    /// this runner speaks. The diff is still shown (it is usually still
    /// meaningful), but it may compare keys this runner doesn't understand, so
    /// the plan says so rather than quietly presenting a possibly-wrong answer.
    pub schema_mismatch: Option<(u32, u32)>,
    pub sections: Vec<SectionPlan>,
}

impl ApplyPlan {
    /// True when this box already matches canonical in every section.
    pub fn is_in_sync(&self) -> bool {
        self.sections.iter().all(SectionPlan::is_clean)
    }

    /// Total count of changes this runner would act on across all sections.
    pub fn actionable_count(&self) -> usize {
        self.sections.iter().map(|s| s.actionable().len()).sum()
    }
}

/// Read a section map into sorted `(key, value)` pairs, keeping only string
/// values. Both sides of the diff are `String→String` by contract; a non-string
/// value is malformed and is skipped rather than panicking.
fn string_pairs(section: Option<&Value>) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = section
        .and_then(Value::as_object)
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Diff canonical against local for one section. **Pure — unit-tested.**
fn diff_section(canonical: Option<&Value>, local: Option<&Value>) -> Vec<Change> {
    let can = string_pairs(canonical);
    let loc = string_pairs(local);
    let loc_map: std::collections::BTreeMap<&str, &str> =
        loc.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let can_map: std::collections::BTreeMap<&str, &str> =
        can.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();

    let mut changes = Vec::new();
    for (k, cv) in &can {
        match loc_map.get(k.as_str()) {
            None => changes.push(Change::Missing {
                key: k.clone(),
                canonical: cv.clone(),
            }),
            Some(lv) if *lv != cv.as_str() => changes.push(Change::Differs {
                key: k.clone(),
                local: (*lv).to_string(),
                canonical: cv.clone(),
            }),
            Some(_) => {}
        }
    }
    for (k, lv) in &loc {
        if !can_map.contains_key(k.as_str()) {
            changes.push(Change::Extra {
                key: k.clone(),
                local: lv.clone(),
            });
        }
    }
    changes.sort_by(|a, b| a.key().cmp(b.key()));
    changes
}

/// Compute the apply plan from a canonical payload and this box's own capture.
/// **Pure — unit-tested.** No I/O, no host mutation.
///
/// The union of both sides' section names is walked, so a section present only
/// locally still surfaces (as `Extra` keys under its policy) rather than being
/// silently dropped.
pub fn compute_plan(
    canonical: &CanonicalConfig,
    local_sections: &Map<String, Value>,
    environment_id: &str,
    local_machine_id: &str,
) -> ApplyPlan {
    // Absent only in a contract-legal-but-unreachable payload; `pull_and_plan`
    // rejects that before we get here.
    let canonical_id = canonical.canonical_machine_id.clone().unwrap_or_default();
    let is_canonical_self = !local_machine_id.is_empty() && local_machine_id == canonical_id;

    let mut names: Vec<&String> = canonical.sections.keys().collect();
    for k in local_sections.keys() {
        if !canonical.sections.contains_key(k) {
            names.push(k);
        }
    }
    names.sort();

    let sections = names
        .into_iter()
        .map(|name| {
            let policy = canonical
                .section_policy
                .get(name)
                .and_then(Value::as_str)
                .map(SectionPolicy::from_wire)
                // A section the server sent no policy for is not implicitly
                // applyable.
                .unwrap_or(ReportOnly);
            let can = canonical.sections.get(name);
            let loc = local_sections.get(name);
            // Absent field / absent section / non-array value all degrade to
            // "nothing derived here" — i.e. exactly the pre-`derived_keys`
            // behavior, never to a wrongly-suppressed action.
            let derived_keys: BTreeSet<String> = canonical
                .derived_keys
                .get(name)
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            SectionPlan {
                section: name.clone(),
                policy,
                changes: diff_section(can, loc),
                local_section_absent: can.is_some() && loc.is_none(),
                derived_keys,
            }
        })
        .collect();

    // Only compare when the server actually stated a version — an absent one
    // is not a mismatch.
    let schema_mismatch = canonical
        .schema_version
        .filter(|v| *v != super::SCHEMA_VERSION)
        .map(|v| (v, super::SCHEMA_VERSION));

    ApplyPlan {
        environment_id: environment_id.to_string(),
        canonical_machine_id: canonical_id,
        canonical_machine_name: canonical.canonical_machine_name.clone(),
        captured_at: canonical.captured_at.clone(),
        is_canonical_self,
        schema_mismatch,
        sections,
    }
}

// ============================================================================
// HTTP pull
// ============================================================================

/// GET the canonical config with `X-Machine-Key` auth.
///
/// Unlike the fire-and-forget capture PUT, this is an **interactive** read — it
/// fails fast with a readable error instead of retrying on a long backoff, so
/// `env pull` never appears to hang.
async fn get_canonical(
    backend_url: &str,
    environment_id: &str,
    machine_key: &str,
) -> Result<CanonicalConfig, String> {
    let url = format!(
        "{}/api/v1/devenv/agent/environments/{}/canonical-config",
        backend_url.trim_end_matches('/'),
        environment_id
    );
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| format!("reqwest client build: {e}"))?;
    let resp = client
        .get(&url)
        .header("X-Machine-Key", machine_key)
        .send()
        .await
        .map_err(|e| format!("GET {url} failed: {}", super::error_chain(&e)))?;

    let status = resp.status();
    let body = resp
        .text()
        .await
        .unwrap_or_else(|_| "<unable to read response body>".to_string());
    if !status.is_success() {
        // Map the backend's typed errors onto guidance the developer can act on.
        return Err(match extract_error_code(&body).as_deref() {
            Some("no_canonical_machine") => format!(
                "this environment has no canonical machine yet — set one in the web \
                 dashboard first (HTTP {status})"
            ),
            Some("canonical_has_no_config") => format!(
                "the canonical machine has not reported a config yet — run `env capture` \
                 on THAT box (HTTP {status})"
            ),
            Some("invalid_machine_key") | Some("machine_revoked") => {
                format!("machine key rejected — re-run `env enroll --code <code>` (HTTP {status})")
            }
            _ => format!("GET {url} -> HTTP {status}: {body}"),
        });
    }
    serde_json::from_str(&body).map_err(|e| format!("malformed canonical-config payload: {e}"))
}

/// Pull `detail.code` out of a FastAPI error body, if present. Best-effort — a
/// body that doesn't match simply yields `None` and the raw body is shown.
fn extract_error_code(body: &str) -> Option<String> {
    serde_json::from_str::<Value>(body)
        .ok()?
        .get("detail")?
        .get("code")?
        .as_str()
        .map(str::to_string)
}

/// Fetch canonical + capture locally + compute the plan. Read-only.
pub async fn pull_and_plan() -> Result<ApplyPlan, String> {
    let cfg = match EnvAgentConfig::load() {
        Some(c) if c.is_enrolled() => c,
        _ => {
            return Err("not enrolled — run `env enroll --code <code>` first".to_string());
        }
    };
    let machine_key = match crate::secure_storage::SecureStorage::new()
        .ok()
        .and_then(|s| s.get_agent_machine_key().ok().flatten())
    {
        Some(k) if !k.is_empty() => k,
        _ => {
            return Err(
                "enrolled but no machine key in secure storage — re-run `env enroll --code <code>`"
                    .to_string(),
            );
        }
    };

    let canonical = get_canonical(&cfg.backend_url, &cfg.environment_id, &machine_key).await?;
    if canonical.canonical_machine_id.is_none() {
        // Contract-legal but unreachable in practice: the endpoint 422s the
        // no-canonical case (handled above as `no_canonical_machine`). Fail with
        // a readable message rather than diffing against a phantom.
        return Err(
            "backend returned a canonical config with no canonical machine — nothing to pull"
                .to_string(),
        );
    }
    let local = super::build_envelope().await;
    Ok(compute_plan(
        &canonical,
        &local.sections,
        &cfg.environment_id,
        &cfg.machine_id,
    ))
}

/// Build a current-thread runtime and run [`pull_and_plan`]. For the sync CLI.
pub fn pull_and_plan_blocking() -> Result<ApplyPlan, String> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("tokio runtime build failed: {e}"))?;
    rt.block_on(pull_and_plan())
}

// ============================================================================
// Rendering
// ============================================================================

/// Render the plan as human-readable text. **Pure — unit-tested.**
pub fn render_plan(plan: &ApplyPlan) -> String {
    let mut out = String::new();
    let who = plan
        .canonical_machine_name
        .clone()
        .unwrap_or_else(|| plan.canonical_machine_id.clone());

    if plan.is_canonical_self {
        out.push_str(&format!(
            "This machine IS the canonical environment ({who}).\nNothing to pull.\n"
        ));
        return out;
    }

    out.push_str(&format!("Canonical environment: {who}\n"));
    if let Some(ts) = &plan.captured_at {
        out.push_str(&format!("Captured at:           {ts}\n"));
    }
    out.push_str(&format!("Environment:           {}\n", plan.environment_id));
    if let Some((theirs, ours)) = plan.schema_mismatch {
        out.push_str(&format!(
            "\n! canonical was captured with envelope schema v{theirs}, this runner speaks \
             v{ours}.\n  The diff below may be incomplete — consider updating the runner.\n"
        ));
    }
    out.push('\n');

    if plan.is_in_sync() {
        out.push_str("This machine is IN SYNC with canonical. Nothing to do.\n");
        return out;
    }

    for s in &plan.sections {
        if s.is_clean() {
            out.push_str(&format!("  {} — in sync\n", s.section));
            continue;
        }
        out.push_str(&format!("  {} [{}]\n", s.section, s.policy.label()));
        if s.local_section_absent {
            out.push_str(
                "    ! no local data for this section (collector unavailable) — cannot compare\n",
            );
        }
        for c in &s.changes {
            // A derived key is shown (the drift is real and worth knowing) but
            // marked, so nobody reads it as something an apply could fix.
            let mark = if s.is_derived(c.key()) {
                " [derived - converges by pulling the repo, not by an apply]"
            } else {
                ""
            };
            match c {
                Change::Missing { key, canonical } => out.push_str(&format!(
                    "    - {key}: missing here (canonical: {canonical}){mark}\n"
                )),
                Change::Differs {
                    key,
                    local,
                    canonical,
                } => out.push_str(&format!("    ~ {key}: {local} -> {canonical}{mark}\n")),
                Change::Extra { key, local } => {
                    out.push_str(&format!("    + {key}: {local} (not in canonical){mark}\n"))
                }
            }
        }
        // Preview-accuracy caveat, not an apply-safety one: env_contract is
        // captured from the CAPTURING PROCESS's environment, so the runner
        // supervisor's env (QONTINUI_API_URL, QONTINUI_RUNNER_ID, …) differs from
        // a plain shell's. The rows are NOT suppressed — a genuinely missing
        // secret must still show.
        if s.section == ENV_CONTRACT_SECTION && !s.changes.is_empty() {
            out.push_str(
                "    note: this section reflects the environment of the process that captured \
                 it,\n          so differences here may be process-scope artifacts rather than \
                 real gaps.\n",
            );
        }
        out.push('\n');
    }

    let n = plan.actionable_count();
    if n == 0 {
        out.push_str(
            "No changes are auto-applyable — everything above is report-only or needs a \
             human decision on this box.\n",
        );
    } else {
        out.push_str(&format!(
            "{n} change(s) are in applyable sections.\n\
             This is a PREVIEW — `env pull` never modifies this machine.\n",
        ));
    }
    out
}

/// Render the plan as JSON (for `--json` / future UI consumers).
/// **Pure — unit-tested.**
pub fn plan_to_json(plan: &ApplyPlan) -> Value {
    let sections: Vec<Value> = plan
        .sections
        .iter()
        .map(|s| {
            let changes: Vec<Value> = s
                .changes
                .iter()
                .map(|c| {
                    let derived = s.is_derived(c.key());
                    match c {
                        Change::Missing { key, canonical } => serde_json::json!({
                            "kind": "missing", "key": key, "canonical": canonical,
                            "derived": derived,
                        }),
                        Change::Differs {
                            key,
                            local,
                            canonical,
                        } => serde_json::json!({
                            "kind": "differs", "key": key, "local": local,
                            "canonical": canonical, "derived": derived,
                        }),
                        Change::Extra { key, local } => serde_json::json!({
                            "kind": "extra", "key": key, "local": local, "derived": derived,
                        }),
                    }
                })
                .collect();
            serde_json::json!({
                "section": s.section,
                "policy": s.policy.wire(),
                "local_section_absent": s.local_section_absent,
                "in_sync": s.is_clean(),
                "actionable_count": s.actionable().len(),
                "derived_keys": s.derived_keys.iter().collect::<Vec<_>>(),
                "process_scoped": s.section == ENV_CONTRACT_SECTION,
                "changes": changes,
            })
        })
        .collect();

    serde_json::json!({
        "environment_id": plan.environment_id,
        "canonical_machine_id": plan.canonical_machine_id,
        "canonical_machine_name": plan.canonical_machine_name,
        "captured_at": plan.captured_at,
        "is_canonical_self": plan.is_canonical_self,
        "schema_mismatch": plan.schema_mismatch.map(|(theirs, ours)| serde_json::json!({
            "canonical": theirs, "runner": ours,
        })),
        "in_sync": plan.is_in_sync(),
        "actionable_count": plan.actionable_count(),
        "sections": sections,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sect(pairs: &[(&str, &str)]) -> Value {
        let mut m = Map::new();
        for (k, v) in pairs {
            m.insert((*k).to_string(), json!(*v));
        }
        Value::Object(m)
    }

    fn canonical_with(sections: Value, policy: Value) -> CanonicalConfig {
        canonical_with_derived(sections, policy, json!({}))
    }

    fn canonical_with_derived(sections: Value, policy: Value, derived: Value) -> CanonicalConfig {
        CanonicalConfig {
            canonical_machine_id: Some("canon-machine".to_string()),
            canonical_machine_name: Some("spaceship".to_string()),
            schema_version: Some(super::super::SCHEMA_VERSION),
            captured_at: Some("2026-07-17T00:00:00Z".to_string()),
            sections: sections.as_object().unwrap().clone(),
            section_policy: policy.as_object().unwrap().clone(),
            derived_keys: derived.as_object().unwrap().clone(),
        }
    }

    #[test]
    fn policy_parses_known_wire_values() {
        assert_eq!(SectionPolicy::from_wire("applyable"), Applyable);
        assert_eq!(
            SectionPolicy::from_wire("secret_report_only"),
            SecretReportOnly
        );
        assert_eq!(
            SectionPolicy::from_wire("destructive_confirm"),
            DestructiveConfirm
        );
        assert_eq!(SectionPolicy::from_wire("report_only"), ReportOnly);
    }

    /// A policy this runner has never heard of must NEVER be read as permission
    /// to mutate — it degrades to report-only.
    #[test]
    fn unknown_policy_degrades_to_report_only() {
        assert_eq!(SectionPolicy::from_wire("apply_everything_now"), ReportOnly);
        assert_eq!(SectionPolicy::from_wire(""), ReportOnly);
    }

    /// `--json` must speak the server's vocabulary, not the human label.
    #[test]
    fn policy_wire_round_trips() {
        for p in [Applyable, SecretReportOnly, DestructiveConfirm, ReportOnly] {
            assert_eq!(SectionPolicy::from_wire(p.wire()), p);
        }
    }

    #[test]
    fn plan_to_json_emits_wire_policy_and_change_kinds() {
        let plan = compute_plan(
            &canonical_with(
                json!({"env_contract": {"QONTINUI_TOKEN": "present"}}),
                json!({"env_contract": "secret_report_only"}),
            ),
            json!({"env_contract": {}}).as_object().unwrap(),
            "env-1",
            "other",
        );
        let v = plan_to_json(&plan);
        assert_eq!(v["sections"][0]["policy"], "secret_report_only");
        assert_eq!(v["sections"][0]["changes"][0]["kind"], "missing");
        assert_eq!(v["actionable_count"], 0);
        assert_eq!(v["in_sync"], false);
    }

    #[test]
    fn diff_detects_missing_differing_and_extra() {
        let can = sect(&[("node", "22.1.0"), ("rustc", "1.82.0")]);
        let loc = sect(&[("node", "20.9.0"), ("python", "3.12.1")]);
        let changes = diff_section(Some(&can), Some(&loc));
        assert_eq!(
            changes,
            vec![
                Change::Differs {
                    key: "node".to_string(),
                    local: "20.9.0".to_string(),
                    canonical: "22.1.0".to_string()
                },
                Change::Extra {
                    key: "python".to_string(),
                    local: "3.12.1".to_string()
                },
                Change::Missing {
                    key: "rustc".to_string(),
                    canonical: "1.82.0".to_string()
                },
            ]
        );
    }

    #[test]
    fn identical_sections_produce_no_changes() {
        let s = sect(&[("node", "22.1.0")]);
        assert!(diff_section(Some(&s), Some(&s)).is_empty());
    }

    /// Only `applyable` sections yield actions, and an `Extra` local key is
    /// never an action (an extra key is not automatically wrong).
    #[test]
    fn actionable_respects_policy_and_skips_extras() {
        let plan = compute_plan(
            &canonical_with(
                json!({
                    "versions": {"node": "22.1.0"},
                    "env_contract": {"QONTINUI_SECRET": "present"},
                    "db_schema": {"alembic_head": "abc123"},
                }),
                json!({
                    "versions": "applyable",
                    "env_contract": "secret_report_only",
                    "db_schema": "destructive_confirm",
                }),
            ),
            json!({
                "versions": {"node": "20.9.0", "extra_tool": "1.0"},
                "env_contract": {},
                "db_schema": {"alembic_head": "old999"},
            })
            .as_object()
            .unwrap(),
            "env-1",
            "some-other-machine",
        );

        let versions = plan
            .sections
            .iter()
            .find(|s| s.section == "versions")
            .unwrap();
        // node differs -> actionable; extra_tool is Extra -> not actionable.
        assert_eq!(versions.actionable().len(), 1);

        // The secret gap is reported but never actionable.
        let env = plan
            .sections
            .iter()
            .find(|s| s.section == "env_contract")
            .unwrap();
        assert_eq!(env.policy, SecretReportOnly);
        assert_eq!(env.changes.len(), 1);
        assert!(env.actionable().is_empty());

        // A destructive schema drift is reported but never actionable.
        let db = plan
            .sections
            .iter()
            .find(|s| s.section == "db_schema")
            .unwrap();
        assert_eq!(db.policy, DestructiveConfirm);
        assert!(db.actionable().is_empty());

        assert_eq!(plan.actionable_count(), 1);
    }

    /// A section the server sent no policy for must not be treated as applyable.
    #[test]
    fn section_without_server_policy_defaults_to_report_only() {
        let plan = compute_plan(
            &canonical_with(json!({"mystery": {"k": "v"}}), json!({})),
            json!({"mystery": {"k": "other"}}).as_object().unwrap(),
            "env-1",
            "other",
        );
        let s = &plan.sections[0];
        assert_eq!(s.policy, ReportOnly);
        assert!(s.actionable().is_empty());
    }

    #[test]
    fn canonical_self_is_detected_and_renders_nothing_to_pull() {
        let plan = compute_plan(
            &canonical_with(json!({"versions": {"node": "22.1.0"}}), json!({})),
            json!({"versions": {"node": "20.0.0"}}).as_object().unwrap(),
            "env-1",
            "canon-machine",
        );
        assert!(plan.is_canonical_self);
        let text = render_plan(&plan);
        assert!(text.contains("IS the canonical environment"));
        assert!(text.contains("Nothing to pull"));
    }

    /// An empty local machine_id must not accidentally match canonical.
    #[test]
    fn empty_local_machine_id_is_not_canonical_self() {
        let plan = compute_plan(
            &canonical_with(json!({}), json!({})),
            &Map::new(),
            "env-1",
            "",
        );
        assert!(!plan.is_canonical_self);
    }

    #[test]
    fn in_sync_plan_renders_nothing_to_do() {
        let plan = compute_plan(
            &canonical_with(
                json!({"versions": {"node": "22.1.0"}}),
                json!({"versions": "applyable"}),
            ),
            json!({"versions": {"node": "22.1.0"}}).as_object().unwrap(),
            "env-1",
            "other",
        );
        assert!(plan.is_in_sync());
        assert!(render_plan(&plan).contains("IN SYNC"));
    }

    /// A canonical section with no local counterpart (collector unavailable) is
    /// distinct from "in sync" — an empty changes list alone would hide it.
    #[test]
    fn absent_local_section_is_flagged_not_reported_in_sync() {
        let plan = compute_plan(
            &canonical_with(
                json!({"db_schema": {}}),
                json!({"db_schema": "destructive_confirm"}),
            ),
            &Map::new(),
            "env-1",
            "other",
        );
        let s = &plan.sections[0];
        assert!(s.local_section_absent);
        assert!(!s.is_clean());
        assert!(render_plan(&plan).contains("no local data"));
    }

    /// The rendered preview must never carry a secret VALUE. The backend already
    /// coerces env_contract to present/absent; this asserts the runner keeps that
    /// property end-to-end.
    #[test]
    fn secret_safety_render_never_emits_a_secret_value() {
        let plan = compute_plan(
            &canonical_with(
                json!({"env_contract": {"QONTINUI_API_TOKEN": "present"}}),
                json!({"env_contract": "secret_report_only"}),
            ),
            json!({"env_contract": {}}).as_object().unwrap(),
            "env-1",
            "other",
        );
        let text = render_plan(&plan);
        assert!(text.contains("QONTINUI_API_TOKEN"));
        assert!(text.contains("present"));
        // The plan reports the NAME + presence, and has no channel for a value.
        assert!(!text.contains("hunter2"));
    }

    /// A canonical captured under a newer envelope schema still diffs, but the
    /// plan must SAY so rather than quietly presenting a possibly-wrong answer.
    #[test]
    fn newer_canonical_schema_is_flagged_in_the_plan() {
        let mut can = canonical_with(
            json!({"versions": {"node": "22.1.0"}}),
            json!({"versions": "applyable"}),
        );
        can.schema_version = Some(super::super::SCHEMA_VERSION + 1);
        let plan = compute_plan(
            &can,
            json!({"versions": {"node": "20.0.0"}}).as_object().unwrap(),
            "env-1",
            "other",
        );
        assert_eq!(
            plan.schema_mismatch,
            Some((
                super::super::SCHEMA_VERSION + 1,
                super::super::SCHEMA_VERSION
            ))
        );
        assert!(render_plan(&plan).contains("this runner speaks"));
    }

    /// Matching schema (and an omitted schema_version, which deserializes to 0)
    /// must not raise a false mismatch.
    #[test]
    fn matching_or_absent_schema_version_is_not_flagged() {
        let plan = compute_plan(
            &canonical_with(json!({}), json!({})),
            &Map::new(),
            "env-1",
            "other",
        );
        assert_eq!(plan.schema_mismatch, None);

        let mut can = canonical_with(json!({}), json!({}));
        can.schema_version = None; // field absent/null on the wire
        let plan = compute_plan(&can, &Map::new(), "env-1", "other");
        assert_eq!(plan.schema_mismatch, None);
    }

    /// The served contract declares `canonical_machine_id` and `schema_version`
    /// NULLABLE. A contract-legal payload must DESERIALIZE (the no-canonical case
    /// is then reported readably) rather than dying as a "malformed payload"
    /// parse error. `serde(default)` alone does not cover an explicit null, so
    /// these fields must be `Option` — this test pins that.
    #[test]
    fn contract_legal_nulls_deserialize() {
        let body = r#"{
            "environment_id": "980c0638-8988-422d-b52a-f08a18813654",
            "canonical_machine_id": null,
            "canonical_machine_name": null,
            "schema_version": null,
            "captured_at": null,
            "sections": {},
            "section_policy": {}
        }"#;
        let parsed: CanonicalConfig =
            serde_json::from_str(body).expect("contract-legal nulls parse");
        assert!(parsed.canonical_machine_id.is_none());
        assert!(parsed.schema_version.is_none());
    }

    /// A payload that omits the optional fields entirely must also parse, and a
    /// payload growing unknown fields must not break older runners.
    #[test]
    fn minimal_and_forward_compatible_payloads_parse() {
        let parsed: CanonicalConfig = serde_json::from_str(
            r#"{"environment_id":"e","canonical_machine_id":"m","future_field":{"x":1}}"#,
        )
        .expect("minimal + unknown fields parse");
        assert_eq!(parsed.canonical_machine_id.as_deref(), Some("m"));
        assert!(parsed.sections.is_empty());
    }

    #[test]
    fn extract_error_code_reads_fastapi_detail() {
        let body = r#"{"detail":{"code":"no_canonical_machine","message":"nope"}}"#;
        assert_eq!(
            extract_error_code(body).as_deref(),
            Some("no_canonical_machine")
        );
        assert_eq!(extract_error_code("not json"), None);
        assert_eq!(extract_error_code(r#"{"detail":"plain string"}"#), None);
    }

    // ------------------------------------------------------------------
    // derived_keys
    // ------------------------------------------------------------------

    /// The acceptance oracle in miniature: a repo-derived key in an APPLYABLE
    /// section is still reported (the drift is real) but is never an action —
    /// it converges by pulling the repo, which no apply on this box can do.
    /// A non-derived sibling in the SAME section must stay actionable, so the
    /// suppression is key-scoped and not section-scoped.
    #[test]
    fn derived_key_is_reported_but_never_actionable() {
        let plan = compute_plan(
            &canonical_with_derived(
                json!({"versions": {"runner_crate_version": "0.9.0", "node": "22.1.0"}}),
                json!({"versions": "applyable"}),
                json!({"versions": ["runner_crate_version"]}),
            ),
            json!({"versions": {"runner_crate_version": "0.8.0", "node": "20.9.0"}})
                .as_object()
                .unwrap(),
            "env-1",
            "other",
        );
        let s = &plan.sections[0];
        assert_eq!(s.policy, Applyable);
        // Both drifts are REPORTED.
        assert_eq!(s.changes.len(), 2);
        assert!(s.is_derived("runner_crate_version"));
        assert!(!s.is_derived("node"));
        // Only the non-derived one is actionable.
        let actionable: Vec<&str> = s.actionable().iter().map(|c| c.key()).collect();
        assert_eq!(actionable, vec!["node"]);
        assert_eq!(plan.actionable_count(), 1);

        let text = render_plan(&plan);
        assert!(text.contains("runner_crate_version"));
        assert!(text.contains("[derived - converges by pulling the repo, not by an apply]"));
        // The marker is key-scoped: the non-derived row must not carry it.
        let node_line = text
            .lines()
            .find(|l| l.contains("node:"))
            .expect("node row rendered");
        assert!(!node_line.contains("[derived"));
    }

    /// When every drift in an applyable section is derived, the plan reports
    /// ZERO actionable changes — the acceptance oracle for the canonical box.
    #[test]
    fn all_derived_section_reports_zero_actionable() {
        let plan = compute_plan(
            &canonical_with_derived(
                json!({"versions": {
                    "runner_crate_version": "0.9.0",
                    "node_package_version": "1.4.0",
                }}),
                json!({"versions": "applyable"}),
                json!({"versions": ["runner_crate_version", "node_package_version"]}),
            ),
            json!({"versions": {
                "runner_crate_version": "0.8.0",
                "node_package_version": "1.3.0",
            }})
            .as_object()
            .unwrap(),
            "env-1",
            "other",
        );
        assert_eq!(plan.sections[0].changes.len(), 2);
        assert_eq!(plan.actionable_count(), 0);
        assert!(render_plan(&plan).contains("No changes are auto-applyable"));
    }

    /// An EMPTY list for a section means "classified, none derived" — it must
    /// not suppress anything. This is the contract's distinguishable case.
    #[test]
    fn empty_derived_list_suppresses_nothing() {
        let plan = compute_plan(
            &canonical_with_derived(
                json!({"versions": {"node": "22.1.0"}}),
                json!({"versions": "applyable"}),
                json!({"versions": []}),
            ),
            json!({"versions": {"node": "20.9.0"}}).as_object().unwrap(),
            "env-1",
            "other",
        );
        assert!(plan.sections[0].derived_keys.is_empty());
        assert_eq!(plan.actionable_count(), 1);
    }

    /// An older backend sends no `derived_keys` at all — behavior must be
    /// byte-identical to the pre-`derived_keys` runner.
    #[test]
    fn absent_derived_keys_preserves_current_behavior() {
        let sections = json!({"versions": {"runner_crate_version": "0.9.0", "node": "22.1.0"}});
        let policy = json!({"versions": "applyable"});
        let local = json!({"versions": {"runner_crate_version": "0.8.0", "node": "20.9.0"}});

        // Field entirely absent on the wire.
        let parsed: CanonicalConfig = serde_json::from_str(
            r#"{"canonical_machine_id":"canon-machine",
                "sections":{"versions":{"runner_crate_version":"0.9.0","node":"22.1.0"}},
                "section_policy":{"versions":"applyable"}}"#,
        )
        .expect("payload without derived_keys parses");
        assert!(parsed.derived_keys.is_empty());

        let from_wire = compute_plan(&parsed, local.as_object().unwrap(), "env-1", "other");
        let reference = compute_plan(
            &canonical_with(sections, policy),
            local.as_object().unwrap(),
            "env-1",
            "other",
        );
        // Both drifts actionable, exactly as before the field existed.
        assert_eq!(from_wire.actionable_count(), 2);
        assert_eq!(from_wire.actionable_count(), reference.actionable_count());
        assert!(!render_plan(&reference).contains("[derived"));
        assert!(reference.sections[0].derived_keys.is_empty());
    }

    /// The `serde(default)` trap, pinned: `#[serde(default)]` fills a MISSING
    /// key, not an explicit `null`. A contract-legal `"derived_keys": null` must
    /// deserialize to an empty map rather than blowing up the whole pull.
    #[test]
    fn explicit_null_derived_keys_deserializes() {
        let parsed: CanonicalConfig = serde_json::from_str(
            r#"{"canonical_machine_id":"m","sections":{},"section_policy":{},
                "derived_keys":null}"#,
        )
        .expect("explicit null derived_keys parses");
        assert!(parsed.derived_keys.is_empty());
    }

    /// A malformed `derived_keys` (not an array, non-string members) degrades to
    /// "nothing derived" — never to a wrongly-suppressed action.
    #[test]
    fn malformed_derived_keys_degrades_to_nothing_derived() {
        let plan = compute_plan(
            &canonical_with_derived(
                json!({"versions": {"node": "22.1.0"}}),
                json!({"versions": "applyable"}),
                json!({"versions": "node"}), // string, not a list
            ),
            json!({"versions": {"node": "20.9.0"}}).as_object().unwrap(),
            "env-1",
            "other",
        );
        assert!(plan.sections[0].derived_keys.is_empty());
        assert_eq!(plan.actionable_count(), 1);
    }

    /// `--json` consumers must see derived-ness per change, and the section's
    /// actionable_count must already exclude it.
    #[test]
    fn plan_to_json_marks_derived_changes() {
        let plan = compute_plan(
            &canonical_with_derived(
                json!({"versions": {"node": "22.1.0", "runner_crate_version": "0.9.0"}}),
                json!({"versions": "applyable"}),
                json!({"versions": ["runner_crate_version"]}),
            ),
            json!({"versions": {"node": "20.9.0", "runner_crate_version": "0.8.0"}})
                .as_object()
                .unwrap(),
            "env-1",
            "other",
        );
        let v = plan_to_json(&plan);
        let changes = v["sections"][0]["changes"].as_array().unwrap();
        for c in changes {
            let expected = c["key"] == "runner_crate_version";
            assert_eq!(c["derived"], expected, "derived flag for {}", c["key"]);
        }
        assert_eq!(v["sections"][0]["derived_keys"][0], "runner_crate_version");
        assert_eq!(v["sections"][0]["actionable_count"], 1);
        assert_eq!(v["actionable_count"], 1);
    }

    // ------------------------------------------------------------------
    // env_contract process-scope caveat
    // ------------------------------------------------------------------

    /// The caveat renders when env_contract has changes — and the rows are NOT
    /// suppressed, because a genuinely missing secret must still show.
    #[test]
    fn env_contract_caveat_renders_when_it_has_changes() {
        let plan = compute_plan(
            &canonical_with(
                json!({"env_contract": {"QONTINUI_API_URL": "present"}}),
                json!({"env_contract": "secret_report_only"}),
            ),
            json!({"env_contract": {}}).as_object().unwrap(),
            "env-1",
            "other",
        );
        let text = render_plan(&plan);
        assert!(text.contains("process that captured it"));
        assert!(text.contains("process-scope artifacts"));
        // The row itself survives the caveat.
        assert!(text.contains("QONTINUI_API_URL"));
    }

    /// No env_contract changes ⇒ no caveat. It must not be unconditional noise,
    /// and must not attach to some other drifting section.
    #[test]
    fn env_contract_caveat_absent_without_env_contract_changes() {
        // env_contract in sync, a different section drifting.
        let plan = compute_plan(
            &canonical_with(
                json!({
                    "env_contract": {"QONTINUI_API_URL": "present"},
                    "versions": {"node": "22.1.0"},
                }),
                json!({"env_contract": "secret_report_only", "versions": "applyable"}),
            ),
            json!({
                "env_contract": {"QONTINUI_API_URL": "present"},
                "versions": {"node": "20.9.0"},
            })
            .as_object()
            .unwrap(),
            "env-1",
            "other",
        );
        let text = render_plan(&plan);
        assert!(text.contains("node"));
        assert!(!text.contains("process-scope artifacts"));

        // And a fully in-sync plan never renders it either.
        let synced = compute_plan(
            &canonical_with(
                json!({"env_contract": {"QONTINUI_API_URL": "present"}}),
                json!({"env_contract": "secret_report_only"}),
            ),
            json!({"env_contract": {"QONTINUI_API_URL": "present"}})
                .as_object()
                .unwrap(),
            "env-1",
            "other",
        );
        assert!(!render_plan(&synced).contains("process-scope artifacts"));
    }

    #[test]
    fn non_string_section_values_are_skipped_not_panicked() {
        let can = json!({"k": 42, "ok": "yes"});
        let pairs = string_pairs(Some(&can));
        assert_eq!(pairs, vec![("ok".to_string(), "yes".to_string())]);
    }
}
