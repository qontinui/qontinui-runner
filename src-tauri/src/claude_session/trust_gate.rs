//! **Derive** workspace trust for a spawn target instead of minting it, and put
//! how strictly that derivation is enforced on the tenant's autonomy dial.
//!
//! Plan `2026-08-20-worktree-spawn-autonomy-and-trust-preconditions`, Phases 2
//! and 4. Phase 1 ([`super::spawn_preconditions`]) made the trust state
//! *nameable*; this module is where the verdict is finally **acted on**.
//!
//! ## The control being protected
//!
//! `hasTrustDialogAccepted` exists to make a human vouch for a directory before
//! that directory's `.claude/settings.json` hooks and `.mcp.json` servers are
//! allowed to auto-execute. [`super::workspace_trust`] writes that flag. Its
//! landed behaviour returns [`TrustOutcome::Trusted`] precisely when the flag
//! was *absent or false* — i.e. its normal path **creates** trust for a
//! directory nobody vouched for. That is ambient trust, and the plan names it as
//! the top risk:
//!
//! > if the three conjuncts ever get relaxed into "trust the path we are about
//! > to use", the control is gone.
//!
//! ## The three conjuncts
//!
//! A trust write is *derived* only when all three hold. Each removes one way the
//! write could become ambient:
//!
//! 1. **[`Conjunct::CoordWorktreeRow`]** — the target is a coord-allocated
//!    worktree, i.e. coord's own resource rather than an arbitrary path.
//! 2. **[`Conjunct::ParentRepoTrusted`]** — the worktree's parent repo ALREADY
//!    reads trusted for that account. Trust is **inherited, never created**.
//! 3. **[`Conjunct::ConfigDirPinned`]** — the write targets the exact
//!    `CLAUDE_CONFIG_DIR` the spawn will use, so the log cannot claim trust for
//!    an account the child never runs under.
//!
//! Conjunct 2 is the load-bearing one. It is what makes this a *derivation*: the
//! only trust this module can produce is trust a human already granted to the
//! enclosing repo, projected onto a worktree cut from it.
//!
//! ## Nothing is gated when nothing is minted
//!
//! [`decide`] short-circuits on [`TrustVerdict::Trusted`]. A spawn into a
//! directory that is *already* trusted mints nothing, so there is no derivation
//! to justify and nothing to block — in every dial position. This is what keeps
//! the strict arm safe: ordinary spawns into the workspace root, a canonical
//! checkout, or any repo an operator has vouched for are untouched. Only a
//! **mint** passes through the gate.
//!
//! ## Absence is UNKNOWN, never satisfied
//!
//! Every conjunct that cannot be evaluated returns [`ConjunctVerdict::Unknown`]
//! with a reason. An UNKNOWN conjunct is **not** a pass: [`Conjuncts::failing`]
//! counts it alongside an outright failure, because a conjunct that could not
//! run has not removed the way-to-ambient-trust it exists to remove.
//!
//! ## Phase 4 — the dial decides how strictly
//!
//! The strength of the gate is the tenant's `implement_tier`, read from coord's
//! `policy/security-and-autonomy` prompt document. See [`AutonomyTier`] and
//! [`posture_for`] for the mapping, and [`DialResolution`] for what each
//! degraded read yields. Shape deliberately mirrors
//! [`crate::agent_authorization`]: a TTL'd snapshot with a last-known-good
//! fallback, a pure decision core with no HTTP/clock/globals, and a stable
//! `rule` string on every verdict.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use once_cell::sync::Lazy;
use serde::Serialize;
use tracing::{debug, info, warn};

use super::spawn_preconditions::TrustVerdict;
use super::workspace_trust::{self, TrustOutcome};

// =============================================================================
// Phase 4 — the autonomy dial
// =============================================================================

/// The coord prompt document carrying the dial, and the clause key inside it.
///
/// Mirrors `qontinui-coord`'s `prompt_documents::AUTONOMY_DIAL_DOCUMENT` /
/// `AUTONOMY_DIAL_KEY`. The duplication is across a repo boundary: coord parses
/// the dial for the MCP `initialize` preamble it sends to a *Claude session*,
/// and the runner is a different consumer, so it reads the document body itself.
///
/// The parser below is a deliberate line-for-line mirror of coord's
/// `parse_autonomy_dial` so the two cannot disagree about what the tenant said.
/// **That is the whole justification, and it stands on its own** — a mirror is
/// the right shape whether or not some other surface also parses the dial.
///
/// An earlier revision of this comment additionally claimed coord "serves no
/// parsed form on any HTTP route". That was a source read of
/// `prompt_documents.rs`, not a route census, and a `coord-route-census.sh`
/// run on 2026-09-06 returned `routes=UNKNOWN` (a source could not be read
/// completely), which settles the question in neither direction. The claim is
/// withdrawn rather than restated: it was never load-bearing for the decision
/// above, and an unverified capability negative in a comment outlives the
/// session that guessed it.
const DIAL_DOCUMENT: &str = "/coord/agent-prompt-documents/policy/security-and-autonomy";
const DIAL_KEY: &str = "implement_tier:";

/// The autonomy tiers, TIGHTEST first — the same vocabulary and the same
/// ordering coord pins in `AUTONOMY_TIERS_TIGHTEST_FIRST`. A value outside this
/// set is [`DialParse::Unrecognised`], never coerced to a neighbour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum AutonomyTier {
    AskFirst,
    DraftRequired,
    Proceed,
}

impl AutonomyTier {
    pub fn as_wire(self) -> &'static str {
        match self {
            AutonomyTier::AskFirst => "ask-first",
            AutonomyTier::DraftRequired => "draft-required",
            AutonomyTier::Proceed => "proceed",
        }
    }

    fn from_wire(s: &str) -> Option<Self> {
        match s {
            "ask-first" => Some(AutonomyTier::AskFirst),
            "draft-required" => Some(AutonomyTier::DraftRequired),
            "proceed" => Some(AutonomyTier::Proceed),
            _ => None,
        }
    }
}

/// The three distinct outcomes of reading `implement_tier` out of a body —
/// coord's [`DialParse`] shape, kept distinct for the same reason: collapsing
/// "no dial line", "an empty value" and "a value we do not recognise" into one
/// `None` turns a suppressed parse failure into a confident verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DialParse {
    Present(AutonomyTier),
    /// An `implement_tier:` line EXISTS but its value is not a known tier.
    /// Carries the raw value (empty string when the line had no value).
    Unrecognised(String),
    /// No `implement_tier:` line anywhere in the body.
    Absent,
}

/// Extract `implement_tier` from a `security-and-autonomy` body.
///
/// Line-based, not a regex, for coord's reason: the clause is authored as an
/// indented `implement_tier: <value>` line, and the surrounding prose names all
/// three tiers, so a looser parser would start "finding" tiers in the prose.
pub fn parse_autonomy_dial(body: &str) -> DialParse {
    for line in body.lines() {
        let Some(rest) = line.trim().strip_prefix(DIAL_KEY) else {
            continue;
        };
        // Only the FIRST token: the prose after the clause discusses the other
        // tiers, and a greedy read would splice them into the value.
        let value = rest.trim().split_whitespace().next().unwrap_or_default();
        let value = value.trim_matches(|c: char| c == '`' || c == '"' || c == '\'');
        if value.is_empty() {
            return DialParse::Unrecognised(String::new());
        }
        return match AutonomyTier::from_wire(value) {
            Some(t) => DialParse::Present(t),
            None => DialParse::Unrecognised(value.to_string()),
        };
    }
    DialParse::Absent
}

/// What the dial resolver managed to obtain. Every arm that is not a tier says
/// WHY, and the two "no tenant" / "read failed" cases are deliberately NOT
/// collapsed — that is the absence-is-not-zero error this whole plan is about.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "dial", rename_all = "snake_case")]
pub enum DialResolution {
    /// A snapshot within [`DIAL_TTL`].
    Fresh { tier: AutonomyTier },
    /// The refresh failed, but a last-known-good snapshot younger than
    /// [`MAX_DIAL_AGE`] is in hand. Decides, flagged with its age.
    Stale { tier: AutonomyTier, age_secs: u64 },
    /// The document was read and carries an `implement_tier:` line we do not
    /// recognise. UNKNOWN — never coerced to a tier.
    Unrecognised { raw: String },
    /// The document was read and carries no `implement_tier:` line at all.
    Absent,
    /// The document could not be read (transport, non-2xx, decode) and no usable
    /// snapshot is held.
    Unresolved { error: String },
    /// This runner holds no device JWT, so it has no coord tenant and there is
    /// **no recorded preference to honour**. Distinct from [`Self::Unresolved`]
    /// on purpose — mirrors [`crate::agent_authorization`]'s `Unpaired` arm.
    Unpaired,
    /// An operator override was set. Named so a log can never present a forced
    /// tier as the tenant's own.
    Override { tier: AutonomyTier },
}

/// How strictly a failing conjunct is enforced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EnforcementPosture {
    /// Today's landed behaviour: write trust best-effort, always spawn. The
    /// derivation is still computed and logged — the audit trail is the whole
    /// deliverable at this posture.
    Report,
    /// A failing or UNKNOWN conjunct **withholds the trust write**. The spawn
    /// still happens; it may face the dialog or silently lose the workspace's
    /// hooks and MCP servers, which is the visible, degraded outcome.
    Withhold,
    /// A failing or UNKNOWN conjunct yields `spawn_blocked`: no trust write and
    /// no spawn.
    Block,
}

/// The dial → posture mapping. Pure, and the single place the two vocabularies
/// meet.
///
/// * `proceed` — the tenant has said security-surface work proceeds and is
///   reported. Preserve the landed behaviour exactly, and report.
/// * `draft-required` — the tenant wants security-surface work attenuated and
///   visible rather than blocked. Withholding the mint is the attenuation: the
///   work happens, the ambient-trust grant does not.
/// * `ask-first` — the tenant wants an operator decision before security-surface
///   work. A mint nobody vouched for is exactly that, so it blocks.
///
/// ## The three degraded arms are NOT one arm
///
/// [`DialResolution::Absent`] and [`DialResolution::Unrecognised`] are **the
/// dial's own absence**: coord answered, and the tenant's document either
/// carries no `implement_tier:` line or carries one nobody can read. That is a
/// stable statement about the served document, it will not self-heal, and the
/// plan is explicit — UNKNOWN reads as the most conservative posture, which is
/// [`EnforcementPosture::Block`]. The cost is bounded by [`decide`]'s
/// short-circuit: an already-trusted target mints nothing and is never blocked,
/// so this only bites a spawn that would otherwise have created trust for an
/// unvouched directory under a tenant that states no preference at all.
///
/// [`DialResolution::Unresolved`] is a **failed read**, which says nothing about
/// the dial. It gets [`EnforcementPosture::Withhold`], and the split is
/// deliberate rather than a relaxation:
///
/// * `Withhold` is already strictly MORE conservative than the landed
///   behaviour — it never mints. The control this whole plan protects is "do
///   not create trust for a directory nobody vouched for", and withholding
///   honours it completely.
/// * `Block` would let a coord outage — or a cold cache on a runner that has
///   simply not made its first read yet — stop every underived spawn on the
///   fleet. That is [`crate::agent_authorization`]'s explicit posture: *"a coord
///   outage must not break the fleet"*, and it is a likelier and larger failure
///   than the one blocking would prevent.
/// * The headless spawn paths run `claude --print`, which does not HANG on an
///   untrusted workspace — it silently drops the workspace's hooks and MCP
///   servers (`super::workspace_trust`). So a withheld spawn degrades visibly
///   instead of stalling, which is the failure the plan's "never a spawn that
///   will hang" clause is about.
///
/// [`DialResolution::Unpaired`] is a third thing again. An unpaired runner has
/// no coord tenant, so there is no preference to honour; anything but
/// [`EnforcementPosture::Report`] would *invent* one and would change local
/// autonomy on an offline box for no stated reason. Same category as
/// [`crate::agent_authorization`]'s `Unpaired → Allow`.
pub fn posture_for(dial: &DialResolution) -> (EnforcementPosture, &'static str) {
    match dial {
        DialResolution::Fresh { tier } => (posture_for_tier(*tier), "dial-fresh"),
        DialResolution::Stale { tier, .. } => (posture_for_tier(*tier), "dial-stale-lkg"),
        DialResolution::Override { tier } => (posture_for_tier(*tier), "dial-operator-override"),
        DialResolution::Unrecognised { .. } => (EnforcementPosture::Block, "dial-unrecognised"),
        DialResolution::Absent => (EnforcementPosture::Block, "dial-absent"),
        DialResolution::Unresolved { .. } => (EnforcementPosture::Withhold, "dial-unresolved-read"),
        DialResolution::Unpaired => (EnforcementPosture::Report, "dial-unpaired-no-tenant"),
    }
}

fn posture_for_tier(tier: AutonomyTier) -> EnforcementPosture {
    match tier {
        AutonomyTier::Proceed => EnforcementPosture::Report,
        AutonomyTier::DraftRequired => EnforcementPosture::Withhold,
        AutonomyTier::AskFirst => EnforcementPosture::Block,
    }
}

/// Operator override for the dial, for testing the flip on a box and for the
/// break-glass case. Announced on every decision it changes (see
/// [`DialResolution::Override`]); there is no quiet override.
const DIAL_OVERRIDE_ENV: &str = "QONTINUI_TRUST_GATE_TIER";

/// How long a successfully-read dial counts as fresh. Same order as
/// [`crate::agent_authorization::REGISTRY_CACHE_TTL`]: an operator flipping the
/// tier sees it take effect on the fleet inside about a minute.
const DIAL_TTL: Duration = Duration::from_secs(90);

/// Hard ceiling on a last-known-good dial. Past this the cold matrix applies —
/// without a cap, a permanently-failing read would honour a snapshot from days
/// ago forever, silently ignoring a tier the operator has since tightened.
const MAX_DIAL_AGE: Duration = Duration::from_secs(12 * 60 * 60);

/// After a failed read, how long before another spawn re-attempts the HTTP call.
const DIAL_ERROR_BACKOFF: Duration = Duration::from_secs(15);

/// Per-attempt timeout. This sits in front of spawns; a dial read has no
/// business costing seconds.
const DIAL_TIMEOUT: Duration = Duration::from_secs(3);

/// Ceiling on ONE allocation-ledger fetch. Longer than [`DIAL_TIMEOUT`] because
/// the response is the whole fleet's worktree census (~1.5k rows on this fleet,
/// one fetch per [`DIAL_TTL`]), and still far short of the 15s the survey's own
/// client allows — that budget is sized for an operator waiting on a report, not
/// for a gate in front of a spawn.
const LEDGER_TIMEOUT: Duration = Duration::from_secs(5);

struct DialCache {
    tier: Option<AutonomyTier>,
    fetched_at: Option<Instant>,
    last_attempt: Option<Instant>,
    last_error: Option<String>,
    /// The last non-tier PARSE outcome, kept so a served-but-unreadable clause
    /// stays distinguishable from a failed read.
    last_parse: Option<DialParse>,
}

static DIAL: Lazy<Mutex<DialCache>> = Lazy::new(|| {
    Mutex::new(DialCache {
        tier: None,
        fetched_at: None,
        last_attempt: None,
        last_error: None,
        last_parse: None,
    })
});

/// Read the operator override, if any. `None` when unset or unparseable — an
/// unparseable override is IGNORED rather than silently tightening or loosening
/// the gate, and it is logged.
fn dial_override() -> Option<AutonomyTier> {
    let raw = std::env::var(DIAL_OVERRIDE_ENV).ok()?;
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    match AutonomyTier::from_wire(raw) {
        Some(t) => Some(t),
        None => {
            warn!(
                value = %raw,
                "{DIAL_OVERRIDE_ENV} is not one of ask-first|draft-required|proceed; ignoring it \
                 and reading the tenant dial"
            );
            None
        }
    }
}

/// Whatever the dial cache holds RIGHT NOW, without touching the network.
///
/// The synchronous door, for the spawn surfaces that cannot await (the PTY
/// seam, the inline-session builders). It never blocks and never fetches — the
/// same posture [`crate::mcp::fleet_policy_poller`]'s caches take for exactly
/// this reason.
pub fn peek_dial() -> DialResolution {
    if let Some(tier) = dial_override() {
        return DialResolution::Override { tier };
    }
    // Checked here as well as in [`resolve_dial`]: without it an unpaired runner
    // would read `Unresolved` from an empty cache and take the most-conservative
    // posture, which is the exact collapse of "no tenant" into "read failed"
    // that the two arms exist to keep apart.
    if crate::auth::device_bearer().is_none() {
        return DialResolution::Unpaired;
    }
    let resolution = {
        let guard = match DIAL.lock() {
            Ok(g) => g,
            // A poisoned lock is a read we cannot make. UNKNOWN, not a tier.
            Err(_) => {
                return DialResolution::Unresolved {
                    error: "dial cache lock poisoned".to_string(),
                }
            }
        };
        snapshot_resolution(&guard)
    };
    // Self-heal a COLD cache. This door cannot await, so it answers honestly
    // from whatever is held — but a runner whose async spawn path has not run
    // yet would otherwise stay cold forever and take the conservative posture on
    // every sync spawn. Detached, so the caller pays nothing; the NEXT spawn
    // sees the answer. Never fires when a snapshot already decided, so a healthy
    // runner makes no extra request.
    if matches!(resolution, DialResolution::Unresolved { .. }) {
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async {
                let _ = resolve_dial().await;
            });
        }
    }
    resolution
}

fn snapshot_resolution(cache: &DialCache) -> DialResolution {
    match (cache.tier, cache.fetched_at) {
        (Some(tier), Some(at)) => {
            let age = at.elapsed();
            if age <= DIAL_TTL {
                DialResolution::Fresh { tier }
            } else if age <= MAX_DIAL_AGE {
                DialResolution::Stale {
                    tier,
                    age_secs: age.as_secs(),
                }
            } else {
                DialResolution::Unresolved {
                    error: format!(
                        "last-known-good dial is {}s old, past the {}s ceiling",
                        age.as_secs(),
                        MAX_DIAL_AGE.as_secs()
                    ),
                }
            }
        }
        _ => match &cache.last_parse {
            Some(DialParse::Unrecognised(raw)) => DialResolution::Unrecognised { raw: raw.clone() },
            Some(DialParse::Absent) => DialResolution::Absent,
            _ => DialResolution::Unresolved {
                error: cache
                    .last_error
                    .clone()
                    .unwrap_or_else(|| "the dial has never been read on this runner".to_string()),
            },
        },
    }
}

/// Resolve the tenant's dial, refreshing at most once per [`DIAL_TTL`] and, on
/// failure, at most once per [`DIAL_ERROR_BACKOFF`].
pub async fn resolve_dial() -> DialResolution {
    if let Some(tier) = dial_override() {
        return DialResolution::Override { tier };
    }

    // An unpaired runner has no tenant, so there is no document to read and no
    // preference to honour. Checked BEFORE the cache so a runner that unpairs
    // stops honouring a snapshot minted under a tenant it no longer belongs to.
    if crate::auth::device_bearer().is_none() {
        return DialResolution::Unpaired;
    }

    let should_fetch = {
        match DIAL.lock() {
            Ok(g) => match (g.fetched_at, g.last_attempt) {
                (Some(at), _) if at.elapsed() <= DIAL_TTL => false,
                (_, Some(last)) if last.elapsed() <= DIAL_ERROR_BACKOFF => false,
                _ => true,
            },
            Err(_) => false,
        }
    };

    if should_fetch {
        let outcome = fetch_dial().await;
        if let Ok(mut g) = DIAL.lock() {
            g.last_attempt = Some(Instant::now());
            match outcome {
                Ok(parse) => {
                    g.last_parse = Some(parse.clone());
                    g.last_error = None;
                    if let DialParse::Present(tier) = parse {
                        let changed = g.tier != Some(tier);
                        g.tier = Some(tier);
                        g.fetched_at = Some(Instant::now());
                        if changed {
                            info!(
                                tier = tier.as_wire(),
                                "trust gate: tenant autonomy dial resolved"
                            );
                        }
                    } else {
                        // A served-but-unreadable clause must not keep a stale
                        // tier alive: the tenant edited the document and what it
                        // now says is UNKNOWN.
                        g.tier = None;
                        g.fetched_at = None;
                    }
                }
                Err(e) => {
                    g.last_error = Some(e);
                }
            }
        }
    }

    match DIAL.lock() {
        Ok(g) => snapshot_resolution(&g),
        Err(_) => DialResolution::Unresolved {
            error: "dial cache lock poisoned".to_string(),
        },
    }
}

/// One dial read against coord's AGENT door (never the operator door, which
/// 403s a device JWT).
async fn fetch_dial() -> Result<DialParse, String> {
    let base = match qontinui_runner_lib::profiles::connected_coord_base() {
        Some(b) => b,
        None => match std::env::var("COORD_HTTP_URL") {
            Ok(v) if !v.trim().is_empty() => v.trim().trim_end_matches('/').to_string(),
            _ => return Err("no coord base resolved".to_string()),
        },
    };
    let url = format!("{}{DIAL_DOCUMENT}", base.trim_end_matches('/'));
    let client = crate::coord_http::coord_client().ok_or("no shared coord HTTP client")?;
    // coord-tenant-scope(device): a READ of this device's own tenant's policy
    // document. It persists nothing and owns no row, so the default device
    // binding is the subject — presenting any other credential would answer
    // about the wrong tenant.
    let resp = crate::auth::attach_device_auth(client.get(&url))
        .timeout(DIAL_TIMEOUT)
        .send()
        .await
        .map_err(|e| format!("GET {url}: {e}"))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(format!("coord returned {status} for GET {url}"));
    }
    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("decode dial document: {e}"))?;
    // Reuse the SETTLED row-envelope reader rather than hand-rolling a second
    // one: it unwraps `{"document":{…}}` or a flat row, trims, and treats an
    // empty body as absent. `policy_context` reads the same router through it.
    let text = crate::mcp::continuation_verdict::rules_from_doc_body(&body)
        .ok_or_else(|| "dial document carried no `body` string".to_string())?;
    Ok(parse_autonomy_dial(&text))
}

// =============================================================================
// Phase 2 — the three conjuncts
// =============================================================================

/// The conjuncts, in the plan's order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Conjunct {
    /// The target has a `coord.agent_worktrees` row — coord's resource, not an
    /// arbitrary path.
    CoordWorktreeRow,
    /// The parent repo already reads trusted for that account.
    ParentRepoTrusted,
    /// The write targets the exact `CLAUDE_CONFIG_DIR` the spawn will use.
    ConfigDirPinned,
}

impl Conjunct {
    pub fn as_str(self) -> &'static str {
        match self {
            Conjunct::CoordWorktreeRow => "coord_worktree_row",
            Conjunct::ParentRepoTrusted => "parent_repo_trusted",
            Conjunct::ConfigDirPinned => "config_dir_pinned",
        }
    }
}

/// One conjunct's answer. **Never a bare bool** — an unevaluated conjunct is
/// UNKNOWN, and UNKNOWN is not a pass.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "verdict", rename_all = "snake_case")]
pub enum ConjunctVerdict {
    /// Satisfied, and `decided_by` names WHICH evidence decided it — a coord row
    /// and a local layout inference are not the same strength of answer, and the
    /// audit trail has to be able to tell them apart.
    Satisfied {
        decided_by: &'static str,
        detail: String,
    },
    Failed {
        decided_by: &'static str,
        detail: String,
    },
    /// The conjunct could not be evaluated. Always carries why.
    Unknown { reason: String },
}

impl ConjunctVerdict {
    pub fn label(&self) -> &'static str {
        match self {
            ConjunctVerdict::Satisfied { .. } => "satisfied",
            ConjunctVerdict::Failed { .. } => "failed",
            ConjunctVerdict::Unknown { .. } => "unknown",
        }
    }

    /// True ONLY for [`Self::Satisfied`]. Named so no call site can read an
    /// `Unknown` as a pass by accident.
    pub fn is_satisfied(&self) -> bool {
        matches!(self, ConjunctVerdict::Satisfied { .. })
    }

    fn decided_by(&self) -> &'static str {
        match self {
            ConjunctVerdict::Satisfied { decided_by, .. }
            | ConjunctVerdict::Failed { decided_by, .. } => decided_by,
            ConjunctVerdict::Unknown { .. } => "unevaluated",
        }
    }

    fn detail(&self) -> String {
        match self {
            ConjunctVerdict::Satisfied { detail, .. } | ConjunctVerdict::Failed { detail, .. } => {
                detail.clone()
            }
            ConjunctVerdict::Unknown { reason } => reason.clone(),
        }
    }
}

/// All three conjuncts, with the derivation attached.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Conjuncts {
    pub coord_worktree_row: ConjunctVerdict,
    pub parent_repo_trusted: ConjunctVerdict,
    pub config_dir_pinned: ConjunctVerdict,
}

impl Conjuncts {
    pub fn all_satisfied(&self) -> bool {
        self.coord_worktree_row.is_satisfied()
            && self.parent_repo_trusted.is_satisfied()
            && self.config_dir_pinned.is_satisfied()
    }

    /// The conjuncts that did NOT pass — failures and UNKNOWNs alike.
    pub fn failing(&self) -> Vec<(Conjunct, &ConjunctVerdict)> {
        [
            (Conjunct::CoordWorktreeRow, &self.coord_worktree_row),
            (Conjunct::ParentRepoTrusted, &self.parent_repo_trusted),
            (Conjunct::ConfigDirPinned, &self.config_dir_pinned),
        ]
        .into_iter()
        .filter(|(_, v)| !v.is_satisfied())
        .collect()
    }

    /// One line naming every conjunct, its verdict, and how it was decided —
    /// the plan's explicit requirement that the audit trail answer "why is this
    /// directory trusted?".
    pub fn derivation(&self) -> String {
        [
            (Conjunct::CoordWorktreeRow, &self.coord_worktree_row),
            (Conjunct::ParentRepoTrusted, &self.parent_repo_trusted),
            (Conjunct::ConfigDirPinned, &self.config_dir_pinned),
        ]
        .into_iter()
        .map(|(c, v)| {
            format!(
                "{}={} by {} ({})",
                c.as_str(),
                v.label(),
                v.decided_by(),
                v.detail()
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
    }
}

// ---------------------------------------------------------------------------
// Conjunct 1 — is this coord's worktree?
// ---------------------------------------------------------------------------

/// What coord's allocation ledger said about the spawn target's path.
///
/// Injected into [`derive_conjuncts`] rather than fetched there, so every arm —
/// including the one where coord never answered — is testable without a
/// network.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoordRowLookup {
    /// A path-keyed `coord.agent_worktrees` row exists. `session_id` is the
    /// allocating session, carried for the audit line.
    Matched { session_id: String },
    /// coord answered and holds no path-keyed row for this target.
    NoRow,
    /// coord could not be asked (unreachable, unpaired, no async context here).
    /// **Not** a "no row" — absence of an answer is UNKNOWN.
    Unavailable { reason: String },
}

/// What the local filesystem says about the target's shape, independent of
/// coord.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalWorktreeShape {
    /// A linked git worktree (its `.git` is a FILE) sitting under one of this
    /// machine's agent-worktree roots. `parent` is the primary checkout its
    /// gitdir points back at; `root` names which agent-worktree root matched.
    UnderAgentWorktreeRoot { parent: PathBuf, root: String },
    /// A linked git worktree, but NOT under an agent-worktree root — a
    /// hand-rolled `git worktree add`, which coord does not own.
    LinkedWorktreeElsewhere { parent: PathBuf },
    /// Not a linked worktree at all: a primary checkout (its `.git` is a
    /// directory) or a plain directory. Definitive.
    NotALinkedWorktree,
    /// The shape could not be established (unreadable `.git` file, unresolvable
    /// workspace root, an unparseable `gitdir:` line).
    Undetermined { reason: String },
}

/// Decide conjunct 1 from coord's answer and the local shape.
///
/// The ladder, and why each rung reads the way it does:
///
/// 1. A coord row is the direct answer to "is this coord's resource?".
/// 2. With no coord answer, a linked worktree under an agent-worktree root is
///    **locally observable evidence** of an allocation — weaker, and LABELLED
///    so, because a hand-rolled `git worktree add` under that root would also
///    match. It is admitted rather than refused because refusing would make a
///    coord outage indistinguishable from an attack, and the other two conjuncts
///    still gate the write.
/// 3. Coord says no row AND the target is definitively not a linked worktree →
///    `Failed`. Both sources agree.
/// 4. Coord says no row while the target IS a linked worktree somewhere →
///    `Unknown`, and this arm is load-bearing rather than defensive. The door
///    behind [`CoordRowLookup`] is `GET /coord/sessions/worktrees`, which JOINS
///    `coord.sessions` — it is not a raw ledger reader, and it was measured
///    answering `count: 0` on a box holding 250 working trees (Phase 5 of this
///    plan, 2026-09-06). A `NoRow` from it is therefore NOT evidence that no
///    `coord.agent_worktrees` row exists, so a `Failed` here would be a
///    confident wrong answer about the very thing the conjunct asks.
/// 5. Nothing could be established → `Unknown`.
pub fn decide_worktree_conjunct(
    coord: &CoordRowLookup,
    local: &LocalWorktreeShape,
) -> ConjunctVerdict {
    match (coord, local) {
        (CoordRowLookup::Matched { session_id }, _) => ConjunctVerdict::Satisfied {
            decided_by: "coord_allocation_row",
            detail: format!("coord.agent_worktrees row allocated by session {session_id}"),
        },
        // coord answered "no row", and the local shape agrees this is not an
        // allocated worktree at all.
        (CoordRowLookup::NoRow, LocalWorktreeShape::NotALinkedWorktree) => {
            ConjunctVerdict::Failed {
                decided_by: "coord_allocation_row",
                detail: "coord holds no allocation row and the target is not a linked git worktree"
                    .to_string(),
            }
        }
        (CoordRowLookup::NoRow, LocalWorktreeShape::LinkedWorktreeElsewhere { parent }) => {
            ConjunctVerdict::Failed {
                decided_by: "coord_allocation_row",
                detail: format!(
                    "coord holds no allocation row; a hand-rolled linked worktree of {} is not \
                     coord's resource",
                    parent.display()
                ),
            }
        }
        // coord says no row, but the target IS under an agent-worktree root. The
        // two disagree; a confident `Failed` would be a wrong answer, and a
        // confident `Satisfied` would ignore coord.
        (CoordRowLookup::NoRow, LocalWorktreeShape::UnderAgentWorktreeRoot { root, .. }) => {
            ConjunctVerdict::Unknown {
                reason: format!(
                    "coord holds no path-keyed allocation row, but the target sits under the \
                     agent-worktree root {root} — the ledger path may be spelled differently"
                ),
            }
        }
        (CoordRowLookup::NoRow, LocalWorktreeShape::Undetermined { reason }) => {
            ConjunctVerdict::Unknown {
                reason: format!(
                    "coord holds no allocation row and the local shape is UNKNOWN: {reason}"
                ),
            }
        }
        // No coord answer: fall to locally observable evidence, labelled.
        (
            CoordRowLookup::Unavailable { reason },
            LocalWorktreeShape::UnderAgentWorktreeRoot { parent, root },
        ) => ConjunctVerdict::Satisfied {
            decided_by: "local_worktree_layout",
            detail: format!(
                "coord unavailable ({reason}); the target is a linked git worktree of {} under \
                 the agent-worktree root {root}",
                parent.display()
            ),
        },
        (CoordRowLookup::Unavailable { reason }, LocalWorktreeShape::NotALinkedWorktree) => {
            ConjunctVerdict::Failed {
                decided_by: "local_worktree_layout",
                detail: format!(
                    "coord unavailable ({reason}); the target is not a linked git worktree, so it \
                     cannot be an allocated one"
                ),
            }
        }
        (
            CoordRowLookup::Unavailable { reason },
            LocalWorktreeShape::LinkedWorktreeElsewhere { parent },
        ) => ConjunctVerdict::Unknown {
            reason: format!(
                "coord unavailable ({reason}); the target is a linked worktree of {} outside every \
                 agent-worktree root, so whether coord allocated it is UNKNOWN",
                parent.display()
            ),
        },
        (
            CoordRowLookup::Unavailable { reason },
            LocalWorktreeShape::Undetermined { reason: r2 },
        ) => ConjunctVerdict::Unknown {
            reason: format!("coord unavailable ({reason}); local shape UNKNOWN: {r2}"),
        },
    }
}

/// Read the `.git` entry at `dir` and classify the target's shape.
///
/// **Depth-agnostic on purpose.** Measured on the operator box 2026-09-06:
/// 223 allocated trees sit at `<root>/<agent-id>/<repo>` and **24 sit a level
/// deeper** at `<root>/<agent-id>/<owner>/<repo>`. A shape check that assumed
/// the two-segment layout — as
/// [`crate::agent_worktree::canonical_paths::allocated_worktree_for_path`] does
/// — would report `NotALinkedWorktree` for those 24 and mint no trust for them.
/// The test here is containment under a root plus the `.git`-is-a-file fact,
/// neither of which cares about depth.
pub fn local_worktree_shape(dir: &Path, roots: &[(String, PathBuf)]) -> LocalWorktreeShape {
    // Classify the enclosing GIT ROOT, not the literal cwd. A spawn into
    // `<worktree>/src-tauri/src` has no `.git` of its own, and reading that as
    // "not a linked worktree" would fail conjunct 1 for every subdirectory spawn
    // — while `project_key` (which decides the trust KEY) walks to the same root.
    // The two must agree about which directory is under discussion.
    let dir = &workspace_trust::git_root(dir).unwrap_or_else(|| dir.to_path_buf());
    let git = dir.join(".git");
    let meta = match std::fs::metadata(&git) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return LocalWorktreeShape::NotALinkedWorktree
        }
        Err(e) => {
            return LocalWorktreeShape::Undetermined {
                reason: format!("could not stat {}: {e}", git.display()),
            }
        }
    };
    if meta.is_dir() {
        // A primary checkout. Definitive.
        return LocalWorktreeShape::NotALinkedWorktree;
    }
    let raw = match std::fs::read_to_string(&git) {
        Ok(r) => r,
        Err(e) => {
            return LocalWorktreeShape::Undetermined {
                reason: format!("could not read {}: {e}", git.display()),
            }
        }
    };
    let Some(parent) = primary_checkout_from_gitdir(&raw) else {
        return LocalWorktreeShape::Undetermined {
            reason: format!(
                "{} carries no parseable `gitdir: <primary>/.git/worktrees/<name>` line",
                git.display()
            ),
        };
    };
    for (label, root) in roots {
        if path_is_within(dir, root) {
            return LocalWorktreeShape::UnderAgentWorktreeRoot {
                parent,
                root: label.clone(),
            };
        }
    }
    LocalWorktreeShape::LinkedWorktreeElsewhere { parent }
}

/// Extract the PRIMARY checkout from a linked worktree's `.git` file.
///
/// The file's one line is `gitdir: <primary>/.git/worktrees/<name>`. The primary
/// checkout is everything before `/.git/worktrees/`. Pure over a string so the
/// separator handling (git writes forward slashes even on Windows; a
/// hand-edited file may not) is directly testable.
pub fn primary_checkout_from_gitdir(raw: &str) -> Option<PathBuf> {
    let line = raw
        .lines()
        .find_map(|l| l.trim().strip_prefix("gitdir:"))?
        .trim();
    if line.is_empty() {
        return None;
    }
    let norm = line.replace('\\', "/");
    // Case-insensitive on the marker: the path segments are git's own, so
    // `.git/worktrees` is stable, but the drive-letter case is not and a
    // lowercase compare of the whole string would corrupt the returned path.
    let marker = "/.git/worktrees/";
    let idx = norm.find(marker)?;
    let primary = &norm[..idx];
    if primary.is_empty() {
        return None;
    }
    Some(PathBuf::from(primary))
}

/// Every agent-worktree root this machine may materialize into, labelled.
///
/// TWO roots, both live on this fleet (measured 2026-09-06 on the operator box:
/// `D:/qontinui-root/agent-worktrees` and `D:/qontinui-root/qontinui-worktrees`
/// both exist and both hold trees). The runner's own resolver
/// ([`crate::agent_worktree::canonical_paths::agent_worktree_root`]) names the
/// second and honours `QONTINUI_WORKTREE_ROOT` / `COORD_WORKTREE_ROOT`; the
/// first is coord's allocation naming, and omitting it would report every tree
/// under it as "not an allocated worktree".
pub fn agent_worktree_roots() -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    let Some(workspace_root) = crate::workspace_paths::runner_workspace_root().into_root() else {
        return out;
    };
    let resolved = crate::agent_worktree::canonical_paths::agent_worktree_root(
        &workspace_root.join(crate::agent_worktree::canonical_paths::WORKTREE_ROOT_DIRNAME),
    );
    out.push(("runner-resolved".to_string(), resolved.clone()));
    let coord_named = workspace_root.join("agent-worktrees");
    if coord_named != resolved {
        out.push(("coord-allocated".to_string(), coord_named));
    }
    out
}

/// Containment test that tolerates Windows separator and drive-case spellings.
/// Lexical: the caller has already resolved both sides, and a `canonicalize`
/// here would refuse a not-yet-materialized root.
fn path_is_within(child: &Path, root: &Path) -> bool {
    let norm = |p: &Path| {
        let s = p.to_string_lossy().replace('\\', "/");
        let s = s.strip_prefix(r"\\?\").unwrap_or(&s).to_string();
        #[cfg(windows)]
        let s = s.to_lowercase();
        s.trim_end_matches('/').to_string()
    };
    let (c, r) = (norm(child), norm(root));
    if r.is_empty() {
        return false;
    }
    c == r || c.starts_with(&format!("{r}/"))
}

// ---------------------------------------------------------------------------
// Conjunct 2 — is the parent repo ALREADY trusted?
// ---------------------------------------------------------------------------

/// Decide conjunct 2: the parent repo's own project key must ALREADY read
/// trusted in the account config the spawn will use.
///
/// `parent` is the primary checkout the worktree was cut from; `read` is the
/// trust probe ([`super::spawn_preconditions::trust_verdict_in`] in production),
/// injected so the whole matrix is testable against fixtures.
///
/// **This is the conjunct that makes the write a derivation.** Only trust a
/// human already granted to the enclosing repo is projected onto the worktree;
/// nothing new is created. An `Untrusted` parent therefore yields `Failed`, and
/// an unreadable one yields `Unknown` — never a pass.
pub fn decide_parent_conjunct(
    parent: Option<&Path>,
    read: impl FnOnce(&str) -> TrustVerdict,
) -> ConjunctVerdict {
    let Some(parent) = parent else {
        return ConjunctVerdict::Unknown {
            reason: "the target's parent repo could not be derived".to_string(),
        };
    };
    let Some(key) = workspace_trust::project_key(parent) else {
        return ConjunctVerdict::Unknown {
            reason: format!(
                "no project key derivable for the parent repo {} (relative or unresolved)",
                parent.display()
            ),
        };
    };
    match read(&key) {
        TrustVerdict::Trusted => ConjunctVerdict::Satisfied {
            decided_by: "parent_project_key",
            detail: format!("{key} already reads hasTrustDialogAccepted=true for this account"),
        },
        TrustVerdict::Untrusted { reason } => ConjunctVerdict::Failed {
            decided_by: "parent_project_key",
            detail: format!(
                "{key} is NOT trusted for this account ({reason}) — trust is inherited, never \
                 created"
            ),
        },
        TrustVerdict::Unknown { reason } => ConjunctVerdict::Unknown {
            reason: format!("the parent repo {key}'s trust state could not be read: {reason}"),
        },
    }
}

// ---------------------------------------------------------------------------
// Conjunct 3 — does the write target the config dir the spawn will use?
// ---------------------------------------------------------------------------

/// Decide conjunct 3 by comparing the config file the write would touch against
/// the one the child will actually read.
///
/// `None` on either side means "the ambient default", which is a value, not an
/// absence — both sides are resolved to a concrete file before the comparison so
/// `Some(~/.claude)` and `None` cannot silently disagree.
pub fn decide_config_dir_conjunct(
    write_target: Option<&Path>,
    child_reads: Option<&Path>,
) -> ConjunctVerdict {
    match (write_target, child_reads) {
        (Some(a), Some(b)) => {
            if path_is_within(a, b) && path_is_within(b, a) {
                ConjunctVerdict::Satisfied {
                    decided_by: "single_account_resolution",
                    detail: format!("the write and the child both use {}", a.display()),
                }
            } else {
                ConjunctVerdict::Failed {
                    decided_by: "single_account_resolution",
                    detail: format!(
                        "the write would touch {} while the child reads {}",
                        a.display(),
                        b.display()
                    ),
                }
            }
        }
        (a, b) => ConjunctVerdict::Unknown {
            reason: format!(
                "the account config file could not be resolved on both sides (write={:?}, \
                 child={:?})",
                a.map(|p| p.display().to_string()),
                b.map(|p| p.display().to_string())
            ),
        },
    }
}

// =============================================================================
// The decision
// =============================================================================

/// What the gate decided for one spawn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum TrustGateDecision {
    /// The target already reads trusted. Nothing is minted, so there is nothing
    /// to derive and nothing to gate.
    AlreadyTrusted,
    /// Every conjunct holds: the write is DERIVED from trust a human already
    /// granted to the parent repo.
    Grant { rule: &'static str },
    /// Do not write; spawn anyway. The spawn may face the dialog or lose the
    /// workspace's hooks and MCP servers — visible, and strictly weaker than
    /// minting ambient trust.
    Withhold {
        rule: &'static str,
        failed: Vec<String>,
    },
    /// Do not write and do not spawn.
    SpawnBlocked {
        rule: &'static str,
        failed: Vec<String>,
    },
}

impl TrustGateDecision {
    pub fn label(&self) -> &'static str {
        match self {
            TrustGateDecision::AlreadyTrusted => "already_trusted",
            TrustGateDecision::Grant { .. } => "grant",
            TrustGateDecision::Withhold { .. } => "withhold",
            TrustGateDecision::SpawnBlocked { .. } => "spawn_blocked",
        }
    }

    /// True when the caller may write the trust flag.
    pub fn writes_trust(&self) -> bool {
        matches!(self, TrustGateDecision::Grant { .. })
    }

    /// True when the caller may go ahead and spawn.
    pub fn allows_spawn(&self) -> bool {
        !matches!(self, TrustGateDecision::SpawnBlocked { .. })
    }

    pub fn rule(&self) -> &'static str {
        match self {
            TrustGateDecision::AlreadyTrusted => "already-trusted-nothing-minted",
            TrustGateDecision::Grant { rule }
            | TrustGateDecision::Withhold { rule, .. }
            | TrustGateDecision::SpawnBlocked { rule, .. } => rule,
        }
    }

    /// The typed refusal a caller turns into a `spawn_blocked` report. `None`
    /// when the spawn may proceed.
    pub fn refusal(&self) -> Option<String> {
        match self {
            TrustGateDecision::SpawnBlocked { rule, failed } => Some(format!(
                "spawn_blocked ({rule}): workspace trust could not be DERIVED for this target \
                 and the tenant's autonomy dial forbids minting it — {}",
                failed.join("; ")
            )),
            _ => None,
        }
    }
}

/// The pure decision core: no HTTP, no clock, no globals.
///
/// The whole matrix, so the failure directions are readable in one place:
///
/// | trust verdict | conjuncts | posture | decision |
/// |---|---|---|---|
/// | `Trusted` | — | any | `AlreadyTrusted` (nothing minted) |
/// | not trusted | all three hold | any | `Grant` |
/// | not trusted | any fails/UNKNOWN | `Report` | `Grant`, derivation logged |
/// | not trusted | any fails/UNKNOWN | `Withhold` | `Withhold` |
/// | not trusted | any fails/UNKNOWN | `Block` | `SpawnBlocked` |
pub fn decide(
    trust: &TrustVerdict,
    conjuncts: &Conjuncts,
    posture: EnforcementPosture,
) -> TrustGateDecision {
    if matches!(trust, TrustVerdict::Trusted) {
        return TrustGateDecision::AlreadyTrusted;
    }
    if conjuncts.all_satisfied() {
        return TrustGateDecision::Grant {
            rule: "derived-from-parent-repo-trust",
        };
    }
    let failed: Vec<String> = conjuncts
        .failing()
        .into_iter()
        .map(|(c, v)| format!("{}={} ({})", c.as_str(), v.label(), v.detail()))
        .collect();
    match posture {
        EnforcementPosture::Report => TrustGateDecision::Grant {
            rule: "dial-permits-underived-mint",
        },
        EnforcementPosture::Withhold => TrustGateDecision::Withhold {
            rule: "dial-withholds-underived-mint",
            failed,
        },
        EnforcementPosture::Block => TrustGateDecision::SpawnBlocked {
            rule: "dial-blocks-underived-mint",
            failed,
        },
    }
}

/// Everything the gate derived for one spawn, flat enough to log and to POST.
#[derive(Debug, Clone, Serialize)]
pub struct TrustGateReport {
    pub cwd: String,
    pub project_key: Option<String>,
    pub account_config_file: Option<String>,
    pub parent_repo: Option<String>,
    pub trust: TrustVerdict,
    pub conjuncts: Conjuncts,
    pub dial: DialResolution,
    pub posture: EnforcementPosture,
    pub decision: TrustGateDecision,
    /// The [`TrustOutcome`] of the write, when one was made. `None` when the
    /// decision withheld it or the target was already trusted.
    pub write_outcome: Option<String>,
}

// =============================================================================
// Production wiring
// =============================================================================

/// Resolve coord's path-keyed allocation ledger for `dir`.
///
/// Reuses [`crate::agent_worktree::custody::coord`]'s index verbatim rather than
/// adding a second reader of the same door — a divergent second join is how the
/// two would start disagreeing about what "allocated" means. The index is cached
/// for [`DIAL_TTL`] because it is one fleet-wide response (~1.5k rows on this
/// fleet) and a per-spawn fetch would be absurd.
///
/// **Only the PATH-keyed arm counts.** `owner_for` also answers from a
/// `(repo, branch)` binding, which names some OTHER worktree's allocation; that
/// arm leaves `allocation_session_id` unset, and `branch: None` disables it
/// outright. Reading it as a row for THIS path would be exactly the "trust the
/// path we are about to use" relaxation the plan forbids.
pub async fn coord_row_lookup(dir: &Path) -> CoordRowLookup {
    if crate::auth::device_bearer().is_none() {
        return CoordRowLookup::Unavailable {
            reason: "runner holds no device JWT".to_string(),
        };
    }
    let index = match worktree_index().await {
        Ok(idx) => idx,
        Err(e) => return CoordRowLookup::Unavailable { reason: e },
    };
    // The ledger records a worktree's ROOT, so look the root up — a spawn into
    // `<worktree>/src-tauri` would otherwise miss its own allocation row, and
    // `norm_path_matches` only tolerates the ledger path being a SUFFIX of the
    // census path, never the census path being deeper.
    let dir = &workspace_trust::git_root(dir).unwrap_or_else(|| dir.to_path_buf());
    let path = dir.to_string_lossy().replace('\\', "/");
    let repo = crate::agent_worktree::canonical_paths::repo_slug_for_path(dir).unwrap_or_default();
    match index
        .owner_for(&path, &repo, None)
        .and_then(|o| o.allocation_session_id)
    {
        Some(session_id) => CoordRowLookup::Matched { session_id },
        None => CoordRowLookup::NoRow,
    }
}

type OwnershipIndex = std::sync::Arc<crate::agent_worktree::custody::coord::CoordOwnership>;

struct IndexCache {
    index: Option<OwnershipIndex>,
    fetched_at: Option<Instant>,
    last_attempt: Option<Instant>,
    last_error: Option<String>,
}

static WORKTREE_INDEX: Lazy<tokio::sync::Mutex<IndexCache>> = Lazy::new(|| {
    tokio::sync::Mutex::new(IndexCache {
        index: None,
        fetched_at: None,
        last_attempt: None,
        last_error: None,
    })
});

async fn worktree_index() -> Result<OwnershipIndex, String> {
    let mut guard = WORKTREE_INDEX.lock().await;
    let fresh = guard
        .fetched_at
        .map(|at| at.elapsed() <= DIAL_TTL)
        .unwrap_or(false);
    if fresh {
        if let Some(idx) = &guard.index {
            return Ok(idx.clone());
        }
    }
    let backing_off = guard
        .last_attempt
        .map(|at| at.elapsed() <= DIAL_ERROR_BACKOFF)
        .unwrap_or(false);
    if !backing_off {
        guard.last_attempt = Some(Instant::now());
        // `fetch_ownership` builds its own 15s-timeout client, which is right for
        // the operator-facing worktree SURVEY it was written for and far too long
        // in front of a spawn. Bound it here rather than forking the fetcher: a
        // timeout falls through to `Unavailable`, which conjunct 1 already knows
        // how to answer from local evidence.
        let fetched = tokio::time::timeout(
            LEDGER_TIMEOUT,
            crate::agent_worktree::custody::coord::fetch_ownership(),
        )
        .await
        .unwrap_or_else(|_| {
            Err(format!(
                "the allocation ledger did not answer within {}s",
                LEDGER_TIMEOUT.as_secs()
            ))
        });
        match fetched {
            Ok(Some(idx)) => {
                let idx = std::sync::Arc::new(idx);
                guard.index = Some(idx.clone());
                guard.fetched_at = Some(Instant::now());
                guard.last_error = None;
                return Ok(idx);
            }
            Ok(None) => {
                guard.last_error = Some("no coord base configured".to_string());
            }
            Err(e) => {
                guard.last_error = Some(e);
            }
        }
    }
    // A last-known-good index still answers, capped exactly like the dial.
    if let (Some(idx), Some(at)) = (&guard.index, guard.fetched_at) {
        if at.elapsed() <= MAX_DIAL_AGE {
            return Ok(idx.clone());
        }
    }
    Err(guard
        .last_error
        .clone()
        .unwrap_or_else(|| "the allocation ledger has never been read on this runner".to_string()))
}

/// Build the three conjuncts for one spawn target.
///
/// Pure over its injected inputs, which is what makes each conjunct testable
/// failing ALONE.
pub fn derive_conjuncts(
    cwd: &Path,
    coord: &CoordRowLookup,
    roots: &[(String, PathBuf)],
    write_target: Option<&Path>,
    child_reads: Option<&Path>,
    read_parent_trust: impl FnOnce(&str) -> TrustVerdict,
) -> (Conjuncts, Option<PathBuf>) {
    let local = local_worktree_shape(cwd, roots);
    let parent = match &local {
        LocalWorktreeShape::UnderAgentWorktreeRoot { parent, .. }
        | LocalWorktreeShape::LinkedWorktreeElsewhere { parent } => Some(parent.clone()),
        LocalWorktreeShape::NotALinkedWorktree | LocalWorktreeShape::Undetermined { .. } => None,
    };
    let conjuncts = Conjuncts {
        coord_worktree_row: decide_worktree_conjunct(coord, &local),
        parent_repo_trusted: decide_parent_conjunct(parent.as_deref(), read_parent_trust),
        config_dir_pinned: decide_config_dir_conjunct(write_target, child_reads),
    };
    (conjuncts, parent)
}

/// Resolve the account config FILE for a `CLAUDE_CONFIG_DIR`, with `None`
/// meaning the ambient default. Mirrors
/// [`super::spawn_preconditions`]'s own resolution so the two cannot drift.
fn config_file_for(config_dir: Option<&str>) -> Option<PathBuf> {
    match config_dir {
        Some(dir) => Some(Path::new(dir).join(workspace_trust::CONFIG_FILE)),
        None => dirs::home_dir().map(|h| h.join(workspace_trust::CONFIG_FILE)),
    }
}

/// **The spawn-path entry point.** Derive trust for `cwd`, act on the verdict,
/// and return what was decided.
///
/// `trust` is the verdict Phase 1 already computed for this spawn — passed in
/// rather than re-probed, because a second probe would key a different question
/// and read as authoritative.
///
/// `config_dir` is the `CLAUDE_CONFIG_DIR` the spawn RESOLVED; `child_config_dir`
/// is what the child will actually be pinned to. They are equal by construction
/// at today's single call site, and conjunct 3 checks that rather than assuming
/// it.
pub async fn pre_accept_for_spawn(
    cwd: &str,
    trust: &TrustVerdict,
    config_dir: Option<&str>,
    child_config_dir: Option<&str>,
) -> TrustGateReport {
    // Nothing will be minted for an already-trusted target, so pay for NEITHER
    // coord read to justify a write that is not going to happen. This is the
    // common path once a repo has been spawned into once, and it must not put
    // two HTTP requests in front of every spawn.
    if matches!(trust, TrustVerdict::Trusted) {
        return finish(
            cwd,
            trust,
            config_dir,
            child_config_dir,
            CoordRowLookup::Unavailable {
                reason: "already trusted; ledger not consulted".to_string(),
            },
            peek_dial(),
        );
    }
    let dial = resolve_dial().await;
    let coord = coord_row_lookup(Path::new(cwd)).await;
    finish(cwd, trust, config_dir, child_config_dir, coord, dial)
}

/// The synchronous door, for spawn surfaces that cannot await.
///
/// Identical except that coord's ledger is only PEEKED (never fetched), so
/// conjunct 1 falls to its locally observable arm. That is the honest shape:
/// this path must not make a network call, and a conjunct it could not ask coord
/// about says so rather than pretending.
pub fn pre_accept_for_spawn_sync(
    cwd: &str,
    trust: &TrustVerdict,
    config_dir: Option<&str>,
    child_config_dir: Option<&str>,
) -> TrustGateReport {
    let dial = peek_dial();
    let coord = CoordRowLookup::Unavailable {
        reason: "no async context on this spawn path; coord ledger not consulted".to_string(),
    };
    finish(cwd, trust, config_dir, child_config_dir, coord, dial)
}

fn finish(
    cwd: &str,
    trust: &TrustVerdict,
    config_dir: Option<&str>,
    child_config_dir: Option<&str>,
    coord: CoordRowLookup,
    dial: DialResolution,
) -> TrustGateReport {
    let (posture, posture_rule) = posture_for(&dial);
    let write_target = config_file_for(config_dir);
    let child_reads = config_file_for(child_config_dir);
    let roots = agent_worktree_roots();
    let project_key = workspace_trust::project_key(Path::new(cwd));

    let read_parent = |key: &str| match &write_target {
        Some(file) => super::spawn_preconditions::trust_verdict_in(file, key),
        None => TrustVerdict::Unknown {
            reason: "no account config file resolved".to_string(),
        },
    };
    let (conjuncts, parent) = derive_conjuncts(
        Path::new(cwd),
        &coord,
        &roots,
        write_target.as_deref(),
        child_reads.as_deref(),
        read_parent,
    );
    let decision = decide(trust, &conjuncts, posture);

    let mut write_outcome = None;
    if decision.writes_trust() {
        if let (Some(file), Some(key)) = (&write_target, &project_key) {
            let outcome = workspace_trust::ensure_trusted_in(file, key);
            // The one line that answers "why is this directory trusted?".
            // `info!`, not `debug!`: this is the record that a live,
            // credential-bearing account file was rewritten to grant a
            // security control, and it must be visible at the default level.
            match &outcome {
                TrustOutcome::Trusted => info!(
                    project_key = %key,
                    config_file = %file.display(),
                    parent_repo = parent.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "<none>".into()),
                    rule = decision.rule(),
                    dial = ?dial,
                    posture = ?posture,
                    posture_rule,
                    derivation = %conjuncts.derivation(),
                    "trust gate: GRANTED workspace trust — derivation recorded"
                ),
                other => debug!(
                    project_key = %key,
                    config_file = %file.display(),
                    outcome = ?other,
                    "trust gate: grant made no change"
                ),
            }
            write_outcome = Some(format!("{outcome:?}"));
        }
    }

    match &decision {
        TrustGateDecision::AlreadyTrusted => debug!(
            cwd = %cwd,
            project_key = project_key.as_deref().unwrap_or("<underivable>"),
            "trust gate: already trusted — nothing minted, nothing gated"
        ),
        TrustGateDecision::Grant { .. } => {}
        TrustGateDecision::Withhold { rule, failed } => warn!(
            cwd = %cwd,
            project_key = project_key.as_deref().unwrap_or("<underivable>"),
            rule,
            dial = ?dial,
            posture_rule,
            failed = %failed.join("; "),
            derivation = %conjuncts.derivation(),
            "trust gate: WITHHELD the trust write — the spawn proceeds and may face the \
             workspace-trust dialog or silently lose this workspace's hooks and MCP servers"
        ),
        TrustGateDecision::SpawnBlocked { rule, failed } => warn!(
            cwd = %cwd,
            project_key = project_key.as_deref().unwrap_or("<underivable>"),
            rule,
            dial = ?dial,
            posture_rule,
            failed = %failed.join("; "),
            derivation = %conjuncts.derivation(),
            "trust gate: SPAWN BLOCKED — workspace trust could not be derived and the tenant's \
             autonomy dial forbids minting it"
        ),
    }

    TrustGateReport {
        cwd: cwd.to_string(),
        project_key,
        account_config_file: write_target.map(|p| p.to_string_lossy().into_owned()),
        parent_repo: parent.map(|p| p.to_string_lossy().into_owned()),
        trust: trust.clone(),
        conjuncts,
        dial,
        posture,
        decision,
        write_outcome,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---------------------------------------------------------------- dial

    #[test]
    fn dial_parses_the_authored_clause_shape() {
        assert_eq!(
            parse_autonomy_dial("preamble\n    implement_tier: proceed\ntrailing prose\n"),
            DialParse::Present(AutonomyTier::Proceed)
        );
        assert_eq!(
            parse_autonomy_dial("implement_tier: `draft-required`"),
            DialParse::Present(AutonomyTier::DraftRequired)
        );
        assert_eq!(
            parse_autonomy_dial("  implement_tier: ask-first  — and the prose names proceed too"),
            DialParse::Present(AutonomyTier::AskFirst)
        );
    }

    /// The parser run over the SHAPE THIS TENANT ACTUALLY SERVES, excerpted
    /// verbatim from `policy/security-and-autonomy` v6 (read 2026-09-06). The
    /// trap it pins: the prose immediately below the clause names all three
    /// tiers in backticks, so anything looser than a line-anchored parse would
    /// "find" a tier in the explanation rather than in the clause.
    #[test]
    fn the_parser_reads_the_clause_this_tenant_serves() {
        let body = "\
## clause: security-surface-implement-tier (operator-authored 2026-07-21)
    implement_tier: proceed

THE AUTONOMY DIAL. One of:
- `proceed` — implement and let it land like ordinary work.
- `draft-required` — implement fully, but open the PR with `gh pr create --draft`.
- `ask-first` — do not implement. Stop at a vetted plan and surface it.
";
        assert_eq!(
            parse_autonomy_dial(body),
            DialParse::Present(AutonomyTier::Proceed)
        );
        // …and the same document with the clause value tightened resolves the
        // other way, which is what makes the dial an operator lever at all.
        assert_eq!(
            parse_autonomy_dial(
                &body.replace("implement_tier: proceed", "implement_tier: ask-first")
            ),
            DialParse::Present(AutonomyTier::AskFirst)
        );
    }

    /// The three outcomes must stay distinguishable: a suppressed parse failure
    /// read as "unchanged" is the false-assurance mode coord's own `DialParse`
    /// exists to prevent.
    #[test]
    fn dial_keeps_unrecognised_absent_and_present_distinct() {
        assert_eq!(
            parse_autonomy_dial("implement_tier: procede"),
            DialParse::Unrecognised("procede".to_string())
        );
        assert_eq!(
            parse_autonomy_dial("implement_tier:   "),
            DialParse::Unrecognised(String::new())
        );
        assert_eq!(
            parse_autonomy_dial("a document that discusses proceed and ask-first in prose"),
            DialParse::Absent
        );
    }

    #[test]
    fn every_dial_resolution_maps_to_a_posture() {
        assert_eq!(
            posture_for(&DialResolution::Fresh {
                tier: AutonomyTier::Proceed
            })
            .0,
            EnforcementPosture::Report
        );
        assert_eq!(
            posture_for(&DialResolution::Fresh {
                tier: AutonomyTier::DraftRequired
            })
            .0,
            EnforcementPosture::Withhold
        );
        assert_eq!(
            posture_for(&DialResolution::Fresh {
                tier: AutonomyTier::AskFirst
            })
            .0,
            EnforcementPosture::Block
        );
        assert_eq!(
            posture_for(&DialResolution::Stale {
                tier: AutonomyTier::Proceed,
                age_secs: 300
            })
            .0,
            EnforcementPosture::Report,
            "a last-known-good tier still decides; it is flagged, not discarded"
        );
    }

    /// The dial's OWN absence — coord answered, the document states no readable
    /// tier — reads as the most conservative posture.
    #[test]
    fn the_dials_own_absence_is_most_conservative() {
        for dial in [
            DialResolution::Absent,
            DialResolution::Unrecognised {
                raw: "yolo".to_string(),
            },
        ] {
            assert_eq!(
                posture_for(&dial).0,
                EnforcementPosture::Block,
                "{dial:?} is the dial's own absence and must read as the most conservative posture"
            );
        }
    }

    /// A FAILED READ is not the dial's absence. It withholds the mint — which
    /// already honours the control this plan protects — without letting a coord
    /// outage or a cold cache stop every underived spawn on the fleet.
    #[test]
    fn a_failed_read_withholds_rather_than_blocking() {
        let (posture, rule) = posture_for(&DialResolution::Unresolved {
            error: "coord unreachable".to_string(),
        });
        assert_eq!(posture, EnforcementPosture::Withhold);
        assert_eq!(rule, "dial-unresolved-read");
        // …and it is still strictly more conservative than the landed behaviour.
        assert_ne!(posture, EnforcementPosture::Report);
    }

    /// …but an UNPAIRED runner is not an unreadable dial. It has no tenant, so
    /// there is no preference to honour. Collapsing the two is the
    /// absence-is-not-zero error this plan is about.
    #[test]
    fn unpaired_is_not_collapsed_into_unresolved() {
        assert_eq!(
            posture_for(&DialResolution::Unpaired).0,
            EnforcementPosture::Report
        );
        assert_ne!(
            posture_for(&DialResolution::Unpaired).0,
            posture_for(&DialResolution::Unresolved {
                error: "x".to_string()
            })
            .0,
            "an unpaired runner and a paired one whose read failed must not collapse"
        );
        assert_ne!(
            posture_for(&DialResolution::Unpaired).0,
            posture_for(&DialResolution::Absent).0,
            "…and neither of those is the dial's own absence"
        );
    }

    // ------------------------------------------------------------ conjunct 1

    fn matched() -> CoordRowLookup {
        CoordRowLookup::Matched {
            session_id: "s1".to_string(),
        }
    }
    fn unavailable() -> CoordRowLookup {
        CoordRowLookup::Unavailable {
            reason: "coord unreachable".to_string(),
        }
    }
    fn under_root() -> LocalWorktreeShape {
        LocalWorktreeShape::UnderAgentWorktreeRoot {
            parent: PathBuf::from("D:/w/qontinui-runner"),
            root: "coord-allocated".to_string(),
        }
    }

    #[test]
    fn a_coord_row_decides_conjunct_one_and_says_so() {
        let v = decide_worktree_conjunct(&matched(), &LocalWorktreeShape::NotALinkedWorktree);
        assert!(v.is_satisfied());
        assert_eq!(v.decided_by(), "coord_allocation_row");
    }

    #[test]
    fn a_coord_outage_falls_to_labelled_local_evidence() {
        let v = decide_worktree_conjunct(&unavailable(), &under_root());
        assert!(v.is_satisfied());
        assert_eq!(
            v.decided_by(),
            "local_worktree_layout",
            "the weaker evidence must be LABELLED, not laundered into a coord answer"
        );
    }

    #[test]
    fn a_plain_directory_fails_conjunct_one_even_with_no_coord() {
        let v = decide_worktree_conjunct(&unavailable(), &LocalWorktreeShape::NotALinkedWorktree);
        assert_eq!(v.label(), "failed");
    }

    /// coord answering "no row" while the tree sits under an agent-worktree root
    /// is a DISAGREEMENT, and a disagreement is UNKNOWN — never a confident
    /// failure and never a confident pass.
    #[test]
    fn coord_and_local_disagreeing_reads_unknown() {
        let v = decide_worktree_conjunct(&CoordRowLookup::NoRow, &under_root());
        assert_eq!(v.label(), "unknown");
    }

    #[test]
    fn an_undetermined_local_shape_is_never_a_pass() {
        let v = decide_worktree_conjunct(
            &unavailable(),
            &LocalWorktreeShape::Undetermined {
                reason: "unreadable".to_string(),
            },
        );
        assert_eq!(v.label(), "unknown");
        assert!(!v.is_satisfied());
    }

    // ----------------------------------------------------- gitdir + layout

    #[test]
    fn the_primary_checkout_is_read_off_the_gitdir_line() {
        assert_eq!(
            primary_checkout_from_gitdir(
                "gitdir: D:/qontinui-root/qontinui-runner/.git/worktrees/qontinui-runner32\n"
            ),
            Some(PathBuf::from("D:/qontinui-root/qontinui-runner"))
        );
        // Backslash spelling, which a hand-edited file can carry.
        assert_eq!(
            primary_checkout_from_gitdir(r"gitdir: D:\w\repo\.git\worktrees\wt1"),
            Some(PathBuf::from("D:/w/repo"))
        );
        // Not a linked worktree marker at all.
        assert_eq!(primary_checkout_from_gitdir("gitdir: D:/w/repo/.git"), None);
        assert_eq!(primary_checkout_from_gitdir("ref: refs/heads/main"), None);
        assert_eq!(primary_checkout_from_gitdir("gitdir:   "), None);
    }

    /// The measured shape that a depth-2 assumption misses: 24 of this fleet's
    /// allocated trees sit at `<root>/<agent-id>/<owner>/<repo>`.
    #[test]
    fn the_layout_check_is_depth_agnostic() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("agent-worktrees");
        let deep = root.join("01a0-uuid").join("qontinui").join("schemas");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(
            deep.join(".git"),
            "gitdir: D:/w/qontinui-schemas/.git/worktrees/wt9\n",
        )
        .unwrap();
        let roots = vec![("coord-allocated".to_string(), root)];
        match local_worktree_shape(&deep, &roots) {
            LocalWorktreeShape::UnderAgentWorktreeRoot { parent, root } => {
                assert_eq!(parent, PathBuf::from("D:/w/qontinui-schemas"));
                assert_eq!(root, "coord-allocated");
            }
            other => panic!("a three-segment allocated tree must still match: {other:?}"),
        }
    }

    /// A spawn into a SUBDIRECTORY of an allocated worktree must classify the
    /// worktree, not the subdirectory — otherwise conjunct 1 fails for every
    /// such spawn while `project_key` happily keys the same tree.
    #[test]
    fn a_subdirectory_of_a_worktree_classifies_the_worktree() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("agent-worktrees");
        let wt = root.join("01a0-uuid").join("qontinui-runner");
        let deep = wt.join("src-tauri").join("src");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(
            wt.join(".git"),
            "gitdir: D:/w/qontinui-runner/.git/worktrees/wt1\n",
        )
        .unwrap();
        let roots = vec![("coord-allocated".to_string(), root)];
        match local_worktree_shape(&deep, &roots) {
            LocalWorktreeShape::UnderAgentWorktreeRoot { parent, .. } => {
                assert_eq!(parent, PathBuf::from("D:/w/qontinui-runner"))
            }
            other => panic!("a subdirectory spawn must classify its worktree: {other:?}"),
        }
    }

    #[test]
    fn a_primary_checkout_is_not_a_linked_worktree() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        assert_eq!(
            local_worktree_shape(&repo, &[]),
            LocalWorktreeShape::NotALinkedWorktree
        );
    }

    #[test]
    fn a_hand_rolled_worktree_outside_every_root_is_named_as_such() {
        let tmp = tempfile::tempdir().unwrap();
        let wt = tmp.path().join("_wt-hand");
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(wt.join(".git"), "gitdir: D:/w/repo/.git/worktrees/hand\n").unwrap();
        let roots = vec![(
            "coord-allocated".to_string(),
            tmp.path().join("agent-worktrees"),
        )];
        assert!(matches!(
            local_worktree_shape(&wt, &roots),
            LocalWorktreeShape::LinkedWorktreeElsewhere { .. }
        ));
    }

    // ------------------------------------------------------------ conjunct 2

    #[test]
    fn an_untrusted_parent_fails_conjunct_two() {
        let v = decide_parent_conjunct(Some(Path::new(".")), |_| TrustVerdict::Untrusted {
            reason: "no entry for this project key",
        });
        // `.` is relative, so the key is underivable → UNKNOWN, which is also
        // not a pass. Exercise the trusted/untrusted arms against a real dir.
        assert!(!v.is_satisfied());

        let tmp = tempfile::tempdir().unwrap();
        let untrusted = decide_parent_conjunct(Some(tmp.path()), |_| TrustVerdict::Untrusted {
            reason: "hasTrustDialogAccepted is explicitly false",
        });
        assert_eq!(untrusted.label(), "failed");
        assert!(untrusted.detail().contains("inherited, never"));
    }

    #[test]
    fn a_trusted_parent_satisfies_conjunct_two() {
        let tmp = tempfile::tempdir().unwrap();
        let v = decide_parent_conjunct(Some(tmp.path()), |_| TrustVerdict::Trusted);
        assert!(v.is_satisfied());
        assert_eq!(v.decided_by(), "parent_project_key");
    }

    #[test]
    fn an_unreadable_parent_config_is_unknown_not_a_pass() {
        let tmp = tempfile::tempdir().unwrap();
        let v = decide_parent_conjunct(Some(tmp.path()), |_| TrustVerdict::Unknown {
            reason: "config did not parse".to_string(),
        });
        assert_eq!(v.label(), "unknown");
        assert!(!v.is_satisfied());
    }

    #[test]
    fn no_parent_repo_is_unknown() {
        let v = decide_parent_conjunct(None, |_| TrustVerdict::Trusted);
        assert_eq!(v.label(), "unknown");
    }

    // ------------------------------------------------------------ conjunct 3

    #[test]
    fn conjunct_three_compares_the_two_resolutions() {
        let a = PathBuf::from("D:/acc/one/.claude.json");
        let b = PathBuf::from("D:/acc/two/.claude.json");
        assert!(decide_config_dir_conjunct(Some(&a), Some(&a)).is_satisfied());
        assert_eq!(
            decide_config_dir_conjunct(Some(&a), Some(&b)).label(),
            "failed"
        );
        assert_eq!(
            decide_config_dir_conjunct(Some(&a), None).label(),
            "unknown"
        );
    }

    // -------------------------------------------------------- the decision

    fn conj(a: bool, b: bool, c: bool) -> Conjuncts {
        let mk = |ok: bool| {
            if ok {
                ConjunctVerdict::Satisfied {
                    decided_by: "test",
                    detail: String::new(),
                }
            } else {
                ConjunctVerdict::Failed {
                    decided_by: "test",
                    detail: "test failure".to_string(),
                }
            }
        };
        Conjuncts {
            coord_worktree_row: mk(a),
            parent_repo_trusted: mk(b),
            config_dir_pinned: mk(c),
        }
    }

    fn untrusted() -> TrustVerdict {
        TrustVerdict::Untrusted {
            reason: "no entry for this project key",
        }
    }

    /// An already-trusted target mints nothing, so no dial position can block
    /// it. This is what keeps the strict arm from breaking ordinary spawns.
    #[test]
    fn already_trusted_is_never_gated_at_any_posture() {
        for posture in [
            EnforcementPosture::Report,
            EnforcementPosture::Withhold,
            EnforcementPosture::Block,
        ] {
            assert_eq!(
                decide(&TrustVerdict::Trusted, &conj(false, false, false), posture),
                TrustGateDecision::AlreadyTrusted
            );
        }
    }

    #[test]
    fn all_three_conjuncts_grant_at_every_posture() {
        for posture in [
            EnforcementPosture::Report,
            EnforcementPosture::Withhold,
            EnforcementPosture::Block,
        ] {
            let d = decide(&untrusted(), &conj(true, true, true), posture);
            assert!(d.writes_trust(), "{posture:?}");
            assert_eq!(d.rule(), "derived-from-parent-repo-trust");
        }
    }

    /// **The dial flip, in BOTH directions**, which is the phase's gate.
    #[test]
    fn flipping_the_dial_changes_the_outcome_both_ways() {
        let c = conj(true, false, true); // conjunct 2 fails
        let permissive = decide(&untrusted(), &c, EnforcementPosture::Report);
        let strict = decide(&untrusted(), &c, EnforcementPosture::Block);

        assert!(
            permissive.writes_trust() && permissive.allows_spawn(),
            "the permissive setting must preserve the landed behaviour: write, spawn"
        );
        assert!(
            !strict.writes_trust() && !strict.allows_spawn(),
            "the conservative setting must withhold the mint AND refuse the spawn"
        );
        assert_ne!(permissive, strict);
        // …and back: the same conjuncts under the permissive posture again.
        assert_eq!(
            decide(&untrusted(), &c, EnforcementPosture::Report),
            permissive
        );
    }

    #[test]
    fn the_middle_posture_withholds_the_mint_but_still_spawns() {
        let d = decide(
            &untrusted(),
            &conj(true, false, true),
            EnforcementPosture::Withhold,
        );
        assert!(!d.writes_trust());
        assert!(d.allows_spawn());
        assert_eq!(d.label(), "withhold");
    }

    /// Each conjunct must be able to block ALONE — otherwise two of them are
    /// decoration.
    #[test]
    fn each_conjunct_blocks_on_its_own() {
        for (i, c) in [
            conj(false, true, true),
            conj(true, false, true),
            conj(true, true, false),
        ]
        .into_iter()
        .enumerate()
        {
            let d = decide(&untrusted(), &c, EnforcementPosture::Block);
            assert!(
                !d.allows_spawn(),
                "conjunct {i} must be able to block on its own"
            );
            match &d {
                TrustGateDecision::SpawnBlocked { failed, .. } => {
                    assert_eq!(failed.len(), 1, "exactly the one failing conjunct is named")
                }
                other => panic!("expected SpawnBlocked, got {other:?}"),
            }
        }
    }

    /// An UNKNOWN conjunct is not a pass — the whole absence-is-UNKNOWN rule,
    /// asserted at the decision layer rather than only at the probes.
    #[test]
    fn an_unknown_conjunct_is_not_a_pass() {
        let c = Conjuncts {
            coord_worktree_row: ConjunctVerdict::Unknown {
                reason: "coord unreachable and shape undetermined".to_string(),
            },
            parent_repo_trusted: ConjunctVerdict::Satisfied {
                decided_by: "test",
                detail: String::new(),
            },
            config_dir_pinned: ConjunctVerdict::Satisfied {
                decided_by: "test",
                detail: String::new(),
            },
        };
        assert!(!c.all_satisfied());
        assert_eq!(c.failing().len(), 1);
        assert!(!decide(&untrusted(), &c, EnforcementPosture::Block).allows_spawn());
    }

    #[test]
    fn a_blocked_spawn_carries_a_typed_refusal_naming_the_conjunct() {
        let d = decide(
            &untrusted(),
            &conj(true, false, true),
            EnforcementPosture::Block,
        );
        let r = d.refusal().expect("a block must carry a refusal");
        assert!(r.starts_with("spawn_blocked ("), "{r}");
        assert!(r.contains("parent_repo_trusted"), "{r}");
        assert!(decide(
            &untrusted(),
            &conj(true, true, true),
            EnforcementPosture::Block
        )
        .refusal()
        .is_none());
    }

    #[test]
    fn the_derivation_line_names_every_conjunct_and_how_it_was_decided() {
        let line = conj(true, false, true).derivation();
        for name in [
            "coord_worktree_row=satisfied",
            "parent_repo_trusted=failed",
            "config_dir_pinned=satisfied",
        ] {
            assert!(line.contains(name), "{line}");
        }
        assert!(line.contains("by test"), "{line}");
    }

    /// The field-observed shape: an entry that EXISTS with the flag explicitly
    /// `false`. It must reach the gate as untrusted, not as trusted.
    #[test]
    fn the_field_observed_explicit_false_shape_reaches_the_gate_as_untrusted() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join(workspace_trust::CONFIG_FILE);
        std::fs::write(
            &file,
            r#"{"projects":{"D:/w/repo":{"hasTrustDialogAccepted":false,"allowedTools":[]}}}"#,
        )
        .unwrap();
        let v = super::super::spawn_preconditions::trust_verdict_in(&file, "D:/w/repo");
        assert_eq!(
            v,
            TrustVerdict::Untrusted {
                reason: "hasTrustDialogAccepted is explicitly false"
            }
        );
        // …and the parent conjunct built from that same read fails rather than
        // passing, which is the arm that stops trust being minted for a repo a
        // human explicitly declined.
        let parent = decide_parent_conjunct(Some(tmp.path()), |k| {
            super::super::spawn_preconditions::trust_verdict_in(&file, k)
        });
        assert_eq!(parent.label(), "failed");
        assert!(!decide(&v, &conj(true, false, true), EnforcementPosture::Block).allows_spawn());
    }
}
