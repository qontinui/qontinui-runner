//! Publishing the binary's **embedded command defaults** to the signed-in
//! account, so the account has a baseline to diff its overrides against.
//!
//! Plan `2026-08-31-runner-publishes-embedded-command-defaults`, Phase 5.
//!
//! The parent module resolves `fresh fetch → disk cache → embedded default`
//! and provisions the result; nothing in that chain ever tells the account
//! what the *default* was. So a user who overrode `/implement-plan` can diff
//! their versions against each other but not against what ships, and "reset
//! to default" cannot preview the body it restores. This module closes that
//! gap by PUT-ing the complete roster to qontinui-web as
//! [`AgentTextUnitDefault`]s — the shared schemas type, so the runner and the
//! web payload cannot drift apart silently (Design decision 8).
//!
//! ## Off the spawn path, once per process, fail-soft
//!
//! `provision_fleet_commands_for_session` is fail-soft by contract and sits on
//! three session-spawn sites; a network write does not belong there (Design
//! decision 5). The publisher runs **once per runner process, in a background
//! task at startup** — hung off the same post-boot block in
//! `mcp_api::start_server` that syncs workflows — and every failure `warn!`s
//! and is swallowed. No token means no request at all.
//!
//! ## The published-set cache
//!
//! A ~323 KB full-set `PUT` on every start would be wasteful, so the set that
//! last published SUCCESSFULLY is recorded beside the override cache in
//! [`PUBLISHED_FILE`]: the sorted `(kind, name, checksum)` triples, the runner
//! version that sent them, the backend they went to, and a schema version.
//! The next run compares and skips when nothing changed. The record carries
//! the same `cache_version` + `backend_url` guard as `CachedOverrides`: a
//! device switched between a local and a prod backend must never conclude
//! "already published" against the wrong one. A rejected or unavailable
//! publish leaves the record exactly as it was.
//!
//! **The runner version is part of the "unchanged" key on purpose.** Two
//! builds with byte-identical bodies would otherwise leave the org's baseline
//! labelled "published by runner v<old>" after an upgrade; re-sending once per
//! build is what the plan budgets ("paid once per runner build, not once per
//! start", Risk 3).
//!
//! ## The checksum is the files-map digest
//!
//! [`agent_text_unit_files_checksum`] over `{ "<name>.md": body }` — the same
//! digest an `AgentTextUnit` override carries — and NOT
//! `agent_commands::agent_command_checksum`, which digests a single body. The
//! two deliberately disagree even for a one-entry map, and a default digested
//! with the wrong one would never compare equal to the override it exists to
//! be diffed against (Design decision 8). [`tests::checksum_pairs_with_an_override`]
//! pins this.
//!
//! ## One endpoint, many sources
//!
//! The wire payload is a list of units carrying `kind`. Today the only source
//! is `FLEET_COMMANDS`; the `include_dir!` skill and agent bundles are out of
//! scope for the plan. Publishing them later is adding a function beside
//! [`embedded_command_units`] and chaining it into [`embedded_units`] — not
//! touching the endpoint, the cache, or the guard (Design decision 6).

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use qontinui_types::agent_text_units::{
    agent_text_unit_entrypoint, agent_text_unit_files_checksum, validate_agent_text_unit_default,
    AgentTextUnitDefault, AgentTextUnitFiles, AgentTextUnitKind,
};
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

/// Filename of the published-set record, written beside the override cache
/// (`CACHE_FILE`) under the runner's per-instance config dir.
pub(crate) const PUBLISHED_FILE: &str = "agent-commands-published.json";

/// Schema version of [`PublishedSet`]. A record written by a different version
/// is ignored — and so the set is re-published, which is the safe direction.
const PUBLISHED_VERSION: u32 = 1;

/// Wall-clock budget for the whole `PUT`. Generous relative to the fetch's 4 s
/// because this is a background write of a few hundred KB with nobody
/// waiting on it; a black-holed backend still ends in a warning, not a hang.
const PUBLISH_TIMEOUT: Duration = Duration::from_secs(30);

/// Path of the defaults endpoint, relative to the API base URL. Fixed wire
/// contract shared with qontinui-web's Phase 4b.
const PUBLISH_ROUTE: &str = "/api/v1/agent-text-units/defaults";

/// Process-wide "has the publisher run?" latch. The boot task calls the entry
/// point once, but the latch makes a second call site harmless rather than a
/// second 323 KB write.
static PUBLISH_CLAIMED: AtomicBool = AtomicBool::new(false);

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

/// The `PUT` body: the runner's version plus the COMPLETE roster. Names absent
/// from `units` are deleted for the org — a full-set replace, mirroring the
/// fetch side's authoritative-empty rule (Design decision 3).
#[derive(Debug, Clone, Serialize)]
pub(crate) struct PublishRequest {
    pub runner_version: String,
    pub units: Vec<AgentTextUnitDefault>,
}

/// The endpoint's answer. `accepted: false` with a 200 is a NORMAL outcome —
/// an older device than the one whose publish is stored — not a fault.
#[derive(Debug, Clone, Deserialize)]
struct PublishResponse {
    accepted: bool,
    #[serde(default)]
    rejected_reason: Option<String>,
    #[serde(default)]
    stored_version: Option<String>,
    #[serde(default)]
    count: u64,
}

/// What one attempt at the `PUT` established. This is the seam the tests
/// script: [`publish_with`] takes any `FnOnce(PublishRequest) -> Future<Output
/// = SendOutcome>` and the production caller passes [`send_publish_http`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SendOutcome {
    /// The backend stored the set.
    Accepted {
        stored_version: Option<String>,
        count: u64,
    },
    /// The backend refused the set. `stale_version` is the older-device arm
    /// (200 + `accepted: false`), which is logged at info; every other refusal
    /// (a 4xx, a checksum mismatch) is a warning.
    Rejected { reason: String, stale_version: bool },
    /// Transport error, 5xx, or an unreadable body — nothing is known about
    /// what the backend holds.
    Unavailable(String),
}

/// What the publisher decided, one line of log per variant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PublishOutcome {
    /// Sent and accepted; the published-set record was (re)written.
    Published { count: usize },
    /// The record already describes this exact set from this exact build to
    /// this exact backend — no request made.
    SkippedUnchanged,
    /// No usable access token — no request made.
    SkippedNoToken,
    /// Sent and refused; the record is untouched.
    Rejected(String),
    /// Could not send, or nothing publishable to send; the record is untouched.
    Unavailable(String),
}

// ---------------------------------------------------------------------------
// The published-set record
// ---------------------------------------------------------------------------

/// One published unit's identity: enough to decide "did this change?"
/// without re-reading bodies. Sorted by derive order — kind, then name, then
/// checksum — so two records over the same set compare equal regardless of
/// roster order.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) struct PublishedUnit {
    pub kind: String,
    pub name: String,
    pub checksum: String,
}

/// The on-disk record of the last SUCCESSFUL publish.
///
/// Guarded exactly like `CachedOverrides`: `cache_version` and `backend_url`
/// are checked on read, and a mismatch on either is a miss.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PublishedSet {
    cache_version: u32,
    backend_url: String,
    runner_version: String,
    published_at: String,
    units: Vec<PublishedUnit>,
}

/// Absolute path of the published-set record for this runner instance, or
/// `None` when the platform has no config dir. Sibling of the override cache
/// by construction — one directory, one convention.
fn published_path() -> Option<PathBuf> {
    super::cache_path().map(|p| p.with_file_name(PUBLISHED_FILE))
}

/// Read the record at `path`, accepting it only when it was written by this
/// record version against `backend_url`. Any IO/parse failure is a miss.
pub(crate) fn read_published_at(path: &Path, backend_url: &str) -> Option<PublishedSet> {
    let raw = std::fs::read_to_string(path).ok()?;
    let record: PublishedSet = match serde_json::from_str(&raw) {
        Ok(r) => r,
        Err(e) => {
            warn!(
                "agent_commands::publish_defaults: published-set record at {} is unparseable \
                 ({e}) — ignoring it (the set will be re-published)",
                path.display()
            );
            return None;
        }
    };
    if record.cache_version != PUBLISHED_VERSION {
        debug!(
            "agent_commands::publish_defaults: published-set record version {} != \
             {PUBLISHED_VERSION} — ignoring it",
            record.cache_version
        );
        return None;
    }
    if record.backend_url != backend_url {
        debug!(
            "agent_commands::publish_defaults: published-set record was written against {:?} \
             but this process resolves {:?} — ignoring it rather than crossing backends",
            record.backend_url, backend_url
        );
        return None;
    }
    Some(record)
}

/// Persist `record` at `path`. Best-effort: a write failure is warned and
/// swallowed (the only cost is one redundant publish on the next start).
pub(crate) fn write_published_at(path: &Path, record: &PublishedSet) {
    let bytes = match serde_json::to_vec_pretty(record) {
        Ok(b) => b,
        Err(e) => {
            warn!("agent_commands::publish_defaults: could not serialize the published-set record ({e})");
            return;
        }
    };
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            warn!(
                "agent_commands::publish_defaults: could not create {} ({e}) — the set will be \
                 re-published next start",
                parent.display()
            );
            return;
        }
    }
    if let Err(e) = crate::fs_atomic::atomic_write(path, &bytes) {
        warn!(
            "agent_commands::publish_defaults: could not write {} ({e}) — the set will be \
             re-published next start",
            path.display()
        );
    }
}

/// The sorted identity triples of `units`.
fn published_units_of(units: &[AgentTextUnitDefault]) -> Vec<PublishedUnit> {
    let mut out: Vec<PublishedUnit> = units
        .iter()
        .map(|u| PublishedUnit {
            kind: u.kind.as_str().to_string(),
            name: u.name.clone(),
            checksum: u.checksum.clone(),
        })
        .collect();
    out.sort();
    out
}

// ---------------------------------------------------------------------------
// Sources
// ---------------------------------------------------------------------------

/// Every embedded default this binary can publish, across every source. Today
/// that is the command bundle alone; a second bundle is a second call chained
/// here, and nothing downstream changes.
pub(crate) fn embedded_units(
    runner_version: &str,
    published_at: &str,
) -> Vec<AgentTextUnitDefault> {
    embedded_command_units(runner_version, published_at)
}

/// `FLEET_COMMANDS` as `kind = command` defaults. A command is the one-file
/// case of the files map: `{ "<name>.md": body }`, the key being exactly
/// [`agent_text_unit_entrypoint`] for the kind, so the published default
/// pairs with a stored override of the same name.
///
/// Iterates the real roster — nothing here may assume its size. A body over
/// `MAX_BODY_BYTES` or a unit that fails the shared validator is warned about
/// and left out; the full-set replace then drops it server-side, which is the
/// honest state for a body that cannot be published.
pub(crate) fn embedded_command_units(
    runner_version: &str,
    published_at: &str,
) -> Vec<AgentTextUnitDefault> {
    let kind = AgentTextUnitKind::command();
    let mut out = Vec::with_capacity(crate::fleet_commands::FLEET_COMMANDS.len());
    for (name, body) in crate::fleet_commands::FLEET_COMMANDS {
        if body.len() > super::MAX_BODY_BYTES {
            warn!(
                "agent_commands::publish_defaults: embedded default {name:?} is {} bytes, over \
                 the {}-byte limit — not publishing it",
                body.len(),
                super::MAX_BODY_BYTES
            );
            continue;
        }
        let mut files = AgentTextUnitFiles::new();
        files.insert(agent_text_unit_entrypoint(&kind, name), (*body).to_string());
        let checksum = agent_text_unit_files_checksum(&files);
        let unit = AgentTextUnitDefault {
            kind: kind.clone(),
            name: (*name).to_string(),
            files,
            checksum,
            published_by_version: runner_version.to_string(),
            published_at: published_at.to_string(),
        };
        if let Err(e) = validate_agent_text_unit_default(&unit) {
            warn!(
                "agent_commands::publish_defaults: embedded default {name:?} fails the shared \
                 validator ({e}) — not publishing it"
            );
            continue;
        }
        out.push(unit);
    }
    out
}

// ---------------------------------------------------------------------------
// Decision
// ---------------------------------------------------------------------------

/// Everything the decision needs that is NOT the token or the transport.
#[derive(Debug, Clone)]
pub(crate) struct PublishContext {
    pub backend_url: String,
    pub runner_version: String,
    /// Where the published-set record lives; `None` disables the skip (every
    /// run publishes) rather than failing.
    pub record_path: Option<PathBuf>,
}

/// The publisher, with its two side-effecting inputs injected.
///
/// `token` is whatever `AuthManager::get_access_token()` yielded (already
/// reduced to `Option`), and `send` is the transport. Pure apart from the
/// record file at `ctx.record_path`, which is why every gate is testable with
/// a temp dir and a scripted `send`.
pub(crate) async fn publish_with<S, F>(
    ctx: &PublishContext,
    token: Option<String>,
    send: S,
) -> PublishOutcome
where
    S: FnOnce(String, PublishRequest) -> F,
    F: Future<Output = SendOutcome>,
{
    let token = match token {
        Some(t) if !t.trim().is_empty() => t,
        _ => {
            info!(
                "agent_commands::publish_defaults: skipped — no stored access token, so the \
                 embedded defaults are not published (sign in to give the account a baseline)"
            );
            return PublishOutcome::SkippedNoToken;
        }
    };

    let published_at = super::now_rfc3339();
    let units = embedded_units(&ctx.runner_version, &published_at);
    if units.is_empty() {
        // A full-set replace with nothing in it would DELETE the org's
        // baseline. Every embedded body failing validation is a build defect,
        // not an instruction to clear the store.
        let why = "no embedded default passed validation".to_string();
        warn!(
            "agent_commands::publish_defaults: unavailable — {why}; not sending an empty set \
             (it would delete the org's baseline)"
        );
        return PublishOutcome::Unavailable(why);
    }

    let identity = published_units_of(&units);
    if let Some(path) = &ctx.record_path {
        if let Some(prev) = read_published_at(path, &ctx.backend_url) {
            if prev.runner_version == ctx.runner_version && prev.units == identity {
                info!(
                    "agent_commands::publish_defaults: skipped — the {} embedded default(s) \
                     are unchanged since the last publish from v{} to {} (at {})",
                    identity.len(),
                    prev.runner_version,
                    prev.backend_url,
                    prev.published_at
                );
                return PublishOutcome::SkippedUnchanged;
            }
        }
    }

    let count = units.len();
    let request = PublishRequest {
        runner_version: ctx.runner_version.clone(),
        units,
    };
    match send(token, request).await {
        SendOutcome::Accepted {
            stored_version,
            count: server_count,
        } => {
            if let Some(path) = &ctx.record_path {
                write_published_at(
                    path,
                    &PublishedSet {
                        cache_version: PUBLISHED_VERSION,
                        backend_url: ctx.backend_url.clone(),
                        runner_version: ctx.runner_version.clone(),
                        published_at,
                        units: identity,
                    },
                );
            }
            info!(
                "agent_commands::publish_defaults: published {count} embedded default(s) as \
                 runner v{} to {} (backend stored {server_count}, stored_version {:?})",
                ctx.runner_version,
                ctx.backend_url,
                stored_version.as_deref().unwrap_or("<none>")
            );
            PublishOutcome::Published { count }
        }
        SendOutcome::Rejected {
            reason,
            stale_version,
        } => {
            if stale_version {
                // An older device than the one whose publish is stored is the
                // NORMAL multi-build-org case (Design decision 4), not a fault.
                info!(
                    "agent_commands::publish_defaults: rejected — {} declined runner v{}'s \
                     {count} default(s): {reason} (a newer build has published; nothing to do)",
                    ctx.backend_url, ctx.runner_version
                );
            } else {
                warn!(
                    "agent_commands::publish_defaults: rejected — {} declined runner v{}'s \
                     {count} default(s): {reason}",
                    ctx.backend_url, ctx.runner_version
                );
            }
            PublishOutcome::Rejected(reason)
        }
        SendOutcome::Unavailable(why) => {
            warn!(
                "agent_commands::publish_defaults: unavailable — could not publish {count} \
                 default(s) to {}: {why} (will retry on the next runner start)",
                ctx.backend_url
            );
            PublishOutcome::Unavailable(why)
        }
    }
}

// ---------------------------------------------------------------------------
// Transport
// ---------------------------------------------------------------------------

/// The endpoint's error envelope on a 4xx, when it sends one.
#[derive(Debug, Deserialize)]
struct ErrorEnvelope {
    #[serde(default)]
    detail: Option<serde_json::Value>,
    #[serde(default)]
    rejected_reason: Option<String>,
}

/// `PUT {base}{PUBLISH_ROUTE}` with the bearer. The production transport for
/// [`publish_with`].
pub(crate) async fn send_publish_http(
    base_url: String,
    token: String,
    request: PublishRequest,
) -> SendOutcome {
    let client = match reqwest::Client::builder().timeout(PUBLISH_TIMEOUT).build() {
        Ok(c) => c,
        Err(e) => return SendOutcome::Unavailable(format!("could not build an HTTP client: {e}")),
    };
    let url = format!("{base_url}{PUBLISH_ROUTE}");
    let resp = match client
        .put(&url)
        .bearer_auth(&token)
        .json(&request)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return SendOutcome::Unavailable(format!("PUT {url} failed: {e}")),
    };
    let status = resp.status();
    if status.is_client_error() {
        let body = resp.text().await.unwrap_or_default();
        let detail = serde_json::from_str::<ErrorEnvelope>(&body)
            .ok()
            .and_then(|e| {
                e.rejected_reason
                    .or_else(|| e.detail.map(|d| d.to_string()))
            })
            .unwrap_or_else(|| body.chars().take(200).collect());
        return SendOutcome::Rejected {
            reason: format!("HTTP {status}: {detail}"),
            stale_version: false,
        };
    }
    if !status.is_success() {
        return SendOutcome::Unavailable(format!("PUT {url} returned HTTP {status}"));
    }
    match resp.json::<PublishResponse>().await {
        Ok(PublishResponse {
            accepted: true,
            stored_version,
            count,
            ..
        }) => SendOutcome::Accepted {
            stored_version,
            count,
        },
        Ok(PublishResponse {
            accepted: false,
            rejected_reason,
            stored_version,
            ..
        }) => SendOutcome::Rejected {
            reason: rejected_reason.unwrap_or_else(|| {
                format!(
                    "backend holds {} and declined this publish",
                    stored_version
                        .map(|v| format!("v{v}"))
                        .unwrap_or_else(|| "a newer version".to_string())
                )
            }),
            stale_version: true,
        },
        Err(e) => SendOutcome::Unavailable(format!("PUT {url} returned an unreadable body: {e}")),
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Claim the once-per-process slot. `true` exactly once.
pub(crate) fn claim_publish_slot() -> bool {
    PUBLISH_CLAIMED
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_ok()
}

/// The stored bearer, or `None` when there is none. Same read the override
/// fetch makes (`AuthManager::get_access_token`, a secure-storage read with no
/// tier gate); moved to a blocking thread because it may touch the keychain.
async fn resolve_token() -> Option<String> {
    let read = tokio::task::spawn_blocking(|| {
        crate::auth::AuthManager::new()
            .get_access_token()
            .ok()
            .filter(|t| !t.trim().is_empty())
    })
    .await;
    match read {
        Ok(token) => token,
        Err(e) => {
            warn!("agent_commands::publish_defaults: the token read panicked ({e}) — treating as no token");
            None
        }
    }
}

/// Publish the embedded defaults once per process. Fail-soft end to end: this
/// never returns an error and never panics past its own task. Call it from a
/// background task after boot — never from a session-spawn path.
pub(crate) async fn publish_embedded_defaults_once() -> PublishOutcome {
    if !claim_publish_slot() {
        debug!("agent_commands::publish_defaults: already ran this process — not publishing again");
        return PublishOutcome::SkippedUnchanged;
    }
    let ctx = PublishContext {
        backend_url: crate::api_config::get_api_base_url(),
        runner_version: env!("CARGO_PKG_VERSION").to_string(),
        record_path: published_path(),
    };
    let token = resolve_token().await;
    let base_url = ctx.backend_url.clone();
    publish_with(&ctx, token, move |token, request| {
        send_publish_http(base_url, token, request)
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    const BACKEND: &str = "https://api.example";

    /// A scripted transport: records every request it receives and answers
    /// with the outcome it was built with.
    #[derive(Clone)]
    struct Stub {
        calls: Arc<Mutex<Vec<PublishRequest>>>,
        answer: SendOutcome,
    }

    impl Stub {
        fn answering(answer: SendOutcome) -> Self {
            Self {
                calls: Arc::new(Mutex::new(Vec::new())),
                answer,
            }
        }

        fn accepting() -> Self {
            Self::answering(SendOutcome::Accepted {
                stored_version: Some("0.0.0-test".to_string()),
                count: 0,
            })
        }

        fn sender(&self) -> impl FnOnce(String, PublishRequest) -> std::future::Ready<SendOutcome> {
            let calls = self.calls.clone();
            let answer = self.answer.clone();
            move |_token, request| {
                calls.lock().unwrap().push(request);
                std::future::ready(answer)
            }
        }

        fn call_count(&self) -> usize {
            self.calls.lock().unwrap().len()
        }

        fn last(&self) -> PublishRequest {
            self.calls
                .lock()
                .unwrap()
                .last()
                .cloned()
                .expect("a request")
        }
    }

    fn ctx(dir: &Path, version: &str) -> PublishContext {
        PublishContext {
            backend_url: BACKEND.to_string(),
            runner_version: version.to_string(),
            record_path: Some(dir.join("nested").join(PUBLISHED_FILE)),
        }
    }

    fn token() -> Option<String> {
        Some("bearer-for-tests".to_string())
    }

    /// Gate (a): roster-size independence. The publisher sends exactly the
    /// real roster — never a literal count — and each unit is the one-file
    /// shape keyed by the kind's entrypoint.
    #[test]
    fn units_cover_the_whole_roster_without_assuming_its_size() {
        let units = embedded_units("1.2.3", "2026-09-05T00:00:00Z");
        let roster = crate::fleet_commands::FLEET_COMMANDS;
        assert_eq!(units.len(), roster.len());
        assert!(
            !units.is_empty(),
            "the bundle must ship at least one command"
        );

        for ((name, body), unit) in roster.iter().zip(&units) {
            assert_eq!(unit.kind, AgentTextUnitKind::command());
            assert_eq!(unit.name, *name);
            assert_eq!(unit.published_by_version, "1.2.3");
            assert_eq!(unit.published_at, "2026-09-05T00:00:00Z");
            // Exactly one file, at the entrypoint path, byte-identical body.
            assert_eq!(unit.files.len(), 1);
            assert_eq!(unit.entrypoint(), format!("{name}.md"));
            assert_eq!(
                unit.files.get(&unit.entrypoint()).map(String::as_str),
                Some(*body)
            );
            assert!(
                unit.checksum_matches(),
                "{name}: carried checksum must match content"
            );
            validate_agent_text_unit_default(unit)
                .unwrap_or_else(|e| panic!("{name}: must pass the shared validator: {e}"));
        }
    }

    /// Design decision 8: the digest is the files-map one, so it pairs with a
    /// stored override — and it is NOT the legacy single-body digest, which
    /// would make every baseline read as drifted.
    #[test]
    fn checksum_pairs_with_an_override() {
        let (name, body) = crate::fleet_commands::FLEET_COMMANDS[0];
        let unit = embedded_units("1.0.0", "2026-09-05T00:00:00Z")
            .into_iter()
            .find(|u| u.name == name)
            .expect("first roster entry is published");

        // What the web stores for an override of the same command.
        let mut override_files = AgentTextUnitFiles::new();
        override_files.insert(format!("{name}.md"), body.to_string());
        assert_eq!(
            unit.checksum,
            agent_text_unit_files_checksum(&override_files)
        );

        // The wrong digest, which must never be the one carried.
        assert_ne!(
            unit.checksum,
            qontinui_types::agent_commands::agent_command_checksum(body),
            "the files-map digest and the single-body digest must differ (Design decision 8)"
        );
    }

    /// Gate (b): the unchanged-set skip. A second run with the same set, build
    /// and backend sends nothing; a version bump or a backend switch sends
    /// again.
    #[tokio::test]
    async fn unchanged_set_skips_the_send() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let ctx1 = ctx(tmp.path(), "1.0.0");

        let first = Stub::accepting();
        let out = publish_with(&ctx1, token(), first.sender()).await;
        assert_eq!(
            out,
            PublishOutcome::Published {
                count: crate::fleet_commands::FLEET_COMMANDS.len()
            }
        );
        assert_eq!(first.call_count(), 1);
        assert_eq!(first.last().runner_version, "1.0.0");
        assert_eq!(
            first.last().units.len(),
            crate::fleet_commands::FLEET_COMMANDS.len(),
            "the COMPLETE roster is sent"
        );
        let record_path = ctx1.record_path.clone().unwrap();
        let record = read_published_at(&record_path, BACKEND).expect("record written");
        assert_eq!(record.runner_version, "1.0.0");
        assert_eq!(
            record.units.len(),
            crate::fleet_commands::FLEET_COMMANDS.len()
        );

        // Same everything: skipped, transport untouched.
        let second = Stub::accepting();
        let out = publish_with(&ctx1, token(), second.sender()).await;
        assert_eq!(out, PublishOutcome::SkippedUnchanged);
        assert_eq!(second.call_count(), 0);

        // A new build re-publishes even with identical bodies (the label
        // must name the running build).
        let ctx2 = ctx(tmp.path(), "1.0.1");
        let third = Stub::accepting();
        let out = publish_with(&ctx2, token(), third.sender()).await;
        assert!(matches!(out, PublishOutcome::Published { .. }));
        assert_eq!(third.call_count(), 1);
        assert_eq!(
            read_published_at(&record_path, BACKEND)
                .unwrap()
                .runner_version,
            "1.0.1"
        );

        // A different backend must not be told "already published".
        let ctx3 = PublishContext {
            backend_url: "http://127.0.0.1:8000".to_string(),
            ..ctx2.clone()
        };
        let fourth = Stub::accepting();
        let out = publish_with(&ctx3, token(), fourth.sender()).await;
        assert!(matches!(out, PublishOutcome::Published { .. }));
        assert_eq!(fourth.call_count(), 1);
    }

    /// Gate (c): a rejected publish leaves the record exactly as it was — on
    /// both the stale-version arm and the fault arm — and so does an
    /// unavailable backend. A rejection with no prior record writes none.
    #[tokio::test]
    async fn rejected_publish_leaves_the_record_untouched() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let record_path = tmp.path().join("nested").join(PUBLISHED_FILE);

        // No record yet + rejection → still no record.
        let ctx_fresh = ctx(tmp.path(), "2.0.0");
        let rejecting = Stub::answering(SendOutcome::Rejected {
            reason: "checksum mismatch".to_string(),
            stale_version: false,
        });
        let out = publish_with(&ctx_fresh, token(), rejecting.sender()).await;
        assert_eq!(
            out,
            PublishOutcome::Rejected("checksum mismatch".to_string())
        );
        assert_eq!(rejecting.call_count(), 1);
        assert!(
            !record_path.exists(),
            "a rejected publish must not write a record"
        );

        // Establish a record, then get rejected on a newer build.
        let accepting = Stub::accepting();
        assert!(matches!(
            publish_with(&ctx_fresh, token(), accepting.sender()).await,
            PublishOutcome::Published { .. }
        ));
        let before = std::fs::read(&record_path).expect("record written");

        let ctx_newer = ctx(tmp.path(), "2.0.1");
        let stale = Stub::answering(SendOutcome::Rejected {
            reason: "backend holds v9.9.9".to_string(),
            stale_version: true,
        });
        let out = publish_with(&ctx_newer, token(), stale.sender()).await;
        assert_eq!(
            out,
            PublishOutcome::Rejected("backend holds v9.9.9".to_string())
        );
        assert_eq!(stale.call_count(), 1);
        assert_eq!(std::fs::read(&record_path).unwrap(), before);

        let unavailable = Stub::answering(SendOutcome::Unavailable("timeout".to_string()));
        let out = publish_with(&ctx_newer, token(), unavailable.sender()).await;
        assert_eq!(out, PublishOutcome::Unavailable("timeout".to_string()));
        assert_eq!(std::fs::read(&record_path).unwrap(), before);

        // And because the record still names 2.0.0, the 2.0.0 build is still
        // "unchanged" — proof the rejection did not silently advance it.
        let idle = Stub::accepting();
        assert_eq!(
            publish_with(&ctx_fresh, token(), idle.sender()).await,
            PublishOutcome::SkippedUnchanged
        );
        assert_eq!(idle.call_count(), 0);
    }

    /// Gate (d): with no token there is no request and no record.
    #[tokio::test]
    async fn no_token_sends_nothing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let ctx = ctx(tmp.path(), "1.0.0");
        let record_path = ctx.record_path.clone().unwrap();

        for absent in [None, Some(String::new()), Some("   ".to_string())] {
            let stub = Stub::accepting();
            let out = publish_with(&ctx, absent, stub.sender()).await;
            assert_eq!(out, PublishOutcome::SkippedNoToken);
            assert_eq!(stub.call_count(), 0, "no token must mean no request");
        }
        assert!(!record_path.exists());
    }

    /// The record refuses a foreign backend, a foreign version and garbage —
    /// the same three rejections `CachedOverrides` has.
    #[test]
    fn record_round_trips_and_refuses_foreign_records() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("nested").join(PUBLISHED_FILE);
        let record = PublishedSet {
            cache_version: PUBLISHED_VERSION,
            backend_url: BACKEND.to_string(),
            runner_version: "1.0.0".to_string(),
            published_at: "2026-09-05T00:00:00Z".to_string(),
            units: vec![PublishedUnit {
                kind: "command".to_string(),
                name: "vet-plan".to_string(),
                checksum: "sha256-abc".to_string(),
            }],
        };
        write_published_at(&path, &record);
        assert_eq!(read_published_at(&path, BACKEND).unwrap(), record);
        assert!(read_published_at(&path, "http://127.0.0.1:8000").is_none());

        let bumped = serde_json::json!({
            "cache_version": PUBLISHED_VERSION + 1,
            "backend_url": BACKEND,
            "runner_version": "1.0.0",
            "published_at": "2026-09-05T00:00:00Z",
            "units": [],
        });
        std::fs::write(&path, serde_json::to_vec(&bumped).unwrap()).unwrap();
        assert!(read_published_at(&path, BACKEND).is_none());

        std::fs::write(&path, b"{not json").unwrap();
        assert!(read_published_at(&path, BACKEND).is_none());

        assert!(read_published_at(&tmp.path().join("absent.json"), BACKEND).is_none());
    }

    /// The identity triples are order-independent, so roster reordering alone
    /// never re-publishes.
    #[test]
    fn identity_is_order_independent() {
        let units = embedded_units("1.0.0", "2026-09-05T00:00:00Z");
        let mut reversed = units.clone();
        reversed.reverse();
        assert_eq!(published_units_of(&units), published_units_of(&reversed));
    }

    /// The once-per-process latch yields exactly once.
    #[test]
    fn publish_slot_is_claimed_once() {
        assert!(claim_publish_slot());
        assert!(!claim_publish_slot());
    }

    /// The wire body is the fixed contract: `runner_version` + `units`, each
    /// unit serialized by the shared type.
    #[test]
    fn request_serializes_to_the_wire_contract() {
        let units = embedded_units("1.0.0", "2026-09-05T00:00:00Z");
        let req = PublishRequest {
            runner_version: "1.0.0".to_string(),
            units,
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["runner_version"], "1.0.0");
        let wire_units = json["units"].as_array().unwrap();
        assert_eq!(
            wire_units.len(),
            crate::fleet_commands::FLEET_COMMANDS.len()
        );
        let first = &wire_units[0];
        for key in [
            "kind",
            "name",
            "files",
            "checksum",
            "published_by_version",
            "published_at",
        ] {
            assert!(first.get(key).is_some(), "unit must carry {key}");
        }
        assert_eq!(first["kind"], "command");
    }
}
