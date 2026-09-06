//! The upsert payload, the HTTP sink, and the local re-scan cache for
//! `qontinui-pr session-archive-backfill`.
//!
//! ## The wire contract (qontinui-web `POST /api/v1/session-repository`)
//!
//! Four properties of that route shape everything here, and each one is
//! load-bearing rather than incidental:
//!
//! 1. **Identity is `(claude_session_id, coalesce(account_label,''))`** — and
//!    NOT the organization. A Claude Code session id is unique per account
//!    home rather than globally, so the home is what disambiguates two
//!    sessions that share an id after a resume rotation. The organization was
//!    deliberately removed from the key during implementation because the web
//!    archiver is a scheduled job with no calling principal and can only ever
//!    write `organization_id = NULL`; with org in the key, the same session
//!    written by both writers produced two rows and never self-healed.
//!
//! 2. **Omitted means untouched.** The route writes only the fields the
//!    request actually carried (`model_fields_set`). That is what lets this
//!    scanner and the web archiver share one row: a metadata promotion cannot
//!    blank the body this scanner archived, and this scanner does not have to
//!    invent values for the lifecycle fields it cannot observe. Every field
//!    below is therefore `Option` + `skip_serializing_if`, and **an unknown
//!    value is left out rather than sent as null**.
//!
//! 3. **This scanner is the SOLE writer of the body**, stamping
//!    `body_source = "disk_verbatim"` (plan §5, "Two ingest paths, one
//!    digest"). The bytes go up exactly as they sit on disk — never through
//!    `redact.rs`, whose live-path sweep §5 measured at 57% false positives
//!    and which would make `content_sha256` unverifiable against the original
//!    file forever. The web archiver's fallback body is stamped
//!    `coord_redacted` precisely so the two can never be confused.
//!
//! 4. **Auth is the runner's own device JWT**, attached by
//!    [`crate::auth::attach_device_auth`]. The route's dual-auth dependency
//!    (`get_audit_actor_user`) accepts it; a route wired to `current_active_user`
//!    alone would be Cognito-only and would leave this corpus permanently
//!    empty, which is why a 401 here gets that diagnosis spelled out rather
//!    than relayed bare.
//!
//! ## `body` vs `body_base64`
//!
//! Exactly one may be sent. A JSON string cannot carry invalid UTF-8, so for
//! anything that is not valid UTF-8 the base64 door is the only one under which
//! "byte-verbatim" survives the transport — sending such a transcript as `body`
//! would replace the bad bytes with U+FFFD and still produce a
//! `content_sha256`, over the wrong bytes. Valid UTF-8 (which every transcript
//! measured so far is) travels as `body`, whose round-trip through
//! `str.encode("utf-8")` server-side is exact and which avoids inflating a
//! 3.5 GB corpus by a third on the wire. Either way the digest is computed over
//! the **bytes read from disk**, before any encoding decision.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use base64::Engine;
use serde::Serialize;
use sha2::{Digest, Sha256};

use super::discovery::AccountHome;
use super::secret_detector::SecretFindings;
use super::tenancy::{TenantAttribution, TenantSource, TenantSourceHistogram};

/// `body_source` for everything this scanner writes.
pub const BODY_SOURCE_DISK_VERBATIM: &str = "disk_verbatim";

/// The `POST /api/v1/session-repository` request body.
///
/// Mirrors the web `SessionArtifactUpsert` schema field for field, minus the
/// ones a runner has no business supplying: there is deliberately **no
/// `organization_id`** (server-derived from the principal — a caller-supplied
/// org is a scope-escalation bug, and the surest defence is to give the request
/// nowhere to put one) and no `body_object_key` / `byte_count` (server-owned:
/// a caller cannot name the key its bytes were stored under).
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct SessionArtifactUpsert {
    pub claude_session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account_label: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<String>,
    /// Always sent, because the row's default is `unknown` and an unstated
    /// provenance is the exact defect the column exists to prevent.
    pub tenant_source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub machine_hostname: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_run_id: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub config_dir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub restore_tier: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub machine_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<String>,

    /// The archived bytes as text — used when they are valid UTF-8.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// The archived bytes, base64 — used when they are not.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body_base64: Option<String>,
    /// Required whenever a body is supplied.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body_source: Option<String>,
    /// Checked server-side against sha256 of the supplied bytes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_sha256: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ai_title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name_source: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_activity_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,

    /// Written by the detector, always as a pair. `kinds` is `Some(vec![])`
    /// when the detector ran and found nothing — the web column distinguishes
    /// that from `NULL` ("never scanned"), and collapsing the two would make
    /// an unscanned row look audited.
    pub secret_finding_count: usize,
    pub secret_finding_kinds: Vec<String>,
}

/// Hex sha256 over the bytes exactly as read from disk.
///
/// Over BYTES, never over a decoded string: a digest over a decoded string is
/// a digest over whatever encoding was in force at that moment, which is
/// precisely the ambiguity `content_sha256` exists to remove.
pub fn digest_bytes(raw: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(raw);
    format!("{:x}", hasher.finalize())
}

/// Attach the archived bytes to a payload under the right door.
pub fn attach_body(payload: &mut SessionArtifactUpsert, raw: &[u8]) {
    payload.content_sha256 = Some(digest_bytes(raw));
    payload.body_source = Some(BODY_SOURCE_DISK_VERBATIM.to_string());
    match std::str::from_utf8(raw) {
        Ok(text) => payload.body = Some(text.to_string()),
        Err(_) => {
            payload.body_base64 = Some(base64::engine::general_purpose::STANDARD.encode(raw));
        }
    }
}

// ===========================================================================
// The local re-scan cache
// ===========================================================================

/// A digest the scanner has already successfully pushed, keyed
/// `<account_label>/<claude_session_id>`.
///
/// The web route is idempotent on its own — it answers `changed: false` and an
/// `X-Session-Unchanged` header for a re-POST of identical content — so this
/// cache is not what makes the backfill safe to re-run. It is what makes
/// re-running **cheap**: without it every pass re-uploads 3.5 GB to be told
/// nothing changed. Losing the cache costs bandwidth, never correctness, which
/// is why it is a plain JSON file with no locking and no schema version.
#[derive(Debug, Clone, Default)]
pub struct ScanState {
    digests: BTreeMap<String, String>,
    dirty: bool,
}

fn state_key(account_label: &str, session_id: &str) -> String {
    format!("{account_label}/{session_id}")
}

impl ScanState {
    /// Load, treating every failure as an empty cache — see the type doc.
    pub fn load(path: &Path) -> Self {
        let digests = std::fs::read_to_string(path)
            .ok()
            .and_then(|t| serde_json::from_str::<BTreeMap<String, String>>(&t).ok())
            .unwrap_or_default();
        Self {
            digests,
            dirty: false,
        }
    }

    pub fn is_known(&self, account_label: &str, session_id: &str, digest: &str) -> bool {
        self.digests
            .get(&state_key(account_label, session_id))
            .is_some_and(|d| d == digest)
    }

    pub fn record(&mut self, account_label: &str, session_id: &str, digest: &str) {
        let key = state_key(account_label, session_id);
        if self.digests.get(&key).map(String::as_str) != Some(digest) {
            self.digests.insert(key, digest.to_string());
            self.dirty = true;
        }
    }

    pub fn len(&self) -> usize {
        self.digests.len()
    }

    pub fn is_empty(&self) -> bool {
        self.digests.is_empty()
    }

    /// Persist if anything changed. Best-effort: a failed save is reported to
    /// the caller but never fails the run, because the cache is an
    /// optimisation and the corpus is already archived by then.
    pub fn save(&mut self, path: &Path) -> Result<()> {
        if !self.dirty {
            return Ok(());
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        let text = serde_json::to_string_pretty(&self.digests)?;
        std::fs::write(path, text).with_context(|| format!("write {}", path.display()))?;
        self.dirty = false;
        Ok(())
    }
}

/// Default location of the re-scan cache — beside the runner's own registry,
/// because it describes the same machine's sessions.
pub fn default_state_path(runner_dir: &Path) -> PathBuf {
    runner_dir.join("session-archive-backfill-state.json")
}

// ===========================================================================
// The sink
// ===========================================================================

/// What one upsert did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UpsertOutcome {
    pub created: bool,
    pub changed: bool,
    pub body_written: bool,
}

/// Where scanned sessions go. A trait so the scan driver is testable without a
/// backend — the same shape `plan_workunit_adapter::body_push::ArtifactSink`
/// uses for the plan library.
#[async_trait::async_trait]
pub trait SessionSink: Send + Sync {
    async fn upsert(&self, payload: &SessionArtifactUpsert) -> Result<UpsertOutcome>;
}

/// Per-request ceiling. Looser than the fleet's 10s pollers because a single
/// transcript can be 7 MB, but it must exist: a black-holing backend with no
/// timeout parks an 8,308-file run indefinitely rather than failing one file.
const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// Production sink: HTTP against qontinui-web with the runner's device JWT.
pub struct HttpSessionSink {
    base: String,
    client: reqwest::Client,
}

impl HttpSessionSink {
    pub fn new(base: impl Into<String>) -> Self {
        Self {
            base: base.into().trim_end_matches('/').to_string(),
            // One long-lived client for the whole run: `reqwest::Client` owns
            // the connection pool and the TLS config, so building one per file
            // would rebuild both 8,308 times and defeat connection reuse
            // entirely.
            client: reqwest::Client::builder()
                .timeout(REQUEST_TIMEOUT)
                .build()
                .unwrap_or_else(|e| {
                    tracing::warn!(
                        error = %e,
                        "session archive: could not build a timeout-bearing HTTP client; \
                         falling back to the default (no timeout)"
                    );
                    reqwest::Client::new()
                }),
        }
    }

    pub fn base(&self) -> &str {
        &self.base
    }
}

#[async_trait::async_trait]
impl SessionSink for HttpSessionSink {
    async fn upsert(&self, payload: &SessionArtifactUpsert) -> Result<UpsertOutcome> {
        let url = format!("{}/api/v1/session-repository", self.base);
        // The row's OWN tenant selects the bearer. The body already carries
        // `tenant_id` + `tenant_source` -- that is the row's attribution, and
        // on its own it does not make the credential correct: qontinui-web
        // derives ownership from the verified bearer, so a second tenant paired
        // to this box would otherwise have every upsert land under whichever
        // binding is default regardless of what the body says. Stating the
        // scope is D1's rule (populate the field AND present that tenant's
        // bearer, because fixing only one half fixes only one class).
        //
        // `for_session` maps an absent tenant -- `ambiguous` and `unknown`,
        // the two labels `tenancy::resolve_tenant` emits with no id -- to
        // `Unresolved` rather than to the default binding, so on a multi-bound
        // device an unattributed transcript degrades to unauthenticated
        // instead of being filed under a tenant nothing established.
        let scope = crate::auth::TenantScope::for_session(
            payload
                .tenant_id
                .as_deref()
                .and_then(|t| uuid::Uuid::parse_str(t.trim()).ok()),
        );
        let resp = crate::auth::attach_device_auth_for(self.client.post(&url).json(payload), scope)
            .send()
            .await
            .context("POST /api/v1/session-repository")?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            if status == reqwest::StatusCode::UNAUTHORIZED
                || status == reqwest::StatusCode::FORBIDDEN
            {
                anyhow::bail!(
                    "upsert session {} -> {status} {text}\n\
                     note: this client presents the runner's coord-issued DEVICE JWT. The \
                     qontinui-web session-repository routes must therefore accept a device \
                     bearer (app.api.deps.get_audit_actor_user, or another user-or-device \
                     dependency); a route wired to `current_active_user` alone is Cognito-only \
                     and rejects it, which would leave this corpus permanently empty.",
                    payload.claude_session_id
                );
            }
            anyhow::bail!(
                "upsert session {} -> {status} {text}",
                payload.claude_session_id
            );
        }
        // `X-Session-Unchanged` is the cheap answer; the body carries the
        // authoritative one. Read the body, because a proxy may drop headers.
        let parsed: serde_json::Value = resp
            .json()
            .await
            .context("parse session-repository upsert response")?;
        Ok(UpsertOutcome {
            created: parsed
                .get("created")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            changed: parsed
                .get("changed")
                .and_then(|v| v.as_bool())
                .unwrap_or(true),
            body_written: parsed
                .get("body_written")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
        })
    }
}

// ===========================================================================
// The run report
// ===========================================================================

/// Everything one backfill pass observed. Printed in full at the end of a run
/// — including the `tenant_source` histogram, which plan §3.6 rule 5 makes a
/// first-class output rather than a debug line.
#[derive(Debug, Clone, Default)]
pub struct BackfillReport {
    /// `(home label, transcripts found)`, scan order.
    pub per_home: Vec<(String, usize)>,
    /// Subagent side-transcripts deliberately left out of the corpus. Stated
    /// rather than silently dropped — it is the whole difference between the
    /// `.jsonl` count on disk and the row count in the archive.
    pub subagent_transcripts_skipped: usize,
    pub scanned: usize,
    pub created: usize,
    pub updated: usize,
    /// The server said the row already carried this exact content.
    pub unchanged_remote: usize,
    /// The local cache already held this digest, so nothing was sent.
    pub unchanged_local: usize,
    /// Empty files — nothing to archive, and a zero-byte body would claim one.
    pub skipped_empty: usize,
    pub errors: usize,
    /// Bytes of transcript actually uploaded.
    pub bytes_archived: u64,
    pub tenant_sources: TenantSourceHistogram,
    /// Transcripts with at least one finding.
    pub files_with_findings: usize,
    /// Total findings across the corpus.
    pub total_findings: usize,
    /// Per-kind file counts.
    pub findings_by_kind: BTreeMap<String, usize>,
    /// `state` values sent, so a reader can see how many rows were called
    /// open.
    pub states: BTreeMap<String, usize>,
    /// Transcripts whose in-body `sessionId` disagreed with the filename.
    pub session_id_mismatches: usize,
    /// Head/tail lines that would not parse as JSON, corpus-wide.
    pub unparsable_window_lines: usize,
    /// Whether coord's D2 repo→tenant rule could be evaluated at all this run.
    ///
    /// Without this the histogram lies by omission: `derived_repo 0` reads as
    /// "no session's repo mapped to a tenant" when the truth is usually "the
    /// rule was never asked". The same absence-is-not-zero discipline the
    /// fleet's `silent-empty-is-unknown` rule states, applied to the report
    /// that reports on it.
    pub repo_rule_available: bool,
    /// The first few failures, verbatim, so a run that ends `errors=17` says
    /// what went wrong instead of making the operator re-run it under a log
    /// filter.
    pub error_samples: Vec<String>,
}

/// How many failures the report quotes verbatim.
const ERROR_SAMPLE_LIMIT: usize = 10;

impl BackfillReport {
    pub fn record_error(&mut self, message: String) {
        self.errors += 1;
        if self.error_samples.len() < ERROR_SAMPLE_LIMIT {
            self.error_samples.push(message);
        }
    }

    pub fn record_findings(&mut self, findings: &SecretFindings) {
        if findings.count == 0 {
            return;
        }
        self.files_with_findings += 1;
        self.total_findings += findings.count;
        for kind in &findings.kinds {
            *self.findings_by_kind.entry(kind.clone()).or_insert(0) += 1;
        }
    }

    pub fn record_attribution(&mut self, attribution: &TenantAttribution) {
        self.tenant_sources.record(attribution.source);
    }

    pub fn record_state(&mut self, state: &str) {
        *self.states.entry(state.to_string()).or_insert(0) += 1;
    }

    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str("session-archive backfill\n");
        out.push_str("========================\n\n");
        out.push_str("account homes:\n");
        for (label, count) in &self.per_home {
            out.push_str(&format!("  {label:<16} {count} transcript(s)\n"));
        }
        if self.subagent_transcripts_skipped > 0 {
            out.push_str(&format!(
                "\n{} subagent side-transcript(s) were EXCLUDED (…/<session-id>/subagents/\
                 agent-*.jsonl). They are not sessions — the stem is an agent id, nothing can \
                 `claude --resume` one — so the archive's row count is the session count above, \
                 not the raw `.jsonl` count on disk.\n",
                self.subagent_transcripts_skipped
            ));
        }
        out.push_str(&format!("\nscanned={}\n", self.scanned));
        out.push_str(&format!(
            "created={} updated={} unchanged_remote={} unchanged_local={} skipped_empty={} errors={}\n",
            self.created,
            self.updated,
            self.unchanged_remote,
            self.unchanged_local,
            self.skipped_empty,
            self.errors
        ));
        out.push_str(&format!(
            "bytes_archived={} ({:.1} MB)\n",
            self.bytes_archived,
            self.bytes_archived as f64 / (1024.0 * 1024.0)
        ));

        out.push_str("\nlifecycle state written:\n");
        if self.states.is_empty() {
            out.push_str("  (none)\n");
        }
        for (state, count) in &self.states {
            out.push_str(&format!("  {state:<10} {count}\n"));
        }
        out.push_str(
            "note: `closeout_state` is deliberately NOT written by this scanner. Plan §3.4 \
             derives it from coord's compliance verdict, the /unattended taxonomy and open \
             gates/PRs — none of which is on disk here — so the rows keep the server's \
             `unknown` default. GET /unfinished reports that bucket separately rather than \
             counting it as clean.\n",
        );

        out.push('\n');
        out.push_str(&self.tenant_sources.render());
        if !self.repo_rule_available {
            out.push_str(
                "\nnote: coord's repo->tenant rule (D2) was NOT evaluated this run, so a \
                 `derived_repo` count of 0 means UNASKED — not `no repo mapped to a tenant`. \
                 coord serves `coord.tenant_repos` only behind an operator Cognito bearer, \
                 which the runner's device JWT cannot present. Pass `--tenant-repo-map <file>` \
                 with an operator-exported repo->tenants projection to enable the arm; the \
                 device-binding half of the rule is still applied locally.\n",
            );
        }

        out.push_str(
            "\nsecret detector (an AUDIT SIGNAL — nothing was masked, no body was \
                      modified, no row was hidden):\n",
        );
        out.push_str(&format!(
            "  files with findings   {} / {}\n  total findings        {}\n",
            self.files_with_findings, self.scanned, self.total_findings
        ));
        if self.findings_by_kind.is_empty() {
            out.push_str("  (no kind matched)\n");
        }
        for (kind, count) in &self.findings_by_kind {
            out.push_str(&format!("  {kind:<32} {count} file(s)\n"));
        }

        if self.session_id_mismatches > 0 {
            out.push_str(&format!(
                "\nnote: {} transcript(s) carry an in-body sessionId that differs from the \
                 filename. The FILENAME won — it is what `claude --resume` and every other \
                 reader in this tree keys on.\n",
                self.session_id_mismatches
            ));
        }
        if self.unparsable_window_lines > 0 {
            out.push_str(&format!(
                "note: {} head/tail line(s) would not parse as JSON. Their transcripts were \
                 archived verbatim regardless — only the derived metadata is thinner.\n",
                self.unparsable_window_lines
            ));
        }
        if !self.error_samples.is_empty() {
            out.push_str("\nfirst failures:\n");
            for e in &self.error_samples {
                out.push_str(&format!("  - {e}\n"));
            }
        }
        out
    }
}

/// Per-home transcript counts, for the report's header and for a dry run that
/// wants the shape of the corpus before committing to a push.
pub fn count_per_home(homes: &[AccountHome]) -> Vec<(String, usize)> {
    homes
        .iter()
        .map(|h| (h.label.clone(), super::discovery::transcripts_in(h).len()))
        .collect()
}

/// The `tenant_source` wire value for a resolved attribution.
pub fn tenant_source_value(source: TenantSource) -> String {
    source.as_str().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_utf8_travels_as_body_and_invalid_bytes_travel_as_base64() {
        let mut p = SessionArtifactUpsert::default();
        attach_body(&mut p, b"{\"type\":\"user\"}\n");
        assert!(p.body.is_some());
        assert!(p.body_base64.is_none());
        assert_eq!(p.body_source.as_deref(), Some(BODY_SOURCE_DISK_VERBATIM));

        let mut p = SessionArtifactUpsert::default();
        attach_body(&mut p, &[0x7b, 0xff, 0x7d]);
        assert!(p.body.is_none(), "invalid UTF-8 must not go as text");
        assert!(p.body_base64.is_some());
    }

    #[test]
    fn the_digest_covers_the_bytes_not_the_encoding() {
        // The base64 door and the text door must produce the SAME digest for
        // the same bytes — that is the whole verifiability claim.
        let utf8 = b"hello\xe2\x82\xac"; // valid UTF-8 (a euro sign)
        let mut text_payload = SessionArtifactUpsert::default();
        attach_body(&mut text_payload, utf8);
        assert_eq!(
            text_payload.content_sha256.as_deref(),
            Some(digest_bytes(utf8).as_str())
        );
        assert_eq!(digest_bytes(utf8).len(), 64);

        // The same bytes fed through the base64 door hash identically — the
        // door is a transport choice, never a content one.
        let invalid = [0x68u8, 0x69, 0xff];
        let mut b64_payload = SessionArtifactUpsert::default();
        attach_body(&mut b64_payload, &invalid);
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(b64_payload.body_base64.as_deref().unwrap())
            .unwrap();
        assert_eq!(decoded, invalid, "base64 must round-trip the exact bytes");
        assert_eq!(
            b64_payload.content_sha256.as_deref(),
            Some(digest_bytes(&invalid).as_str())
        );
    }

    #[test]
    fn an_omitted_field_is_absent_from_the_json_rather_than_null() {
        // The web route writes only the fields the request CARRIED. A null
        // would blank the web archiver's metadata on the shared row.
        let p = SessionArtifactUpsert {
            claude_session_id: "abc".into(),
            tenant_source: "unknown".into(),
            ..Default::default()
        };
        let json = serde_json::to_string(&p).unwrap();
        assert!(!json.contains("\"repo\""), "absent field leaked: {json}");
        assert!(!json.contains("null"), "a null reached the wire: {json}");
        // The three that are ALWAYS sent.
        assert!(json.contains("\"claude_session_id\":\"abc\""));
        assert!(json.contains("\"tenant_source\":\"unknown\""));
        assert!(json.contains("\"secret_finding_kinds\":[]"));
    }

    #[test]
    fn an_empty_kind_list_is_sent_as_an_empty_array_not_omitted() {
        // `[]` = the detector ran and found nothing; NULL = never scanned.
        // Omitting the field would report every clean transcript as unscanned.
        let p = SessionArtifactUpsert::default();
        let json = serde_json::to_string(&p).unwrap();
        assert!(json.contains("\"secret_finding_count\":0"));
        assert!(json.contains("\"secret_finding_kinds\":[]"));
    }

    #[test]
    fn the_scan_state_round_trips_and_only_writes_when_dirty() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("state.json");
        let mut state = ScanState::load(&path);
        assert!(state.is_empty());
        state.record("gmail", "s1", "deadbeef");
        state.save(&path).unwrap();

        let reloaded = ScanState::load(&path);
        assert!(reloaded.is_known("gmail", "s1", "deadbeef"));
        assert!(!reloaded.is_known("gmail", "s1", "other"));
        // The same id under a DIFFERENT account home is a different session —
        // that is what the identity key says.
        assert!(!reloaded.is_known("hotmail", "s1", "deadbeef"));
        assert_eq!(reloaded.len(), 1);
    }

    #[test]
    fn a_corrupt_state_file_degrades_to_an_empty_cache() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("state.json");
        std::fs::write(&path, b"{not json").unwrap();
        assert!(ScanState::load(&path).is_empty());
    }

    #[test]
    fn an_unasked_repo_rule_is_said_out_loud_rather_than_read_as_a_zero() {
        let unasked = BackfillReport {
            scanned: 1,
            repo_rule_available: false,
            ..Default::default()
        };
        assert!(unasked.render().contains("UNASKED"));

        let asked = BackfillReport {
            scanned: 1,
            repo_rule_available: true,
            ..Default::default()
        };
        assert!(!asked.render().contains("UNASKED"));
    }

    #[test]
    fn the_report_names_every_tenant_bucket_and_the_closeout_abstention() {
        let mut report = BackfillReport {
            scanned: 3,
            ..Default::default()
        };
        report.record_state("closed");
        report.record_attribution(&TenantAttribution {
            tenant_id: None,
            source: TenantSource::Unknown,
        });
        let rendered = report.render();
        for s in TenantSource::ALL {
            assert!(rendered.contains(s.as_str()), "missing {}", s.as_str());
        }
        assert!(rendered.contains("closeout_state"));
        assert!(rendered.contains("nothing was masked"));
    }

    #[test]
    fn error_samples_are_capped_but_the_count_is_not() {
        let mut report = BackfillReport::default();
        for i in 0..25 {
            report.record_error(format!("failure {i}"));
        }
        assert_eq!(report.errors, 25);
        assert_eq!(report.error_samples.len(), ERROR_SAMPLE_LIMIT);
        assert!(report.render().contains("failure 0"));
    }
}
