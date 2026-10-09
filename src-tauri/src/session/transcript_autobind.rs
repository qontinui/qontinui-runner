//! The transcript watcher binds EVERY transcript it sees to a coord session.
//! Plan `2026-10-06-closed-sessions-whose-work-is-unfinished-are-found-fleet-wide-and-resumed`
//! Phase 1.
//!
//! ## Why
//!
//! The tailer only emits for sessions the registrar's in-memory R4 index maps.
//! Only the `claude --resume` sniffer and `POST /sessions/transcript-bind`
//! wrote to that index, so on merytshost 1 of 30 transcripts was tailed
//! (`sessions_tailed=1 sessions_unbound=29`). A session that never reaches R4
//! also has no coord row that carries its transcript, account or finished mark
//! — which is what makes a closed-but-unfinished session unfindable.
//!
//! ## What
//!
//! For each transcript the watcher tails, identified by its file-named id
//! (`<config>/projects/<cwd>/<id>.jsonl`):
//!
//! 1. ask coord which `coord.sessions` row answers for that harness id
//!    ([`crate::mcp::session_work_status::resolve_coord_row`] — the latest row,
//!    since the id is not unique);
//! 2. [`decide`]: adopt that row, MINT one only when coord answered `unknown`,
//!    and DEFER (never mint) on anything that does not settle the question —
//!    a second row beside an existing one is the duplicate-row defect;
//! 3. bind through the one binder, `SessionTranscriptTailer::bind_and_replay`
//!    (-> `AiCoordRegistrar::bind_transcript_session`), which also replays the
//!    unsent prefix on first bind and stamps `account_label` / `config_dir` on
//!    a minted row.
//!
//! Consent is unchanged: Gate 1 (`cloud_sync_enabled`) is checked here before
//! any coord traffic and again inside `bind_and_replay`; the tenant's
//! `transcript_sync_enabled` is enforced downstream by the emitter exactly as
//! for a sniffed pane. Attempts are throttled per session ([`AttemptLedger`])
//! so a coord outage costs one bounded probe per window, not one per append.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::claude_session::coord_register::ResumeParams;
use crate::mcp::session_work_status::RowResolution;
use crate::session::session_transcript_tailer::{
    AutoBindKind, BindRefusal, BindRequest, SessionTranscriptTailer,
};

/// Minimum spacing between two attempts for one session.
pub const ATTEMPT_WINDOW: Duration = Duration::from_secs(60);

/// Who a transcript file belongs to, read from its path alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptIdentity {
    /// The file stem — the Claude Code session id.
    pub claude_session_id: String,
    /// The account home holding `projects/`.
    pub config_dir: String,
    /// The config dir's BASENAME (`.claude-gmail`, `.claude`) — the wire
    /// `account_label`, matching coord's respawn contract
    /// (`agents_spawn::invalid_account_label` accepts a basename and refuses
    /// a path). NOT the fleet's short label (`gmail`/`unknown`).
    pub account_label: String,
}

/// Read `<config>/projects/<cwd>/<id>.jsonl`. `None` unless the stem is a
/// UUID and the file sits exactly two directories under a `projects` dir.
pub fn identity_from_path(path: &Path) -> Option<TranscriptIdentity> {
    let stem = path.file_stem()?.to_str()?;
    Uuid::parse_str(stem).ok()?;
    if path.extension()?.to_str()? != "jsonl" {
        return None;
    }
    let projects = path.parent()?.parent()?;
    if projects.file_name()?.to_str()? != "projects" {
        return None;
    }
    let config_home = projects.parent()?;
    let account_label = config_home.file_name()?.to_str()?.to_string();
    let config_dir = config_home.to_str()?.to_string();
    Some(TranscriptIdentity {
        claude_session_id: stem.to_string(),
        config_dir,
        account_label,
    })
}

/// What to do with a transcript given what coord said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Bind to the row coord already has.
    Adopt(Uuid),
    /// coord has no session for this id: register a fresh row owned by this
    /// tenant (already proven resolvable and consenting — see [`mint_gate`]).
    Mint(Uuid),
    /// coord's answer does not settle it; try again next window.
    Defer(String),
}

/// Pure: the only place a row may be minted is an explicit `unknown`, and then
/// only with a tenant to own it. `mint_tenant` is the gate's verdict
/// ([`mint_gate`]); `None` means minting is not licensed and the answer is a
/// defer, never a tenant-less row.
pub fn decide(resolution: &RowResolution, mint_tenant: Option<Uuid>) -> Decision {
    match resolution {
        RowResolution::Existing(id) => Decision::Adopt(*id),
        RowResolution::Unknown => match mint_tenant {
            Some(t) => Decision::Mint(t),
            None => Decision::Defer("minting not licensed for this transcript".to_string()),
        },
        RowResolution::Unresolved(why) => Decision::Defer(why.clone()),
    }
}

/// Pure: the tenant a minted row may be owned by, from the machine's tenant
/// pin, the device's binding count and its default binding.
///
/// A pinned machine names its tenant. An unpinned machine bound to ONE tenant
/// has an unambiguous default. An unpinned machine bound to SEVERAL does not:
/// coord's `unknown` was answered under the default credential only, which
/// says nothing about the tenants the transcript might belong to, so minting
/// there could file the session under the wrong tenant — refuse.
pub fn mint_tenant(
    pin: crate::session::tenant_pin::TenantPin,
    binding_count: usize,
    default_binding: Option<Uuid>,
) -> Option<Uuid> {
    use crate::session::tenant_pin::TenantPin;
    match pin {
        TenantPin::Pinned(t) => Some(t),
        TenantPin::Unpinned if binding_count <= 1 => default_binding,
        TenantPin::Unpinned | TenantPin::Unresolvable => None,
    }
}

/// How long a tenant's `transcript_sync_enabled` answer is reused.
const POLICY_REUSE: Duration = Duration::from_secs(300);

static POLICY_CACHE: Mutex<Option<HashMap<Uuid, (Instant, bool)>>> = Mutex::new(None);
static POLICY_FETCH: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn cached_policy(tenant: Uuid) -> Option<bool> {
    let g = POLICY_CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    g.as_ref()
        .and_then(|m| m.get(&tenant))
        .filter(|(at, _)| at.elapsed() < POLICY_REUSE)
        .map(|(_, v)| *v)
}

/// The tenant's `transcript_sync_enabled`, from coord's `/tenant-policy`
/// (presenting the queried tenant's own credential slot). `None` means NOT
/// ESTABLISHED (unreachable, refused, undecodable) — which a caller must not
/// read as consent. Cached for [`POLICY_REUSE`]; one fetch at a time, so a boot
/// storm costs one request per tenant.
async fn tenant_transcript_sync_enabled(tenant: Uuid) -> Option<bool> {
    if let Some(v) = cached_policy(tenant) {
        return Some(v);
    }
    let _one = POLICY_FETCH.lock().await;
    if let Some(v) = cached_policy(tenant) {
        return Some(v);
    }
    let (base, _src) = qontinui_runner_lib::profiles::coord_base_with_source();
    let url = format!(
        "{}/tenant-policy?tenant_id={tenant}",
        base.trim_end_matches('/')
    );
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
        .ok()?;
    let resp =
        crate::coord_http::coord_get_for(&client, &url, crate::auth::TenantScope::Owned(tenant))
            .send()
            .await
            .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let body: serde_json::Value = resp.json().await.ok()?;
    let enabled = body.get("transcript_sync_enabled")?.as_bool()?;
    POLICY_CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get_or_insert_with(HashMap::new)
        .insert(tenant, (Instant::now(), enabled));
    Some(enabled)
}

/// Licence a mint: the tenant must be resolvable ([`mint_tenant`]) AND its
/// `transcript_sync_enabled` must be observed `true`. A minted row carries the
/// absolute `config_dir` on the wire, which a tenant that opted out of
/// transcript sync has not agreed to send — so an unobserved or `false` answer
/// mints nothing.
async fn mint_gate() -> Result<Uuid, String> {
    let tenant = mint_tenant(
        crate::session::tenant_pin::resolve_tenant_pin(),
        crate::auth::device_binding_count(),
        crate::auth::default_binding_tenant(),
    )
    .ok_or_else(|| {
        "tenant not resolved (unpinned multi-tenant device, or no binding): not minting".to_string()
    })?;
    match tenant_transcript_sync_enabled(tenant).await {
        Some(true) => Ok(tenant),
        Some(false) => Err(format!(
            "tenant {tenant} has transcript sync off: not minting"
        )),
        None => Err(format!(
            "tenant {tenant}'s transcript_sync_enabled not established: not minting"
        )),
    }
}

/// Per-session attempt throttle.
#[derive(Default)]
pub struct AttemptLedger {
    last: Mutex<HashMap<String, Instant>>,
}

impl AttemptLedger {
    /// `true` (and records `now`) when no attempt for `key` started within
    /// [`ATTEMPT_WINDOW`] of `now`.
    pub fn try_begin(&self, key: &str, now: Instant) -> bool {
        let mut g = self
            .last
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match g.get(key) {
            Some(prev) if now.saturating_duration_since(*prev) < ATTEMPT_WINDOW => false,
            _ => {
                g.insert(key.to_string(), now);
                true
            }
        }
    }
}

static LEDGER: std::sync::LazyLock<AttemptLedger> =
    std::sync::LazyLock::new(AttemptLedger::default);

/// Apply a [`Decision`] through the tailer's binder. Blocking (file I/O in the
/// replay). Returns what to count, or `None` when the key was already bound.
pub fn apply_decision(
    tailer: &SessionTranscriptTailer,
    identity: &TranscriptIdentity,
    path: &Path,
    decision: &Decision,
    cloud_sync_enabled: bool,
) -> Option<AutoBindKind> {
    let adopt = match decision {
        Decision::Adopt(id) => Some(*id),
        Decision::Mint(_) => None,
        Decision::Defer(why) => {
            debug!(
                "transcript_autobind: {} deferred: {why}",
                identity.claude_session_id
            );
            return Some(AutoBindKind::Deferred);
        }
    };
    let req = BindRequest {
        adopt,
        tenant: match decision {
            Decision::Mint(t) => Some(*t),
            _ => None,
        },
        resume: ResumeParams {
            account_label: Some(identity.account_label.clone()),
            config_dir: Some(identity.config_dir.clone()),
        },
    };
    match tailer.bind_and_replay(&identity.claude_session_id, path, req, cloud_sync_enabled) {
        Ok(o) if o.already_bound => None,
        Ok(o) => {
            info!(
                "transcript_autobind: bound {} to coord session {} ({}; replayed {} bytes)",
                identity.claude_session_id,
                o.coord_session_id,
                if o.adopted { "adopted" } else { "minted" },
                o.replayed_bytes
            );
            Some(if o.adopted {
                AutoBindKind::Adopted
            } else {
                AutoBindKind::Minted
            })
        }
        Err(BindRefusal::Unreadable(d)) => {
            warn!(
                "transcript_autobind: {} IS bound but its prefix replay failed: {d}",
                identity.claude_session_id
            );
            Some(if adopt.is_some() {
                AutoBindKind::Adopted
            } else {
                AutoBindKind::Minted
            })
        }
        Err(e) => {
            debug!(
                "transcript_autobind: {} not bound: {e:?}",
                identity.claude_session_id
            );
            Some(AutoBindKind::Deferred)
        }
    }
}

/// Settle what to do for an unbound transcript once coord has answered.
///
/// Only an `unknown` answer reaches the mint arm, and it is licensed in two
/// steps before any row is written: an undelivered `Started` already in the
/// outbox for this session is ADOPTED (a just-spawned pane whose row coord has
/// not received yet — minting would create a second row beside it), and
/// otherwise [`mint_gate`] must name a consenting tenant.
async fn settle(
    tailer: &Arc<SessionTranscriptTailer>,
    identity: &TranscriptIdentity,
    resolution: &RowResolution,
) -> Decision {
    if !matches!(resolution, RowResolution::Unknown) {
        return decide(resolution, None);
    }
    let (t, csid) = (tailer.clone(), identity.claude_session_id.clone());
    let pending = qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked(move || {
        t.pending_started_row(&csid)
    })
    .await
    .ok()
    .flatten();
    if let Some(row) = pending {
        return Decision::Adopt(row);
    }
    match mint_gate().await {
        Ok(tenant) => decide(resolution, Some(tenant)),
        Err(why) => Decision::Defer(why),
    }
}

/// Bind `path`'s session if it is not bound yet. Called by the watcher when it
/// starts tailing a transcript and again when an append finds it unbound.
/// Never fails — and never blocks — the tail: the cheap local checks run
/// inline and the coord round trip runs in a DETACHED task, so a slow or dark
/// coord cannot stall the tail loop (and with it the transcript's emission).
pub fn spawn_ensure_bound(tailer: &Arc<SessionTranscriptTailer>, path: &Path) {
    let Some(identity) = identity_from_path(path) else {
        return;
    };
    if tailer.is_bound(&identity.claude_session_id) {
        return;
    }
    tailer.note_seen_unbound(&identity.claude_session_id);
    let cloud_sync = crate::settings::get_cloud_sync_enabled();
    if !cloud_sync {
        // Consent withheld: no coord traffic, no binding.
        return;
    }
    if !LEDGER.try_begin(&identity.claude_session_id, Instant::now()) {
        return;
    }
    let (tailer, path) = (tailer.clone(), path.to_path_buf());
    tokio::spawn(async move {
        bind_one(&tailer, identity, &path, cloud_sync).await;
    });
}

async fn bind_one(
    tailer: &Arc<SessionTranscriptTailer>,
    identity: TranscriptIdentity,
    path: &Path,
    cloud_sync: bool,
) {
    let resolution =
        crate::mcp::session_work_status::resolve_coord_row(&identity.claude_session_id).await;
    let decision = settle(tailer, &identity, &resolution).await;
    let (t, p) = (tailer.clone(), path.to_path_buf());
    let joined = qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked(move || {
        apply_decision(&t, &identity, &p, &decision, cloud_sync).inspect(|k| t.note_autobind(*k))
    })
    .await;
    if let Err(e) = joined {
        warn!("transcript_autobind: bind task failed to run: {e}");
    }
}

/// Transcript targets for a set of lifecycle-store records: one
/// `(claude_session_id, transcript path)` per Claude-provider record whose
/// transcript is on disk. `resolve` is injected so the selection is testable
/// without a real config dir.
pub fn lifecycle_transcript_targets(
    records: &[crate::session::session_lifecycle_store::TerminalSessionRecord],
    resolve: impl Fn(Option<&str>, Option<&str>, &str) -> Option<std::path::PathBuf>,
) -> Vec<(String, std::path::PathBuf)> {
    records
        .iter()
        .filter(|r| r.provider == crate::session::session_lifecycle_store::DEFAULT_PROVIDER)
        .filter_map(|r| {
            resolve(
                r.config_dir.as_deref(),
                r.working_dir.as_deref(),
                &r.claude_session_id,
            )
            .map(|p| (r.claude_session_id.clone(), p))
        })
        .collect()
}

/// Delays before each attempt of an owed-finish bind ([`spawn_owed_finish_bind`]).
/// The first is immediate; the rest back off so a dark coord is probed ~5 times
/// over ~12 minutes rather than hammered, and the whole schedule is owned by a
/// detached task, so it outlives the transcript tail that may have ended.
pub const OWED_BIND_BACKOFF: [Duration; 5] = [
    Duration::ZERO,
    Duration::from_secs(5),
    Duration::from_secs(30),
    Duration::from_secs(120),
    Duration::from_secs(600),
];

/// Bind ONE lifecycle-store record whose finished mark coord is OWED, so
/// `register_inner` can deliver it. Unlike [`spawn_ensure_bound`] this does NOT
/// go through the 60 s attempt throttle (that throttle exists to space tail
/// retries; here a finish is waiting and a throttled-away attempt would leave
/// the mark local-only until some unrelated append) and it retries on
/// [`OWED_BIND_BACKOFF`] until the session is bound. A record whose transcript
/// is not on disk has nothing to tail and is left alone; consent withheld
/// (`cloud_sync_enabled` off) binds nothing. Detached; never blocks the caller.
pub fn spawn_owed_finish_bind(
    tailer: &Arc<SessionTranscriptTailer>,
    rec: &crate::session::session_lifecycle_store::TerminalSessionRecord,
) {
    let targets = lifecycle_transcript_targets(std::slice::from_ref(rec), |c, w, id| {
        crate::session::past_sessions::resolve_transcript_path(c, w, id)
    });
    for (_, path) in targets {
        let Some(identity) = identity_from_path(&path) else {
            continue;
        };
        if !crate::settings::get_cloud_sync_enabled() {
            return;
        }
        let tailer = tailer.clone();
        tokio::spawn(async move {
            for delay in OWED_BIND_BACKOFF {
                tokio::time::sleep(delay).await;
                if tailer.is_bound(&identity.claude_session_id) {
                    return;
                }
                bind_one(&tailer, identity.clone(), &path, true).await;
                if tailer.is_bound(&identity.claude_session_id) {
                    return;
                }
            }
            warn!(
                "transcript_autobind: {} still unbound after {} attempts; its finished mark \
                 stays local-only until the session next registers",
                identity.claude_session_id,
                OWED_BIND_BACKOFF.len()
            );
        });
    }
}

/// Boot pass: bind every lifecycle-store session. The transcript watcher covers
/// transcripts it sees under the config dirs; this covers the registry's own
/// view, so a session the registry knows is never left without a coord row to
/// carry its finished mark. A record owed a finish takes the un-throttled,
/// retrying path ([`spawn_owed_finish_bind`]); the rest go through the ordinary
/// throttled binder. Both detach, so this returns without waiting on coord.
pub async fn bind_lifecycle_sessions(
    tailer: &Arc<SessionTranscriptTailer>,
    store: &crate::session::session_lifecycle_store::SessionLifecycleStore,
) {
    let records = store.all_records();
    let mut with_transcript = 0usize;
    for rec in &records {
        let targets = lifecycle_transcript_targets(std::slice::from_ref(rec), |c, w, id| {
            crate::session::past_sessions::resolve_transcript_path(c, w, id)
        });
        with_transcript += targets.len();
        if rec.finished_at.is_some() && !rec.finish_synced {
            spawn_owed_finish_bind(tailer, rec);
        } else {
            for (_, path) in targets {
                spawn_ensure_bound(tailer, &path);
            }
        }
    }
    info!(
        "transcript_autobind: boot bind of {} lifecycle session(s) ({with_transcript} with a transcript on disk)",
        records.len()
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude_session::coord_register::AiCoordRegistrar;
    use crate::session::local_store::OutboxWriter;
    use crate::session::transcript_emitter::TranscriptEmitter;
    use crate::session::SessionEventKind;

    const CSID: &str = "7e0b5d6a-9b8e-4f2c-a3d1-c1d9f0e7a2b4";

    #[test]
    fn identity_reads_config_dir_and_account_from_the_path() {
        let p =
            Path::new("/home/u/.claude-gmail/projects/-home-u-proj").join(format!("{CSID}.jsonl"));
        let id = identity_from_path(&p).expect("identity");
        assert_eq!(id.claude_session_id, CSID);
        assert_eq!(id.config_dir, "/home/u/.claude-gmail");
        assert_eq!(id.account_label, ".claude-gmail");
    }

    #[test]
    fn identity_refuses_paths_that_are_not_a_session_transcript() {
        // Not a UUID stem.
        assert!(identity_from_path(Path::new("/c/.claude/projects/p/notes.jsonl")).is_none());
        // Wrong depth: not directly under projects/<cwd>/.
        assert!(
            identity_from_path(Path::new(&format!("/c/.claude/other/p/{CSID}.jsonl"))).is_none()
        );
        // Wrong extension.
        assert!(
            identity_from_path(Path::new(&format!("/c/.claude/projects/p/{CSID}.txt"))).is_none()
        );
        // The default home's label is its basename too, not a failure.
        let id = identity_from_path(Path::new(&format!(
            "/home/u/.claude/projects/p/{CSID}.jsonl"
        )))
        .unwrap();
        assert_eq!(id.account_label, ".claude");
    }

    #[test]
    fn only_an_explicit_unknown_mints() {
        let row = Uuid::new_v4();
        let tenant = Uuid::new_v4();
        assert_eq!(
            decide(&RowResolution::Existing(row), None),
            Decision::Adopt(row)
        );
        assert_eq!(
            decide(&RowResolution::Unknown, Some(tenant)),
            Decision::Mint(tenant)
        );
        // An unknown with no licensed tenant is a defer, never a tenant-less mint.
        assert!(matches!(
            decide(&RowResolution::Unknown, None),
            Decision::Defer(_)
        ));
        assert!(matches!(
            decide(
                &RowResolution::Unresolved("coord down".into()),
                Some(tenant)
            ),
            Decision::Defer(_)
        ));
    }

    #[test]
    fn mint_tenant_refuses_an_ambiguous_multi_tenant_device() {
        use crate::session::tenant_pin::TenantPin;
        let (pinned, default) = (Uuid::new_v4(), Uuid::new_v4());
        assert_eq!(
            mint_tenant(TenantPin::Pinned(pinned), 3, Some(default)),
            Some(pinned)
        );
        assert_eq!(
            mint_tenant(TenantPin::Unpinned, 1, Some(default)),
            Some(default)
        );
        assert_eq!(mint_tenant(TenantPin::Unpinned, 2, Some(default)), None);
        assert_eq!(mint_tenant(TenantPin::Unpinned, 1, None), None);
        assert_eq!(mint_tenant(TenantPin::Unresolvable, 1, Some(default)), None);
    }

    #[test]
    fn the_ledger_spaces_attempts_per_session() {
        let l = AttemptLedger::default();
        let t0 = Instant::now();
        assert!(l.try_begin("a", t0));
        assert!(
            !l.try_begin("a", t0 + Duration::from_secs(5)),
            "inside window"
        );
        assert!(l.try_begin("b", t0), "another session is independent");
        assert!(l.try_begin("a", t0 + ATTEMPT_WINDOW), "window elapsed");
    }

    fn tailer(
        dir: &Path,
    ) -> (
        Arc<SessionTranscriptTailer>,
        Arc<AiCoordRegistrar>,
        Arc<OutboxWriter>,
    ) {
        let outbox = Arc::new(OutboxWriter::open(dir.join("outbox.jsonl")).unwrap());
        let machine_id = Uuid::new_v4();
        let registrar = Arc::new(AiCoordRegistrar::with_tenant_resolver(
            outbox.clone(),
            machine_id,
            || None,
        ));
        let emitter = Arc::new(TranscriptEmitter::new(
            outbox.clone(),
            machine_id,
            registrar.clone(),
        ));
        (
            Arc::new(SessionTranscriptTailer::new(emitter, registrar.clone())),
            registrar,
            outbox,
        )
    }

    fn transcript(dir: &Path, csid: &str, body: &str) -> std::path::PathBuf {
        let d = dir.join(".claude-paktis").join("projects").join("proj");
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join(format!("{csid}.jsonl"));
        std::fs::write(&f, body).unwrap();
        f
    }

    /// `register_inner` is gated on a process-global env var other suites
    /// toggle under their own lock, so retry briefly rather than flake.
    fn apply_retrying(
        t: &SessionTranscriptTailer,
        id: &TranscriptIdentity,
        p: &Path,
        d: &Decision,
    ) -> Option<AutoBindKind> {
        for _ in 0..100 {
            let r = apply_decision(t, id, p, d, true);
            if r != Some(AutoBindKind::Deferred) {
                return r;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        Some(AutoBindKind::Deferred)
    }

    #[test]
    fn mint_binds_stamps_the_account_and_replays_the_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let (t, registrar, outbox) = tailer(dir.path());
        let path = transcript(
            dir.path(),
            CSID,
            "{\"type\":\"user\"}\n{\"type\":\"assistant\"}\n",
        );
        let id = identity_from_path(&path).unwrap();
        let tenant = Uuid::new_v4();
        t.note_seen_unbound(CSID);
        assert_eq!(
            t.coverage().sessions_unbound,
            1,
            "a seen-but-unbound transcript is a hole"
        );

        let kind = apply_retrying(&t, &id, &path, &Decision::Mint(tenant));
        assert_eq!(kind, Some(AutoBindKind::Minted));
        assert!(registrar.session_id_for(CSID).is_some(), "R4 now maps it");
        assert_eq!(t.coverage().sessions_unbound, 0, "binding clears the hole");

        let started: Vec<_> = outbox
            .pending()
            .unwrap()
            .into_iter()
            .filter(|r| r.event_kind == SessionEventKind::Started.as_str())
            .collect();
        assert_eq!(started.len(), 1, "exactly one Started row");
        assert_eq!(started[0].payload["account_label"], ".claude-paktis");
        assert_eq!(started[0].payload["tenant_id"], tenant.to_string());
        assert_eq!(
            started[0].payload["config_dir"],
            dir.path().join(".claude-paktis").to_str().unwrap()
        );
        assert_eq!(started[0].payload["claude_code_session_id"], CSID);
        assert!(
            outbox
                .pending()
                .unwrap()
                .iter()
                .any(|r| r.event_kind == SessionEventKind::OutputChunk.as_str()),
            "the unsent prefix was backfilled"
        );
    }

    #[test]
    fn adopt_binds_to_coords_row_and_writes_no_started() {
        let dir = tempfile::tempdir().unwrap();
        let (t, registrar, outbox) = tailer(dir.path());
        let path = transcript(dir.path(), CSID, "{\"type\":\"user\"}\n");
        let id = identity_from_path(&path).unwrap();
        let row = Uuid::new_v4();

        let kind = apply_retrying(&t, &id, &path, &Decision::Adopt(row));
        assert_eq!(kind, Some(AutoBindKind::Adopted));
        assert_eq!(registrar.session_id_for(CSID), Some(row));
        assert!(
            !outbox
                .pending()
                .unwrap()
                .iter()
                .any(|r| r.event_kind == SessionEventKind::Started.as_str()),
            "an adopted row already exists coord-side"
        );
        // A second pass never re-registers or double-counts.
        assert_eq!(
            apply_decision(&t, &id, &path, &Decision::Mint(Uuid::new_v4()), true),
            None
        );
        assert_eq!(registrar.session_id_for(CSID), Some(row));
    }

    #[test]
    fn defer_and_consent_off_bind_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let (t, registrar, _outbox) = tailer(dir.path());
        let path = transcript(dir.path(), CSID, "{\"type\":\"user\"}\n");
        let id = identity_from_path(&path).unwrap();

        assert_eq!(
            apply_decision(&t, &id, &path, &Decision::Defer("coord down".into()), true),
            Some(AutoBindKind::Deferred)
        );
        // Gate 1 off: bind_and_replay refuses before registering anything.
        assert_eq!(
            apply_decision(&t, &id, &path, &Decision::Mint(Uuid::new_v4()), false),
            Some(AutoBindKind::Deferred)
        );
        assert!(registrar.session_id_for(CSID).is_none());
    }

    #[test]
    fn an_undelivered_started_row_is_found_so_a_mint_adopts_it() {
        let dir = tempfile::tempdir().unwrap();
        let (t, _registrar, outbox) = tailer(dir.path());
        assert_eq!(t.pending_started_row(CSID), None);
        let row = Uuid::new_v4();
        outbox
            .record(
                Uuid::new_v4(),
                row,
                SessionEventKind::Started,
                serde_json::json!({ "id": row, "claude_code_session_id": CSID }),
            )
            .unwrap();
        assert_eq!(t.pending_started_row(CSID), Some(row));
        assert_eq!(
            t.pending_started_row("00000000-0000-4000-8000-000000000000"),
            None,
            "another session's pending Started is not this one's"
        );
    }

    #[test]
    fn autobind_outcomes_reach_the_coverage_report() {
        let dir = tempfile::tempdir().unwrap();
        let (t, _r, _o) = tailer(dir.path());
        t.note_autobind(AutoBindKind::Adopted);
        t.note_autobind(AutoBindKind::Minted);
        t.note_autobind(AutoBindKind::Minted);
        t.note_autobind(AutoBindKind::Deferred);
        let c = t.coverage();
        assert_eq!(
            (c.autobind_adopted, c.autobind_minted, c.autobind_deferred),
            (1, 2, 1)
        );
    }

    fn lifecycle_rec(
        csid: &str,
        provider: &str,
    ) -> crate::session::session_lifecycle_store::TerminalSessionRecord {
        let mut rec = crate::session::session_lifecycle_store::TerminalSessionRecord {
            claude_session_id: csid.to_string(),
            config_dir: None,
            working_dir: None,
            page_id: "default".to_string(),
            zone_index: 0,
            title: None,
            terminal_id: "t".to_string(),
            opened_at: 0,
            last_seen_at: 0,
            state: "closed".to_string(),
            closed_at: None,
            close_reason: None,
            provider: provider.to_string(),
            origin: None,
            restore_pending_at: None,
            confirmed_at: None,
            handle: None,
            account_label: None,
            account_wrapper: None,
            session_name: None,
            name_source: None,
            tenant_id: None,
            task_run_id: None,
            bypass_permissions: None,
            restored_from_boot_at: None,
            restore_tier: None,
            finished_at: None,
            wind_down_outcome: None,
            wind_down_at: None,
            finish_reason: None,
            finish_synced: false,
            spawn_device_default: None,
            adopted_from: None,
        };
        rec.working_dir = Some("/w".into());
        rec
    }

    #[test]
    fn the_owed_finish_bind_starts_immediately_and_backs_off() {
        assert_eq!(
            OWED_BIND_BACKOFF[0],
            Duration::ZERO,
            "no throttle on attempt 1"
        );
        assert!(
            OWED_BIND_BACKOFF.windows(2).all(|w| w[0] < w[1]),
            "strictly increasing backoff"
        );
    }

    #[test]
    fn lifecycle_targets_cover_every_claude_record_with_a_transcript() {
        let other = "11111111-1111-4111-8111-111111111111";
        let missing = "22222222-2222-4222-8222-222222222222";
        let recs = vec![
            lifecycle_rec(CSID, "claude"),
            lifecycle_rec(other, "gemini"),
            lifecycle_rec(missing, "claude"),
        ];
        let t = lifecycle_transcript_targets(&recs, |_, _, id| {
            (id != missing).then(|| std::path::PathBuf::from(format!("/x/{id}.jsonl")))
        });
        assert_eq!(t.len(), 1, "gemini and transcript-less records are skipped");
        assert_eq!(t[0].0, CSID);
    }

    #[test]
    fn a_bound_lifecycle_session_delivers_its_owed_finish_with_a_reason() {
        let dir = tempfile::tempdir().unwrap();
        let (t, registrar, outbox) = tailer(dir.path());
        let store = Arc::new(
            crate::session::session_lifecycle_store::SessionLifecycleStore::open(
                dir.path().join("terminal-sessions.json"),
            )
            .unwrap(),
        );
        registrar.attach_lifecycle_store(store.clone());
        store.record_open(lifecycle_rec(CSID, "claude"));
        // Finish BEFORE any coord row exists: stays local-only.
        let fin = store
            .set_finished(CSID, true, Some("dismissed".into()))
            .unwrap();
        assert_eq!(
            registrar.finish_session(CSID, fin.record.finished_at),
            crate::session::session_lifecycle_store::FinishSync::LocalOnly(
                crate::session::session_lifecycle_store::LocalOnlyReason::NoCoordSession
            )
        );

        let path = transcript(dir.path(), CSID, "{\"type\":\"user\"}\n");
        let id = identity_from_path(&path).unwrap();
        assert_eq!(
            apply_retrying(&t, &id, &path, &Decision::Mint(Uuid::new_v4())),
            Some(AutoBindKind::Minted)
        );
        let finished: Vec<_> = outbox
            .pending()
            .unwrap()
            .into_iter()
            .filter(|r| r.event_kind == SessionEventKind::Finished.as_str())
            .collect();
        assert_eq!(finished.len(), 1, "binding delivers the owed mark");
        assert_eq!(finished[0].payload["finish_reason"], "dismissed");
    }
}
