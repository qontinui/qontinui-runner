//! **Phase 1** of plan
//! `2026-08-26-claude-code-session-repository-in-qontinui-web`: the one-shot
//! scanner behind `qontinui-pr session-archive-backfill`.
//!
//! It walks every discovered Claude Code account home, parses each JSONL
//! head/tail for metadata, and upserts head rows plus **byte-verbatim**
//! archived bodies through `POST /api/v1/session-repository`.
//!
//! ## Why the backfill is Phase 1 and not Phase 5
//!
//! Rebuilding the primary runner closes every interactive Claude Code tab, and
//! the recovery procedure today is to screenshot the terminal tabs first. The
//! transcripts are not lost when that fails — they are on disk — but nothing
//! durable *knows the session existed and was unfinished*. The transcripts
//! already on disk are the entire operator ask, and archiving them needs no
//! live path at all. This is the only phase that makes the next rebuild
//! survivable.
//!
//! Measured on the operator box 2026-08-27 across 7 discovered account homes:
//! **8,303 `.jsonl` files, of which 6,407 are sessions** — the remaining 1,896
//! are subagent side-transcripts, which are not sessions and are excluded with
//! a stated count (see [`discovery::transcripts_in`]). The plan's headline
//! "8,308" figure is the raw file count, so the archive's row count is expected
//! to land near 6,407, not 8,308.
//!
//! ## The three rules this module exists to keep
//!
//! 1. **Bodies are never mutated.** The archive is byte-verbatim and its
//!    `content_sha256` covers the bytes as they sit on disk, so an export can
//!    be verified against the original file. Plan §5 measured the shipped
//!    `redact.rs` sweep at **57% false positives** over a 41 MB slice of this
//!    corpus while missing every long-lived shape it should have caught, so
//!    masking here would corrupt the corpus this plan exists to make
//!    searchable *and* make the digest permanently unverifiable. Instead
//!    [`secret_detector`] records `secret_finding_count` +
//!    `secret_finding_kinds` — an audit signal, never a gate and never a mask.
//!
//! 2. **Tenancy is recorded with its provenance, never guessed silently.**
//!    [`tenancy`] owns that; §3.6 rule 5 makes the `tenant_source` histogram a
//!    first-class output of every run, which [`push::BackfillReport::render`]
//!    prints in full including the empty buckets.
//!
//! 3. **Anything unobservable is left OUT, not defaulted.** The web route
//!    treats an omitted field as "leave it alone", which is what lets this
//!    scanner and the web archiver share one row. So `closeout_state` (§3.4
//!    derives it from signals that are not on disk), `launch_command` (never
//!    recorded anywhere) and `state = "abandoned"` (nothing distinguishes it
//!    from closed) are simply never sent.
//!
//! ## Idempotence
//!
//! `(claude_session_id, account_label)` is the row identity and
//! `content_sha256` is what makes a re-POST a no-op, so re-running is safe by
//! construction on the server side. [`push::ScanState`] additionally makes it
//! **cheap**, by not re-uploading a transcript whose digest has not moved.
//! Local analysis still runs on every pass, so the report is complete every
//! time rather than only on the first.

pub mod discovery;
pub mod metadata;
pub mod push;
pub mod secret_detector;
pub mod tenancy;

use std::collections::HashMap;
use std::path::PathBuf;

use uuid::Uuid;

use discovery::AccountHome;
use metadata::RegistryRecord;
use push::{BackfillReport, ScanState, SessionArtifactUpsert, SessionSink};
use tenancy::{RepoTenantCandidates, RepoTenantMap};

/// How long a transcript with no registry record may sit untouched before this
/// scanner calls its session `closed`.
///
/// The registry is the authority whenever it knows the session. This threshold
/// only decides the sessions it does NOT know — an account home the runner
/// never hosted, or a session from before the registry existed. A day is well
/// past any plausible think-time gap in a live pane and well short of the
/// corpus's 55-day span, so it separates "this tab is still open right now"
/// from "this is history" without either being a close call.
pub const DEFAULT_IDLE_CUTOFF_MS: i64 = 24 * 60 * 60 * 1000;

/// Everything a run needs, resolved by the caller so the driver is pure over
/// its inputs and testable without a machine.
pub struct BackfillOptions {
    /// The account homes to walk, in scan order.
    pub homes: Vec<AccountHome>,
    /// The runner's own session registry, merged across instances.
    pub registry: HashMap<String, RegistryRecord>,
    /// This device's coord tenant bindings — the `coord.tenant_devices` half
    /// of the D2 rule, and the sole-binding fallback.
    pub device_bindings: Vec<Uuid>,
    /// An operator-supplied projection of `coord.tenant_repos`. `None` means
    /// coord's repo rule is UNAVAILABLE from here — which is not the same as
    /// it having returned nothing; see [`tenancy`].
    pub repo_map: Option<RepoTenantMap>,
    /// This machine's coord device id.
    pub device_id: Option<String>,
    /// This machine's hostname, for the head row's `machine_hostname`.
    pub machine_hostname: Option<String>,
    /// `machine.json::active_tenant_id` — the machine-wide default pin. The
    /// LAST tenant source, consulted only when the device's binding list is
    /// empty; see `tenancy::sole_binding` for why an empty list is UNKNOWN
    /// rather than zero, and why the pin can never be labelled better than
    /// `derived_sole_binding`.
    pub machine_pin_tenant: Option<Uuid>,
    /// See [`DEFAULT_IDLE_CUTOFF_MS`].
    pub idle_cutoff_ms: i64,
    /// Wall clock at the start of the run, unix millis.
    pub now_ms: i64,
    /// Archive at most this many transcripts (scan order).
    pub limit: Option<usize>,
    /// Only this account label.
    pub account_filter: Option<String>,
    /// Ignore the local digest cache and re-send everything.
    pub force: bool,
}

/// Millis since the unix epoch as an RFC-3339 UTC timestamp, or `None` for a
/// value that is not a plausible timestamp.
///
/// A zero or negative `openedAt` is a registry row that never recorded one;
/// sending `1970-01-01` would put a fabricated lifetime on the head row.
fn ms_to_rfc3339(ms: Option<i64>) -> Option<String> {
    let ms = ms.filter(|m| *m > 0)?;
    chrono::DateTime::from_timestamp_millis(ms).map(|dt| dt.to_rfc3339())
}

/// The lifecycle `state` for one session.
///
/// The registry wins whenever it knows the session — it is the runner's own
/// observation of the pane, not an inference. For a session it does not know,
/// the only evidence on disk is when the transcript was last written, so the
/// answer is `open` inside the idle window and `closed` outside it.
///
/// `abandoned` is never returned. Nothing on disk distinguishes an abandoned
/// session from a closed one, and inventing the difference would put a guess
/// in a lifecycle column that other surfaces filter on.
fn derive_state(
    registry: Option<&RegistryRecord>,
    last_activity_ms: Option<i64>,
    now_ms: i64,
    idle_cutoff_ms: i64,
) -> &'static str {
    if let Some(state) = registry.and_then(|r| r.state.as_deref()) {
        return match state {
            "open" => "open",
            _ => "closed",
        };
    }
    match last_activity_ms {
        Some(ms) if now_ms.saturating_sub(ms) <= idle_cutoff_ms => "open",
        _ => "closed",
    }
}

/// Build the upsert payload for one transcript.
///
/// Split out from [`backfill`] so every derivation it performs is unit-testable
/// without a filesystem, a registry or a backend.
#[allow(clippy::too_many_arguments)]
pub fn build_payload(
    home: &AccountHome,
    claude_session_id: &str,
    raw: &[u8],
    meta: &metadata::TranscriptMetadata,
    findings: &secret_detector::SecretFindings,
    registry: Option<&RegistryRecord>,
    attribution: &tenancy::TenantAttribution,
    opts: &BackfillOptions,
) -> SessionArtifactUpsert {
    let working_dir = meta
        .working_dir
        .clone()
        .or_else(|| registry.and_then(|r| r.working_dir.clone()));
    let last_activity_at = meta
        .last_activity_at
        .clone()
        .or_else(|| ms_to_rfc3339(registry.and_then(|r| r.last_seen_at_ms)));
    let last_activity_ms = last_activity_at
        .as_deref()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.timestamp_millis())
        .or_else(|| registry.and_then(|r| r.last_seen_at_ms));

    let mut payload = SessionArtifactUpsert {
        claude_session_id: claude_session_id.to_string(),
        account_label: Some(home.label.clone()),

        tenant_id: attribution.tenant_id.map(|t| t.to_string()),
        tenant_source: attribution.source.as_str().to_string(),
        device_id: opts
            .device_id
            .as_deref()
            // The column is a UUID server-side, so a non-UUID device id is
            // dropped rather than sent to be 422'd — `machine_id` below is a
            // free-text column and still carries it.
            .filter(|d| Uuid::parse_str(d).is_ok())
            .map(str::to_string),
        machine_hostname: opts.machine_hostname.clone(),

        task_run_id: registry.and_then(|r| r.task_run_id.clone()),

        config_dir: Some(home.config_dir.to_string_lossy().to_string()),
        repo: metadata::repo_from_working_dir(working_dir.as_deref()),
        working_dir,
        git_branch: meta.git_branch.clone(),
        // Every file under a Claude Code account home's `projects/` tree was
        // written by Claude Code. That is an observation, not a default.
        provider: Some(
            registry
                .and_then(|r| r.provider.clone())
                .unwrap_or_else(|| "claude".to_string()),
        ),
        restore_tier: registry.and_then(|r| r.restore_tier.clone()),
        machine_id: opts.device_id.clone(),
        // Only the affirmative case is recorded: `bypassPermissions: false`
        // says the session was not in bypass mode, which is not the same as
        // knowing which mode it WAS in.
        permission_mode: registry
            .and_then(|r| r.bypass_permissions)
            .filter(|b| *b)
            .map(|_| "bypassPermissions".to_string()),

        turn_count: Some(meta.turn_count),
        first_prompt: meta.first_prompt.clone(),
        last_prompt: meta.last_prompt.clone(),
        ai_title: meta
            .ai_title
            .clone()
            .or_else(|| registry.and_then(|r| r.title.clone())),
        session_name: registry.and_then(|r| r.session_name.clone()),
        name_source: registry.and_then(|r| r.name_source.clone()),

        started_at: meta
            .started_at
            .clone()
            .or_else(|| ms_to_rfc3339(registry.and_then(|r| r.opened_at_ms))),
        last_activity_at,
        // Only a registry that recorded a close can date one. A transcript's
        // last line is when it stopped being written, which is `last_activity`
        // — presenting it as `ended_at` would assert a close that may never
        // have happened.
        ended_at: ms_to_rfc3339(registry.and_then(|r| r.closed_at_ms)),
        state: Some(
            derive_state(registry, last_activity_ms, opts.now_ms, opts.idle_cutoff_ms).to_string(),
        ),

        secret_finding_count: findings.count,
        secret_finding_kinds: findings.kinds.clone(),
        ..Default::default()
    };
    push::attach_body(&mut payload, raw);
    payload
}

/// Run one backfill pass.
///
/// `sink` is `None` for a dry run: every transcript is still read, digested,
/// detector-scanned and attributed — so the report, the histogram and the
/// findings are exactly what a real run would produce — and nothing is sent.
/// That matters at this corpus size: a dry run is how an operator sees what
/// 8,308 uploads would say before making them.
pub async fn backfill(
    opts: &BackfillOptions,
    sink: Option<&dyn SessionSink>,
    state: &mut ScanState,
) -> BackfillReport {
    let mut report = BackfillReport {
        // Recorded up front so the report can say whether `derived_repo 0`
        // means "nothing mapped" or "never asked".
        repo_rule_available: opts.repo_map.is_some(),
        ..Default::default()
    };
    // Counts transcripts that reached the WRITE decision — i.e. everything the
    // cache did not already account for. Deliberately not "successfully
    // pushed": `--limit 10` has to bound a dry run too, and a dry run pushes
    // nothing at all.
    let mut attempted = 0usize;

    for home in &opts.homes {
        if let Some(filter) = opts.account_filter.as_deref() {
            if home.label != filter {
                continue;
            }
        }
        let transcripts = discovery::transcripts_in(home);
        report
            .per_home
            .push((home.label.clone(), transcripts.len()));
        report.subagent_transcripts_skipped += discovery::count_subagent_transcripts(home);

        for path in transcripts {
            if let Some(limit) = opts.limit {
                if attempted >= limit {
                    break;
                }
            }
            let Some(session_id) = path.file_stem().and_then(|s| s.to_str()) else {
                report.record_error(format!("{}: unreadable filename", path.display()));
                continue;
            };
            report.scanned += 1;

            let raw = match std::fs::read(&path) {
                Ok(bytes) => bytes,
                Err(e) => {
                    report.record_error(format!("{}: read failed: {e}", path.display()));
                    continue;
                }
            };
            if raw.is_empty() {
                // A zero-byte transcript has nothing to archive, and a body of
                // zero bytes would claim an archive this store does not hold.
                report.skipped_empty += 1;
                continue;
            }

            let digest = push::digest_bytes(&raw);
            let meta = metadata::parse_transcript(&raw);
            let findings = secret_detector::scan_bytes(&raw);
            report.record_findings(&findings);
            report.unparsable_window_lines += meta.unparsable_window_lines;
            if meta
                .session_id_in_body
                .as_deref()
                .is_some_and(|s| s != session_id)
            {
                report.session_id_mismatches += 1;
            }

            let registry = opts.registry.get(session_id);
            let session_tenant = registry
                .and_then(|r| r.tenant_id.as_deref())
                .and_then(|t| Uuid::parse_str(t.trim()).ok());
            let repo = metadata::repo_from_working_dir(
                meta.working_dir
                    .as_deref()
                    .or_else(|| registry.and_then(|r| r.working_dir.as_deref())),
            );
            let candidates = match &opts.repo_map {
                Some(map) => map.candidates(repo.as_deref(), &opts.device_bindings),
                // No map means coord's D2 rule could not be evaluated from
                // here at all. Deliberately NOT an empty candidate set.
                None => RepoTenantCandidates::Unavailable,
            };
            let attribution = tenancy::resolve_tenant(
                session_tenant,
                &candidates,
                &opts.device_bindings,
                opts.machine_pin_tenant,
            );
            report.record_attribution(&attribution);

            let payload = build_payload(
                home,
                session_id,
                &raw,
                &meta,
                &findings,
                registry,
                &attribution,
                opts,
            );
            if let Some(s) = payload.state.as_deref() {
                report.record_state(s);
            }

            if !opts.force && state.is_known(&home.label, session_id, &digest) {
                report.unchanged_local += 1;
                continue;
            }

            attempted += 1;

            let Some(sink) = sink else {
                // Dry run: everything above already happened, which is the
                // point — only the write is withheld.
                continue;
            };

            match sink.upsert(&payload).await {
                Ok(outcome) => {
                    if outcome.body_written {
                        report.bytes_archived += raw.len() as u64;
                    }
                    if outcome.created {
                        report.created += 1;
                    } else if outcome.changed {
                        report.updated += 1;
                    } else {
                        report.unchanged_remote += 1;
                    }
                    state.record(&home.label, session_id, &digest);
                }
                Err(e) => {
                    report.record_error(format!("{}: {e:#}", path.display()));
                }
            }
        }
    }

    report
}

/// One string field out of `~/.qontinui/machine.json`.
///
/// `crate::machine_identity` owns that file and reads `device_id` from it; this
/// reads the two neighbouring fields the archive needs without duplicating its
/// resolver. Fail-open on every error, exactly like that module's own loaders.
fn machine_file_field(key: &str) -> Option<String> {
    crate::machine_identity::machine_file_path()
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        .and_then(|v| v.get(key).and_then(|h| h.as_str()).map(str::to_string))
        .filter(|s| !s.trim().is_empty())
}

/// This machine's hostname, from `~/.qontinui/machine.json` (which the device
/// registration already writes) falling back to the platform env var.
pub fn machine_hostname() -> Option<String> {
    machine_file_field("hostname").or_else(|| {
        std::env::var("COMPUTERNAME")
            .or_else(|_| std::env::var("HOSTNAME"))
            .ok()
            .filter(|s| !s.trim().is_empty())
    })
}

/// `machine.json::active_tenant_id` — the machine-wide default tenant pin.
///
/// The LAST tenant source, and a deliberately weak one: it is documented as
/// "the default for NEW sessions", not "the only tenant this device serves".
/// [`tenancy::sole_binding`] consults it only when the device's binding list is
/// empty and never labels it better than `derived_sole_binding`.
pub fn machine_pin_tenant() -> Option<Uuid> {
    machine_file_field("active_tenant_id").and_then(|s| Uuid::parse_str(s.trim()).ok())
}

/// Where the re-scan cache lives by default.
pub fn default_state_path() -> PathBuf {
    push::default_state_path(&metadata::default_runner_dir())
}

#[cfg(test)]
mod tests {
    use super::*;
    use push::UpsertOutcome;
    use std::sync::Mutex;

    fn home(dir: &std::path::Path, label: &str) -> AccountHome {
        AccountHome {
            config_dir: dir.to_path_buf(),
            label: label.to_string(),
            wrapper: "claude".to_string(),
        }
    }

    fn opts_for(homes: Vec<AccountHome>) -> BackfillOptions {
        BackfillOptions {
            homes,
            registry: HashMap::new(),
            device_bindings: Vec::new(),
            repo_map: None,
            device_id: Some("6b2a4a6e-0000-0000-0000-000000000001".into()),
            machine_hostname: Some("test-box".into()),
            machine_pin_tenant: None,
            idle_cutoff_ms: DEFAULT_IDLE_CUTOFF_MS,
            // Well past the SAMPLE transcript's own 2026-08-26 timestamp, so the
            // idle heuristic reads it as history rather than as a live pane.
            now_ms: 1_790_000_000_000,
            limit: None,
            account_filter: None,
            force: false,
        }
    }

    /// A sink that records what it was asked to write.
    #[derive(Default)]
    struct RecordingSink {
        seen: Mutex<Vec<SessionArtifactUpsert>>,
    }

    #[async_trait::async_trait]
    impl SessionSink for RecordingSink {
        async fn upsert(&self, payload: &SessionArtifactUpsert) -> anyhow::Result<UpsertOutcome> {
            self.seen.lock().unwrap().push(payload.clone());
            Ok(UpsertOutcome {
                created: true,
                changed: true,
                body_written: true,
            })
        }
    }

    fn write_transcript(home_dir: &std::path::Path, project: &str, session: &str, body: &str) {
        let dir = home_dir.join("projects").join(project);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("{session}.jsonl")), body).unwrap();
    }

    const SAMPLE: &str = r#"{"type":"summary","summary":"A test session"}
{"type":"user","cwd":"D:/qontinui-root/qontinui-runner","sessionId":"sess-a","gitBranch":"main","timestamp":"2026-08-26T09:00:00Z","message":{"role":"user","content":"do the thing"}}"#;

    #[tokio::test]
    async fn a_scanned_transcript_is_archived_verbatim_with_its_own_digest() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join(".claude-gmail");
        write_transcript(&dir, "proj", "sess-a", SAMPLE);

        let sink = RecordingSink::default();
        let mut state = ScanState::default();
        let report = backfill(
            &opts_for(vec![home(&dir, "gmail")]),
            Some(&sink),
            &mut state,
        )
        .await;

        assert_eq!(report.scanned, 1);
        assert_eq!(report.created, 1);
        assert_eq!(report.errors, 0);
        let seen = sink.seen.lock().unwrap();
        let p = &seen[0];
        assert_eq!(p.claude_session_id, "sess-a");
        assert_eq!(p.account_label.as_deref(), Some("gmail"));
        assert_eq!(p.body.as_deref(), Some(SAMPLE), "the body must be verbatim");
        assert_eq!(
            p.content_sha256.as_deref(),
            Some(push::digest_bytes(SAMPLE.as_bytes()).as_str())
        );
        assert_eq!(
            p.body_source.as_deref(),
            Some(push::BODY_SOURCE_DISK_VERBATIM)
        );
        assert_eq!(p.repo.as_deref(), Some("qontinui-runner"));
        assert_eq!(p.git_branch.as_deref(), Some("main"));
        assert_eq!(p.ai_title.as_deref(), Some("A test session"));
        assert_eq!(p.first_prompt.as_deref(), Some("do the thing"));
    }

    #[tokio::test]
    async fn a_dry_run_produces_the_full_report_and_writes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join(".claude-gmail");
        write_transcript(&dir, "proj", "sess-a", SAMPLE);

        let mut state = ScanState::default();
        let report = backfill(&opts_for(vec![home(&dir, "gmail")]), None, &mut state).await;

        assert_eq!(report.scanned, 1);
        assert_eq!(report.created, 0);
        assert_eq!(report.bytes_archived, 0);
        // The histogram and the state tally are still complete — that is the
        // whole value of a dry run over an 8,308-file corpus.
        assert_eq!(report.tenant_sources.total(), 1);
        assert_eq!(report.states.get("closed").copied(), Some(1));
        assert!(state.is_empty(), "a dry run must not populate the cache");
    }

    #[tokio::test]
    async fn a_second_pass_skips_unchanged_content_but_still_reports_on_it() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join(".claude-gmail");
        write_transcript(&dir, "proj", "sess-a", SAMPLE);
        let homes = vec![home(&dir, "gmail")];

        let sink = RecordingSink::default();
        let mut state = ScanState::default();
        let _ = backfill(&opts_for(homes.clone()), Some(&sink), &mut state).await;
        let second = backfill(&opts_for(homes.clone()), Some(&sink), &mut state).await;

        assert_eq!(second.unchanged_local, 1);
        assert_eq!(second.created, 0);
        assert_eq!(sink.seen.lock().unwrap().len(), 1, "no re-upload");
        // The report is complete on the second pass too.
        assert_eq!(second.scanned, 1);
        assert_eq!(second.tenant_sources.total(), 1);

        // …and `--force` re-sends it.
        let mut forced = opts_for(homes);
        forced.force = true;
        let third = backfill(&forced, Some(&sink), &mut state).await;
        assert_eq!(third.created, 1);
        assert_eq!(sink.seen.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn an_empty_transcript_is_skipped_rather_than_archived_as_zero_bytes() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join(".claude-gmail");
        write_transcript(&dir, "proj", "sess-empty", "");

        let sink = RecordingSink::default();
        let mut state = ScanState::default();
        let report = backfill(
            &opts_for(vec![home(&dir, "gmail")]),
            Some(&sink),
            &mut state,
        )
        .await;
        assert_eq!(report.skipped_empty, 1);
        assert!(sink.seen.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn the_same_session_id_under_two_homes_stays_two_rows() {
        // The identity key is (claude_session_id, account_label) precisely
        // because a session id is unique per account home, not globally.
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join(".claude-gmail");
        let b = tmp.path().join(".claude-hotmail");
        write_transcript(&a, "proj", "sess-a", SAMPLE);
        write_transcript(&b, "proj", "sess-a", SAMPLE);

        let sink = RecordingSink::default();
        let mut state = ScanState::default();
        let report = backfill(
            &opts_for(vec![home(&a, "gmail"), home(&b, "hotmail")]),
            Some(&sink),
            &mut state,
        )
        .await;

        assert_eq!(report.scanned, 2);
        let seen = sink.seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        let labels: Vec<&str> = seen
            .iter()
            .map(|p| p.account_label.as_deref().unwrap())
            .collect();
        assert_eq!(labels, vec!["gmail", "hotmail"]);
    }

    #[tokio::test]
    async fn the_account_filter_and_limit_bound_a_run() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join(".claude-gmail");
        let b = tmp.path().join(".claude-hotmail");
        write_transcript(&a, "proj", "s1", SAMPLE);
        write_transcript(&a, "proj", "s2", SAMPLE);
        write_transcript(&b, "proj", "s3", SAMPLE);
        let homes = vec![home(&a, "gmail"), home(&b, "hotmail")];

        let sink = RecordingSink::default();
        let mut state = ScanState::default();
        let mut o = opts_for(homes.clone());
        o.account_filter = Some("hotmail".into());
        let filtered = backfill(&o, Some(&sink), &mut state).await;
        assert_eq!(filtered.scanned, 1);

        let sink2 = RecordingSink::default();
        let mut state2 = ScanState::default();
        let mut o2 = opts_for(homes.clone());
        o2.limit = Some(1);
        let limited = backfill(&o2, Some(&sink2), &mut state2).await;
        assert_eq!(sink2.seen.lock().unwrap().len(), 1);
        assert_eq!(limited.created, 1);

        // `--limit` must bound a DRY RUN too. It counts transcripts that
        // reached the write decision, not successful writes — a dry run makes
        // none of the latter, so counting those would make the flag inert
        // exactly where an operator reaches for it first.
        let mut state3 = ScanState::default();
        let mut o3 = opts_for(homes);
        o3.limit = Some(1);
        let dry = backfill(&o3, None, &mut state3).await;
        assert_eq!(dry.scanned, 1, "the limit did not bound the dry run");
    }

    #[test]
    fn the_registry_state_beats_the_idle_heuristic() {
        let open = RegistryRecord {
            state: Some("open".into()),
            ..Default::default()
        };
        // Ancient last activity, but the runner says the pane is open.
        assert_eq!(
            derive_state(
                Some(&open),
                Some(0),
                1_756_200_000_000,
                DEFAULT_IDLE_CUTOFF_MS
            ),
            "open"
        );

        let closed = RegistryRecord {
            state: Some("closed".into()),
            ..Default::default()
        };
        // Activity one second ago, but the runner recorded a close.
        assert_eq!(
            derive_state(
                Some(&closed),
                Some(1_756_199_999_000),
                1_756_200_000_000,
                DEFAULT_IDLE_CUTOFF_MS
            ),
            "closed"
        );
    }

    #[test]
    fn an_unknown_session_is_open_only_inside_the_idle_window() {
        let now = 1_756_200_000_000i64;
        assert_eq!(
            derive_state(None, Some(now - 1000), now, DEFAULT_IDLE_CUTOFF_MS),
            "open"
        );
        assert_eq!(
            derive_state(
                None,
                Some(now - DEFAULT_IDLE_CUTOFF_MS - 1),
                now,
                DEFAULT_IDLE_CUTOFF_MS
            ),
            "closed"
        );
        // No evidence at all is `closed`, never `open` — a transcript with no
        // timestamps is history, and defaulting to open would report the
        // entire pre-timestamp corpus as live.
        assert_eq!(
            derive_state(None, None, now, DEFAULT_IDLE_CUTOFF_MS),
            "closed"
        );
    }

    #[test]
    fn abandoned_is_never_emitted() {
        for state in ["open", "closed", "abandoned", "weird"] {
            let r = RegistryRecord {
                state: Some(state.into()),
                ..Default::default()
            };
            let got = derive_state(Some(&r), None, 0, DEFAULT_IDLE_CUTOFF_MS);
            assert!(got == "open" || got == "closed", "{state} -> {got}");
        }
    }

    #[test]
    fn a_zero_timestamp_is_not_dated_to_1970() {
        assert_eq!(ms_to_rfc3339(Some(0)), None);
        assert_eq!(ms_to_rfc3339(None), None);
        assert!(ms_to_rfc3339(Some(1_756_200_000_000))
            .unwrap()
            .starts_with("2025-"));
    }

    #[tokio::test]
    async fn a_registry_tenant_is_recorded_as_derived_sole_binding() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join(".claude-gmail");
        write_transcript(&dir, "proj", "sess-a", SAMPLE);

        let tenant = Uuid::from_u128(42);
        let mut registry = HashMap::new();
        registry.insert(
            "sess-a".to_string(),
            RegistryRecord {
                tenant_id: Some(tenant.to_string()),
                state: Some("closed".into()),
                session_name: Some("08-26 backfill".into()),
                name_source: Some("operator".into()),
                bypass_permissions: Some(true),
                ..Default::default()
            },
        );
        let mut o = opts_for(vec![home(&dir, "gmail")]);
        o.registry = registry;

        let sink = RecordingSink::default();
        let mut state = ScanState::default();
        let report = backfill(&o, Some(&sink), &mut state).await;

        let seen = sink.seen.lock().unwrap();
        let p = &seen[0];
        assert_eq!(p.tenant_id.as_deref(), Some(tenant.to_string().as_str()));
        assert_eq!(p.tenant_source, "derived_sole_binding");
        assert_ne!(p.tenant_source, "declared");
        assert_eq!(p.session_name.as_deref(), Some("08-26 backfill"));
        assert_eq!(p.permission_mode.as_deref(), Some("bypassPermissions"));
        assert_eq!(
            report
                .tenant_sources
                .get(tenancy::TenantSource::DerivedSoleBinding),
            1
        );
    }

    #[tokio::test]
    async fn closeout_state_and_launch_command_are_never_sent() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join(".claude-gmail");
        write_transcript(&dir, "proj", "sess-a", SAMPLE);

        let sink = RecordingSink::default();
        let mut state = ScanState::default();
        let _ = backfill(
            &opts_for(vec![home(&dir, "gmail")]),
            Some(&sink),
            &mut state,
        )
        .await;

        let seen = sink.seen.lock().unwrap();
        let json = serde_json::to_string(&seen[0]).unwrap();
        assert!(
            !json.contains("closeout_state"),
            "§3.4 derives closeout_state from signals that are not on disk"
        );
        assert!(!json.contains("launch_command"));
        assert!(!json.contains("ended_at"), "no registry close, no ended_at");
    }
}
