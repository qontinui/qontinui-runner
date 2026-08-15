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
    /// Keys THIS BOX could not MEASURE — from the local capture's
    /// `unknown_keys`, not from the server. Today that means a toolchain probe
    /// that exceeded the capture budget: the key is absent from the local
    /// section, so it diffs as [`Change::Missing`], but "we could not read it"
    /// is not "you do not have it" and must never become an install action.
    ///
    /// A parallel per-key set, exactly like `derived_keys`, and deliberately not
    /// a value-shape change — see `collectors::VersionsCapture` for why a
    /// `{value, status}` value or a `"<unknown>"` sentinel would each relocate
    /// the bug rather than fix it.
    pub unknown_keys: BTreeSet<String>,
}

impl SectionPlan {
    /// True when this section has nothing to report — and, load-bearing, when
    /// everything it would have compared was actually READ.
    ///
    /// An unmeasured key makes this false even with zero changes. If canonical's
    /// stored capture and this box BOTH timed out on `rustc`, both omit the key,
    /// [`diff_section`] yields nothing, and a definition resting on the change
    /// list alone would report "in sync" over a key neither side ever read —
    /// a positive claim computed from an absence, which is the fleet's
    /// `silent-empty-is-unknown` rule. Silence is the one thing this cannot be
    /// allowed to read as agreement.
    pub fn is_clean(&self) -> bool {
        self.changes.is_empty() && !self.local_section_absent && self.unknown_keys.is_empty()
    }

    /// Keys in this section this box could not measure that produced NO diff row
    /// — canonical does not carry them either, so there is nothing to show in
    /// the change list and nothing but this to say they exist.
    pub fn silently_unmeasured_keys(&self) -> Vec<&String> {
        self.unknown_keys
            .iter()
            .filter(|k| !self.changes.iter().any(|c| c.key() == k.as_str()))
            .collect()
    }

    /// True when this key's value is derived from the repo — it converges by
    /// pulling the repo, not by anything this runner could apply.
    pub fn is_derived(&self, key: &str) -> bool {
        self.derived_keys.contains(key)
    }

    /// True when this box could not MEASURE this key's local value. The drift
    /// row is real and is still shown; what it means is "unknown", not "missing".
    pub fn is_unknown(&self, key: &str) -> bool {
        self.unknown_keys.contains(key)
    }

    /// True when THIS ROW is about a key the box could not measure — the ONE
    /// definition every renderer and every count uses.
    ///
    /// Scoped to [`Change::Missing`] for the same reason [`actionable`](Self::actionable)
    /// is: an unmeasured key is ABSENT locally, so it can only diff as
    /// `Missing`, and a `Differs`/`Extra` row on such a key is self-contradictory
    /// — it reports a local value for a key we are simultaneously claiming we
    /// never read.
    ///
    /// It exists because the two renderers used to DISAGREE about exactly that
    /// row. The text renderer stamped `[unknown - …]` on any change whose key
    /// was in the set, while `plan_to_json` hardcoded `"unknown": false` on the
    /// `Differs`/`Extra` arms as a constant-by-construction — so a hand-built
    /// `SectionPlan` (which the tests themselves build) made the same row read
    /// two opposite ways depending on which output you looked at. One function
    /// now decides, and the JSON constant is a fact about this function rather
    /// than about a construction nobody could check from the call site.
    pub fn change_is_unknown(&self, change: &Change) -> bool {
        matches!(change, Change::Missing { .. }) && self.is_unknown(change.key())
    }

    /// How many reported changes in this section are about an unmeasured key.
    /// Drives the operator-facing summary — an unknown that produced no change
    /// (canonical lacks the key too) is not worth a line.
    pub fn unknown_change_count(&self) -> usize {
        self.changes
            .iter()
            .filter(|c| self.change_is_unknown(c))
            .count()
    }

    /// Changes this runner would actually act on, given the policy. Only
    /// `Applyable` sections yield actions; `Extra` keys never do; repo-derived
    /// keys never do **regardless of policy** (applying one would fight the repo
    /// rather than reconcile the box); and neither does a `Missing` on a key this
    /// box could not measure — acting on one would install over a version nobody
    /// ever read.
    ///
    /// The unmeasured filter is scoped to [`Change::Missing`] ON PURPOSE, and
    /// that scope is the whole safety argument. "Unmeasured ⇒ do not act" is
    /// sound only because an unmeasured key is ABSENT locally and can therefore
    /// only diff as `Missing`; applied to every kind, the same line would convert
    /// a real `Change::Differs` — a drift this box measured on both sides — into
    /// a silently suppressed non-action, which is the original defect's mirror
    /// image. The invariant is enforced in [`compute_plan`], which intersects the
    /// unmeasured set against the keys actually absent from the local section;
    /// this filter is written so that even a caller who defeats that enforcement
    /// cannot lose a `Differs`.
    pub fn actionable(&self) -> Vec<&Change> {
        if self.policy != Applyable {
            return Vec::new();
        }
        self.changes
            .iter()
            .filter(|c| !matches!(c, Change::Extra { .. }))
            .filter(|c| !self.is_derived(c.key()))
            .filter(|c| !(matches!(c, Change::Missing { .. }) && self.is_unknown(c.key())))
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
    /// True when this box already matches canonical in every section **and
    /// every key that verdict rests on was actually read**.
    ///
    /// A key this box could not measure makes this false even when it produced
    /// no diff row: see [`SectionPlan::is_clean`] for why a positive in-sync
    /// claim must never be computed partly over never-read keys. The
    /// unmeasured keys are still enumerated — [`unmeasured_key_count`](Self::unmeasured_key_count)
    /// and the renderers — so the operator gets a reason, not just a `false`.
    pub fn is_in_sync(&self) -> bool {
        self.sections.iter().all(SectionPlan::is_clean)
    }

    /// Total keys this box could not measure, whether or not they produced a
    /// diff row. This is the SET size; [`unknown_count`](Self::unknown_count) is
    /// the row count. They differ exactly when canonical also lacks a key — the
    /// case that would otherwise read as agreement.
    pub fn unmeasured_key_count(&self) -> usize {
        self.sections.iter().map(|s| s.unknown_keys.len()).sum()
    }

    /// Unmeasured keys sitting in an `Applyable` section — the ones for which
    /// "no changes are auto-applyable" would be a statement about a measurement
    /// gap rather than about the box.
    pub fn unmeasured_in_applyable_count(&self) -> usize {
        self.sections
            .iter()
            .filter(|s| s.policy == Applyable)
            .map(|s| s.unknown_keys.len())
            .sum()
    }

    /// Total count of changes this runner would act on across all sections.
    pub fn actionable_count(&self) -> usize {
        self.sections.iter().map(|s| s.actionable().len()).sum()
    }

    /// Total count of reported changes that are about a key this box could not
    /// measure. Never overlaps [`actionable_count`](Self::actionable_count).
    pub fn unknown_count(&self) -> usize {
        self.sections
            .iter()
            .map(SectionPlan::unknown_change_count)
            .sum()
    }

    /// Unmeasured keys that produced NO drift row anywhere — canonical lacks
    /// them too, so the only thing standing between them and reading as
    /// agreement is that they are counted here.
    pub fn silently_unmeasured_count(&self) -> usize {
        self.sections
            .iter()
            .map(|s| s.silently_unmeasured_keys().len())
            .sum()
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

/// Read a per-section key list (`derived_keys` / `unknown_keys`) into a set.
///
/// Degradation is deliberate and always in the SAFE direction — towards the
/// behavior that existed before the field did — because both callers suppress an
/// apply action, so a malformed list must never suppress one by accident. It is
/// not uniform, though:
///
/// - An absent entry, an explicit null, or a non-array value degrade WHOLESALE
///   to the empty set: there is no list to read at all.
/// - Non-string MEMBERS degrade PER MEMBER, keeping the readable ones —
///   `["rustc", 7]` yields `{"rustc"}`, not `{}`. Dropping the whole list would
///   discard a legible suppression because of an unrelated malformed sibling.
fn string_list(value: Option<&Value>) -> BTreeSet<String> {
    value
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
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
///
/// `local_unknown_keys` is the LOCAL capture's `unknown_keys` (section name →
/// key list, straight off [`super::ConfigEnvelope`]) — the keys this box failed
/// to MEASURE. It is a required parameter rather than an optional setter
/// precisely because forgetting it is the defect: an unmeasured key looks
/// exactly like an absent one, and an absent one is an install action. Pass an
/// empty map only where there genuinely is no capture to speak of.
///
/// The set is not trusted verbatim: it is intersected against the keys actually
/// absent from the matching local section, so a set that did not come from THIS
/// capture cannot suppress a drift this box measured. See the intersection site
/// below.
pub fn compute_plan(
    canonical: &CanonicalConfig,
    local_sections: &Map<String, Value>,
    local_unknown_keys: &Map<String, Value>,
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
            let derived_keys: BTreeSet<String> = string_list(canonical.derived_keys.get(name));
            // Same degradation rule, same reason: a malformed/absent list means
            // "nothing unmeasured here", i.e. exactly the pre-`unknown_keys`
            // behavior — never a wrongly-suppressed action.
            //
            // Then INTERSECTED against the keys genuinely absent from the local
            // section. `unknown_keys` suppresses an apply action, and that
            // suppression is sound only under the invariant "an unmeasured key
            // is absent locally, so it can only diff as Missing". That invariant
            // is maintained by `collect_versions` — but this is a `pub fn` taking
            // an ARBITRARY set, so it cannot be assumed here: a caller passing a
            // set not co-generated with its section (the
            // `~/.qontinui/last_env_capture.json` cache, a future server-echoed
            // set, a merged set) would otherwise turn a real `Change::Differs`
            // into a suppressed non-action. The intersection makes the invariant
            // hold BY CONSTRUCTION for every caller, at the one place both the
            // set and the section are in hand; a key that claims to be unmeasured
            // while carrying a local value is simply not believed.
            let local_keys: BTreeSet<String> =
                string_pairs(loc).into_iter().map(|(k, _)| k).collect();
            let unknown_keys: BTreeSet<String> = string_list(local_unknown_keys.get(name))
                .into_iter()
                .filter(|k| !local_keys.contains(k))
                .collect();
            SectionPlan {
                section: name.clone(),
                policy,
                changes: diff_section(can, loc),
                local_section_absent: can.is_some() && loc.is_none(),
                derived_keys,
                unknown_keys,
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
        &local.unknown_keys,
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
        // Keys that were never read and produced no row. Without this line the
        // section header would be the only trace of them, and a section whose
        // ONLY finding is an unread key would print as an empty block.
        for key in s.silently_unmeasured_keys() {
            out.push_str(&format!(
                "    ? {key}: could not be measured here, and canonical carries no value for it \
                 either — there is nothing to compare, which is NOT the same as agreeing\n"
            ));
        }
        if s.local_section_absent {
            out.push_str(
                "    ! no local data for this section (collector unavailable) — cannot compare\n",
            );
        }
        for c in &s.changes {
            // A derived or unmeasured key is shown (the drift is real and worth
            // knowing) but marked, so nobody reads it as something an apply
            // could fix. `unknown` wins when both would apply: it is the
            // stronger statement — we did not read this value at all.
            //
            // Through `change_is_unknown`, so this renderer and `plan_to_json`
            // cannot disagree about the same row.
            let unknown = s.change_is_unknown(c);
            let mark = if unknown {
                " [unknown - the local probe exceeded its budget; NOT measured, NOT missing]"
            } else if s.is_derived(c.key()) {
                " [derived - converges by pulling the repo, not by an apply]"
            } else {
                ""
            };
            match c {
                // Distinct glyph AND distinct verb: an operator scanning this
                // must be able to tell "we could not measure it" from "you do
                // not have it", because only the second is something to install.
                Change::Missing { key, canonical } if unknown => out.push_str(&format!(
                    "    ? {key}: could not be measured here (canonical: {canonical}){mark}\n"
                )),
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

    // Counted over the SET, not the rows: a key neither side measured produces
    // no row at all, and that is exactly the case an operator must not read as
    // agreement.
    let unmeasured = plan.unmeasured_key_count();
    if unmeasured > 0 {
        out.push_str(&format!(
            "{unmeasured} key(s) could NOT be measured on this box (the probe exceeded its budget \
             twice).\nThey are reported as UNKNOWN, never as missing, and `env apply` will \
             not act on them — re-run `env capture` on a less busy box to resolve them.\n",
        ));
        // Counted from the keys THEMSELVES, not as `unmeasured - rows`: the
        // subtraction assumed every unmeasured key produces at most one row and
        // that every such row is counted as unknown, which is true of a computed
        // plan and not of a hand-built one. `silently_unmeasured_keys` is the
        // same predicate the per-section lines above are printed from, so the
        // count and the lines can never disagree.
        let silent = plan.silently_unmeasured_count();
        if silent > 0 {
            out.push_str(&format!(
                "  {silent} of them produced no drift row at all, because canonical carries no \
                 value for them either. That is an absence of evidence, NOT evidence of agreement \
                 — this machine cannot be called in sync over them.\n",
            ));
        }
        out.push('\n');
    }

    let n = plan.actionable_count();
    let blind = plan.unmeasured_in_applyable_count();
    if n == 0 && blind > 0 {
        // "Everything above is report-only or needs a human decision" would be
        // false here: the count is zero partly because a key in an APPLYABLE
        // section was never read, which is a measurement gap, not a policy one.
        out.push_str(&format!(
            "No changes are auto-applyable right now — but {blind} key(s) in applyable \
             section(s) could not be measured on this box, so this is NOT a statement that \
             nothing needs doing. Everything else above is report-only or needs a human \
             decision.\n",
        ));
    } else if n == 0 {
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
                    // The SAME predicate the text renderer uses. See
                    // `SectionPlan::change_is_unknown`.
                    let unknown = s.change_is_unknown(c);
                    match c {
                        // The wire word: `kind: "unknown"`. An unmeasured key is
                        // absent locally, so it reaches here as `Missing` — but
                        // emitting it as `"missing"` is precisely the confusion
                        // this branch removes, and a `--json` consumer must not
                        // have to cross-reference a flag to avoid it. The
                        // `unknown` boolean is emitted on EVERY change too, so a
                        // consumer that switches on `kind` and one that reads
                        // flags both get the truth.
                        Change::Missing { key, canonical } if unknown => serde_json::json!({
                            "kind": "unknown", "key": key, "canonical": canonical,
                            "derived": derived, "unknown": true,
                        }),
                        Change::Missing { key, canonical } => serde_json::json!({
                            "kind": "missing", "key": key, "canonical": canonical,
                            "derived": derived, "unknown": false,
                        }),
                        // `unknown` is false on these two arms, and now it is
                        // false because `change_is_unknown` SAYS so rather than
                        // because a comment asserts a construction the call site
                        // cannot check. Both arms require a local value, and a
                        // `"kind": "differs", "unknown": true` object would say
                        // both "we read both sides" and "we read neither" — so
                        // the predicate is scoped to `Missing` and the text
                        // renderer reads the same one, which is what stops the
                        // two outputs from contradicting each other on a
                        // hand-built plan.
                        Change::Differs {
                            key,
                            local,
                            canonical,
                        } => serde_json::json!({
                            "kind": "differs", "key": key, "local": local,
                            "canonical": canonical, "derived": derived, "unknown": unknown,
                        }),
                        Change::Extra { key, local } => serde_json::json!({
                            "kind": "extra", "key": key, "local": local, "derived": derived,
                            "unknown": unknown,
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
                "unknown_keys": s.unknown_keys.iter().collect::<Vec<_>>(),
                "unknown_count": s.unknown_change_count(),
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
        // False whenever ANY key went unmeasured, even with zero diff rows: a
        // positive in-sync claim is never computed over keys nobody read.
        // `unmeasured_key_count` is the reason it can be false with an empty
        // change list, so a consumer never has to guess.
        "in_sync": plan.is_in_sync(),
        "actionable_count": plan.actionable_count(),
        // Disjoint from `actionable_count` by construction — `actionable()`
        // filters unknown keys out. This counts ROWS an operator can see.
        "unknown_count": plan.unknown_count(),
        // …and this counts the SET, including keys that produced no row because
        // canonical lacks them too. `unmeasured_key_count > unknown_count` is
        // exactly the silent case.
        "unmeasured_key_count": plan.unmeasured_key_count(),
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

    /// [`compute_plan`] for a capture that measured everything it attempted —
    /// the case almost every test below is about. The unmeasured-key tests call
    /// [`compute_plan`] directly with a real `unknown_keys` map, so the
    /// parameter is never invisible in the tests that exist to exercise it.
    fn plan_of(
        canonical: &CanonicalConfig,
        local_sections: &Map<String, Value>,
        environment_id: &str,
        local_machine_id: &str,
    ) -> ApplyPlan {
        compute_plan(
            canonical,
            local_sections,
            &Map::new(),
            environment_id,
            local_machine_id,
        )
    }

    /// A local `unknown_keys` map: section name → key list, exactly the shape
    /// [`super::super::ConfigEnvelope::unknown_keys`] puts on the wire.
    fn unknown(section: &str, keys: &[&str]) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert(section.to_string(), json!(keys));
        m
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
        let plan = plan_of(
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
        let plan = plan_of(
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
        let plan = plan_of(
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
        let plan = plan_of(
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
        let plan = plan_of(
            &canonical_with(json!({}), json!({})),
            &Map::new(),
            "env-1",
            "",
        );
        assert!(!plan.is_canonical_self);
    }

    #[test]
    fn in_sync_plan_renders_nothing_to_do() {
        let plan = plan_of(
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
        let plan = plan_of(
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
        let plan = plan_of(
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
        let plan = plan_of(
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
        let plan = plan_of(
            &canonical_with(json!({}), json!({})),
            &Map::new(),
            "env-1",
            "other",
        );
        assert_eq!(plan.schema_mismatch, None);

        let mut can = canonical_with(json!({}), json!({}));
        can.schema_version = None; // field absent/null on the wire
        let plan = plan_of(&can, &Map::new(), "env-1", "other");
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
        let plan = plan_of(
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
        let plan = plan_of(
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
        let plan = plan_of(
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

        let from_wire = plan_of(&parsed, local.as_object().unwrap(), "env-1", "other");
        let reference = plan_of(
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
        let plan = plan_of(
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
        let plan = plan_of(
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
    // unmeasured (probe-budget) keys
    // ------------------------------------------------------------------

    /// **The INVERSION of the defect this branch fixes.** The predecessor,
    /// `unmeasured_version_key_is_actionable_documents_the_defect` (commit
    /// "test: pin the capture-probe-budget defect in the env pull plan"),
    /// asserted `actionable_count() == 1` on exactly this input: a `versions`
    /// probe that exceeded the capture budget omits its key, the pull diffs the
    /// omission as `Change::Missing`, and `Missing` in an applyable, non-derived
    /// section is an install action — for a toolchain version nobody ever read.
    ///
    /// Same input, opposite verdict: the key is still REPORTED (the operator
    /// should know the reading is missing) and is no longer ACTIONABLE.
    #[test]
    fn unmeasured_version_key_is_reported_but_never_actionable() {
        let plan = compute_plan(
            &canonical_with(
                json!({"versions": {"rustc": "rustc 1.82.0 (f6e511eec 2024-10-15)"}}),
                json!({"versions": "applyable"}),
            ),
            // What a timed-out `rustc --version` capture actually looks like:
            // the key is simply not there.
            json!({"versions": {"probe_scope_kind": "declared"}})
                .as_object()
                .unwrap(),
            &unknown("versions", &["rustc"]),
            "env-1",
            "other",
        );
        let s = &plan.sections[0];
        assert_eq!(s.policy, Applyable);
        assert!(!s.is_derived("rustc"));
        assert!(s.is_unknown("rustc"));
        // Still REPORTED, and still as a diff row.
        assert!(matches!(
            s.changes.iter().find(|c| c.key() == "rustc"),
            Some(Change::Missing { .. })
        ));
        assert_eq!(s.unknown_change_count(), 1);
        // Never ACTIONABLE — this is the fix.
        assert!(s.actionable().is_empty());
        assert_eq!(plan.actionable_count(), 0);
        assert_eq!(plan.unknown_count(), 1);
    }

    /// The over-suppression guard, and the reason `version_of_within` returns a
    /// three-armed outcome rather than a bool. A tool that genuinely is not
    /// installed produces `ProbeOutcome::Failed`, which records NO unknown key —
    /// so its `Missing` must stay actionable. Without this, "never act on an
    /// unknown" could quietly become "never act on `versions` at all", and the
    /// apply would stop fixing the boxes it exists to fix.
    #[test]
    fn a_genuinely_failed_probe_still_counts_as_actionable() {
        let plan = compute_plan(
            &canonical_with(
                json!({"versions": {
                    "rustc": "rustc 1.82.0 (f6e511eec 2024-10-15)",
                    "node": "v22.1.0",
                }}),
                json!({"versions": "applyable"}),
            ),
            // Neither key captured — but only `rustc` timed out. `node` simply
            // is not installed on this box.
            json!({"versions": {"probe_scope_kind": "declared"}})
                .as_object()
                .unwrap(),
            &unknown("versions", &["rustc"]),
            "env-1",
            "other",
        );
        let s = &plan.sections[0];
        assert!(s.is_unknown("rustc"));
        assert!(!s.is_unknown("node"));
        let actionable: Vec<&str> = s.actionable().iter().map(|c| c.key()).collect();
        assert_eq!(
            actionable,
            vec!["node"],
            "a genuinely-absent tool must still be installable"
        );
        assert_eq!(plan.actionable_count(), 1);
        assert_eq!(plan.unknown_count(), 1);
    }

    /// Suppression is key-scoped, not section-scoped: a real drift sitting
    /// beside an unmeasured key stays actionable. Same property
    /// `derived_key_is_reported_but_never_actionable` pins for derived keys.
    #[test]
    fn unknown_key_does_not_suppress_its_measured_siblings() {
        let plan = compute_plan(
            &canonical_with(
                json!({"versions": {"rustc": "rustc 1.82.0", "node": "v22.1.0"}}),
                json!({"versions": "applyable"}),
            ),
            json!({"versions": {"node": "v20.9.0"}})
                .as_object()
                .unwrap(),
            &unknown("versions", &["rustc"]),
            "env-1",
            "other",
        );
        let s = &plan.sections[0];
        assert_eq!(s.changes.len(), 2);
        let actionable: Vec<&str> = s.actionable().iter().map(|c| c.key()).collect();
        assert_eq!(actionable, vec!["node"]);
    }

    /// An operator must be able to tell "we could not measure this" from "you
    /// are missing this" by READING the plan — so the two get different glyphs,
    /// different verbs, and only one gets the budget explanation.
    #[test]
    fn render_distinguishes_unmeasured_from_missing() {
        let plan = compute_plan(
            &canonical_with(
                json!({"versions": {"rustc": "rustc 1.82.0", "node": "v22.1.0"}}),
                json!({"versions": "applyable"}),
            ),
            json!({"versions": {}}).as_object().unwrap(),
            &unknown("versions", &["rustc"]),
            "env-1",
            "other",
        );
        let text = render_plan(&plan);
        let rustc_line = text
            .lines()
            .find(|l| l.contains("rustc:"))
            .expect("rustc row rendered");
        let node_line = text
            .lines()
            .find(|l| l.contains("node:"))
            .expect("node row rendered");

        assert!(
            rustc_line.contains("could not be measured here"),
            "{rustc_line}"
        );
        assert!(rustc_line.trim_start().starts_with('?'), "{rustc_line}");
        assert!(rustc_line.contains("[unknown"), "{rustc_line}");
        // The genuinely-missing sibling keeps the old wording and glyph.
        assert!(node_line.contains("missing here"), "{node_line}");
        assert!(node_line.trim_start().starts_with('-'), "{node_line}");
        assert!(!node_line.contains("[unknown"), "{node_line}");

        assert!(text.contains("1 key(s) could NOT be measured on this box"));
        // …and the unknown one did not inflate the applyable count.
        assert!(text.contains("1 change(s) are in applyable sections"));
    }

    /// `--json` consumers get the distinction as a first-class `kind`, not as a
    /// flag they have to remember to read — plus the per-section key list and
    /// the plan-level total.
    #[test]
    fn plan_to_json_emits_the_unknown_kind_and_counts() {
        let plan = compute_plan(
            &canonical_with(
                json!({"versions": {"rustc": "rustc 1.82.0", "node": "v22.1.0"}}),
                json!({"versions": "applyable"}),
            ),
            json!({"versions": {}}).as_object().unwrap(),
            &unknown("versions", &["rustc"]),
            "env-1",
            "other",
        );
        let v = plan_to_json(&plan);
        let changes = v["sections"][0]["changes"].as_array().unwrap();
        let rustc = changes.iter().find(|c| c["key"] == "rustc").unwrap();
        let node = changes.iter().find(|c| c["key"] == "node").unwrap();

        assert_eq!(rustc["kind"], "unknown");
        assert_eq!(rustc["unknown"], true);
        // The canonical value is still carried — the row is informative, just
        // not actionable.
        assert_eq!(rustc["canonical"], "rustc 1.82.0");
        assert_eq!(node["kind"], "missing");
        assert_eq!(node["unknown"], false);

        assert_eq!(v["sections"][0]["unknown_keys"][0], "rustc");
        assert_eq!(v["sections"][0]["unknown_count"], 1);
        assert_eq!(v["sections"][0]["actionable_count"], 1);
        assert_eq!(v["unknown_count"], 1);
        assert_eq!(v["actionable_count"], 1);
    }

    /// An unmeasured key that canonical ALSO lacks produces no diff ROW, so it
    /// must not inflate `unknown_count` — that counter describes rows an
    /// operator can see, not set membership.
    ///
    /// But it must NOT therefore be silent, and it must not be called in sync.
    /// If canonical's stored capture and this box both timed out on `rustc`,
    /// both omit the key and `diff_section` yields nothing — and the old
    /// behaviour printed "This machine is IN SYNC with canonical. Nothing to
    /// do." while the two versions genuinely differed. A positive claim computed
    /// over a key nobody read is the fleet's `silent-empty-is-unknown` rule; the
    /// VERDICT changes, the counter does not.
    #[test]
    fn unknown_key_absent_from_canonical_is_reported_and_never_called_in_sync() {
        let plan = compute_plan(
            &canonical_with(
                json!({"versions": {"node": "v22.1.0"}}),
                json!({"versions": "applyable"}),
            ),
            json!({"versions": {"node": "v22.1.0"}})
                .as_object()
                .unwrap(),
            &unknown("versions", &["rustc"]),
            "env-1",
            "other",
        );
        assert!(plan.sections[0].is_unknown("rustc"));
        // The row counter is unchanged: there is genuinely no row.
        assert_eq!(plan.unknown_count(), 0);
        // The SET counter is what carries the fact.
        assert_eq!(plan.unmeasured_key_count(), 1);
        assert!(
            !plan.is_in_sync(),
            "a verdict resting on a key nobody read is not a verdict"
        );
        assert!(!plan.sections[0].is_clean());

        let text = render_plan(&plan);
        assert!(
            !text.contains("IN SYNC with canonical"),
            "must not claim in sync over an unread key: {text}"
        );
        assert!(text.contains("could NOT be measured"), "{text}");
        assert!(
            text.contains("NOT evidence of agreement"),
            "the silent case needs saying out loud: {text}"
        );
        // The key itself is named, not just counted.
        assert!(text.contains("rustc"), "{text}");

        let v = plan_to_json(&plan);
        assert_eq!(v["in_sync"], false);
        assert_eq!(v["unknown_count"], 0);
        assert_eq!(v["unmeasured_key_count"], 1);
    }

    /// The narrow-suppression guard, and the mirror image of the original bug.
    /// `actionable()` suppresses an unmeasured key only on `Change::Missing` —
    /// the only kind the "unmeasured ⇒ absent locally" invariant covers. A
    /// `Differs` means this box DID read a local value, so a set that claims the
    /// key is unmeasured must not be able to suppress it.
    #[test]
    fn a_differs_on_a_key_listed_as_unknown_is_still_actionable() {
        let canonical = canonical_with(
            json!({"versions": {"node": "v22.1.0"}}),
            json!({"versions": "applyable"}),
        );
        let local = json!({"versions": {"node": "v20.9.0"}});

        let plan = compute_plan(
            &canonical,
            local.as_object().unwrap(),
            // A set NOT co-generated with this section — the cache/echoed/merged
            // caller `compute_plan`'s signature allows.
            &unknown("versions", &["node"]),
            "env-1",
            "other",
        );
        let s = &plan.sections[0];
        // The intersection refuses the claim outright: the key carries a local
        // value, so it cannot have been unmeasured.
        assert!(
            !s.is_unknown("node"),
            "a key with a local value must not be believed unmeasured"
        );
        assert!(matches!(
            s.changes.iter().find(|c| c.key() == "node"),
            Some(Change::Differs { .. })
        ));
        let actionable: Vec<&str> = s.actionable().iter().map(|c| c.key()).collect();
        assert_eq!(
            actionable,
            vec!["node"],
            "a measured drift must stay actionable"
        );
        assert_eq!(plan.actionable_count(), 1);
        assert_eq!(plan.unmeasured_key_count(), 0);

        // Belt and braces: even with the intersection defeated — a SectionPlan
        // built by hand, as a future caller could — the filter itself must not
        // drop a `Differs`.
        let hand_built = SectionPlan {
            section: "versions".to_string(),
            policy: Applyable,
            changes: vec![Change::Differs {
                key: "node".to_string(),
                local: "v20.9.0".to_string(),
                canonical: "v22.1.0".to_string(),
            }],
            local_section_absent: false,
            derived_keys: BTreeSet::new(),
            unknown_keys: ["node".to_string()].into_iter().collect(),
        };
        assert_eq!(
            hand_built.actionable().len(),
            1,
            "the suppression must be scoped to Missing, not to every change kind"
        );

        // …and JSON never emits the contradictory pairing.
        let v = plan_to_json(&plan);
        let node = v["sections"][0]["changes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["key"] == "node")
            .unwrap()
            .clone();
        assert_eq!(node["kind"], "differs");
        assert_eq!(node["unknown"], false);
    }

    /// The two renderers must say the SAME thing about the same row.
    ///
    /// They did not: the text renderer marked any change whose key was in
    /// `unknown_keys`, while `plan_to_json` hardcoded `"unknown": false` on the
    /// `Differs`/`Extra` arms. On a hand-built `SectionPlan` — the shape the
    /// test right above this one already constructs — an operator reading the
    /// preview and a consumer reading `--json` got opposite answers about
    /// whether the value had been measured.
    #[test]
    fn both_renderers_agree_about_which_rows_are_unknown() {
        let hand_built = SectionPlan {
            section: "versions".to_string(),
            policy: Applyable,
            changes: vec![
                // Claimed unmeasured AND carrying a local value: contradictory,
                // and the case the two renderers used to split on.
                Change::Differs {
                    key: "node".to_string(),
                    local: "v20.9.0".to_string(),
                    canonical: "v22.1.0".to_string(),
                },
                Change::Extra {
                    key: "python".to_string(),
                    local: "3.13.0".to_string(),
                },
                // The honest unknown shape: absent locally.
                Change::Missing {
                    key: "rustc".to_string(),
                    canonical: "1.82.0".to_string(),
                },
            ],
            local_section_absent: false,
            derived_keys: BTreeSet::new(),
            unknown_keys: ["node", "python", "rustc"]
                .into_iter()
                .map(str::to_string)
                .collect(),
        };
        let plan = ApplyPlan {
            environment_id: "env-1".to_string(),
            canonical_machine_id: "other".to_string(),
            canonical_machine_name: None,
            captured_at: None,
            is_canonical_self: false,
            schema_mismatch: None,
            sections: vec![hand_built],
        };

        let text = render_plan(&plan);
        let json = plan_to_json(&plan);
        let row = |key: &str| {
            json["sections"][0]["changes"]
                .as_array()
                .unwrap()
                .iter()
                .find(|c| c["key"] == key)
                .unwrap()
                .clone()
        };

        // A row with a local value is never "unknown" — in EITHER renderer.
        assert_eq!(row("node")["unknown"], false);
        assert_eq!(row("python")["unknown"], false);
        for line in text
            .lines()
            .filter(|l| l.contains("node:") || l.contains("python:"))
        {
            assert!(
                !line.contains("[unknown"),
                "text renderer marked a row JSON calls measured: {line}"
            );
        }

        // …and the absent one IS, in both.
        assert_eq!(row("rustc")["unknown"], true);
        assert_eq!(row("rustc")["kind"], "unknown");
        assert!(
            text.lines()
                .any(|l| l.contains("rustc:") && l.contains("[unknown")),
            "text renderer dropped the mark JSON kept:\n{text}"
        );
    }

    /// "No changes are auto-applyable — everything above is report-only or needs
    /// a human decision" is FALSE when the count is zero because a key in an
    /// applyable section was never read. That is a measurement gap, not a policy
    /// one, and reads as "nothing needs doing".
    #[test]
    fn zero_actionable_because_unmeasured_does_not_read_as_nothing_to_do() {
        let plan = compute_plan(
            &canonical_with(
                json!({"versions": {"rustc": "rustc 1.82.0"}}),
                json!({"versions": "applyable"}),
            ),
            json!({"versions": {}}).as_object().unwrap(),
            &unknown("versions", &["rustc"]),
            "env-1",
            "other",
        );
        assert_eq!(plan.actionable_count(), 0);
        assert_eq!(plan.unmeasured_in_applyable_count(), 1);

        let text = render_plan(&plan);
        assert!(
            text.contains("NOT a statement that nothing needs doing"),
            "{text}"
        );
        assert!(
            !text.contains("everything above is report-only"),
            "the report-only wording is false here: {text}"
        );
    }

    /// A report-only section's unmeasured key is still unmeasured, but it is not
    /// a reason to qualify the APPLYABLE verdict — nothing there was going to be
    /// applied anyway.
    #[test]
    fn unmeasured_in_a_report_only_section_keeps_the_plain_wording() {
        let plan = compute_plan(
            &canonical_with(
                json!({"versions": {"rustc": "rustc 1.82.0"}}),
                json!({"versions": "report_only"}),
            ),
            json!({"versions": {}}).as_object().unwrap(),
            &unknown("versions", &["rustc"]),
            "env-1",
            "other",
        );
        assert_eq!(plan.unmeasured_in_applyable_count(), 0);
        let text = render_plan(&plan);
        assert!(text.contains("everything above is report-only"), "{text}");
        // …while the key itself is still surfaced.
        assert!(text.contains("could NOT be measured"), "{text}");
    }

    /// Non-string members degrade PER MEMBER, not wholesale — the readable
    /// entries survive an unrelated malformed sibling.
    #[test]
    fn string_list_degrades_per_member_not_wholesale() {
        assert_eq!(
            string_list(Some(&json!(["rustc", 7]))),
            ["rustc".to_string()].into_iter().collect::<BTreeSet<_>>(),
            "a legible member must survive a malformed sibling"
        );
        assert_eq!(
            string_list(Some(&json!(["rustc", null, "node", {"a": 1}]))),
            ["rustc".to_string(), "node".to_string()]
                .into_iter()
                .collect::<BTreeSet<_>>()
        );
        // Wholesale degradation is for the cases where there is no list to read.
        assert!(string_list(None).is_empty());
        assert!(string_list(Some(&json!(null))).is_empty());
        assert!(string_list(Some(&json!("rustc"))).is_empty());
        assert!(string_list(Some(&json!([7, false]))).is_empty());
    }

    /// An empty / absent / malformed local `unknown_keys` must behave EXACTLY
    /// like the pre-`unknown_keys` runner — a suppression that can be triggered
    /// by a malformed payload is a worse bug than the one it fixes.
    #[test]
    fn absent_or_malformed_unknown_keys_suppresses_nothing() {
        let canonical = || {
            canonical_with(
                json!({"versions": {"rustc": "rustc 1.82.0"}}),
                json!({"versions": "applyable"}),
            )
        };
        let local = json!({"versions": {}});

        for (label, unknown_map) in [
            ("absent", Map::new()),
            (
                "empty list",
                json!({"versions": []}).as_object().unwrap().clone(),
            ),
            ("wrong section", unknown("services", &["rustc"])),
            // Not an array.
            (
                "not a list",
                json!({"versions": "rustc"}).as_object().unwrap().clone(),
            ),
            // Non-string members.
            (
                "non-string members",
                json!({"versions": [7]}).as_object().unwrap().clone(),
            ),
        ] {
            let plan = compute_plan(
                &canonical(),
                local.as_object().unwrap(),
                &unknown_map,
                "env-1",
                "other",
            );
            let versions = plan
                .sections
                .iter()
                .find(|s| s.section == "versions")
                .unwrap();
            assert!(
                !versions.is_unknown("rustc"),
                "{label}: must not mark rustc unmeasured"
            );
            assert_eq!(
                plan.actionable_count(),
                1,
                "{label}: must stay actionable, exactly as before the field existed"
            );
        }
    }

    /// Unmeasured never becomes actionable by borrowing a policy either: the
    /// suppression sits inside `actionable()`, which already short-circuits on
    /// a non-`Applyable` policy, so the two rules compose rather than race.
    #[test]
    fn unknown_key_in_a_report_only_section_is_still_never_actionable() {
        let plan = compute_plan(
            &canonical_with(
                json!({"versions": {"rustc": "rustc 1.82.0"}}),
                json!({"versions": "report_only"}),
            ),
            json!({"versions": {}}).as_object().unwrap(),
            &unknown("versions", &["rustc"]),
            "env-1",
            "other",
        );
        assert_eq!(plan.sections[0].policy, ReportOnly);
        assert_eq!(plan.actionable_count(), 0);
        assert_eq!(plan.unknown_count(), 1);
    }

    // ------------------------------------------------------------------
    // env_contract process-scope caveat
    // ------------------------------------------------------------------

    /// The caveat renders when env_contract has changes — and the rows are NOT
    /// suppressed, because a genuinely missing secret must still show.
    #[test]
    fn env_contract_caveat_renders_when_it_has_changes() {
        let plan = plan_of(
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
        let plan = plan_of(
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
        let synced = plan_of(
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
