//! Make a stalled spawn **observable**: typed workspace-trust and coord-credential
//! preconditions, computed for the account a spawn actually resolved, plus the
//! bounded no-output window that turns a silent hang into a typed report.
//!
//! Plan `2026-08-20-worktree-spawn-autonomy-and-trust-preconditions`, Phase 1.
//!
//! ## The defect this instrument exists to remove
//!
//! An autonomous spawn into a not-yet-trusted directory hangs on a TUI prompt no
//! one is there to answer, and the runner reports *nothing at all* — no exit, no
//! failure, no log line. From coord's side a stalled spawn and a slow one are the
//! same observation. Separately, a spawn that cannot reach coord's allocation door
//! fails in a way that every shipped reader reported as an undifferentiated
//! "unknown", so "no credential exists" and "a credential exists and our reader
//! discarded it" produced the same verdict.
//!
//! **Nothing here acts on a verdict.** Every spawn that would have happened still
//! happens, whatever this module says; this phase only has to make the two states
//! nameable and distinct. The acting-on-it half landed as Phases 2 and 4 and
//! lives in [`crate::claude_session::trust_gate`], which consumes
//! [`TrustVerdict`] from here rather than re-probing it — a second trust probe
//! would key a different question and still read as authoritative.
//!
//! ## Absence is UNKNOWN, never zero
//!
//! Every verdict carries a *reason* on the arm that could not conclude. An
//! undifferentiated `Unknown` is exactly the defect being removed, so:
//!
//! * [`TrustVerdict::Untrusted`] says WHY it read untrusted (no entry / the flag
//!   is explicitly `false` — the shape actually observed in the field / no
//!   `projects` map / no config file at all), and [`TrustVerdict::Unknown`] is
//!   reserved for the probe that genuinely could not run (unreadable config,
//!   unparseable config, no key derivable, no config dir resolved).
//! * [`CredentialVerdict::CredentialAbsent`] and
//!   [`CredentialVerdict::CredentialUnreadable`] are separate arms, decided by
//!   whether any *independent* credential source holds bytes while our reader
//!   returned nothing. The instrument must never infer the credential's state
//!   from its own reader's silence.
//!
//! ## Trust: one key derivation, not two
//!
//! The trust *verdict* reuses [`workspace_trust::project_key`] and the same
//! `$CLAUDE_CONFIG_DIR/.claude.json` → `projects[<key>].hasTrustDialogAccepted`
//! contract as the trust *write* that already ships beside it. A second, divergent
//! key derivation would produce a verdict about a key the CLI never looks up —
//! which is worse than no verdict, because it reads as authoritative.
//!
//! **The verdict is taken BEFORE the pre-accept write.** Taken after, it would
//! read `Trusted` on every spawn by construction and answer nothing. What it
//! reports is therefore "would this spawn have faced the trust dialog?", which is
//! the observable the phase is for.
//!
//! ## The credential ladder
//!
//! One request per rung, stopping at the first that settles it, so the whole thing
//! is cheap enough to run per spawn:
//!
//! | Rung | Observation | Verdict |
//! |---|---|---|
//! | local | no source holds a credential | `CredentialAbsent` |
//! | local | a source holds one, our reader returned nothing | `CredentialUnreadable` |
//! | `OPTIONS` allocate | timeout / no response | `CoordUnreachable` |
//! | `OPTIONS` allocate | any status | the allocation door is live -- says nothing about the credential |
//! | authed `GET` | timeout / no response | `CoordUnreachable` |
//! | authed `GET` | `2xx` | `CredentialOk` -- coord verified the bearer |
//! | authed `GET` | `400` / `422` | `CredentialOk` -- **accepted**, rejected on content |
//! | authed `GET` | `403` | `WrongTier` -- accepted, wrong door, **not** an auth failure |
//! | authed `GET` | `401` | `CredentialAbsentOrExpired` |
//! | authed `GET` | anything else | `Unmapped` -- named, never folded into an auth verdict |
//!
//! A `403 tenant_not_resolved` is the wrong *door* (the `agent-` twin is the right
//! one), not a wrong credential. Folding it into an auth verdict is the named risk
//! this table exists to avoid.
//!
//! ### The plan asked the credential rung to be a `POST` to the allocation door. It cannot be.
//!
//! The plan specifies that rung as `POST /agents/allocate` with a deliberately
//! malformed body, reading `422` as `credential_ok` and `401` as
//! `credential_absent_or_expired`. **Measured against the live hosted coord on
//! 2026-09-06, that is false, and a ladder built on it would have been a
//! permanently-green instrument:**
//!
//! ```text
//! OPTIONS /agents/allocate                                  -> 405  (route live)
//! POST    /agents/allocate  '"probe"'       no auth         -> 422  invalid type: string ... expected struct AllocateRequest
//! POST    /agents/allocate  '"probe"'       garbage bearer  -> 422  (identical)
//! POST    /agents/allocate  '{}'            no auth         -> 422  missing field `device_id`
//! POST    /agents/allocate  well-formed     no auth         -> 400  device_id ... is not registered in coord.devices
//! ```
//!
//! The body extractor runs BEFORE any auth extractor, and the handler then
//! authenticates from the body's own `device_id` rather than from a bearer -- so
//! **`401` is unreachable on that route, and `422` comes back with no credential
//! at all.** A ladder mapping `422 -> credential_ok` there would report
//! `credential_ok` on a box holding no credential whatsoever, which is exactly the
//! class of defect this phase exists to delete. The plan's own falsification
//! clause covers it: this falsifies the *instrument*, so the instrument changed.
//!
//! What is implemented keeps both halves of the plan's intent and drops only the
//! mechanism:
//!
//! * the **allocation door is still probed** -- `OPTIONS /agents/allocate`, whose
//!   status is recorded verbatim in
//!   [`CredentialPrecondition::allocate_door_status`], so "the door a spawning
//!   agent needs answered" stays an observable;
//! * the **credential rung moves to a door where the bearer IS the gate** --
//!   [`CREDENTIAL_PATH`], measured `401` both unauthenticated and with a garbage
//!   bearer on the same day, so its `401` means what the plan's table says.
//!
//! `exp` is decoded (unverified — this is a liveness read, not a validation) so
//! *presence is never mistaken for validity*. [`ExpRead`] separates "not a JWT"
//! from "a JWT carrying no `exp` claim", because a token that never expires and a
//! token we cannot parse are different facts.

use std::path::{Path, PathBuf};
use std::time::Duration;

use base64::Engine as _;
use serde::Serialize;
use tokio::sync::watch;
use tracing::{debug, info, warn};

use super::workspace_trust::{self, CONFIG_FILE, PROJECTS_KEY, TRUST_FLAG};

// =============================================================================
// Tunables
// =============================================================================

/// How long a freshly spawned child may produce **no output at all** before the
/// spawn is reported `spawn_stalled`.
///
/// Justification for 120s. A healthy headless `claude -p` spawn's first output is
/// gated on CLI boot, the workspace's MCP server handshakes and model
/// time-to-first-token; on a loaded box in this fleet even the runner's own
/// `/health` has been sampled at 10s, and coord-mcp provisioning sits in front of
/// the spawn. A window under ~60s would therefore fire on healthy-but-slow spawns
/// and the report would become noise. Against that, the failure being detected —
/// a trust dialog with no one to answer it — is *unbounded*, so any finite window
/// separates the two classes; the only cost of a generous one is how long the
/// operator waits for the report. 120s is roughly an order of magnitude above the
/// healthy tail and still inside a single operator attention span.
const DEFAULT_STALL_WINDOW_SECS: u64 = 120;

/// Override for [`DEFAULT_STALL_WINDOW_SECS`], in seconds. `0` disables stall
/// reporting entirely (the watcher is never armed); an unparseable value is
/// ignored in favour of the default rather than silently disabling the
/// instrument.
const STALL_WINDOW_ENV: &str = "QONTINUI_SPAWN_STALL_SECS";

/// Per-rung HTTP timeout. Two rungs, so the whole ladder is bounded by ~2× this.
/// Short on purpose: the ladder runs concurrently with a spawn, and a rung that
/// has not answered in 3s is reported as `CoordUnreachable`, which is the honest
/// verdict for a door that cannot answer inside a spawn's own lifetime.
const RUNG_TIMEOUT: Duration = Duration::from_secs(3);

/// The allocation door itself. Probed for LIVENESS only (`OPTIONS`), because it
/// authenticates from the request body's `device_id` rather than from a bearer --
/// see the module docs for the measurement.
const ALLOCATE_PATH: &str = "/agents/allocate";

/// The door the CREDENTIAL rung probes: an agent-facing, device-JWT-authed READ.
/// Measured 2026-09-06 on the hosted coord: `401` with no bearer and `401` with a
/// garbage bearer, so a `401` here is an auth answer and not a schema answer. A
/// read, so the rung mutates nothing however the credential resolves.
const CREDENTIAL_PATH: &str = "/coord/agent-gates";

/// The public hosted coord, used when no profile resolves one. Matches the
/// documented `$COORD_HTTP_URL` default.
const DEFAULT_COORD_BASE: &str = "https://coord.qontinui.io";

// =============================================================================
// Trust
// =============================================================================

/// Whether the `(project key, account config dir)` pair a spawn resolved reads
/// trusted — i.e. whether the CLI will find `hasTrustDialogAccepted: true` where
/// it is about to look.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "verdict", rename_all = "snake_case")]
pub enum TrustVerdict {
    /// The flag is present and `true`. The spawn will not face the dialog.
    Trusted,
    /// The config was read and understood, and it does not grant trust. `reason`
    /// distinguishes the field-observed explicit-`false` shape from a plain
    /// missing entry — they need different fixes.
    Untrusted { reason: &'static str },
    /// The probe could not conclude. Always carries why; an undifferentiated
    /// unknown is the defect this module exists to remove.
    Unknown { reason: String },
}

impl TrustVerdict {
    /// A short, stable label for logs and dashboards.
    pub fn label(&self) -> &'static str {
        match self {
            TrustVerdict::Trusted => "trusted",
            TrustVerdict::Untrusted { .. } => "untrusted",
            TrustVerdict::Unknown { .. } => "unknown",
        }
    }
}

/// The full derivation of a trust verdict — every input, so the audit trail can
/// answer "why did this read untrusted?" without re-running anything.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TrustPrecondition {
    /// The directory the child will be spawned in.
    pub cwd: String,
    /// The `projects` key the CLI will look up — the enclosing git root, not the
    /// cwd. `None` when it could not be derived.
    pub project_key: Option<String>,
    /// The account config FILE the verdict was read from.
    pub config_file: Option<String>,
    pub verdict: TrustVerdict,
}

/// Read `projects[key].hasTrustDialogAccepted` out of one account config file.
///
/// Pure over the filesystem, so the fixture cases (flag true / explicitly false /
/// entry absent / file absent / unparseable) are directly testable.
///
/// **A missing config file is `Untrusted`, not `Unknown`.** The file is the exact
/// one the child's pinned `CLAUDE_CONFIG_DIR` points at, and a CLI that finds no
/// config there creates a fresh one with no `projects` map — which means the
/// dialog. That is a conclusion, not an absence of one.
pub fn trust_verdict_in(config_file: &Path, key: &str) -> TrustVerdict {
    let raw = match std::fs::read_to_string(config_file) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return TrustVerdict::Untrusted {
                reason: "account config file absent",
            };
        }
        Err(e) => {
            return TrustVerdict::Unknown {
                reason: format!("config unreadable: {e}"),
            };
        }
    };
    let doc: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(e) => {
            return TrustVerdict::Unknown {
                reason: format!("config did not parse: {e}"),
            };
        }
    };
    let Some(projects) = doc.get(PROJECTS_KEY) else {
        return TrustVerdict::Untrusted {
            reason: "config has no projects map",
        };
    };
    let Some(projects) = projects.as_object() else {
        return TrustVerdict::Unknown {
            reason: "projects is not a JSON object".to_string(),
        };
    };
    let Some(entry) = projects.get(key) else {
        return TrustVerdict::Untrusted {
            reason: "no entry for this project key",
        };
    };
    match entry.get(TRUST_FLAG) {
        Some(serde_json::Value::Bool(true)) => TrustVerdict::Trusted,
        Some(serde_json::Value::Bool(false)) => TrustVerdict::Untrusted {
            reason: "hasTrustDialogAccepted is explicitly false",
        },
        Some(other) => TrustVerdict::Unknown {
            reason: format!("hasTrustDialogAccepted is not a boolean: {other}"),
        },
        None => TrustVerdict::Untrusted {
            reason: "entry carries no hasTrustDialogAccepted flag",
        },
    }
}

/// Resolve the account config FILE a spawn's verdict must be read from.
///
/// `config_dir` is the dir the spawn pinned into `CLAUDE_CONFIG_DIR`; `None`
/// means the child inherits the ambient default, which is `~/.claude.json`.
fn config_file_for(config_dir: Option<&str>) -> Result<PathBuf, String> {
    match config_dir {
        Some(dir) => Ok(Path::new(dir).join(CONFIG_FILE)),
        None => dirs::home_dir()
            .map(|h| h.join(CONFIG_FILE))
            .ok_or_else(|| "no config dir resolved and no home directory".to_string()),
    }
}

/// The pre-spawn trust check, for an EXPLICIT `(cwd, config dir)` pair.
///
/// The config dir must be the one the spawn actually resolved. A verdict computed
/// against ambient env instead would be a silent no-op that makes the log claim
/// trust while the spawn still hangs — the named risk in the plan.
pub fn trust_precondition(cwd: &str, config_dir: Option<&str>) -> TrustPrecondition {
    if cwd.trim().is_empty() {
        return TrustPrecondition {
            cwd: cwd.to_string(),
            project_key: None,
            config_file: None,
            verdict: TrustVerdict::Unknown {
                reason: "working dir is empty".to_string(),
            },
        };
    }
    let Some(key) = workspace_trust::project_key(Path::new(cwd)) else {
        return TrustPrecondition {
            cwd: cwd.to_string(),
            project_key: None,
            config_file: None,
            verdict: TrustVerdict::Unknown {
                reason: "project key underivable (path is relative or did not resolve)".to_string(),
            },
        };
    };
    let file = match config_file_for(config_dir) {
        Ok(f) => f,
        Err(reason) => {
            return TrustPrecondition {
                cwd: cwd.to_string(),
                project_key: Some(key),
                config_file: None,
                verdict: TrustVerdict::Unknown { reason },
            };
        }
    };
    let verdict = trust_verdict_in(&file, &key);
    TrustPrecondition {
        cwd: cwd.to_string(),
        project_key: Some(key),
        config_file: Some(file.to_string_lossy().into_owned()),
        verdict,
    }
}

// =============================================================================
// Credential — the local rung
// =============================================================================

/// What a JWT's payload says about its own expiry, WITHOUT verifying anything.
///
/// Three outcomes, deliberately not two: a token that is not a JWT at all and a
/// JWT that simply carries no `exp` claim are different facts, and collapsing
/// them would report an eternal credential as garbage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "exp", rename_all = "snake_case")]
pub enum ExpRead {
    /// A decodable JWT carrying `exp` (unix seconds).
    At { unix: i64 },
    /// A decodable JWT payload with no `exp` claim — never expires by its own
    /// statement.
    NoClaim,
    /// Not a 3-segment JWT, or the payload did not base64/JSON-decode. A legacy
    /// opaque `qontinui_runner_<random>` bearer lands here.
    NotAJwt,
}

/// Decode a JWT's `exp` claim without verifying the signature.
///
/// Deliberately NOT `auth::decode_jwt_exp`: that helper collapses "no `exp`
/// claim" into `None` alongside "not a JWT", which is precisely the distinction
/// this instrument is required to make. Signature verification stays out of scope
/// — coord re-verifies on every call; this is a liveness read.
pub fn read_exp(token: &str) -> ExpRead {
    let token = token.trim();
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return ExpRead::NotAJwt;
    }
    // The JWT spec mandates unpadded base64url; accept the padded form too, as
    // the rest of this codebase's decoders do.
    let Ok(payload) = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(parts[1])
        .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(parts[1]))
    else {
        return ExpRead::NotAJwt;
    };
    let Ok(claims) = serde_json::from_slice::<serde_json::Value>(&payload) else {
        return ExpRead::NotAJwt;
    };
    if !claims.is_object() {
        return ExpRead::NotAJwt;
    }
    match claims.get("exp").and_then(|v| v.as_i64()) {
        Some(unix) => ExpRead::At { unix },
        None => ExpRead::NoClaim,
    }
}

/// What the LOCAL rung learned, before any request is made.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "local", rename_all = "snake_case")]
pub enum LocalCredential {
    /// A JWT whose `exp` is in the future.
    Live { exp_unix: i64, expires_in_secs: i64 },
    /// A JWT whose `exp` has passed. Reported, but NOT settled locally — the wire
    /// is authoritative, and a skewed local clock must not be able to declare a
    /// working credential dead.
    Expired {
        exp_unix: i64,
        expired_secs_ago: i64,
    },
    /// A JWT with no `exp` claim: present, and locally unfalsifiable.
    NoExpiry,
    /// Present but not a decodable JWT — a legacy opaque bearer. "Cannot judge"
    /// must not read as "fine".
    Opaque,
    /// Our reader returned nothing AND no independent source holds anything.
    Absent,
    /// Our reader returned nothing while an independent source DOES hold a
    /// credential. The reader is the fault, not the box. Naming this separately
    /// is the whole reason the local rung exists.
    Unreadable { reason: String },
}

/// One independently-checkable place a device credential can live: a label and
/// whether it currently holds bytes. Never the bytes themselves — a struct that
/// cannot hold the secret cannot leak it into a log.
#[derive(Debug, Clone, Copy)]
pub struct CredentialSource {
    pub name: &'static str,
    pub holds_something: bool,
}

/// Classify the local rung from our reader's output and the independent sources.
///
/// Pure, so the "reader broken while a credential exists" case — the case that
/// motivated splitting `absent` from `unreadable` — is testable without a
/// keychain.
pub fn classify_local_credential(
    reader_output: Option<&str>,
    now_unix: i64,
    sources: &[CredentialSource],
) -> LocalCredential {
    match reader_output.map(str::trim).filter(|s| !s.is_empty()) {
        Some(token) => match read_exp(token) {
            ExpRead::At { unix } if unix > now_unix => LocalCredential::Live {
                exp_unix: unix,
                expires_in_secs: unix - now_unix,
            },
            ExpRead::At { unix } => LocalCredential::Expired {
                exp_unix: unix,
                expired_secs_ago: now_unix - unix,
            },
            ExpRead::NoClaim => LocalCredential::NoExpiry,
            ExpRead::NotAJwt => LocalCredential::Opaque,
        },
        None => {
            let holders: Vec<&str> = sources
                .iter()
                .filter(|s| s.holds_something)
                .map(|s| s.name)
                .collect();
            if holders.is_empty() {
                LocalCredential::Absent
            } else {
                LocalCredential::Unreadable {
                    reason: format!(
                        "the credential reader returned nothing while {} holds a credential",
                        holders.join(" + ")
                    ),
                }
            }
        }
    }
}

/// Production wiring of [`classify_local_credential`] against this runner's
/// credential chain.
fn probe_local_credential() -> LocalCredential {
    let am = crate::auth::AuthManager::new();
    // `is_store_present_but_unreadable` is the single most direct evidence of the
    // shape this arm exists for: the store is THERE and we cannot open it.
    if am.is_store_present_but_unreadable() {
        return LocalCredential::Unreadable {
            reason: "the runner credential store is present but could not be decrypted".to_string(),
        };
    }
    let sources = [
        CredentialSource {
            name: "$COORD_DEVICE_JWT",
            holds_something: std::env::var("COORD_DEVICE_JWT")
                .map(|v| !v.trim().is_empty())
                .unwrap_or(false),
        },
        CredentialSource {
            name: "~/.qontinui/coord-device-jwt",
            holds_something: dirs::home_dir()
                .map(|h| h.join(".qontinui").join("coord-device-jwt"))
                .and_then(|p| std::fs::read_to_string(p).ok())
                .map(|v| !v.trim().is_empty())
                .unwrap_or(false),
        },
        CredentialSource {
            name: "the runner credential store",
            holds_something: am.has_tokens(),
        },
    ];
    classify_local_credential(
        crate::auth::device_bearer().as_deref(),
        chrono::Utc::now().timestamp(),
        &sources,
    )
}

// =============================================================================
// Credential — the wire rungs
// =============================================================================

/// One rung's raw observation: either the transport failed, or a status came
/// back. `Status(0)` is the `000` of the plan's table — a response that never
/// arrived — and is treated identically to a transport failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RungObservation {
    Transport(String),
    Status(u16),
}

/// The typed answer to "can this box reach the allocation door with a
/// credential coord accepts?".
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "verdict", rename_all = "snake_case")]
pub enum CredentialVerdict {
    /// Network or DNS. Says nothing about the credential.
    CoordUnreachable { detail: String },
    /// The request was ACCEPTED and rejected on content. The credential is fine.
    CredentialOk { status: u16 },
    /// Accepted, wrong door — e.g. `403 tenant_not_resolved`, whose fix is the
    /// `agent-` twin route. **Never** an auth failure.
    WrongTier { detail: String },
    /// `401`. The wire cannot separate these two, and neither will we.
    CredentialAbsentOrExpired,
    /// No credential exists anywhere we can see — established from the
    /// independent sources, never inferred from our reader's silence.
    CredentialAbsent,
    /// A credential exists and our reader could not produce it.
    CredentialUnreadable { reason: String },
    /// A status the ladder does not map. Named rather than guessed at, so a
    /// coord change shows up as an unmapped status instead of a wrong verdict.
    Unmapped { status: u16 },
}

impl CredentialVerdict {
    pub fn label(&self) -> &'static str {
        match self {
            CredentialVerdict::CoordUnreachable { .. } => "coord_unreachable",
            CredentialVerdict::CredentialOk { .. } => "credential_ok",
            CredentialVerdict::WrongTier { .. } => "wrong_tier",
            CredentialVerdict::CredentialAbsentOrExpired => "credential_absent_or_expired",
            CredentialVerdict::CredentialAbsent => "credential_absent",
            CredentialVerdict::CredentialUnreadable { .. } => "credential_unreadable",
            CredentialVerdict::Unmapped { .. } => "unmapped",
        }
    }
}

/// Map the local rung. `Some` settles the ladder; `None` means keep climbing.
///
/// An EXPIRED local token does not settle: the wire is authoritative and a
/// skewed clock must not be able to declare a working credential dead. That is
/// also the plan's falsification test — "reports `credential_absent_or_expired`
/// on a box where a fresh mint succeeds by hand" — pointed at the instrument.
pub fn classify_local_rung(local: &LocalCredential) -> Option<CredentialVerdict> {
    match local {
        LocalCredential::Absent => Some(CredentialVerdict::CredentialAbsent),
        LocalCredential::Unreadable { reason } => Some(CredentialVerdict::CredentialUnreadable {
            reason: reason.clone(),
        }),
        LocalCredential::Live { .. }
        | LocalCredential::Expired { .. }
        | LocalCredential::NoExpiry
        | LocalCredential::Opaque => None,
    }
}

/// Map the `OPTIONS` rung. `Some` settles the ladder; `None` means the route is
/// live and the next rung decides.
///
/// Any status at all means the route answered. `405` is the expected one and is
/// not special-cased: a coord that starts answering `204` to `OPTIONS` would
/// otherwise turn into a false `CoordUnreachable`.
pub fn classify_options_rung(obs: &RungObservation) -> Option<CredentialVerdict> {
    match obs {
        RungObservation::Transport(detail) => Some(CredentialVerdict::CoordUnreachable {
            detail: detail.clone(),
        }),
        RungObservation::Status(0) => Some(CredentialVerdict::CoordUnreachable {
            detail: "no response (000)".to_string(),
        }),
        RungObservation::Status(_) => None,
    }
}

/// Map the authenticated-read rung. Always settles.
pub fn classify_credential_rung(obs: &RungObservation) -> CredentialVerdict {
    match obs {
        RungObservation::Transport(detail) => CredentialVerdict::CoordUnreachable {
            detail: detail.clone(),
        },
        RungObservation::Status(0) => CredentialVerdict::CoordUnreachable {
            detail: "no response (000)".to_string(),
        },
        RungObservation::Status(401) => CredentialVerdict::CredentialAbsentOrExpired,
        RungObservation::Status(403) => CredentialVerdict::WrongTier {
            detail: "403: accepted, wrong tier or wrong door (the `agent-` twin) — \
                     not an auth failure"
                .to_string(),
        },
        // The bearer was VERIFIED. The ordinary healthy answer.
        RungObservation::Status(s) if (200..300).contains(s) => {
            CredentialVerdict::CredentialOk { status: *s }
        }
        // Also past auth: coord accepted the credential and rejected the request
        // on its content. Which of 400/422 a given tower stack produces is not a
        // fact about the credential.
        RungObservation::Status(s @ (400 | 422)) => CredentialVerdict::CredentialOk { status: *s },
        RungObservation::Status(s) => CredentialVerdict::Unmapped { status: *s },
    }
}

/// Which rung of the ladder produced the verdict.
pub const RUNG_LOCAL: &str = "local";
pub const RUNG_OPTIONS: &str = "allocate_options";
pub const RUNG_CREDENTIAL: &str = "authed_read";

/// The ladder's whole answer: the verdict, the rung that produced it, and the
/// local read, which is reported even when a later rung settled the verdict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CredentialPrecondition {
    pub rung: &'static str,
    pub verdict: CredentialVerdict,
    pub local: LocalCredential,
    /// The door the settling rung probed, when a request was made.
    pub door: Option<String>,
    /// The `OPTIONS /agents/allocate` status, when that rung ran. Kept even
    /// though it settles nothing about the credential: "the allocation door a
    /// spawning agent needs answered" is its own observable, and the plan asked
    /// for that door to be probed.
    pub allocate_door_status: Option<u16>,
}

/// Resolve the coord base the ladder probes: the connected profile's, else
/// `$COORD_HTTP_URL`, else [`DEFAULT_COORD_BASE`].
///
/// Total by construction, so there is no "which door?" verdict — the chain
/// always names one. That is deliberate: a `CoordBaseUnresolved` arm could
/// never be produced here, and a verdict that cannot occur is a verdict that
/// rots. If the last rung is reached the door is the documented hosted one, and
/// the report carries it in `door` so a reader can see which was probed.
fn coord_base() -> String {
    if let Some(base) = qontinui_runner_lib::profiles::connected_coord_base() {
        return base;
    }
    match std::env::var("COORD_HTTP_URL") {
        Ok(v) if !v.trim().is_empty() => v.trim().trim_end_matches('/').to_string(),
        _ => DEFAULT_COORD_BASE.to_string(),
    }
}

/// Run the ladder. At most two requests, each bounded by [`RUNG_TIMEOUT`].
pub async fn run_credential_ladder() -> CredentialPrecondition {
    let local = probe_local_credential();
    if let Some(verdict) = classify_local_rung(&local) {
        return CredentialPrecondition {
            rung: RUNG_LOCAL,
            verdict,
            local,
            door: None,
            allocate_door_status: None,
        };
    }

    let base = coord_base();
    let base = base.trim_end_matches('/');
    let allocate_url = format!("{base}{ALLOCATE_PATH}");
    let Some(client) = crate::coord_http::coord_client() else {
        return CredentialPrecondition {
            rung: RUNG_LOCAL,
            verdict: CredentialVerdict::CoordUnreachable {
                detail: "no shared coord HTTP client".to_string(),
            },
            local,
            door: None,
            allocate_door_status: None,
        };
    };

    // Rung 2 — liveness of the ALLOCATION door, the one a spawning agent needs.
    // Deliberately UNAUTHENTICATED: it answers "is there a door", and this route
    // cannot answer anything about a bearer anyway (module docs).
    let options = match client
        .request(reqwest::Method::OPTIONS, &allocate_url)
        .timeout(RUNG_TIMEOUT)
        .send()
        .await
    {
        Ok(resp) => RungObservation::Status(resp.status().as_u16()),
        Err(e) => RungObservation::Transport(format!("{e}")),
    };
    let allocate_door_status = match &options {
        RungObservation::Status(s) => Some(*s),
        RungObservation::Transport(_) => None,
    };
    if let Some(verdict) = classify_options_rung(&options) {
        return CredentialPrecondition {
            rung: RUNG_OPTIONS,
            verdict,
            local,
            door: Some(allocate_url),
            allocate_door_status,
        };
    }

    // Rung 3 — the credential itself, against a door where the bearer IS the
    // gate. A READ: whatever the credential turns out to be, nothing is
    // allocated, written or claimed.
    let credential_url = format!("{base}{CREDENTIAL_PATH}");
    // coord-tenant-scope(device): a liveness probe of the DEVICE's own coord
    // access. It reads and persists nothing, so there is no row to own and no
    // tenant to state — the default binding's credential IS the subject under
    // test, and presenting any other one would answer about the wrong box.
    let read = match crate::auth::attach_device_auth(client.get(&credential_url))
        .timeout(RUNG_TIMEOUT)
        .send()
        .await
    {
        Ok(resp) => RungObservation::Status(resp.status().as_u16()),
        Err(e) => RungObservation::Transport(format!("{e}")),
    };
    CredentialPrecondition {
        rung: RUNG_CREDENTIAL,
        verdict: classify_credential_rung(&read),
        local,
        door: Some(credential_url),
        allocate_door_status,
    }
}

// =============================================================================
// The per-spawn bundle
// =============================================================================

/// Where the ladder has got to for one spawn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum CredentialProbe {
    /// Started and not finished. Honest, and distinct from "no answer exists".
    Pending,
    /// Never started, with the reason. (No async runtime on this code path.)
    NotRun {
        reason: String,
    },
    Settled(CredentialPrecondition),
}

/// Everything known about one spawn's preconditions, with the derivation
/// attached so a log line answers "why did this read untrusted?" on its own.
#[derive(Debug, Clone, Serialize)]
pub struct SpawnPreconditionSnapshot {
    pub cwd: String,
    pub project_key: Option<String>,
    /// The per-spawn account PIN the caller supplied (`LaunchPayload::account`,
    /// already resolved to a config dir), or `None` when the spawn took the
    /// runner's own rotation. Kept beside `account_config_dir` on purpose: they
    /// are equal whenever a pin was honoured, and a report where they differ is
    /// itself the bug this phase exists to surface.
    pub account: Option<String>,
    /// The `CLAUDE_CONFIG_DIR` the child will actually run under.
    pub account_config_dir: Option<String>,
    pub account_config_file: Option<String>,
    /// Which arm of the account resolver produced `account_config_dir`.
    pub config_dir_source: String,
    pub trust: TrustVerdict,
    pub credential: CredentialProbe,
}

/// The live per-spawn handle: a settled trust verdict plus a credential ladder
/// running concurrently.
///
/// The ladder is **not** awaited on the spawn path. Blocking a spawn on two HTTP
/// requests would be a behavior change, and this phase changes no behavior; the
/// stall reporter reads whatever the ladder has by the time the window expires,
/// which — with a 120s window and a ~6s ladder — is always a settled value in
/// practice, and an honest `Pending` if it is not.
#[derive(Debug, Clone)]
pub struct SpawnPreconditions {
    cwd: String,
    project_key: Option<String>,
    account: Option<String>,
    account_config_dir: Option<String>,
    account_config_file: Option<String>,
    config_dir_source: String,
    trust: TrustVerdict,
    credential: watch::Receiver<CredentialProbe>,
}

impl SpawnPreconditions {
    /// Compute the pre-spawn verdicts for an EXPLICIT account, and start the
    /// credential ladder in the background.
    ///
    /// Call this BEFORE the trust pre-accept write, or the trust verdict reads
    /// `Trusted` on every spawn and answers nothing.
    pub fn evaluate(
        cwd: &str,
        account: Option<&str>,
        config_dir: Option<&str>,
        config_dir_source: &str,
    ) -> Self {
        let trust = trust_precondition(cwd, config_dir);

        let (tx, credential) = watch::channel(CredentialProbe::Pending);
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn(async move {
                    let settled = run_credential_ladder().await;
                    info!(
                        rung = settled.rung,
                        verdict = settled.verdict.label(),
                        door = settled.door.as_deref().unwrap_or("<none>"),
                        allocate_door_status = ?settled.allocate_door_status,
                        local = ?settled.local,
                        "spawn precondition: credential_precondition"
                    );
                    let _ = tx.send(CredentialProbe::Settled(settled));
                });
            }
            Err(e) => {
                let _ = tx.send(CredentialProbe::NotRun {
                    reason: format!("no tokio runtime on this path: {e}"),
                });
            }
        }

        let me = Self {
            cwd: trust.cwd,
            project_key: trust.project_key,
            account: account.map(str::to_string),
            account_config_dir: config_dir.map(str::to_string),
            account_config_file: trust.config_file,
            config_dir_source: config_dir_source.to_string(),
            trust: trust.verdict,
            credential,
        };
        me.log_trust();
        me
    }

    /// Emit the trust verdict WITH its full derivation. `untrusted` and
    /// `unknown` are warnings — this phase does not act on them, so the log is
    /// the entire deliverable and it must be visible at the default level.
    fn log_trust(&self) {
        let derivation = format!(
            "cwd={} project_key={} config_dir={} ({}) account={} config_file={}",
            self.cwd,
            self.project_key.as_deref().unwrap_or("<underivable>"),
            self.account_config_dir.as_deref().unwrap_or("<ambient>"),
            self.config_dir_source,
            self.account.as_deref().unwrap_or("<unpinned>"),
            self.account_config_file.as_deref().unwrap_or("<none>"),
        );
        match &self.trust {
            TrustVerdict::Trusted => debug!(
                verdict = "trusted",
                %derivation,
                "spawn precondition: trust_precondition"
            ),
            TrustVerdict::Untrusted { reason } => warn!(
                verdict = "untrusted",
                reason = *reason,
                %derivation,
                "spawn precondition: trust_precondition — this spawn would face the workspace \
                 trust dialog (reported only; the pre-accept write runs next)"
            ),
            TrustVerdict::Unknown { reason } => warn!(
                verdict = "unknown",
                reason = %reason,
                %derivation,
                "spawn precondition: trust_precondition could not be established"
            ),
        }
    }

    /// The trust verdict this spawn resolved, taken BEFORE any pre-accept write.
    ///
    /// Exposed for [`crate::claude_session::trust_gate`], which ACTS on it
    /// (Phase 2). It is deliberately the only trust probe in the spawn path: a
    /// second derivation would key a different question and still read as
    /// authoritative.
    pub fn trust_verdict(&self) -> &TrustVerdict {
        &self.trust
    }

    /// Whatever the ladder has right now. Never blocks.
    pub fn credential(&self) -> CredentialProbe {
        self.credential.borrow().clone()
    }

    pub fn snapshot(&self) -> SpawnPreconditionSnapshot {
        SpawnPreconditionSnapshot {
            cwd: self.cwd.clone(),
            project_key: self.project_key.clone(),
            account: self.account.clone(),
            account_config_dir: self.account_config_dir.clone(),
            account_config_file: self.account_config_file.clone(),
            config_dir_source: self.config_dir_source.clone(),
            trust: self.trust.clone(),
            credential: self.credential(),
        }
    }
}

// =============================================================================
// The stall window
// =============================================================================

/// The no-output window, or `None` when stall reporting is disabled.
///
/// See [`DEFAULT_STALL_WINDOW_SECS`] for why the default is what it is. An
/// unparseable override falls back to the default rather than disabling the
/// instrument — a typo must not silently switch observability off.
pub fn stall_window() -> Option<Duration> {
    let secs = match std::env::var(STALL_WINDOW_ENV) {
        Ok(raw) => match raw.trim().parse::<u64>() {
            Ok(v) => v,
            Err(_) => {
                warn!(
                    value = %raw,
                    "{STALL_WINDOW_ENV} is not a number; using the {DEFAULT_STALL_WINDOW_SECS}s \
                     default"
                );
                DEFAULT_STALL_WINDOW_SECS
            }
        },
        Err(_) => DEFAULT_STALL_WINDOW_SECS,
    };
    (secs > 0).then(|| Duration::from_secs(secs))
}

/// The typed `spawn_stalled` report body.
///
/// Flat by construction: everything an operator needs to act — the cwd, the
/// account config dir, and both verdicts — without a second lookup.
#[derive(Debug, Clone, Serialize)]
pub struct SpawnStalledBody {
    /// Fixed discriminator, so a consumer can key on it without shape-sniffing.
    pub phase: &'static str,
    /// How long the child produced no output at all.
    pub silent_secs: u64,
    pub pid: Option<i64>,
    #[serde(flatten)]
    pub preconditions: SpawnPreconditionSnapshot,
}

impl SpawnStalledBody {
    pub fn new(pre: &SpawnPreconditions, silent: Duration, pid: Option<i64>) -> Self {
        Self {
            phase: "spawn_stalled",
            silent_secs: silent.as_secs(),
            pid,
            preconditions: pre.snapshot(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---------------------------------------------------------------- trust

    fn cfg(body: &str) -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(CONFIG_FILE);
        std::fs::write(&path, body).unwrap();
        (tmp, path)
    }

    #[test]
    fn trust_flag_true_reads_trusted() {
        let (_t, p) = cfg(r#"{"projects":{"D:/w/repo":{"hasTrustDialogAccepted":true}}}"#);
        assert_eq!(trust_verdict_in(&p, "D:/w/repo"), TrustVerdict::Trusted);
    }

    /// The shape actually observed in the field: an entry that EXISTS with the
    /// flag explicitly `false`. It must not read as trusted, and it must not
    /// read the same as a plain missing entry.
    #[test]
    fn trust_flag_explicitly_false_reads_untrusted_with_its_own_reason() {
        let (_t, p) =
            cfg(r#"{"projects":{"D:/w/repo":{"hasTrustDialogAccepted":false,"allowedTools":[]}}}"#);
        assert_eq!(
            trust_verdict_in(&p, "D:/w/repo"),
            TrustVerdict::Untrusted {
                reason: "hasTrustDialogAccepted is explicitly false"
            }
        );
    }

    #[test]
    fn missing_entry_reads_untrusted() {
        let (_t, p) = cfg(r#"{"projects":{"D:/w/other":{"hasTrustDialogAccepted":true}}}"#);
        assert_eq!(
            trust_verdict_in(&p, "D:/w/repo"),
            TrustVerdict::Untrusted {
                reason: "no entry for this project key"
            }
        );
    }

    #[test]
    fn entry_without_the_flag_reads_untrusted() {
        let (_t, p) = cfg(r#"{"projects":{"D:/w/repo":{"allowedTools":[]}}}"#);
        assert_eq!(
            trust_verdict_in(&p, "D:/w/repo"),
            TrustVerdict::Untrusted {
                reason: "entry carries no hasTrustDialogAccepted flag"
            }
        );
    }

    #[test]
    fn config_with_no_projects_map_reads_untrusted() {
        let (_t, p) = cfg(r#"{"numStartups":1}"#);
        assert_eq!(
            trust_verdict_in(&p, "D:/w/repo"),
            TrustVerdict::Untrusted {
                reason: "config has no projects map"
            }
        );
    }

    /// An account that has never run: the CLI will create a fresh config with no
    /// `projects` map, which means the dialog. A conclusion, not an absence.
    #[test]
    fn absent_config_file_reads_untrusted() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(
            trust_verdict_in(&tmp.path().join(CONFIG_FILE), "D:/w/repo"),
            TrustVerdict::Untrusted {
                reason: "account config file absent"
            }
        );
    }

    /// Unparseable is UNKNOWN, never a clean-looking negative — we cannot see the
    /// trust state at all.
    #[test]
    fn unparseable_config_reads_unknown_with_a_reason() {
        let (_t, p) = cfg("{ this is not json");
        match trust_verdict_in(&p, "D:/w/repo") {
            TrustVerdict::Unknown { reason } => {
                assert!(reason.starts_with("config did not parse"), "got {reason}")
            }
            other => panic!("expected Unknown, got {other:?}"),
        }
    }

    #[test]
    fn non_boolean_flag_reads_unknown() {
        let (_t, p) = cfg(r#"{"projects":{"D:/w/repo":{"hasTrustDialogAccepted":"yes"}}}"#);
        assert!(matches!(
            trust_verdict_in(&p, "D:/w/repo"),
            TrustVerdict::Unknown { .. }
        ));
    }

    /// A relative cwd can derive no key, so there is nothing to look up — and
    /// the verdict says so rather than inventing an answer.
    #[test]
    fn underivable_project_key_is_unknown_not_untrusted() {
        let pre = trust_precondition("some/relative/dir", Some("C:/nope"));
        assert!(matches!(pre.verdict, TrustVerdict::Unknown { .. }));
        assert_eq!(pre.project_key, None);
    }

    #[test]
    fn empty_cwd_is_unknown() {
        let pre = trust_precondition("   ", None);
        assert_eq!(
            pre.verdict,
            TrustVerdict::Unknown {
                reason: "working dir is empty".to_string()
            }
        );
    }

    /// End-to-end over the real key derivation: a git repo whose account config
    /// has no entry reads untrusted, and the SAME repo reads trusted once the
    /// entry is there. This is the phase's gate, in miniature.
    #[test]
    fn precondition_flips_with_the_account_config() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir(repo.join(".git")).unwrap();
        let acct = tmp.path().join("acct");
        std::fs::create_dir_all(&acct).unwrap();
        std::fs::write(acct.join(CONFIG_FILE), r#"{"projects":{}}"#).unwrap();

        let cwd = repo.to_string_lossy().into_owned();
        let dir = acct.to_string_lossy().into_owned();

        let before = trust_precondition(&cwd, Some(dir.as_str()));
        assert_eq!(
            before.verdict,
            TrustVerdict::Untrusted {
                reason: "no entry for this project key"
            }
        );
        let key = before.project_key.clone().unwrap();

        // Reuse the SHIPPED writer, so the test also pins that the verdict reads
        // exactly what the pre-accept writes.
        assert_eq!(
            workspace_trust::ensure_trusted_in(&acct.join(CONFIG_FILE), &key),
            workspace_trust::TrustOutcome::Trusted
        );
        assert_eq!(
            trust_precondition(&cwd, Some(dir.as_str())).verdict,
            TrustVerdict::Trusted
        );
    }

    // ------------------------------------------------------------ exp decode

    fn jwt(payload: &str) -> String {
        use base64::Engine as _;
        let e = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        format!(
            "{}.{}.{}",
            e.encode(br#"{"alg":"HS256","typ":"JWT"}"#),
            e.encode(payload.as_bytes()),
            e.encode(b"sig"),
        )
    }

    #[test]
    fn exp_decodes_when_present() {
        assert_eq!(
            read_exp(&jwt(r#"{"exp":1799999999,"sub":"d"}"#)),
            ExpRead::At { unix: 1799999999 }
        );
    }

    #[test]
    fn exp_absent_is_not_the_same_as_not_a_jwt() {
        assert_eq!(read_exp(&jwt(r#"{"sub":"d"}"#)), ExpRead::NoClaim);
        assert_eq!(read_exp("qontinui_runner_abc123"), ExpRead::NotAJwt);
    }

    #[test]
    fn malformed_jwt_segments_are_not_a_jwt() {
        assert_eq!(read_exp("a.b.c"), ExpRead::NotAJwt);
        assert_eq!(read_exp(&jwt("not json at all")), ExpRead::NotAJwt);
        assert_eq!(read_exp(""), ExpRead::NotAJwt);
        assert_eq!(read_exp("only.two"), ExpRead::NotAJwt);
    }

    // -------------------------------------------------------- local rung

    const NOW: i64 = 1_700_000_000;

    fn src(name: &'static str, holds: bool) -> CredentialSource {
        CredentialSource {
            name,
            holds_something: holds,
        }
    }

    #[test]
    fn a_live_token_is_live() {
        let t = jwt(&format!(r#"{{"exp":{}}}"#, NOW + 3600));
        assert_eq!(
            classify_local_credential(Some(&t), NOW, &[]),
            LocalCredential::Live {
                exp_unix: NOW + 3600,
                expires_in_secs: 3600
            }
        );
    }

    /// Presence is not validity — the whole reason `exp` is decoded here.
    #[test]
    fn an_expired_token_is_not_reported_as_present_and_fine() {
        let t = jwt(&format!(r#"{{"exp":{}}}"#, NOW - 60));
        assert_eq!(
            classify_local_credential(Some(&t), NOW, &[]),
            LocalCredential::Expired {
                exp_unix: NOW - 60,
                expired_secs_ago: 60
            }
        );
    }

    #[test]
    fn no_source_and_no_reader_output_is_absent() {
        assert_eq!(
            classify_local_credential(None, NOW, &[src("env", false), src("file", false)]),
            LocalCredential::Absent
        );
    }

    /// THE case the plan splits the verdicts for: a working credential exists and
    /// our reader discarded it. Reporting that as `absent` is what made the two
    /// states indistinguishable.
    #[test]
    fn a_silent_reader_over_a_live_source_is_unreadable_not_absent() {
        match classify_local_credential(None, NOW, &[src("env", false), src("store", true)]) {
            LocalCredential::Unreadable { reason } => assert!(reason.contains("store"), "{reason}"),
            other => panic!("expected Unreadable, got {other:?}"),
        }
    }

    #[test]
    fn a_blank_reader_output_counts_as_no_output() {
        assert_eq!(
            classify_local_credential(Some("   "), NOW, &[]),
            LocalCredential::Absent
        );
    }

    #[test]
    fn absent_and_unreadable_settle_the_ladder_but_expired_does_not() {
        assert_eq!(
            classify_local_rung(&LocalCredential::Absent),
            Some(CredentialVerdict::CredentialAbsent)
        );
        assert!(matches!(
            classify_local_rung(&LocalCredential::Unreadable {
                reason: "x".to_string()
            }),
            Some(CredentialVerdict::CredentialUnreadable { .. })
        ));
        // The wire is authoritative for an expired token — a skewed local clock
        // must not be able to declare a working credential dead.
        assert_eq!(
            classify_local_rung(&LocalCredential::Expired {
                exp_unix: 1,
                expired_secs_ago: 2
            }),
            None
        );
        assert_eq!(classify_local_rung(&LocalCredential::Opaque), None);
        assert_eq!(classify_local_rung(&LocalCredential::NoExpiry), None);
    }

    // -------------------------------------------------------- wire rungs

    #[test]
    fn options_405_means_the_allocation_door_is_live_and_settles_nothing() {
        assert_eq!(
            classify_options_rung(&RungObservation::Status(405)),
            None,
            "405 says the route exists; it says nothing about the credential"
        );
        assert_eq!(classify_options_rung(&RungObservation::Status(204)), None);
    }

    #[test]
    fn options_000_or_timeout_is_coord_unreachable() {
        assert!(matches!(
            classify_options_rung(&RungObservation::Status(0)),
            Some(CredentialVerdict::CoordUnreachable { .. })
        ));
        assert!(matches!(
            classify_options_rung(&RungObservation::Transport("timed out".to_string())),
            Some(CredentialVerdict::CoordUnreachable { .. })
        ));
    }

    /// The healthy answer: coord verified the bearer and served the read.
    #[test]
    fn a_2xx_means_the_credential_was_verified() {
        assert_eq!(
            classify_credential_rung(&RungObservation::Status(200)),
            CredentialVerdict::CredentialOk { status: 200 }
        );
        assert_eq!(
            classify_credential_rung(&RungObservation::Status(204)),
            CredentialVerdict::CredentialOk { status: 204 }
        );
    }

    /// Past auth and rejected on content is still an accepted credential.
    #[test]
    fn a_422_or_400_means_the_credential_was_accepted() {
        assert_eq!(
            classify_credential_rung(&RungObservation::Status(422)),
            CredentialVerdict::CredentialOk { status: 422 }
        );
        assert_eq!(
            classify_credential_rung(&RungObservation::Status(400)),
            CredentialVerdict::CredentialOk { status: 400 }
        );
    }

    /// The named risk: a `403 tenant_not_resolved` is the wrong DOOR, and folding
    /// it into an auth verdict would send an operator after a credential that is
    /// fine.
    #[test]
    fn a_403_is_wrong_tier_and_never_an_auth_failure() {
        let v = classify_credential_rung(&RungObservation::Status(403));
        assert!(matches!(v, CredentialVerdict::WrongTier { .. }), "{v:?}");
        assert_ne!(v, CredentialVerdict::CredentialAbsentOrExpired);
    }

    #[test]
    fn a_401_is_the_only_auth_failure() {
        assert_eq!(
            classify_credential_rung(&RungObservation::Status(401)),
            CredentialVerdict::CredentialAbsentOrExpired
        );
    }

    /// A status the table does not cover is NAMED. Folding an unknown into the
    /// nearest verdict is how an instrument starts lying.
    #[test]
    fn an_unmapped_status_is_named_not_guessed() {
        assert_eq!(
            classify_credential_rung(&RungObservation::Status(500)),
            CredentialVerdict::Unmapped { status: 500 }
        );
        assert_eq!(
            classify_credential_rung(&RungObservation::Status(429)),
            CredentialVerdict::Unmapped { status: 429 }
        );
    }

    #[test]
    fn credential_rung_transport_failure_is_unreachable_not_an_auth_failure() {
        assert!(matches!(
            classify_credential_rung(&RungObservation::Transport("dns".to_string())),
            CredentialVerdict::CoordUnreachable { .. }
        ));
        assert!(matches!(
            classify_credential_rung(&RungObservation::Status(0)),
            CredentialVerdict::CoordUnreachable { .. }
        ));
    }

    /// The measurement that moved the credential rung off the allocation door,
    /// pinned as an assertion so the reasoning cannot quietly rot: a `422` from a
    /// malformed body is what that route returns with NO credential at all, so
    /// reading it as `credential_ok` would have made the instrument permanently
    /// green. Recorded here because the route's behaviour is coord's, not ours —
    /// if it ever changes, this test is the note explaining why the rung moved.
    #[test]
    fn the_allocation_door_cannot_answer_a_credential_question() {
        // Measured 2026-09-06 against the hosted coord: identical status with no
        // bearer and with a garbage bearer.
        let no_auth = RungObservation::Status(422);
        let garbage_bearer = RungObservation::Status(422);
        assert_eq!(
            classify_credential_rung(&no_auth),
            classify_credential_rung(&garbage_bearer),
            "the two are indistinguishable, which is why this rung is not run \
             against /agents/allocate"
        );
    }

    // ------------------------------------------------------- stall window

    #[test]
    fn stall_window_defaults_and_can_be_overridden_or_disabled() {
        let _g = crate::test_env::env_lock();
        std::env::remove_var(STALL_WINDOW_ENV);
        assert_eq!(
            stall_window(),
            Some(Duration::from_secs(DEFAULT_STALL_WINDOW_SECS))
        );

        std::env::set_var(STALL_WINDOW_ENV, "7");
        assert_eq!(stall_window(), Some(Duration::from_secs(7)));

        std::env::set_var(STALL_WINDOW_ENV, "0");
        assert_eq!(stall_window(), None, "0 disables the watcher");

        // A typo must not silently switch observability off.
        std::env::set_var(STALL_WINDOW_ENV, "later");
        assert_eq!(
            stall_window(),
            Some(Duration::from_secs(DEFAULT_STALL_WINDOW_SECS))
        );
        std::env::remove_var(STALL_WINDOW_ENV);
    }

    // ------------------------------------------------------- the bundle

    /// Without a runtime the ladder cannot start, and the probe SAYS so instead
    /// of reporting a clean-looking negative.
    #[test]
    fn evaluate_without_a_runtime_reports_not_run() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir(repo.join(".git")).unwrap();

        let repo_s = repo.to_string_lossy().into_owned();
        let acct_s = tmp.path().to_string_lossy().into_owned();
        let pre = SpawnPreconditions::evaluate(
            &repo_s,
            Some("acct"),
            Some(acct_s.as_str()),
            "request_override",
        );
        assert!(matches!(pre.credential(), CredentialProbe::NotRun { .. }));
        assert_eq!(
            pre.snapshot().trust,
            TrustVerdict::Untrusted {
                reason: "account config file absent"
            }
        );

        // The snapshot carries the whole derivation — cwd, key, config dir and
        // account — which is what makes the stall report self-explaining.
        let snap = pre.snapshot();
        assert_eq!(snap.account.as_deref(), Some("acct"));
        assert!(snap.project_key.is_some());
        assert!(snap.account_config_file.is_some());
        assert_eq!(snap.config_dir_source, "request_override");
    }

    #[test]
    fn the_stalled_body_carries_both_verdicts_and_the_cwd() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let repo_s = repo.to_string_lossy().into_owned();
        let acct_s = tmp.path().to_string_lossy().into_owned();
        let pre = SpawnPreconditions::evaluate(&repo_s, None, Some(acct_s.as_str()), "manual");
        let body = SpawnStalledBody::new(&pre, Duration::from_secs(120), Some(42));
        let v = serde_json::to_value(&body).unwrap();
        assert_eq!(v["phase"], "spawn_stalled");
        assert_eq!(v["silent_secs"], 120);
        assert_eq!(v["pid"], 42);
        assert!(v["cwd"].is_string());
        assert!(v["account_config_dir"].is_string());
        assert!(v["trust"]["verdict"].is_string());
        assert!(v["credential"]["state"].is_string());
    }
}
