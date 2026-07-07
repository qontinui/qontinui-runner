//! PR-open detection in session transcripts (PR Shepherd Phase 1 of plan
//! `2026-07-04-runner-pr-shepherd.md`).
//!
//! ## Trigger
//!
//! A `gh pr create` Bash `tool_use` block in an interactive session's JSONL
//! transcript is the runner-observed signal that the session opened a PR —
//! the event that should seed a `pr_watch_state` row so the PR's fate is
//! owned after the session ends. We piggyback on the same
//! [`super::transcript_watcher`] tail loop that [`super::commit_report`]
//! uses for `git push` detection, with the same tolerant-parser style.
//!
//! ## Correlation
//!
//! Unlike a push (self-contained on one line), a PR-open resolves across TWO
//! transcript lines: the assistant record carrying the `tool_use` block (with
//! its block `id` and the command's `working_dir`), and a later user record
//! carrying the matching `tool_result` (matched by `tool_use_id`) whose text
//! contains the created PR's URL (`https://github.com/<owner>/<repo>/pull/<n>`
//! — `gh pr create` prints it on success). [`PrCreateTracker`] holds the
//! pending observations per tail session and emits:
//!
//! - [`PrOpenEvent::Resolved`] when the tool_result carried the URL, or
//! - [`PrOpenEvent::NeedsBranchLookup`] when it didn't (command failed,
//!   `--web` flow, truncated output) or never arrived within [`PENDING_TTL`]
//!   — the PR watcher then resolves the branch to a PR number via
//!   `GET /pulls?head=<owner>:<branch>` on its next poll.
//!
//! ## Wire path
//!
//! Events don't write the database from the tail loop. They enqueue a
//! [`PendingPrSeed`] on the process-global queue in
//! [`crate::trigger_system::pr_shepherd`]; the PR watcher drains it on its
//! next poll, enforces the per-repo allowlist, and performs the upsert. Git
//! resolution for branch-lookup events (remote → `owner/repo`, HEAD branch)
//! runs on the blocking pool via [`handle_pr_open_event`], exactly like
//! `commit_report::handle_push_observation`.

use std::time::{Duration, SystemTime};

use serde_json::Value;
use tracing::{debug, info};
use uuid::Uuid;

use crate::trigger_system::pr_shepherd::{self, PendingPrSeed, SeedIdentity, SeedTarget};

/// How long a `gh pr create` observation waits for its tool_result before
/// falling back to branch-lookup resolution. Generous — the CLI returns in
/// seconds, but the result line only lands when the assistant turn flushes.
const PENDING_TTL: Duration = Duration::from_secs(10 * 60);

/// Cap on simultaneously-pending observations per tail session. A session
/// realistically opens PRs one at a time; on overflow the oldest observation
/// falls back to branch lookup rather than being lost.
const MAX_PENDING: usize = 8;

/// A detected `gh pr create` invocation, pending its tool_result. Pure parse
/// output — no git has been run yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrCreateObservation {
    /// The tool_use block `id`, used to match the later tool_result.
    pub tool_use_id: String,
    /// Working directory to run git in (same resolution order as
    /// [`super::commit_report::resolve_working_dir`]).
    pub working_dir: String,
}

/// A PR-open event ready to act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrOpenEvent {
    /// The tool_result carried the PR URL — repo + number known.
    Resolved {
        working_dir: String,
        repo_full_name: String,
        pr_number: u64,
    },
    /// No URL available — resolve `working_dir`'s branch to a PR number via
    /// the GitHub API on the PR watcher's next poll.
    NeedsBranchLookup { working_dir: String },
}

// ── Pure parsing ─────────────────────────────────────────────────────────────

/// Return true if a shell command string contains a `gh pr create`
/// invocation.
///
/// Tolerant of the common shapes the agent emits: `gh pr create --title …`,
/// `cd "<dir>" && gh pr create …`, env-prefixed (`FOO=bar gh pr create`),
/// piped/redirected. Conservative in the same way
/// [`super::commit_report::command_is_git_push`] is: `gh` must be the first
/// token of a shell segment, followed by `pr` then `create` as the next
/// non-flag tokens — so `gh pr list`, `gh pr view`, and `echo gh pr create`
/// don't match.
pub fn command_is_gh_pr_create(command: &str) -> bool {
    for segment in command.split(['&', ';', '|', '\n']) {
        let toks: Vec<&str> = segment.split_whitespace().collect();
        // Skip leading env-assignments (`FOO=bar gh pr create`).
        let mut start = 0;
        while start < toks.len() && toks[start].contains('=') {
            start += 1;
        }
        if toks.get(start) != Some(&"gh") {
            continue;
        }
        // Expect `pr` then `create` as the next non-flag tokens.
        let mut want = "pr";
        let mut i = start + 1;
        while i < toks.len() {
            let t = toks[i];
            if t.starts_with('-') {
                i += 1;
                continue;
            }
            if t == want {
                if want == "create" {
                    return true;
                }
                want = "create";
                i += 1;
                continue;
            }
            break;
        }
    }
    false
}

/// Extract the first GitHub PR URL (`https://github.com/<owner>/<repo>/pull/<n>`)
/// from free text, returning `(owner/repo, pr_number)`. Skips non-`pull`
/// github.com URLs (repo links, issues, commit links).
pub fn extract_pr_url(text: &str) -> Option<(String, u64)> {
    const PREFIX: &str = "https://github.com/";
    for (idx, _) in text.match_indices(PREFIX) {
        let rest = &text[idx + PREFIX.len()..];
        // Take the URL path up to the first whitespace.
        let path = rest.split_whitespace().next().unwrap_or("");
        let mut parts = path.split('/');
        let (Some(owner), Some(repo), Some(kind), Some(num_tok)) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        if kind != "pull" || owner.is_empty() || repo.is_empty() {
            continue;
        }
        // Trailing punctuation ("…/pull/42.") is common in prose — take the
        // leading digit run only.
        let digits: String = num_tok.chars().take_while(|c| c.is_ascii_digit()).collect();
        if let Ok(n) = digits.parse::<u64>() {
            return Some((format!("{owner}/{repo}"), n));
        }
    }
    None
}

/// Walk a parsed `{type:"assistant"}` JSONL record and emit a
/// [`PrCreateObservation`] for each Bash `tool_use` block whose command is a
/// `gh pr create`. Pure (no I/O). Mirrors
/// [`super::commit_report::extract_push_observations`], additionally
/// capturing the block `id` for tool_result correlation.
pub fn extract_pr_create_observations(record: &Value) -> Vec<PrCreateObservation> {
    let transcript_cwd = record.get("cwd").and_then(|c| c.as_str()).unwrap_or("");

    let Some(content) = record
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_array())
    else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for block in content {
        if block.get("type").and_then(|t| t.as_str()) != Some("tool_use") {
            continue;
        }
        if block.get("name").and_then(|n| n.as_str()) != Some("Bash") {
            continue;
        }
        let Some(id) = block.get("id").and_then(|i| i.as_str()) else {
            continue;
        };
        let Some(command) = block
            .get("input")
            .and_then(|i| i.get("command"))
            .and_then(|c| c.as_str())
        else {
            continue;
        };
        if command_is_gh_pr_create(command) {
            out.push(PrCreateObservation {
                tool_use_id: id.to_string(),
                working_dir: super::commit_report::resolve_working_dir(command, transcript_cwd),
            });
        }
    }
    out
}

/// Walk a parsed `{type:"user"}` JSONL record and collect its `tool_result`
/// blocks as `(tool_use_id, text)`. The result `content` is either a plain
/// string or an array of `{type:"text", text}` blocks — both are flattened.
fn tool_result_texts(record: &Value) -> Vec<(String, String)> {
    let Some(content) = record
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_array())
    else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for block in content {
        if block.get("type").and_then(|t| t.as_str()) != Some("tool_result") {
            continue;
        }
        let Some(id) = block.get("tool_use_id").and_then(|i| i.as_str()) else {
            continue;
        };
        let text = match block.get("content") {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Array(blocks)) => blocks
                .iter()
                .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join("\n"),
            _ => String::new(),
        };
        out.push((id.to_string(), text));
    }
    out
}

// ── Stateful tool_use ↔ tool_result correlation ──────────────────────────────

/// Per-tail-session tracker: holds pending `gh pr create` observations until
/// their tool_result arrives (or [`PENDING_TTL`] expires) and emits
/// [`PrOpenEvent`]s. One instance per `tail_session` invocation — state never
/// crosses sessions.
pub struct PrCreateTracker {
    pending: Vec<(PrCreateObservation, SystemTime)>,
}

impl PrCreateTracker {
    pub fn new() -> Self {
        Self {
            pending: Vec::new(),
        }
    }

    /// Feed one raw JSONL transcript line; returns any PR-open events it
    /// completed (URL matched, no-URL result, or TTL-expired fallback).
    /// Malformed / irrelevant lines return an empty vec — never panics.
    pub fn observe_line(&mut self, line: &str) -> Vec<PrOpenEvent> {
        self.observe_line_at(line, SystemTime::now())
    }

    /// [`Self::observe_line`] with an injectable clock, for TTL tests.
    fn observe_line_at(&mut self, line: &str, now: SystemTime) -> Vec<PrOpenEvent> {
        let mut out = self.flush_expired(now);

        let trimmed = line.trim();
        if trimmed.is_empty() {
            return out;
        }
        let Ok(record) = serde_json::from_str::<Value>(trimmed) else {
            return out;
        };

        match record.get("type").and_then(|t| t.as_str()) {
            Some("assistant") => {
                for obs in extract_pr_create_observations(&record) {
                    if self.pending.len() >= MAX_PENDING {
                        // Overflow: the oldest pending falls back rather than
                        // being silently lost.
                        let (old, _) = self.pending.remove(0);
                        out.push(PrOpenEvent::NeedsBranchLookup {
                            working_dir: old.working_dir,
                        });
                    }
                    self.pending.push((obs, now));
                }
            }
            Some("user") => {
                for (tool_use_id, text) in tool_result_texts(&record) {
                    let Some(pos) = self
                        .pending
                        .iter()
                        .position(|(o, _)| o.tool_use_id == tool_use_id)
                    else {
                        continue;
                    };
                    let (obs, _) = self.pending.remove(pos);
                    match extract_pr_url(&text) {
                        Some((repo_full_name, pr_number)) => out.push(PrOpenEvent::Resolved {
                            working_dir: obs.working_dir,
                            repo_full_name,
                            pr_number,
                        }),
                        // Result arrived without a URL (failure, `--web`
                        // flow, truncation) — branch lookup finds the PR on
                        // the next poll if one actually exists.
                        None => out.push(PrOpenEvent::NeedsBranchLookup {
                            working_dir: obs.working_dir,
                        }),
                    }
                }
            }
            _ => {}
        }

        out
    }

    /// Convert observations older than [`PENDING_TTL`] into branch-lookup
    /// fallbacks.
    fn flush_expired(&mut self, now: SystemTime) -> Vec<PrOpenEvent> {
        let mut out = Vec::new();
        self.pending.retain(|(obs, seen_at)| {
            let expired = now
                .duration_since(*seen_at)
                .map(|age| age > PENDING_TTL)
                .unwrap_or(false);
            if expired {
                out.push(PrOpenEvent::NeedsBranchLookup {
                    working_dir: obs.working_dir.clone(),
                });
            }
            !expired
        });
        out
    }
}

// ── Identity + seed handoff (impure) ─────────────────────────────────────────

/// Resolve the seed identity for a transcript session. When the registrar's
/// R4 index maps the transcript id (as a `task_run_id`) to a coord session,
/// the session IS a runner task run — key the seed on `task_run_id`.
/// Otherwise it is a pure interactive session: key on `authoring_session_id`,
/// using the transcript session UUID itself — the same identity the
/// `AgentLogEmitter` uses as the coord agent id — as the durable handle.
pub fn seed_identity(session_id: &str, coord_session_id: Option<Uuid>) -> SeedIdentity {
    match coord_session_id {
        Some(coord_id) => SeedIdentity::TaskRun {
            task_run_id: session_id.to_string(),
            authoring_session_id: Some(coord_id.to_string()),
        },
        None => SeedIdentity::Session {
            authoring_session_id: session_id.to_string(),
        },
    }
}

/// Full pipeline for one PR-open event: resolve git facts where needed and
/// enqueue a [`PendingPrSeed`] for the PR watcher's next poll. Best-effort —
/// logs and returns on any miss. Synchronous git calls (branch-lookup path)
/// are cheap and expected to run on the blocking pool, mirroring
/// `commit_report::handle_push_observation`.
pub fn handle_pr_open_event(event: PrOpenEvent, identity: SeedIdentity) {
    if !pr_shepherd::autoseed_enabled() {
        return;
    }
    let target = match &event {
        PrOpenEvent::Resolved {
            repo_full_name,
            pr_number,
            ..
        } => SeedTarget::Pr {
            repo_full_name: repo_full_name.clone(),
            pr_number: *pr_number,
        },
        PrOpenEvent::NeedsBranchLookup { working_dir } => {
            // Reuse the push-report resolver: origin remote → `owner/repo`,
            // HEAD → branch. Detached HEAD / non-repo dirs skip cleanly.
            let Some(resolved) = super::commit_report::resolve_push(working_dir) else {
                debug!(
                    "pr_open_report: could not resolve repo/branch in {} — skipping seed",
                    working_dir
                );
                return;
            };
            SeedTarget::Branch {
                repo_full_name: resolved.repo,
                branch: resolved.branch,
            }
        }
    };
    info!(
        "pr_open_report: enqueueing PR-watch seed ({:?} / {:?})",
        target, identity
    );
    pr_shepherd::enqueue_seed(PendingPrSeed { identity, target });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn detects_gh_pr_create_shapes() {
        assert!(command_is_gh_pr_create("gh pr create"));
        assert!(command_is_gh_pr_create(
            "gh pr create --title \"feat: x\" --body \"y\""
        ));
        assert!(command_is_gh_pr_create(
            "cd \"C:/repo\" && gh pr create --fill 2>&1 | tail -5"
        ));
        assert!(command_is_gh_pr_create("GH_TOKEN=x gh pr create --fill"));
        assert!(command_is_gh_pr_create(
            "git push -u origin feat/x && gh pr create --fill"
        ));
    }

    #[test]
    fn ignores_non_create_gh_and_non_gh() {
        assert!(!command_is_gh_pr_create("gh pr list --head feat/x"));
        assert!(!command_is_gh_pr_create("gh pr view 42"));
        assert!(!command_is_gh_pr_create("gh pr merge 42"));
        assert!(!command_is_gh_pr_create("gh issue create"));
        assert!(!command_is_gh_pr_create("echo gh pr create"));
        assert!(!command_is_gh_pr_create("git push"));
    }

    #[test]
    fn extracts_pr_url_from_text() {
        assert_eq!(
            extract_pr_url("https://github.com/qontinui/qontinui-runner/pull/460"),
            Some(("qontinui/qontinui-runner".to_string(), 460))
        );
        assert_eq!(
            extract_pr_url(
                "Creating pull request…\nhttps://github.com/o/r/pull/7\nDone."
            ),
            Some(("o/r".to_string(), 7))
        );
        // Trailing punctuation after the number.
        assert_eq!(
            extract_pr_url("see https://github.com/o/r/pull/12."),
            Some(("o/r".to_string(), 12))
        );
        // Non-pull URLs first, pull URL later — must find the pull one.
        assert_eq!(
            extract_pr_url(
                "repo https://github.com/o/r and https://github.com/o/r/pull/3"
            ),
            Some(("o/r".to_string(), 3))
        );
        assert_eq!(extract_pr_url("https://github.com/o/r/issues/9"), None);
        assert_eq!(extract_pr_url("no url here"), None);
    }

    fn assistant_line(tool_use_id: &str, command: &str, cwd: &str) -> String {
        json!({
            "type": "assistant",
            "cwd": cwd,
            "message": { "content": [
                {"type": "text", "text": "opening a PR"},
                {"type": "tool_use", "id": tool_use_id, "name": "Bash",
                 "input": {"command": command}}
            ]}
        })
        .to_string()
    }

    fn tool_result_line(tool_use_id: &str, content: Value) -> String {
        json!({
            "type": "user",
            "message": { "content": [
                {"type": "tool_result", "tool_use_id": tool_use_id, "content": content}
            ]}
        })
        .to_string()
    }

    #[test]
    fn tracker_resolves_url_from_string_tool_result() {
        let mut tracker = PrCreateTracker::new();
        let events = tracker.observe_line(&assistant_line(
            "toolu_1",
            "gh pr create --fill",
            "C:/repo",
        ));
        assert!(events.is_empty(), "tool_use alone emits nothing");

        let events = tracker.observe_line(&tool_result_line(
            "toolu_1",
            json!("https://github.com/qontinui/qontinui-runner/pull/99\n"),
        ));
        assert_eq!(
            events,
            vec![PrOpenEvent::Resolved {
                working_dir: "C:/repo".to_string(),
                repo_full_name: "qontinui/qontinui-runner".to_string(),
                pr_number: 99,
            }]
        );
        // Consumed — a repeat result for the same id is ignored.
        assert!(tracker
            .observe_line(&tool_result_line("toolu_1", json!("again")))
            .is_empty());
    }

    #[test]
    fn tracker_resolves_url_from_array_tool_result() {
        let mut tracker = PrCreateTracker::new();
        tracker.observe_line(&assistant_line("toolu_2", "gh pr create", "/repo"));
        let events = tracker.observe_line(&tool_result_line(
            "toolu_2",
            json!([{"type": "text", "text": "https://github.com/o/r/pull/5"}]),
        ));
        assert_eq!(
            events,
            vec![PrOpenEvent::Resolved {
                working_dir: "/repo".to_string(),
                repo_full_name: "o/r".to_string(),
                pr_number: 5,
            }]
        );
    }

    #[test]
    fn tracker_honors_cd_prefix_for_working_dir() {
        let mut tracker = PrCreateTracker::new();
        tracker.observe_line(&assistant_line(
            "toolu_3",
            "cd \"C:/right/repo\" && gh pr create --fill",
            "C:/wrong",
        ));
        let events = tracker.observe_line(&tool_result_line(
            "toolu_3",
            json!("https://github.com/o/r/pull/1"),
        ));
        assert_eq!(
            events,
            vec![PrOpenEvent::Resolved {
                working_dir: "C:/right/repo".to_string(),
                repo_full_name: "o/r".to_string(),
                pr_number: 1,
            }]
        );
    }

    #[test]
    fn tracker_falls_back_when_result_has_no_url() {
        let mut tracker = PrCreateTracker::new();
        tracker.observe_line(&assistant_line("toolu_4", "gh pr create", "/repo"));
        let events = tracker.observe_line(&tool_result_line(
            "toolu_4",
            json!("pull request create failed: GraphQL: something"),
        ));
        assert_eq!(
            events,
            vec![PrOpenEvent::NeedsBranchLookup {
                working_dir: "/repo".to_string(),
            }]
        );
    }

    #[test]
    fn tracker_expires_pending_to_branch_lookup() {
        let mut tracker = PrCreateTracker::new();
        let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let events =
            tracker.observe_line_at(&assistant_line("toolu_5", "gh pr create", "/repo"), t0);
        assert!(events.is_empty());

        // Just under the TTL — still pending.
        let events = tracker.observe_line_at("{}", t0 + PENDING_TTL);
        assert!(events.is_empty());

        // Past the TTL — expires to a branch-lookup fallback.
        let events =
            tracker.observe_line_at("{}", t0 + PENDING_TTL + Duration::from_secs(1));
        assert_eq!(
            events,
            vec![PrOpenEvent::NeedsBranchLookup {
                working_dir: "/repo".to_string(),
            }]
        );
        // And it's gone — no repeat emission.
        assert!(tracker
            .observe_line_at("{}", t0 + PENDING_TTL + Duration::from_secs(2))
            .is_empty());
    }

    #[test]
    fn tracker_ignores_unrelated_results_and_malformed_lines() {
        let mut tracker = PrCreateTracker::new();
        tracker.observe_line(&assistant_line("toolu_6", "gh pr create", "/repo"));
        // A tool_result for some OTHER tool_use is ignored.
        assert!(tracker
            .observe_line(&tool_result_line("toolu_other", json!("output")))
            .is_empty());
        // Malformed / non-JSON lines never panic and emit nothing.
        assert!(tracker.observe_line("not json").is_empty());
        assert!(tracker.observe_line("").is_empty());
        // Non-Bash tool_use and non-create Bash are not tracked.
        let line = json!({
            "type": "assistant",
            "message": { "content": [
                {"type": "tool_use", "id": "t7", "name": "Read",
                 "input": {"file_path": "gh pr create"}},
                {"type": "tool_use", "id": "t8", "name": "Bash",
                 "input": {"command": "gh pr list"}}
            ]}
        })
        .to_string();
        assert!(tracker.observe_line(&line).is_empty());
        assert!(tracker
            .observe_line(&tool_result_line("t8", json!("output")))
            .is_empty());
    }

    #[test]
    fn seed_identity_prefers_task_run_mapping() {
        let coord_id = Uuid::new_v4();
        assert_eq!(
            seed_identity("tr-123", Some(coord_id)),
            SeedIdentity::TaskRun {
                task_run_id: "tr-123".to_string(),
                authoring_session_id: Some(coord_id.to_string()),
            }
        );
        assert_eq!(
            seed_identity("sess-abc", None),
            SeedIdentity::Session {
                authoring_session_id: "sess-abc".to_string(),
            }
        );
    }
}
