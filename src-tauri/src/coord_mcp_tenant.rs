//! Compare each coord answer's tenant with the tenant of the repo the session
//! is standing in, and SAY it when they differ.
//!
//! Plan `2026-09-20-a-sessions-tenant-follows-its-repo-and-every-coord-answer-names-its-tenant`
//! Phase 2. Coord stamps every `/mcp` `tools/call` result with the tenant that
//! answered it (`result._meta["io.qontinui/answered_by"]`, that plan's Phase
//! 1). The runner knows the OTHER half: the repo in the session's spawn
//! directory, and which tenant that repo belongs to
//! ([`qontinui_runner_lib::repo_tenant::CwdTenant`], frozen on the nonce at
//! mint). [`tenant_verdict`] compares the two on `tenant_id` — slugs are
//! mutable and display-only (D-D).
//!
//! ## Why the proxy rewrites the body at all (D-C)
//!
//! `_meta` is invisible to the model: Claude Code does not surface it. So a
//! disagreement that only lived in `_meta` would reach nobody. The coord-mcp
//! proxy therefore appends ONE extra `content` text block to the result —
//! and only when the verdict is not agreement. On `Agree` / `NoRepo` the
//! upstream bytes pass through byte-identical, as they always have.
//!
//! - `Mismatch` → a `TENANT MISMATCH` block on EVERY call: every such answer is
//!   another project's data.
//! - `ExpectedUnknown` / `AnswerUnstamped` → a `TENANT UNVERIFIED` block once per
//!   nonce (its first tool call) and on every `coord_query_identity` /
//!   `coord_orient` — the calls a session makes precisely to learn who it is.
//! - A body that does not parse, and a JSON-RPC `error` response, are never
//!   rewritten.
//!
//! ## The caller-named rule
//!
//! A session whose tenant was NAMED — the spawn picker / `--tenant` /
//! `provision-session {tenant}`, or a workspace declaration (tiers 1a/1b/1c of
//! `coord_mcp::decide_session_tenant`) — chose its tenant on purpose, e.g. a
//! cross-tenant steward working from inside one repo. Its expectation is the
//! NAMED tenant, not the repo's: an answer from the named tenant is agreement
//! by declaration (`source: caller_named`), and only an answer from some third
//! tenant is a mismatch. An explicit choice is not a silent lie.
//!
//! ## F2 — the verdict describes the SPAWN directory
//!
//! The expectation is resolved once, from the workdir the nonce was minted
//! for. A session that later `cd`s into another tenant's repo is not
//! re-resolved: the proxy cannot observe a child's cwd, and a session's tenant
//! is fixed at spawn. The verdict says which repo it compared against.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use qontinui_runner_lib::repo_tenant::CwdTenant;
use serde::Serialize;
use uuid::Uuid;

/// The `_meta` key coord stamps on every `/mcp` tool result.
pub(crate) const ANSWERED_BY_META_KEY: &str = "io.qontinui/answered_by";

/// The tools whose whole purpose is "who am I" — the UNVERIFIED notice is
/// repeated on each of them, not only on a nonce's first call.
const IDENTITY_TOOLS: &[&str] = &["coord_query_identity", "coord_orient"];

// ===========================================================================
// the two halves being compared
// ===========================================================================

/// What coord said about who answered (`_meta["io.qontinui/answered_by"]`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AnsweredBy {
    /// `None` when coord stamped the answer but its principal carries no
    /// tenant — not comparable, so it verdicts as [`TenantVerdict::AnswerUnstamped`].
    pub tenant_id: Option<Uuid>,
    /// Display only. `None` when coord could not look it up (`slug_state:
    /// "unknown"`).
    pub tenant_slug: Option<String>,
    pub principal_kind: Option<String>,
    pub observed_at: Option<String>,
}

impl AnsweredBy {
    /// Read the stamp off one tool RESULT object. `None` = no stamp at all (a
    /// coord that predates it). A stamp whose `tenant_id` is present but not a
    /// uuid is read as naming no tenant: a shape we cannot compare.
    pub(crate) fn from_result(result: &serde_json::Value) -> Option<Self> {
        let stamp = result.get("_meta")?.get(ANSWERED_BY_META_KEY)?;
        if !stamp.is_object() {
            return None;
        }
        let str_field = |k: &str| {
            stamp
                .get(k)
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from)
        };
        let slug_resolved = stamp.get("slug_state").and_then(|v| v.as_str()) != Some("unknown");
        Some(AnsweredBy {
            tenant_id: str_field("tenant_id").and_then(|t| Uuid::parse_str(&t).ok()),
            tenant_slug: str_field("tenant_slug").filter(|_| slug_resolved),
            principal_kind: str_field("principal_kind"),
            observed_at: str_field("observed_at"),
        })
    }

    /// `<slug> (<id>)`, or `<id> (<id>)` when the slug is unknown.
    fn describe(&self) -> String {
        match self.tenant_id {
            Some(id) => format!(
                "{} ({id})",
                self.tenant_slug.clone().unwrap_or_else(|| id.to_string())
            ),
            None => "<no tenant>".to_string(),
        }
    }
}

/// A session tenant somebody NAMED rather than inherited — see the module doc.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CallerNamed {
    pub tenant_id: Uuid,
    /// `spawn_tenant` (picker / `--tenant` / provision-session), or
    /// `declared: <tier>` for a workspace declaration.
    pub source: String,
}

/// The expectation a nonce carries: the cwd's tenant (resolved once, off the
/// hot path) and whether the session's tenant was caller-named.
///
/// The cell is SHARED between clones of the binding. The mint kicks off its
/// resolution in the background; the proxy's first `tools/call` awaits the
/// same cell (bounded by [`qontinui_runner_lib::repo_tenant::CWD_TENANT_BUDGET`]),
/// so the resolver runs exactly once per binding whoever gets there first.
#[derive(Debug, Clone, Default)]
pub(crate) struct SessionExpectation {
    cwd: Arc<tokio::sync::OnceCell<CwdTenant>>,
    caller_named: Option<CallerNamed>,
}

impl SessionExpectation {
    /// Not yet resolved; resolves on first use.
    pub(crate) fn pending(caller_named: Option<CallerNamed>) -> Self {
        Self {
            cwd: Arc::default(),
            caller_named,
        }
    }

    /// Already known — a restored binding.
    pub(crate) fn known(cwd: CwdTenant, caller_named: Option<CallerNamed>) -> Self {
        Self {
            cwd: Arc::new(tokio::sync::OnceCell::new_with(Some(cwd))),
            caller_named,
        }
    }

    /// The resolved expectation, or `None` while resolution has not finished.
    pub(crate) fn get(&self) -> Option<&CwdTenant> {
        self.cwd.get()
    }

    pub(crate) fn caller_named(&self) -> Option<&CallerNamed> {
        self.caller_named.as_ref()
    }

    /// Resolve (once) and return the expectation for `workdir`. `None` means
    /// the binding has no usable workdir, which is UNKNOWN — never `NoRepo`.
    pub(crate) async fn resolve(&self, workdir: Option<&str>) -> CwdTenant {
        let workdir = workdir.map(str::to_string);
        self.cwd
            .get_or_init(|| async move { resolve_workdir(workdir.as_deref()).await })
            .await
            .clone()
    }

    /// Start resolving in the background when a runtime is available (the mint
    /// path), then run `on_resolved` — the mint uses it to re-persist the nonce
    /// set, so the resolved expectation reaches the store instead of the
    /// still-empty cell the mint's own snapshot saw. Without a runtime (a sync
    /// unit test) nothing is spawned and the first proxy call resolves it.
    pub(crate) fn spawn_resolution(
        &self,
        workdir: Option<&str>,
        on_resolved: impl FnOnce() + Send + 'static,
    ) {
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            let this = self.clone();
            let workdir = workdir.map(str::to_string);
            handle.spawn(async move {
                this.resolve(workdir.as_deref()).await;
                on_resolved();
            });
        }
    }
}

async fn resolve_workdir(workdir: Option<&str>) -> CwdTenant {
    match workdir {
        Some(w) if std::path::Path::new(w).is_absolute() => {
            qontinui_runner_lib::repo_tenant::cwd_tenant_for_path(std::path::Path::new(w)).await
        }
        Some(w) => CwdTenant::Unknown {
            reason: format!("the session's workdir {w:?} is not an absolute path"),
        },
        None => CwdTenant::Unknown {
            reason: "the session's nonce records no workdir".to_string(),
        },
    }
}

// ===========================================================================
// the verdict
// ===========================================================================

/// What the answer was compared WITH, as the mismatch notice names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExpectedTenant {
    pub tenant_id: Uuid,
    /// `Some(repo)` when the expectation came from the cwd's repo.
    pub repo: Option<String>,
    /// `canonical_repos`, or `caller_named: <source>`.
    pub source: String,
}

/// Why agreement was reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AgreeSource {
    /// The answer's tenant is the cwd repo's.
    Repo,
    /// The answer's tenant is the one the caller named (see module doc).
    CallerNamed,
}

/// The comparison of one coord answer with the session's expectation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TenantVerdict {
    Agree {
        source: AgreeSource,
    },
    Mismatch {
        expected: ExpectedTenant,
        answered: AnsweredBy,
    },
    /// The expectation could not be established; `why` names it.
    ExpectedUnknown {
        why: String,
    },
    /// The answer carries no comparable stamp (coord predates it, the stamp
    /// names no tenant, or the body could not be read).
    AnswerUnstamped,
    /// The session's workdir is not inside a repo: nothing to expect.
    NoRepo,
}

impl TenantVerdict {
    /// The `verdict` label: the metric's series name and the wire value.
    pub(crate) fn label(&self) -> &'static str {
        match self {
            TenantVerdict::Agree { .. } => "agree",
            TenantVerdict::Mismatch { .. } => "mismatch",
            TenantVerdict::ExpectedUnknown { .. } => "expected_unknown",
            TenantVerdict::AnswerUnstamped => "answer_unstamped",
            TenantVerdict::NoRepo => "no_repo",
        }
    }

    fn index(&self) -> usize {
        match self {
            TenantVerdict::Agree { .. } => 0,
            TenantVerdict::Mismatch { .. } => 1,
            TenantVerdict::ExpectedUnknown { .. } => 2,
            TenantVerdict::AnswerUnstamped => 3,
            TenantVerdict::NoRepo => 4,
        }
    }
}

/// The comparison. Pure; compares on `tenant_id` only.
///
/// No input arm defaults to `Agree`: agreement requires a stamped answer AND a
/// known expectation (the repo's single owner, or the caller-named tenant)
/// naming the same tenant.
pub(crate) fn tenant_verdict(
    expected: &CwdTenant,
    answered: Option<&AnsweredBy>,
    caller_named: Option<&CallerNamed>,
) -> TenantVerdict {
    let answered_tenant = answered.and_then(|a| a.tenant_id.map(|t| (t, a)));

    if let Some(named) = caller_named {
        let Some((tenant, answer)) = answered_tenant else {
            return TenantVerdict::AnswerUnstamped;
        };
        return if tenant == named.tenant_id {
            TenantVerdict::Agree {
                source: AgreeSource::CallerNamed,
            }
        } else {
            TenantVerdict::Mismatch {
                expected: ExpectedTenant {
                    tenant_id: named.tenant_id,
                    repo: None,
                    source: format!("caller_named: {}", named.source),
                },
                answered: answer.clone(),
            }
        };
    }

    match expected {
        CwdTenant::NoRepo => TenantVerdict::NoRepo,
        CwdTenant::Unknown { reason } => TenantVerdict::ExpectedUnknown {
            why: reason.clone(),
        },
        CwdTenant::RepoUnregistered { repo } => TenantVerdict::ExpectedUnknown {
            why: format!("repo {repo} has no owning tenant in coord's registry"),
        },
        CwdTenant::Several { repo, tenant_ids } => TenantVerdict::ExpectedUnknown {
            why: format!(
                "repo {repo} is registered to {} tenants ({})",
                tenant_ids.len(),
                tenant_ids
                    .iter()
                    .map(Uuid::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        },
        CwdTenant::Resolved {
            tenant_id,
            repo,
            source,
            ..
        } => match answered_tenant {
            None => TenantVerdict::AnswerUnstamped,
            Some((t, _)) if t == *tenant_id => TenantVerdict::Agree {
                source: AgreeSource::Repo,
            },
            Some((_, answer)) => TenantVerdict::Mismatch {
                expected: ExpectedTenant {
                    tenant_id: *tenant_id,
                    repo: Some(repo.clone()),
                    source: source.clone(),
                },
                answered: answer.clone(),
            },
        },
    }
}

/// The text block appended for a verdict, or `None` for one that is silent.
pub(crate) fn verdict_notice(
    verdict: &TenantVerdict,
    answered: Option<&AnsweredBy>,
) -> Option<String> {
    let answered_label = || match answered {
        Some(a) => match (a.tenant_slug.as_deref(), a.tenant_id) {
            (Some(slug), _) => slug.to_string(),
            (None, Some(id)) => id.to_string(),
            (None, None) => "a principal with no tenant".to_string(),
        },
        None => "unstamped".to_string(),
    };
    match verdict {
        TenantVerdict::Agree { .. } | TenantVerdict::NoRepo => None,
        TenantVerdict::Mismatch { expected, answered } => {
            // The expected tenant's slug is not known here (the repo registry
            // serves ids); the answer's slug is used only when it names the
            // same tenant, which in a mismatch it never does.
            let expected_label = format!("{} ({})", expected.tenant_id, expected.tenant_id);
            Some(match &expected.repo {
                Some(repo) => format!(
                    "TENANT MISMATCH: this answer came from tenant {}. The repo in this \
                     session's working directory ({repo}) belongs to tenant {expected_label}. \
                     Treat this answer as another project's data.",
                    answered.describe()
                ),
                None => format!(
                    "TENANT MISMATCH: this answer came from tenant {}. This session's tenant \
                     was named explicitly ({}) as tenant {expected_label}. Treat this answer as \
                     another project's data.",
                    answered.describe(),
                    expected.source
                ),
            })
        }
        TenantVerdict::ExpectedUnknown { why } => Some(format!(
            "TENANT UNVERIFIED: answered by {}; the working directory's tenant could not be \
             determined ({why}).",
            answered_label()
        )),
        TenantVerdict::AnswerUnstamped => Some(format!(
            "TENANT UNVERIFIED: answered by {}; the answering tenant could not be compared \
             with the working directory's (coord's answer carries no comparable \
             answering-tenant stamp).",
            answered_label()
        )),
    }
}

// ===========================================================================
// the body rewrite
// ===========================================================================

/// The `tools/call` requests in a JSON-RPC request body, `id → tool name`.
/// Empty when the body is not JSON or carries no `tools/call`.
fn tools_calls_in(request: &[u8]) -> Vec<(serde_json::Value, String)> {
    let Ok(parsed) = serde_json::from_slice::<serde_json::Value>(request) else {
        return Vec::new();
    };
    let one = |v: &serde_json::Value| -> Option<(serde_json::Value, String)> {
        if v.get("method").and_then(|m| m.as_str()) != Some("tools/call") {
            return None;
        }
        let name = v
            .get("params")
            .and_then(|p| p.get("name"))
            .and_then(|n| n.as_str())
            .unwrap_or("")
            .to_string();
        Some((
            v.get("id").cloned().unwrap_or(serde_json::Value::Null),
            name,
        ))
    };
    match &parsed {
        serde_json::Value::Array(elems) => elems.iter().filter_map(one).collect(),
        other => one(other).into_iter().collect(),
    }
}

/// One verdict the rewrite reached, with the stamp it was reached on.
#[derive(Debug, Clone)]
pub(crate) struct ObservedVerdict {
    pub verdict: TenantVerdict,
    pub answered: Option<AnsweredBy>,
}

/// Apply the verdict to one response body.
///
/// `claim_first_notice` answers "may the once-per-nonce UNVERIFIED notice be
/// appended now?" and marks it spent; it is asked at most once per tool
/// response that needs it.
///
/// Returns the body to forward — the SAME `Bytes` (no re-serialisation) unless
/// a block was appended — and every verdict reached, for metering. A request
/// with no `tools/call` returns no verdicts and the body untouched.
pub(crate) fn apply_tenant_verdict(
    request: &[u8],
    response: bytes::Bytes,
    expected: &CwdTenant,
    caller_named: Option<&CallerNamed>,
    claim_first_notice: &mut dyn FnMut() -> bool,
) -> (bytes::Bytes, Vec<ObservedVerdict>) {
    let calls = tools_calls_in(request);
    if calls.is_empty() {
        return (response, Vec::new());
    }
    let unreadable = || {
        calls
            .iter()
            .map(|_| ObservedVerdict {
                verdict: TenantVerdict::AnswerUnstamped,
                answered: None,
            })
            .collect::<Vec<_>>()
    };
    let Ok(mut parsed) = serde_json::from_slice::<serde_json::Value>(&response) else {
        // Not JSON (an SSE frame, a truncated body): forwarded verbatim and
        // counted as unstamped — nothing about the answer could be read.
        return (response, unreadable());
    };

    let mut verdicts = Vec::new();
    let mut changed = false;
    let mut handle_one = |obj: &mut serde_json::Value, tool: &str| {
        // A JSON-RPC error carries no result to stamp or to append to.
        if obj.get("error").is_some() {
            return;
        }
        let Some(result) = obj.get_mut("result") else {
            return;
        };
        let answered = AnsweredBy::from_result(result);
        let verdict = tenant_verdict(expected, answered.as_ref(), caller_named);
        // A result with no `content` array has nowhere to carry a notice, so
        // it must not spend the once-per-nonce one either.
        let appendable = result.get("content").is_some_and(|c| c.is_array());
        let append = appendable
            && match &verdict {
                TenantVerdict::Mismatch { .. } => true,
                TenantVerdict::ExpectedUnknown { .. } | TenantVerdict::AnswerUnstamped => {
                    // Claim first, so the first call spends the once-per-nonce
                    // notice even when it is an identity tool.
                    let first = claim_first_notice();
                    first || IDENTITY_TOOLS.contains(&tool)
                }
                TenantVerdict::Agree { .. } | TenantVerdict::NoRepo => false,
            };
        if append {
            if let (Some(text), Some(content)) = (
                verdict_notice(&verdict, answered.as_ref()),
                result.get_mut("content").and_then(|c| c.as_array_mut()),
            ) {
                content.push(serde_json::json!({ "type": "text", "text": text }));
                changed = true;
            }
        }
        verdicts.push(ObservedVerdict { verdict, answered });
    };

    match &mut parsed {
        serde_json::Value::Array(elems) => {
            for elem in elems.iter_mut() {
                let id = elem.get("id").cloned().unwrap_or(serde_json::Value::Null);
                if let Some((_, tool)) = calls.iter().find(|(cid, _)| *cid == id) {
                    let tool = tool.clone();
                    handle_one(elem, &tool);
                }
            }
        }
        obj => {
            let tool = calls[0].1.clone();
            handle_one(obj, &tool);
        }
    }

    if !changed {
        return (response, verdicts);
    }
    match serde_json::to_vec(&parsed) {
        Ok(v) => (bytes::Bytes::from(v), verdicts),
        // Cannot happen for a value that parsed; forward verbatim rather than
        // fail a coord answer over a notice.
        Err(_) => (response, verdicts),
    }
}

// ===========================================================================
// metering + per-nonce state
// ===========================================================================

/// `coord_mcp_tenant_verdict_total{verdict}` — per process since boot, served
/// on `GET /health` as `coordMcpTenantVerdict`. A series at 0 is "observed
/// none"; a runner build that serves no such key is UNKNOWN.
fn verdict_counters() -> &'static [AtomicU64; 5] {
    static COUNTERS: OnceLock<[AtomicU64; 5]> = OnceLock::new();
    COUNTERS.get_or_init(Default::default)
}

const VERDICT_LABELS: [&str; 5] = [
    "agree",
    "mismatch",
    "expected_unknown",
    "answer_unstamped",
    "no_repo",
];

/// The latest verdict one nonce's answers reached.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct VerdictRecord {
    pub verdict: String,
    /// The notice-free explanation: the mismatch's two tenants, or the why.
    pub detail: Option<String>,
    pub answered_tenant_id: Option<String>,
    pub answered_tenant_slug: Option<String>,
    pub observed_at: String,
}

#[derive(Debug, Default)]
struct NonceTenantState {
    unverified_notice_sent: bool,
    latest: Option<VerdictRecord>,
    touched: u64,
}

/// Bound on remembered nonces — the live nonce set is itself bounded, and a
/// dead nonce's state is worth nothing, so the least-recently-touched go first.
const NONCE_STATE_CAP: usize = 2048;

fn nonce_states() -> &'static Mutex<(u64, HashMap<String, NonceTenantState>)> {
    static STATES: OnceLock<Mutex<(u64, HashMap<String, NonceTenantState>)>> = OnceLock::new();
    STATES.get_or_init(|| Mutex::new((0, HashMap::new())))
}

fn with_state<R>(nonce: &str, f: impl FnOnce(&mut NonceTenantState) -> R) -> R {
    let mut guard = nonce_states().lock().unwrap_or_else(|e| e.into_inner());
    let (clock, map) = &mut *guard;
    *clock += 1;
    let now = *clock;
    if !map.contains_key(nonce) && map.len() >= NONCE_STATE_CAP {
        if let Some(oldest) = map
            .iter()
            .min_by_key(|(_, s)| s.touched)
            .map(|(k, _)| k.clone())
        {
            map.remove(&oldest);
        }
    }
    let state = map.entry(nonce.to_string()).or_default();
    state.touched = now;
    f(state)
}

/// Spend `nonce`'s once-per-nonce UNVERIFIED notice: `true` the first time.
pub(crate) fn claim_unverified_notice(nonce: &str) -> bool {
    with_state(nonce, |s| {
        !std::mem::replace(&mut s.unverified_notice_sent, true)
    })
}

/// Meter each verdict and remember the last one for `nonce`.
pub(crate) fn record_verdicts(nonce: Option<&str>, verdicts: &[ObservedVerdict]) {
    for v in verdicts {
        verdict_counters()[v.verdict.index()].fetch_add(1, Ordering::Relaxed);
    }
    let (Some(nonce), Some(last)) = (nonce, verdicts.last()) else {
        return;
    };
    let record = VerdictRecord {
        verdict: last.verdict.label().to_string(),
        detail: match &last.verdict {
            TenantVerdict::Mismatch { expected, answered } => Some(format!(
                "answered by {}, expected {} ({})",
                answered.describe(),
                expected.tenant_id,
                expected.source
            )),
            TenantVerdict::ExpectedUnknown { why } => Some(why.clone()),
            _ => None,
        },
        answered_tenant_id: last
            .answered
            .as_ref()
            .and_then(|a| a.tenant_id)
            .map(|t| t.to_string()),
        answered_tenant_slug: last.answered.as_ref().and_then(|a| a.tenant_slug.clone()),
        observed_at: chrono::Utc::now().to_rfc3339(),
    };
    with_state(nonce, |s| s.latest = Some(record));
}

/// The latest verdict recorded for `nonce`, if any call has been compared.
pub(crate) fn latest_verdict(nonce: &str) -> Option<VerdictRecord> {
    nonce_states()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .1
        .get(nonce)
        .and_then(|s| s.latest.clone())
}

/// `GET /health` `coordMcpTenantVerdict`.
pub(crate) fn verdict_counts_json() -> serde_json::Value {
    let counts: serde_json::Map<String, serde_json::Value> = VERDICT_LABELS
        .iter()
        .enumerate()
        .map(|(i, label)| {
            (
                label.to_string(),
                serde_json::Value::from(verdict_counters()[i].load(Ordering::Relaxed)),
            )
        })
        .collect();
    serde_json::json!({ "coord_mcp_tenant_verdict_total": counts })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(n: u8) -> Uuid {
        Uuid::from_bytes([n; 16])
    }

    fn answered(tenant: Option<Uuid>, slug: Option<&str>) -> AnsweredBy {
        AnsweredBy {
            tenant_id: tenant,
            tenant_slug: slug.map(String::from),
            principal_kind: Some("device".into()),
            observed_at: Some("2026-09-27T00:00:00Z".into()),
        }
    }

    fn resolved(tenant: Uuid) -> CwdTenant {
        CwdTenant::Resolved {
            tenant_id: tenant,
            repo: "acme/pizzeria".into(),
            source: "canonical_repos".into(),
            observed_at: "t0".into(),
        }
    }

    /// Every expectation arm × every answer arm × caller-named or not — and no
    /// combination reaches `Agree` unless a stamped answer names the expected
    /// (or named) tenant.
    #[test]
    fn tenant_verdict_covers_every_arm_combination() {
        let (a, b, c) = (t(0xA1), t(0xB2), t(0xC3));
        let expectations = [
            ("resolved_a", resolved(a)),
            ("no_repo", CwdTenant::NoRepo),
            (
                "unregistered",
                CwdTenant::RepoUnregistered {
                    repo: "acme/x".into(),
                },
            ),
            (
                "several",
                CwdTenant::Several {
                    repo: "acme/shared".into(),
                    tenant_ids: vec![a, b],
                },
            ),
            (
                "unknown",
                CwdTenant::Unknown {
                    reason: "coord unreachable".into(),
                },
            ),
        ];
        let answers = [
            ("stamped_a", Some(answered(Some(a), Some("pizzeria")))),
            ("stamped_b", Some(answered(Some(b), Some("qontinui")))),
            ("stamped_no_tenant", Some(answered(None, None))),
            ("unstamped", None),
        ];
        let named = [
            ("not_named", None),
            (
                "named_a",
                Some(CallerNamed {
                    tenant_id: a,
                    source: "spawn_tenant".into(),
                }),
            ),
            (
                "named_c",
                Some(CallerNamed {
                    tenant_id: c,
                    source: "declared: $QONTINUI_TENANT_ID".into(),
                }),
            ),
        ];
        for (en, e) in &expectations {
            for (an, ans) in &answers {
                for (nn, n) in &named {
                    let v = tenant_verdict(e, ans.as_ref(), n.as_ref());
                    let case = format!("{en} × {an} × {nn} → {v:?}");
                    let answer_tenant = ans.as_ref().and_then(|x| x.tenant_id);
                    let expected: TenantVerdict = match (n, answer_tenant) {
                        (Some(_), None) => TenantVerdict::AnswerUnstamped,
                        (Some(named), Some(at)) if at == named.tenant_id => TenantVerdict::Agree {
                            source: AgreeSource::CallerNamed,
                        },
                        (Some(_), Some(_)) => {
                            assert!(matches!(v, TenantVerdict::Mismatch { .. }), "{case}");
                            continue;
                        }
                        (None, _) => match (e, answer_tenant) {
                            (CwdTenant::NoRepo, _) => TenantVerdict::NoRepo,
                            (CwdTenant::Resolved { .. }, None) => TenantVerdict::AnswerUnstamped,
                            (CwdTenant::Resolved { tenant_id, .. }, Some(at))
                                if at == *tenant_id =>
                            {
                                TenantVerdict::Agree {
                                    source: AgreeSource::Repo,
                                }
                            }
                            (CwdTenant::Resolved { .. }, Some(_)) => {
                                assert!(matches!(v, TenantVerdict::Mismatch { .. }), "{case}");
                                continue;
                            }
                            _ => {
                                assert!(
                                    matches!(v, TenantVerdict::ExpectedUnknown { .. }),
                                    "{case}"
                                );
                                continue;
                            }
                        },
                    };
                    assert_eq!(v, expected, "{case}");
                    if matches!(v, TenantVerdict::Agree { .. }) {
                        assert!(answer_tenant.is_some(), "Agree without a stamp: {case}");
                    }
                }
            }
        }
    }

    #[test]
    fn several_and_unregistered_name_their_why() {
        let v = tenant_verdict(
            &CwdTenant::Several {
                repo: "acme/shared".into(),
                tenant_ids: vec![t(1), t(2)],
            },
            Some(&answered(Some(t(1)), None)),
            None,
        );
        assert!(
            matches!(&v, TenantVerdict::ExpectedUnknown { why } if why.contains("2 tenants")),
            "{v:?}"
        );
        let v = tenant_verdict(
            &CwdTenant::RepoUnregistered {
                repo: "acme/x".into(),
            },
            None,
            None,
        );
        assert!(
            matches!(&v, TenantVerdict::ExpectedUnknown { why } if why.contains("acme/x")),
            "{v:?}"
        );
    }

    #[test]
    fn the_stamp_is_read_off_meta_and_a_bad_tenant_id_is_not_comparable() {
        let a = t(0xA1);
        let r = serde_json::json!({"content": [], "_meta": {ANSWERED_BY_META_KEY: {
            "tenant_id": a.to_string(), "tenant_slug": "pizzeria", "slug_state": "resolved",
            "principal_kind": "device", "observed_at": "x"}}});
        let got = AnsweredBy::from_result(&r).unwrap();
        assert_eq!(got.tenant_id, Some(a));
        assert_eq!(got.tenant_slug.as_deref(), Some("pizzeria"));
        // slug_state unknown: the slug is not presented as current.
        let r = serde_json::json!({"_meta": {ANSWERED_BY_META_KEY: {
            "tenant_id": a.to_string(), "tenant_slug": "stale", "slug_state": "unknown"}}});
        assert_eq!(AnsweredBy::from_result(&r).unwrap().tenant_slug, None);
        let r = serde_json::json!({"_meta": {ANSWERED_BY_META_KEY: {"tenant_id": "nope"}}});
        assert_eq!(AnsweredBy::from_result(&r).unwrap().tenant_id, None);
        assert_eq!(
            AnsweredBy::from_result(&serde_json::json!({"content": []})),
            None
        );
    }

    // ---- the body rewrite, driven through a stub upstream -----------------

    fn call(tool: &str) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "jsonrpc": "2.0", "id": 7, "method": "tools/call",
            "params": {"name": tool, "arguments": {}}
        }))
        .unwrap()
    }

    fn stamped_body(tenant: Uuid, slug: &str) -> String {
        // Deliberately NOT serde_json's canonical spacing, so a
        // re-serialisation on the pass-through arm would change the bytes.
        format!(
            "{{\"jsonrpc\":\"2.0\", \"id\":7,\"result\":{{\"content\":[{{\"type\":\"text\",\"text\":\"ok\"}}],\
             \"isError\":false,\"_meta\":{{\"{ANSWERED_BY_META_KEY}\":{{\"tenant_id\":\"{tenant}\",\
             \"tenant_slug\":\"{slug}\",\"slug_state\":\"resolved\",\"principal_kind\":\"device\",\
             \"observed_at\":\"2026-09-27T00:00:00Z\"}}}}}}}}"
        )
    }

    /// A minimal stand-in for coord's `/mcp`: answers every POST with the
    /// configured body, byte for byte.
    async fn stub_upstream(body: String) -> String {
        use axum::routing::post;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = axum::Router::new().route(
            "/mcp",
            post(move || {
                let body = body.clone();
                async move {
                    (
                        [(axum::http::header::CONTENT_TYPE, "application/json")],
                        body,
                    )
                }
            }),
        );
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        format!("http://{addr}/mcp")
    }

    async fn fetch(url: &str, request: &[u8]) -> bytes::Bytes {
        reqwest::Client::new()
            .post(url)
            .body(request.to_vec())
            .send()
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap()
    }

    fn texts(body: &[u8]) -> Vec<String> {
        let v: serde_json::Value = serde_json::from_slice(body).unwrap();
        v["result"]["content"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|c| c["text"].as_str().map(String::from))
            .collect()
    }

    #[tokio::test]
    async fn agree_passes_the_upstream_body_through_byte_identical() {
        let a = t(0xA1);
        let url = stub_upstream(stamped_body(a, "pizzeria")).await;
        let req = call("coord_orient");
        let upstream = fetch(&url, &req).await;
        let mut never = || -> bool { panic!("an agreeing answer must not ask for a notice") };
        let (out, verdicts) =
            apply_tenant_verdict(&req, upstream.clone(), &resolved(a), None, &mut never);
        assert_eq!(
            out, upstream,
            "Agree must forward the upstream bytes verbatim"
        );
        assert_eq!(
            out.as_ptr(),
            upstream.as_ptr(),
            "…without re-serialising them"
        );
        assert_eq!(verdicts.len(), 1);
        assert_eq!(verdicts[0].verdict.label(), "agree");

        // NoRepo is silent too.
        let (out, _) =
            apply_tenant_verdict(&req, upstream.clone(), &CwdTenant::NoRepo, None, &mut never);
        assert_eq!(out, upstream);
    }

    #[tokio::test]
    async fn mismatch_appends_one_block_on_every_call() {
        let (a, b) = (t(0xA1), t(0xB2));
        let url = stub_upstream(stamped_body(a, "pizzeria")).await;
        let mut first = true;
        let mut notice = || std::mem::replace(&mut first, false);
        for tool in ["coord_memory_search", "coord_inbox", "coord_memory_search"] {
            let req = call(tool);
            let upstream = fetch(&url, &req).await;
            let (out, verdicts) =
                apply_tenant_verdict(&req, upstream.clone(), &resolved(b), None, &mut notice);
            let tx = texts(&out);
            assert_eq!(tx.len(), 2, "exactly ONE block appended on {tool}: {tx:?}");
            assert_eq!(tx[0], "ok", "the original content is untouched");
            assert!(tx[1].starts_with("TENANT MISMATCH: this answer came from tenant pizzeria ("));
            assert!(tx[1].contains("(acme/pizzeria) belongs to tenant"));
            assert!(tx[1].contains(&b.to_string()));
            assert!(tx[1].ends_with("Treat this answer as another project's data."));
            assert_eq!(verdicts[0].verdict.label(), "mismatch");
        }
    }

    #[tokio::test]
    async fn unverified_is_appended_once_per_nonce_and_on_identity_tools() {
        let a = t(0xA1);
        let url = stub_upstream(stamped_body(a, "pizzeria")).await;
        let unknown = CwdTenant::Unknown {
            reason: "coord repo registry unreadable: 503".into(),
        };
        let nonce = format!("test-nonce-{}", Uuid::new_v4());
        let mut claim = || claim_unverified_notice(&nonce);
        let mut appended = Vec::new();
        for tool in [
            "coord_memory_search",
            "coord_memory_search",
            "coord_orient",
            "coord_inbox",
            "coord_query_identity",
        ] {
            let req = call(tool);
            let upstream = fetch(&url, &req).await;
            let (out, verdicts) =
                apply_tenant_verdict(&req, upstream.clone(), &unknown, None, &mut claim);
            assert_eq!(verdicts[0].verdict.label(), "expected_unknown");
            let tx = texts(&out);
            if tx.len() == 2 {
                assert!(tx[1].starts_with("TENANT UNVERIFIED: answered by pizzeria;"));
                assert!(tx[1].contains("503"));
            } else {
                assert_eq!(out, upstream, "no notice ⇒ verbatim bytes");
            }
            appended.push(tx.len() == 2);
        }
        assert_eq!(appended, vec![true, false, true, false, true]);
    }

    #[tokio::test]
    async fn an_unstamped_answer_is_unverified_and_says_unstamped() {
        let url = stub_upstream(
            r#"{"jsonrpc":"2.0","id":7,"result":{"content":[{"type":"text","text":"ok"}]}}"#
                .to_string(),
        )
        .await;
        let req = call("coord_orient");
        let upstream = fetch(&url, &req).await;
        let mut yes = || true;
        let (out, verdicts) = apply_tenant_verdict(&req, upstream, &resolved(t(1)), None, &mut yes);
        assert_eq!(verdicts[0].verdict.label(), "answer_unstamped");
        assert!(texts(&out)[1].starts_with("TENANT UNVERIFIED: answered by unstamped;"));
    }

    #[tokio::test]
    async fn an_unparseable_body_is_forwarded_verbatim_and_counted_unstamped() {
        let raw = "event: message\ndata: {not json".to_string();
        let url = stub_upstream(raw.clone()).await;
        let req = call("coord_orient");
        let upstream = fetch(&url, &req).await;
        let mut never = || -> bool { panic!("nothing is appended to a body that did not parse") };
        let (out, verdicts) =
            apply_tenant_verdict(&req, upstream.clone(), &resolved(t(1)), None, &mut never);
        assert_eq!(out, upstream);
        assert_eq!(&out[..], raw.as_bytes());
        assert_eq!(verdicts.len(), 1);
        assert_eq!(verdicts[0].verdict.label(), "answer_unstamped");
    }

    #[tokio::test]
    async fn a_jsonrpc_error_is_never_rewritten() {
        let raw = r#"{"jsonrpc":"2.0","id":7,"error":{"code":-32602,"message":"bad"}}"#;
        let url = stub_upstream(raw.to_string()).await;
        let req = call("coord_orient");
        let upstream = fetch(&url, &req).await;
        let mut never = || -> bool { panic!("an error response is never annotated") };
        let (out, verdicts) =
            apply_tenant_verdict(&req, upstream.clone(), &resolved(t(1)), None, &mut never);
        assert_eq!(out, upstream);
        assert!(verdicts.is_empty());
    }

    #[test]
    fn a_request_without_tools_call_is_not_touched() {
        let req = br#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#;
        let body = bytes::Bytes::from_static(b"{\"result\":{\"tools\":[]}}");
        let mut never = || -> bool { panic!("not a tools/call") };
        let (out, verdicts) =
            apply_tenant_verdict(req, body.clone(), &resolved(t(1)), None, &mut never);
        assert_eq!(out, body);
        assert!(verdicts.is_empty());
    }

    #[test]
    fn a_caller_named_session_agrees_with_its_named_tenant_despite_the_repo() {
        let (named, repo_owner) = (t(0xC3), t(0xA1));
        let req = call("coord_orient");
        let body = bytes::Bytes::from(stamped_body(named, "steward"));
        let cn = CallerNamed {
            tenant_id: named,
            source: "spawn_tenant".into(),
        };
        let mut never = || -> bool { panic!("agreement by declaration is silent") };
        let (out, verdicts) = apply_tenant_verdict(
            &req,
            body.clone(),
            &resolved(repo_owner),
            Some(&cn),
            &mut never,
        );
        assert_eq!(out, body);
        assert_eq!(
            verdicts[0].verdict,
            TenantVerdict::Agree {
                source: AgreeSource::CallerNamed
            }
        );
    }

    #[test]
    fn a_batch_is_annotated_per_element() {
        let (a, b) = (t(0xA1), t(0xB2));
        let req = serde_json::to_vec(&serde_json::json!([
            {"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"coord_inbox"}},
            {"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"coord_orient"}},
        ]))
        .unwrap();
        let stamp =
            |t: Uuid| serde_json::json!({ANSWERED_BY_META_KEY: {"tenant_id": t.to_string()}});
        let body = serde_json::to_vec(&serde_json::json!([
            {"jsonrpc":"2.0","id":1,"result":{"content":[],"_meta":stamp(b)}},
            {"jsonrpc":"2.0","id":2,"result":{"content":[],"_meta":stamp(a)}},
        ]))
        .unwrap();
        let mut never = || false;
        let (out, verdicts) =
            apply_tenant_verdict(&req, body.into(), &resolved(a), None, &mut never);
        let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v[0]["result"]["content"].as_array().unwrap().len(), 1);
        assert_eq!(v[1]["result"]["content"].as_array().unwrap().len(), 0);
        assert_eq!(verdicts.len(), 2);
    }

    #[test]
    fn verdicts_are_metered_and_the_latest_is_remembered_per_nonce() {
        let nonce = format!("test-nonce-{}", Uuid::new_v4());
        let before = verdict_counters()[1].load(Ordering::Relaxed);
        let v = ObservedVerdict {
            verdict: TenantVerdict::Mismatch {
                expected: ExpectedTenant {
                    tenant_id: t(2),
                    repo: Some("acme/x".into()),
                    source: "canonical_repos".into(),
                },
                answered: answered(Some(t(1)), Some("pizzeria")),
            },
            answered: Some(answered(Some(t(1)), Some("pizzeria"))),
        };
        record_verdicts(Some(&nonce), &[v]);
        assert!(verdict_counters()[1].load(Ordering::Relaxed) > before);
        let rec = latest_verdict(&nonce).unwrap();
        assert_eq!(rec.verdict, "mismatch");
        assert_eq!(rec.answered_tenant_slug.as_deref(), Some("pizzeria"));
        let json = verdict_counts_json();
        for label in VERDICT_LABELS {
            assert!(
                json["coord_mcp_tenant_verdict_total"][label].is_u64(),
                "{label}"
            );
        }
    }

    #[tokio::test]
    async fn the_expectation_resolves_once_and_a_missing_workdir_is_unknown() {
        let e = SessionExpectation::pending(None);
        assert!(e.get().is_none());
        let got = e.resolve(None).await;
        assert!(matches!(got, CwdTenant::Unknown { .. }));
        // Resolved once: a later call with a real dir does not re-resolve.
        let again = e.resolve(Some("/")).await;
        assert_eq!(again, got);
        let rel = SessionExpectation::default()
            .resolve(Some("relative/dir"))
            .await;
        assert!(matches!(rel, CwdTenant::Unknown { .. }), "{rel:?}");
    }
}
