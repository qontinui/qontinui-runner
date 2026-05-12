//! UI Bridge invoke proxy — Phase 3I.1 + 3I.2.
//!
//! Allows external HTTP callers to invoke a curated allowlist of Tauri
//! commands over the UI Bridge HTTP surface, without having to go through
//! `page/evaluate + __TAURI_INTERNALS__` gymnastics.
//!
//! # Flow
//!
//! 1. HTTP handler generates a fresh `request_id` (uuid v4).
//! 2. Creates a `tokio::sync::oneshot` channel; stashes the sender in the
//!    [`InvokeRequestStore`] keyed by `request_id`.
//! 3. Emits Tauri event `ui-bridge:invoke-request` with
//!    `{ request_id, command, args }` to the React frontend.
//! 4. The React side calls `invoke(command, args)` and emits
//!    `ui-bridge:invoke-response` with `{ request_id, ok, result, error }`.
//! 5. A global Tauri listener installed at `mcp_api.rs` startup parses the
//!    response and calls [`InvokeRequestStore::deliver`] which fires the
//!    matching oneshot.
//! 6. The HTTP handler awaits the receiver with a configurable timeout,
//!    returning the result (or 504 on timeout, 500 on frontend error).
//!
//! # Allowlist
//!
//! Only commands in [`UI_BRIDGE_COMMANDS`] may be invoked. The HTTP handler
//! returns 400 for anything else. This avoids exposing arbitrary Tauri
//! commands to the HTTP surface — some commands accept filesystem paths,
//! credentials, or PTY-level process control and must not be reachable
//! from external clients without explicit curation.

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;
use tokio::sync::Mutex;

/// Strongly-typed response from the React frontend for an invoke request.
///
/// `ok=true` means the Tauri command completed (`invoke(...)` resolved);
/// `result` holds its return value (possibly `Null` for `()` returns).
/// `ok=false` means `invoke(...)` threw — `error` carries the string the
/// command returned (e.g. `Err(String)` from the Rust side).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvokeResponse {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Keyed store of pending oneshot senders — one per in-flight invoke.
///
/// Multiple invokes can be in-flight at once, unlike `TokenFlowStore`
/// which is single-slot. The HashMap is keyed by `request_id` (a uuid v4
/// string) which we generate fresh for every call.
pub struct InvokeRequestStore {
    pending: Arc<Mutex<HashMap<String, oneshot::Sender<InvokeResponse>>>>,
}

impl Default for InvokeRequestStore {
    fn default() -> Self {
        Self::new()
    }
}

impl InvokeRequestStore {
    pub fn new() -> Self {
        Self {
            pending: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Register a new pending invoke.
    ///
    /// Takes ownership of the oneshot sender and stashes it under
    /// `request_id`. If an entry for that id already exists (shouldn't
    /// happen with uuid v4, but defensive), it is silently replaced —
    /// the prior waiter will observe a dropped-sender error via
    /// `Receiver::await`.
    pub async fn register(&self, request_id: String, sender: oneshot::Sender<InvokeResponse>) {
        let mut guard = self.pending.lock().await;
        guard.insert(request_id, sender);
    }

    /// Deliver a response to a pending invoke by id.
    ///
    /// Removes the entry from the map and sends the response through the
    /// oneshot. If the receiver has already been dropped (e.g. the HTTP
    /// handler timed out and bailed), the response is silently discarded —
    /// this mirrors the "best effort" semantics of oneshot channels.
    /// Returns `true` if a pending entry existed for this id.
    pub async fn deliver(&self, request_id: &str, response: InvokeResponse) -> bool {
        let mut guard = self.pending.lock().await;
        if let Some(sender) = guard.remove(request_id) {
            // Ignore send errors — if the receiver is gone we can't do
            // anything about it, and the caller already logged the timeout.
            let _ = sender.send(response);
            true
        } else {
            false
        }
    }

    /// Cancel a pending invoke — removes the entry without delivering.
    ///
    /// Called by the HTTP handler on timeout so a subsequent late
    /// response doesn't linger in the map. The oneshot sender is dropped,
    /// which closes the channel — any residual receivers would observe a
    /// `RecvError` (but by the time cancel runs, the HTTP handler's
    /// receiver is already discarded).
    pub async fn cancel(&self, request_id: &str) {
        let mut guard = self.pending.lock().await;
        guard.remove(request_id);
    }
}

/// Metadata for one command in the UI Bridge invoke allowlist.
///
/// `args_schema` and `response_schema` are JSON string literals describing
/// the wire shape callers should expect — they're surfaced verbatim via
/// `GET /ui-bridge/commands` so agents can discover the contract without
/// reading Rust source.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct ProxyableCommand {
    pub name: &'static str,
    pub description: &'static str,
    pub args_schema: &'static str,
    pub response_schema: &'static str,
    /// Whether the startup invoke-proxy probe may call this command with
    /// empty args. Defaults to `true` via [`ProxyableCommand::default_probe`].
    ///
    /// Set to `false` for commands that
    /// 1. accept empty args (no required keys in their schema), AND
    /// 2. have observable side effects when invoked with empty args.
    ///
    /// Example: [`dismiss_recent_crash`] clears `/health.recent_crash`. The
    /// probe is otherwise unable to distinguish "empty args accepted"
    /// (healthy) from "command ran for real and did something" — and for
    /// destructive side effects the "for real" branch is unacceptable. The
    /// probe covers missing-required-key detection for commands with
    /// required args; skipping it for no-arg commands gives up schema-drift
    /// coverage we wouldn't have anyway (there's no required key to be
    /// missing).
    ///
    /// [`dismiss_recent_crash`]: crate::crash_dumps::dismiss_recent_crash
    #[serde(default = "default_probe_safe")]
    pub probe_with_empty_args: bool,
}

/// Default for [`ProxyableCommand::probe_with_empty_args`] — commands
/// opt in to probing by default.
const fn default_probe_safe() -> bool {
    true
}

/// The static allowlist of Tauri commands reachable via
/// `POST /ui-bridge/invoke/{command_name}`.
///
/// Schemas reflect the HTTP caller's wire contract (camelCase top-level
/// arg names), which Tauri's IPC converts to the Rust command's
/// snake_case parameter names. See
/// `src-tauri/src/commands/web_integration.rs` for the authoritative Rust
/// signatures. Adding a command here makes it callable over HTTP; do not
/// add commands that accept arbitrary filesystem paths or PTY handles
/// without a dedicated threat review.
pub const UI_BRIDGE_COMMANDS: &[ProxyableCommand] = &[
    ProxyableCommand {
        name: "get_web_integration_status",
        description: "Return the persisted web-integration settings plus live registration state (runner id, last heartbeat, last registration error).",
        args_schema: "{}",
        response_schema: "{ \"enabled\": boolean, \"backendUrl\": string, \"runnerTokenMasked\": string, \"runnerId\": string | null, \"lastHeartbeatAt\": string | null, \"registrationError\": string | null }",
        probe_with_empty_args: true,
    },
    ProxyableCommand {
        name: "save_web_integration_settings",
        description: "Persist web-integration settings (enable flag, backend URL, runner token, optional web base URL) and trigger re-registration with the configured backend. `webBaseUrl` is optional and only needed when the Next.js web UI runs on a different host than the API backend.",
        args_schema: "{ \"enabled\": boolean, \"backendUrl\": string, \"runnerToken\": string, \"webBaseUrl\"?: string | null }",
        response_schema: "null",
        probe_with_empty_args: true,
    },
    ProxyableCommand {
        name: "test_web_integration_connection",
        description: "Probe the given backend URL + runner token by making a throwaway runner-registration call and immediately deleting the created entry. Returns the transient runner id (for debugging only; do not reuse it).",
        args_schema: "{ \"backendUrl\": string, \"runnerToken\": string }",
        response_schema: "{ \"runner_id\": string }",
        probe_with_empty_args: true,
    },
    ProxyableCommand {
        name: "start_web_token_flow",
        description: "Open the user's browser at `{backend_url}/connect-runner?state=...&callback=...&runner_name=...`, stashing state for the eventual callback that applies the issued runner token. `backendUrl` is optional — when omitted, uses the persisted backend URL.",
        args_schema: "{ \"backendUrl\"?: string | null }",
        response_schema: "null",
        // `backendUrl` is optional, so an empty-args probe would actually
        // open the user's browser. Skip.
        probe_with_empty_args: false,
    },
    ProxyableCommand {
        name: "emit_extraction_script",
        description: "Synthesise a one-line JS extraction expression for the scripted-output indirection (already registered in Tauri — this entry only allowlists it over HTTP). Maps to a 500 with `{ kind, message }` error body on failure; `kind` is one of `cost_cap` (per-task_run call cap exceeded), `token_budget` (input/output token budget exhausted), `timeout` (LLM exceeded 5s), `breaker_open` (shared Claude circuit breaker is Open), `disabled` (global kill switch off), `llm_error`, or `invalid_response`.",
        args_schema: "{ \"goal\": string, \"schemaHint\": object, \"outputPreview\": string, \"taskRunId\"?: string | null }",
        response_schema: "{ \"expression\": string, \"modelId\": string, \"tokensIn\": number, \"tokensOut\": number, \"source\": \"cache\" | \"llm\", \"cacheTier\": number | null, \"provider\": string, \"cacheCreationTokens\": number, \"cacheReadTokens\": number }",
        probe_with_empty_args: true,
    },
    ProxyableCommand {
        name: "emit_scripted_output_event",
        description: "Record a TS-originated scripted-output activity-timeline event through the Rust emitter's FK-aware insert path (already registered in Tauri — this entry only allowlists it over HTTP). `name` must be one of `scripted_output.attempted`, `scripted_output.worker_ok`, `scripted_output.bytes_avoided`, or `scripted_output.fallback`; any other value is rejected. `metadata` defaults to `{}` if omitted; `taskRunId` is optional and falls back to NULL on a miss in `task_runs` (raw value preserved in `metadata.task_run_id_raw`).",
        args_schema: "{ \"name\": string, \"metadata\"?: object, \"taskRunId\"?: string | null }",
        response_schema: "null",
        probe_with_empty_args: true,
    },
    ProxyableCommand {
        name: "get_scripted_output_stats",
        description: "Aggregate `source_type = 'scripted_output'` activity-timeline events into a single stat block (already registered in Tauri — this entry only allowlists it over HTTP). `taskRunId` is optional: when omitted or `null`, returns global stats across all runs (including the unassigned bucket); otherwise scopes to that run.",
        args_schema: "{ \"taskRunId\"?: string | null }",
        response_schema: "{ \"attempted\": number, \"cacheHit\": number, \"llmOk\": number, \"workerOk\": number, \"bytesAvoided\": number, \"fallbacks\": { [reason: string]: number }, \"totalTokensIn\": number, \"totalTokensOut\": number, \"cacheCreationTokens\": number, \"cacheReadTokens\": number }",
        probe_with_empty_args: true,
    },
    ProxyableCommand {
        name: "get_oneshot_stats",
        description: "Process-local counters for `OneshotLlm` adapter calls (Phase 0 of productivity-stack-product-readiness). Resets on runner restart. Backs the sibling tile on `ScriptedOutputPanel`.",
        args_schema: "{}",
        response_schema: "{ \"callsTotal\": { [provider: string]: number }, \"errorsTotal\": { [kind: string]: number }, \"cacheHitsTotal\": { [provider: string]: number }, \"cacheReadTokens\": number, \"cacheCreationTokens\": number, \"totalTokensIn\": number, \"totalTokensOut\": number }",
        probe_with_empty_args: true,
    },
    ProxyableCommand {
        name: "get_scripted_output_settings",
        description: "Read the persisted `ScriptedOutputSettings` (provider mode, model override, Gemma local endpoint, Gemma model alias, kill switch). Used by the provider-selection panel on the LLM Analytics tab to render form state.",
        args_schema: "{}",
        response_schema: "{ \"enabled\": boolean, \"model\": string | null, \"provider\": \"auto\" | \"claude_api_warm\" | \"claude_api\" | \"gemma_local_warm\", \"gemma_local_endpoint\": string, \"gemma_local_model_alias\": string }",
        probe_with_empty_args: true,
    },
    ProxyableCommand {
        name: "save_scripted_output_settings",
        description: "Persist a new `ScriptedOutputSettings`. Picked up by the next emit call with no runner restart. Callers should pass the full object — missing optional fields default via serde at load time.",
        args_schema: "{ \"settings\": { \"enabled\": boolean, \"model\"?: string | null, \"provider\": \"auto\" | \"claude_api_warm\" | \"claude_api\" | \"gemma_local_warm\", \"gemma_local_endpoint\": string, \"gemma_local_model_alias\": string } }",
        response_schema: "{ \"enabled\": boolean, \"model\": string | null, \"provider\": \"auto\" | \"claude_api_warm\" | \"claude_api\" | \"gemma_local_warm\", \"gemma_local_endpoint\": string, \"gemma_local_model_alias\": string }",
        // Write command — skip the startup empty-args probe so we never
        // race the settings file during boot.
        probe_with_empty_args: false,
    },
    ProxyableCommand {
        name: "report_ui_error",
        description: "Record a UI error observed by the React error boundary. Coalesces repeat reports with the same message/digest into a single record (incrementing `count`, refreshing `reported_at`, pinning `first_seen`).",
        args_schema: "{ \"message\": string, \"stack\"?: string | null, \"componentStack\"?: string | null, \"digest\"?: string | null }",
        response_schema: "null",
        probe_with_empty_args: true,
    },
    ProxyableCommand {
        name: "clear_ui_error",
        description: "Clear the current UI error state (called on error boundary recovery).",
        args_schema: r#"{"type":"object","properties":{},"additionalProperties":false}"#,
        response_schema: r#"{"type":"null"}"#,
        // Destructive + takes empty args → probing it on startup would
        // race the React ErrorBoundary's own first report.
        probe_with_empty_args: false,
    },
    ProxyableCommand {
        name: "get_ui_error",
        description: "Read the current UI error state, or null if none.",
        args_schema: r#"{"type":"object","properties":{},"additionalProperties":false}"#,
        response_schema: r#"{"type":["object","null"],"properties":{"message":{"type":"string"},"stack":{"type":["string","null"]},"component_stack":{"type":["string","null"]},"digest":{"type":["string","null"]},"first_seen":{"type":"string"},"reported_at":{"type":"string"},"count":{"type":"integer"}}}"#,
        probe_with_empty_args: true,
    },
    ProxyableCommand {
        name: "dismiss_recent_crash",
        description: "Acknowledge the startup crash-dump banner, clearing /health.recent_crash and flipping derived_status back to healthy. The on-disk crash_*.txt file is left intact for forensics.",
        args_schema: r#"{"type":"object","properties":{},"additionalProperties":false}"#,
        response_schema: r#"{"type":"null"}"#,
        // Destructive + takes empty args → probing it on startup would
        // clear the very crash dump we just surfaced to the user.
        probe_with_empty_args: false,
    },
    // Saved-projects registry (user-curated project list populated by the
    // setup wizard and surfaced in the UI Bridge Integration panel).
    ProxyableCommand {
        name: "list_saved_projects",
        description: "Return the user-curated list of projects persisted in settings.json (populated by the setup wizard's project picker; consumed by the UI Bridge Integration panel dropdown). Empty array on first run.",
        args_schema: r#"{"type":"object","properties":{},"additionalProperties":false}"#,
        response_schema: r#"{"type":"array","items":{"type":"object","required":["path","name","projectType","manifest"],"properties":{"path":{"type":"string"},"name":{"type":"string"},"projectType":{"type":"string"},"manifest":{"type":"string"}}}}"#,
        probe_with_empty_args: true,
    },
    ProxyableCommand {
        name: "save_saved_projects",
        description: "Atomically replace the entire saved-projects list. Used by the setup wizard on commit.",
        args_schema: r#"{"type":"object","required":["projects"],"properties":{"projects":{"type":"array","items":{"type":"object","required":["path","name","projectType","manifest"],"properties":{"path":{"type":"string"},"name":{"type":"string"},"projectType":{"type":"string"},"manifest":{"type":"string"}}}}}}"#,
        response_schema: r#"{"type":"null"}"#,
        // Destructive (replaces the entire list). Probing with empty args
        // would wipe the user's saved projects every startup.
        probe_with_empty_args: false,
    },
    ProxyableCommand {
        name: "add_saved_project",
        description: "Append a project to the saved list. Idempotent by normalized path.",
        args_schema: r#"{"type":"object","required":["project"],"properties":{"project":{"type":"object","required":["path","name","projectType","manifest"],"properties":{"path":{"type":"string"},"name":{"type":"string"},"projectType":{"type":"string"},"manifest":{"type":"string"}}}}}"#,
        response_schema: r#"{"type":"null"}"#,
        // Required `project` arg; empty-args probe would error.
        probe_with_empty_args: false,
    },
    ProxyableCommand {
        name: "remove_saved_project",
        description: "Remove a saved project by path. No-op if the path isn't in the list.",
        args_schema: r#"{"type":"object","required":["path"],"properties":{"path":{"type":"string"}}}"#,
        response_schema: r#"{"type":"null"}"#,
        // Required `path` arg; empty-args probe would error.
        probe_with_empty_args: false,
    },
    // Productivity Stack — Phase 3 (in-product /decompose-plan replacement).
    ProxyableCommand {
        name: "decompose_plan",
        description: "Decompose a plan markdown into a structured task graph + populate the upcoming-file claim registry. Reads the plan, computes a SHA-256 hash for idempotency, asks the active LLM provider to identify phases/tasks/claims/dependencies, then POSTs the structured payload to /plans/decompose. Returns `{ planId, taskCount, versionHash, idempotentSkip, stamped }`. When no LLM provider is configured, returns the error string \"Configure an LLM provider in Settings → AI to use Decompose Plan.\" so the calling modal can show the affordance.",
        args_schema: r#"{"type":"object","required":["planPath"],"properties":{"planPath":{"type":"string"}}}"#,
        response_schema: r#"{"type":"object","required":["planId","taskCount","versionHash","idempotentSkip","stamped"],"properties":{"planId":{"type":"string"},"taskCount":{"type":"integer"},"versionHash":{"type":"string"},"idempotentSkip":{"type":"boolean"},"stamped":{"type":"boolean"}}}"#,
        // Required `planPath` arg; empty-args probe would always fail.
        probe_with_empty_args: false,
    },
    // Productivity Stack — Phase 5 (in-product /summarize-session and /rewind-session replacements).
    ProxyableCommand {
        name: "summarize_session",
        description: "Summarize a finished AI session: extract learnings via the configured `OneshotLlm` and persist them to `productivity_knowledge`. Encodes the slash command's verdict-driven Outcome-tag rule (failed-attempt sessions get `## Outcome: APPROACH FAILED — do not retry without addressing X` prepended to each learning body). When no LLM provider is configured, falls back to inserting a single placeholder knowledge row with `area=\"other\"` and `body=\"LLM provider not configured; manual summary required.\"` so the user has a UI affordance.",
        args_schema: r#"{"type":"object","required":["taskRunId"],"properties":{"taskRunId":{"type":"string"}}}"#,
        response_schema: r#"{"type":"object","required":["taskRunId","verdict","learningCount","byArea","placeholder"],"properties":{"taskRunId":{"type":"string"},"verdict":{"type":"string"},"learningCount":{"type":"integer"},"byArea":{"type":"object","additionalProperties":{"type":"integer"}},"placeholder":{"type":"boolean"}}}"#,
        // Required `taskRunId` arg; empty-args probe would always fail.
        probe_with_empty_args: false,
    },
    ProxyableCommand {
        name: "rewind_session",
        description: "Rewind a failed AI session: restore pre-edit file snapshots (sha256-verified), kill the failed worker, and (by default) spawn a replacement with failure-context prepended. Pass `noReplay: true` for revert + leave-tab-empty (manual re-prompt). File-restore + kill are LLM-independent; the summarize step that builds the failure-context block silently skips when no LLM is configured.",
        args_schema: r#"{"type":"object","required":["taskRunId"],"properties":{"taskRunId":{"type":"string"},"noReplay":{"type":["boolean","null"]}}}"#,
        response_schema: r#"{"type":"object","required":["taskRunId","filesRestored","filesSkipped","summarized"],"properties":{"taskRunId":{"type":"string"},"filesRestored":{"type":"integer"},"filesSkipped":{"type":"integer"},"replaySessionId":{"type":["string","null"]},"summarized":{"type":"boolean"},"verdict":{"type":["string","null"]}}}"#,
        // Required `taskRunId` arg; destructive (mutates filesystem +
        // kills sessions). Probe-with-empty-args would always fail
        // anyway, but mark explicit for safety.
        probe_with_empty_args: false,
    },
    // Productivity Stack — Phase 6 follow-up (worker observability).
    ProxyableCommand {
        name: "list_workers",
        description: "List every registered pty-backed Claude worker, joined with TerminalManager titles and the coordinator's view of each worker's currently-assigned task. Read-only observability for the Workers panel and external debugging tools. Returns an array of `{ taskRunId, terminalId, terminalTitle, state, assignedTaskId, createdAtMs }` ordered by worker creation time (oldest first). State is one of `\"ready\"`, `\"processing\"`, `\"closed\"`. Empty list when no workers are registered.",
        args_schema: r#"{"type":"object","properties":{},"additionalProperties":false}"#,
        response_schema: r#"{"type":"array","items":{"type":"object","required":["taskRunId","terminalId","state","createdAtMs"],"properties":{"taskRunId":{"type":"string"},"terminalId":{"type":"string"},"terminalTitle":{"type":["string","null"]},"state":{"type":"string","enum":["ready","processing","closed"]},"assignedTaskId":{"type":["string","null"]},"createdAtMs":{"type":"integer"}}}}"#,
        // Read-only — empty args is the canonical call shape.
        probe_with_empty_args: true,
    },
    // Coord/terminal smoke commands — bi-directional title sync (Phase 2
    // of runner-dispatch-and-terminal-ux-fixes-plan) + worker spawn.
    ProxyableCommand {
        name: "terminal_set_title",
        description: "Update a terminal session's display title. Mirrors OSC 0/2 titles emitted by the PTY into TerminalSession.title and broadcasts terminal-title-changed to other webviews / WS subscribers. Pair to ZoneGrid's onTitleChange handler.",
        args_schema: r#"{"type":"object","required":["terminalId","title"],"properties":{"terminalId":{"type":"string"},"title":{"type":"string"}},"additionalProperties":false}"#,
        response_schema: r#"{"type":"object","required":["success"],"properties":{"success":{"type":"boolean"},"message":{"type":["string","null"]},"data":{"type":["object","null"]}}}"#,
        // Required `terminalId` + `title`; empty-args probe would always
        // fail, and a real call would mutate session state.
        probe_with_empty_args: false,
    },
    ProxyableCommand {
        name: "spawn_worker_session",
        description: "Spawn a Claude-Code-backed worker PTY pre-sized to the dominant zone dimensions and register it under a fresh task_run_id in SessionManager.worker_sessions. Used by the Productivity tab Workers panel and coord soak smokes.",
        args_schema: r#"{"type":"object","properties":{"titleHint":{"type":["string","null"]}},"additionalProperties":false}"#,
        response_schema: r#"{"type":"object","required":["mode"],"properties":{"mode":{"type":"string"},"terminalId":{"type":["string","null"]},"taskRunId":{"type":["string","null"]}}}"#,
        // Spawns a PTY child process — side-effectful even with empty
        // args. Probe must skip.
        probe_with_empty_args: false,
    },
];

/// Whether a command name is in the UI Bridge invoke allowlist.
///
/// Used by the HTTP handler to gate the dispatch — anything not in the
/// list should return 400 before the request_id is even allocated.
pub fn is_allowlisted(name: &str) -> bool {
    UI_BRIDGE_COMMANDS.iter().any(|c| c.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn register_and_deliver_round_trip() {
        let store = InvokeRequestStore::new();
        let (tx, rx) = oneshot::channel();
        store.register("req-1".to_string(), tx).await;

        let response = InvokeResponse {
            ok: true,
            result: Some(serde_json::json!({ "hello": "world" })),
            error: None,
        };
        assert!(store.deliver("req-1", response.clone()).await);

        let received = rx.await.expect("receiver should observe sent value");
        assert!(received.ok);
        assert_eq!(received.result, response.result);
        assert!(received.error.is_none());
    }

    #[tokio::test]
    async fn deliver_unknown_request_id_is_noop() {
        let store = InvokeRequestStore::new();
        let delivered = store
            .deliver(
                "does-not-exist",
                InvokeResponse {
                    ok: true,
                    result: Some(serde_json::Value::Null),
                    error: None,
                },
            )
            .await;
        assert!(!delivered);
    }

    #[tokio::test]
    async fn cancel_removes_entry_so_late_deliver_is_noop() {
        let store = InvokeRequestStore::new();
        let (tx, _rx) = oneshot::channel();
        store.register("req-2".to_string(), tx).await;

        store.cancel("req-2").await;

        let delivered = store
            .deliver(
                "req-2",
                InvokeResponse {
                    ok: false,
                    result: None,
                    error: Some("too late".to_string()),
                },
            )
            .await;
        assert!(!delivered);
    }

    #[tokio::test]
    async fn error_response_round_trip() {
        let store = InvokeRequestStore::new();
        let (tx, rx) = oneshot::channel();
        store.register("req-3".to_string(), tx).await;

        let response = InvokeResponse {
            ok: false,
            result: None,
            error: Some("frontend invoke failed: missing required key settings".to_string()),
        };
        assert!(store.deliver("req-3", response.clone()).await);
        let received = rx.await.expect("receiver should observe error value");
        assert!(!received.ok);
        assert_eq!(received.error, response.error);
    }

    #[test]
    fn is_allowlisted_recognizes_known_commands() {
        assert!(is_allowlisted("get_web_integration_status"));
        assert!(is_allowlisted("save_web_integration_settings"));
        assert!(is_allowlisted("test_web_integration_connection"));
        assert!(is_allowlisted("start_web_token_flow"));
    }

    #[test]
    fn is_allowlisted_recognizes_scripted_output_commands() {
        assert!(is_allowlisted("emit_extraction_script"));
        assert!(is_allowlisted("emit_scripted_output_event"));
        assert!(is_allowlisted("get_scripted_output_stats"));
    }

    #[test]
    fn is_allowlisted_recognizes_ui_error_commands() {
        assert!(is_allowlisted("report_ui_error"));
        assert!(is_allowlisted("clear_ui_error"));
        assert!(is_allowlisted("get_ui_error"));
    }

    #[test]
    fn is_allowlisted_recognizes_coord_terminal_commands() {
        assert!(is_allowlisted("terminal_set_title"));
        assert!(is_allowlisted("spawn_worker_session"));
    }

    #[test]
    fn is_allowlisted_rejects_unknown_commands() {
        assert!(!is_allowlisted("rm_rf_my_disk"));
        assert!(!is_allowlisted("execute_sql"));
        assert!(!is_allowlisted(""));
    }

    #[test]
    fn allowlist_is_not_empty_and_schemas_are_populated() {
        assert!(!UI_BRIDGE_COMMANDS.is_empty());
        for cmd in UI_BRIDGE_COMMANDS {
            assert!(!cmd.name.is_empty(), "command name must not be empty");
            assert!(
                !cmd.description.is_empty(),
                "command {} needs a description",
                cmd.name
            );
            assert!(
                !cmd.args_schema.is_empty(),
                "command {} needs an args_schema",
                cmd.name
            );
            assert!(
                !cmd.response_schema.is_empty(),
                "command {} needs a response_schema",
                cmd.name
            );
        }
    }
}
