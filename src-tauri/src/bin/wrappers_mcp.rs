//! Stdio MCP server exposing the runner's installed wrapper actions as tools.
//!
//! Spawned by `claude` CLI via `claude mcp add qontinui-wrappers -- <path>`.
//! Reads JSON-RPC 2.0 requests on stdin, writes responses on stdout. All
//! logging goes to STDERR — stdout is reserved for protocol traffic and a
//! single stray byte breaks the framing.
//!
//! Backend: makes blocking HTTP calls to `<base>/wrappers/...`. The base URL
//! is resolved (in order):
//!   1. `QONTINUI_RUNNER_PRIMARY_URL` — full URL, **validated to be loopback
//!      HTTP only**: `http://(127.0.0.1|localhost):<port>` with no path,
//!      query, fragment, or userinfo. Anything else logs a warning and is
//!      ignored so a poisoned env can't redirect dispatch (and credentials)
//!      to an attacker-controlled host.
//!   2. `QONTINUI_PRIMARY_PORT` — port only; URL is `http://127.0.0.1:<port>`.
//!      Matches the runner's primary-discovery convention (see
//!      `crate::instance::primary_port`).
//!   3. `QONTINUI_RUNNER_API_PORT` — port only; kept for back-compat with
//!      pre-primary-ownership setups.
//!   4. Default `http://127.0.0.1:9876`.
//!
//! Tool name format: `wrapper_<wrapperId>__<actionId_underscored>`. Dashes in
//! the action id are replaced with underscores so the name is a legal MCP
//! tool identifier (the canonical action id with original dashes is kept in
//! the reverse map for the dispatch call).
//!
//! Tool catalog is refreshed:
//!   - Once at startup, so `initialize` returns sane capabilities even when
//!     the first request after handshake is `tools/call`.
//!   - On every `tools/list`, so a wrapper installed mid-session appears
//!     without restarting the MCP bin (which is the AI client's lifetime).
//!   - On a `tools/call` for an unknown tool name (one retry), covering
//!     clients that don't re-list before calling.
//!   - On a push from the runner's `GET /wrappers/events` SSE stream — a
//!     background thread long-polls that endpoint, refreshes the cache on
//!     every event, and emits `notifications/tools/list_changed` upstream
//!     so AI clients update their tool palettes without re-listing.
//!
//! If a refresh returns empty while we previously had tools, the existing
//! cache is preserved — almost always a transient runner-down blip rather
//! than the user truly uninstalling every wrapper.

use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use qontinui_runner_lib::mcp_spill::{SpillRecord, SpillStore};

const SERVER_NAME: &str = "qontinui-wrappers";
const SERVER_VERSION: &str = "0.1.0";
const PROTOCOL_VERSION: &str = "2024-11-05";

// JSON-RPC 2.0 error codes.
const ERR_PARSE: i64 = -32700;
const ERR_INVALID_REQUEST: i64 = -32600;
const ERR_METHOD_NOT_FOUND: i64 = -32601;
const ERR_INVALID_PARAMS: i64 = -32602;
const ERR_INTERNAL: i64 = -32603;

// ---------------------------------------------------------------------------
// JSON-RPC envelopes
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct JsonRpcRequest {
    #[allow(dead_code)]
    #[serde(default)]
    jsonrpc: String,
    #[serde(default)]
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

fn rpc_success(id: Option<Value>, result: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id.unwrap_or(Value::Null),
        "result": result,
    })
}

fn rpc_error(id: Option<Value>, code: i64, message: impl Into<String>) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id.unwrap_or(Value::Null),
        "error": {
            "code": code,
            "message": message.into(),
        },
    })
}

// ---------------------------------------------------------------------------
// Tool catalog
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct ToolEntry {
    /// MCP-visible tool name, e.g. `wrapper_v0__create_component`.
    name: String,
    /// Wrapper id (`v0`).
    wrapper_id: String,
    /// Original action id with dashes preserved (`create-component`) — what
    /// the dispatch endpoint expects.
    action_id: String,
    /// Human-readable description.
    description: String,
    /// JSON Schema for the action's params (opaque pass-through from the
    /// runner). Defaults to `{ "type": "object" }` if the wrapper provides
    /// no schema.
    input_schema: Value,
}

fn sanitize_action_id(action_id: &str) -> String {
    action_id.replace('-', "_")
}

/// Fetch every wrapper from the runner and flatten its actions into a tool
/// list. On any failure (runner not up, network error, malformed response)
/// we log to stderr and return an empty list so `initialize` still
/// succeeds — Claude can later call `tools/list` again after the runner
/// boots, but typically the user just relaunches.
fn fetch_tools(base: &str) -> Vec<ToolEntry> {
    let client = match reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[wrappers-mcp] failed to build http client: {}", e);
            return Vec::new();
        }
    };

    let url = format!("{}/wrappers", base);
    let resp = match client.get(&url).send() {
        Ok(r) => r,
        Err(e) => {
            eprintln!(
                "[wrappers-mcp] GET {} failed: {} (runner not up? starting with empty tool list)",
                url, e
            );
            return Vec::new();
        }
    };

    if !resp.status().is_success() {
        eprintln!(
            "[wrappers-mcp] GET {} returned {} — empty tool list",
            url,
            resp.status()
        );
        return Vec::new();
    }

    let body: Value = match resp.json() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("[wrappers-mcp] decode {}: {}", url, e);
            return Vec::new();
        }
    };

    // Expected envelope: { "success": true, "data": [Wrapper, ...] }
    let wrappers = match body.get("data").and_then(|d| d.as_array()) {
        Some(arr) => arr,
        None => {
            eprintln!(
                "[wrappers-mcp] {} response missing data[] array — empty tool list",
                url
            );
            return Vec::new();
        }
    };

    let mut tools = Vec::new();
    for wrapper in wrappers {
        let wrapper_id = match wrapper.get("id").and_then(|v| v.as_str()) {
            Some(id) => id.to_string(),
            None => continue,
        };
        let display_name = wrapper
            .get("manifest")
            .and_then(|m| m.get("displayName"))
            .and_then(|v| v.as_str())
            .unwrap_or(&wrapper_id)
            .to_string();
        let actions = match wrapper.get("actions").and_then(|a| a.as_array()) {
            Some(a) => a,
            None => continue,
        };
        for action in actions {
            let action_id = match action.get("id").and_then(|v| v.as_str()) {
                Some(id) => id.to_string(),
                None => continue,
            };
            let description = action
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let input_schema = action
                .get("paramSchema")
                .cloned()
                .filter(|v| !v.is_null())
                .unwrap_or_else(|| json!({ "type": "object" }));

            let name = format!("wrapper_{}__{}", wrapper_id, sanitize_action_id(&action_id));
            let full_desc = if description.is_empty() {
                format!("{} action: {}", display_name, action_id)
            } else {
                format!("{} — {}", display_name, description)
            };
            tools.push(ToolEntry {
                name,
                wrapper_id: wrapper_id.clone(),
                action_id,
                description: full_desc,
                input_schema,
            });
        }
    }

    eprintln!(
        "[wrappers-mcp] loaded {} tool(s) from {} wrapper(s)",
        tools.len(),
        wrappers.len()
    );
    tools
}

fn build_reverse_map(tools: &[ToolEntry]) -> HashMap<String, (String, String)> {
    tools
        .iter()
        .map(|t| (t.name.clone(), (t.wrapper_id.clone(), t.action_id.clone())))
        .collect()
}

/// Refresh `tools` and `reverse_map` in place from the runner. Cheap (~5ms
/// loopback HTTP). If the fetch returns empty while we previously had
/// tools, the existing cache is preserved — see the module docstring.
fn refresh_tools(
    base: &str,
    tools: &mut Vec<ToolEntry>,
    reverse_map: &mut HashMap<String, (String, String)>,
) {
    let fresh = fetch_tools(base);
    if fresh.is_empty() && !tools.is_empty() {
        eprintln!(
            "[wrappers-mcp] refresh returned 0 tools — keeping previous {} cached",
            tools.len()
        );
        return;
    }
    *reverse_map = build_reverse_map(&fresh);
    *tools = fresh;
}

fn tools_list_payload(tools: &[ToolEntry]) -> Value {
    let mut arr: Vec<Value> = tools
        .iter()
        .map(|t| {
            json!({
                "name": t.name,
                "description": t.description,
                "inputSchema": t.input_schema,
            })
        })
        .collect();
    arr.push(subagent_tool_entry());
    arr.push(read_spill_tool_entry());
    json!({ "tools": arr })
}

// ---------------------------------------------------------------------------
// Static built-in tools (not wrapper-backed)
// ---------------------------------------------------------------------------

/// Name of the static subagent-analysis tool. Dispatched BEFORE the dynamic
/// `wrapper_*` reverse map, so a wrapper action can never shadow it.
const SUBAGENT_TOOL_NAME: &str = "analyze_with_subagent";

/// Static tool entry for subagent analysis (plan
/// 2026-07-15-runner-pi-deepseek-subagent-analysis, Phase 4). Backed by the
/// runner's `POST /subagent/analyze` route, not by a wrapper.
fn subagent_tool_entry() -> Value {
    json!({
        "name": SUBAGENT_TOOL_NAME,
        "description": "Offload a file-analysis task to a subagent and get the analysis text \
            back as the tool result, without spending main-session context reading the files. \
            provider 'pi' stages read-only copies of the files in a temp dir and lets the pi \
            coding agent (DeepSeek-backed by default) explore them agentically; provider \
            'deepseek' inlines the file contents into a single one-shot DeepSeek API call \
            (text files only, 256KB/file, 1MB total).",
        "inputSchema": {
            "type": "object",
            "properties": {
                "provider": {
                    "type": "string",
                    "enum": ["pi", "deepseek"],
                    "description": "Subagent to use: 'pi' (agentic exploration) or 'deepseek' (one-shot inline analysis)."
                },
                "prompt": {
                    "type": "string",
                    "description": "The analysis question or instruction."
                },
                "file_refs": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Absolute paths of the files to analyze."
                },
                "timeout_secs": {
                    "type": "integer",
                    "description": "Wall-clock budget in seconds (default 300)."
                }
            },
            "required": ["provider", "prompt"]
        }
    })
}

/// Handle a `tools/call` for the static subagent tool: POST the arguments
/// straight through to the runner's `/subagent/analyze` route (the body
/// shapes match by construction) and unwrap the `ApiResponse<String>`
/// envelope.
///
/// **Deliberate input/output asymmetry.** `subagent/mod.rs:24-27` bounds this
/// tool's *inputs* with a HARD ERROR (`MAX_FILE_BYTES` / `MAX_TOTAL_BYTES`),
/// reasoning that failing fast beats a confusing API-side truncation. Its
/// *output* — `Ok(response.output)` at `subagent/mod.rs:142,263` — is uncapped,
/// and reaches the agent through [`text_result`] below, which spills instead of
/// erroring. That is not an inconsistency to be tidied away: a caller can make
/// an input smaller, but nobody can make a result smaller after the work is
/// done, so erroring on an oversized output would destroy analysis that was
/// already paid for. Inputs get the hard error; outputs get the preview plus a
/// locator (plan `2026-08-20-runner-mcp-tool-output-spill`, "Design decision").
fn dispatch_subagent_call(base: &str, arguments: Value) -> Value {
    let timeout_secs = arguments
        .get("timeout_secs")
        .and_then(Value::as_u64)
        .unwrap_or(300);

    // Client timeout = subagent budget + headroom for staging/queueing.
    let client = match reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(timeout_secs + 60))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return tool_error(
                SUBAGENT_TOOL_NAME,
                format!("http client init failed: {}", e),
            )
        }
    };

    let url = format!("{}/subagent/analyze", base);
    let resp = match client.post(&url).json(&arguments).send() {
        Ok(r) => r,
        Err(e) => {
            return tool_error(
                SUBAGENT_TOOL_NAME,
                format!("subagent dispatch HTTP error: {}", e),
            )
        }
    };

    let status = resp.status();
    let parsed: Value = match resp.json() {
        Ok(v) => v,
        Err(e) => {
            return tool_error(
                SUBAGENT_TOOL_NAME,
                format!("subagent dispatch returned non-JSON: {}", e),
            )
        }
    };

    let success = parsed
        .get("success")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !status.is_success() || !success {
        // `parsed` is the WHOLE upstream body when it carries no `error`
        // string, so this message is as unbounded as any success body — which
        // is why the error path goes through the same spill policy.
        let msg = parsed
            .get("error")
            .and_then(Value::as_str)
            .map(|s| s.to_string())
            .unwrap_or_else(|| format!("{} returned {}: {}", url, status, parsed));
        return tool_error(
            SUBAGENT_TOOL_NAME,
            format!("subagent analysis failed: {}", msg),
        );
    }

    let text = parsed
        .get("data")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    // The subagent returns prose, never a serialized structure — this path
    // does not go through `to_string_pretty`.
    text_result(SUBAGENT_TOOL_NAME, "text/plain", text)
}

/// Name of the static spill-retrieval tool. Like [`SUBAGENT_TOOL_NAME`] it is
/// dispatched BEFORE the dynamic `wrapper_*` reverse map, so a wrapper action
/// can never shadow it — which matters more here than for the subagent tool,
/// since a shadowed retrieval turns every locator this server has already
/// issued into a dead pointer.
const READ_SPILL_TOOL_NAME: &str = "read_spilled_result";

/// Default slice size when a caller names no `length`.
const DEFAULT_READ_LENGTH: u64 = 16 * 1024;

/// Room left between the largest slice we will return and the spill threshold,
/// to cover the retrieval result's own header lines.
const READ_LENGTH_HEADROOM: u64 = 4096;

/// Floor on a slice, so a caller passing `length: 0` still makes progress.
const MIN_READ_LENGTH: u64 = 256;

/// Largest slice [`dispatch_read_spill`] will return.
///
/// Derived from the spill threshold rather than fixed, because it is what keeps
/// a retrieval from re-spilling itself: the returned body is one slice plus a
/// short header, so capping the slice at `threshold - READ_LENGTH_HEADROOM`
/// guarantees the retrieval result stays under the threshold and the reader
/// never has to read a spill of a spill.
fn max_read_length() -> u64 {
    let threshold = spill_threshold_bytes() as u64;
    if threshold == 0 {
        // Spilling is disabled, so no spill can exist to read; the value is
        // unreachable in practice and only has to be sane.
        return DEFAULT_READ_LENGTH;
    }
    threshold
        .saturating_sub(READ_LENGTH_HEADROOM)
        .max(MIN_READ_LENGTH)
}

/// Static tool entry for spill retrieval (plan
/// `2026-08-20-runner-mcp-tool-output-spill`, Phase 3). Backed by this
/// process's own [`SpillStore`] — it reads files the same process wrote, which
/// is why it is a tool here rather than an HTTP route on the runner bin.
fn read_spill_tool_entry() -> Value {
    json!({
        "name": READ_SPILL_TOOL_NAME,
        "description": "Read a byte range of a tool result that was too large to return inline. \
            When a result comes back marked PARTIAL RESULT it carries a `spill_id`; pass that id \
            here to fetch the full body in pieces. Returns the slice plus the offset to continue \
            from, so a large result can be pulled a range at a time instead of all at once.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "spill_id": {
                    "type": "string",
                    "description": "The `spill_id` printed in the partial result."
                },
                "offset": {
                    "type": "integer",
                    "description": "Byte offset to start at (default 0). Use the `next_offset` from the previous slice to continue."
                },
                "length": {
                    "type": "integer",
                    "description": "How many bytes to return (default 16384). Clamped to what fits in one inline result."
                }
            },
            "required": ["spill_id"]
        }
    })
}

/// Handle a `tools/call` for the static spill-retrieval tool.
fn dispatch_read_spill(store: Option<&SpillStore>, arguments: Value) -> Value {
    let store = match store {
        Some(s) => s,
        None => {
            return tool_error(
                READ_SPILL_TOOL_NAME,
                "no spill store is open in this server process, so no result has ever been \
                 spilled and there is nothing to read"
                    .to_string(),
            )
        }
    };
    let spill_id = match arguments.get("spill_id").and_then(Value::as_str) {
        Some(s) if !s.is_empty() => s,
        _ => {
            return tool_error(
                READ_SPILL_TOOL_NAME,
                format!(
                    "{READ_SPILL_TOOL_NAME} requires a non-empty 'spill_id' string — the \
                     `spill_id` printed in the partial result"
                ),
            )
        }
    };
    let offset = arguments.get("offset").and_then(Value::as_u64).unwrap_or(0);
    let length = arguments
        .get("length")
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_READ_LENGTH)
        .clamp(MIN_READ_LENGTH, max_read_length());

    let slice = match store.read(spill_id, offset, length) {
        Ok(s) => s,
        Err(e) => {
            return tool_error(
                READ_SPILL_TOOL_NAME,
                format!("cannot read spill '{spill_id}': {e}"),
            )
        }
    };

    let mut text = format!(
        "Spill {} — bytes {}-{} of {}{}.\ntool: {}\ncontent-type: {}\n",
        slice.record.id,
        slice.offset,
        slice.next_offset,
        slice.record.byte_len,
        if slice.record.is_error {
            " (this body was an ERROR result)"
        } else {
            ""
        },
        slice.record.tool,
        slice.record.content_type,
    );
    if slice.is_final() {
        text.push_str("This slice reaches the END of the body — nothing follows it.\n");
    } else {
        text.push_str(&format!(
            "MORE REMAINS — continue with {{\"spill_id\": \"{}\", \"offset\": {}}}.\n",
            slice.record.id, slice.next_offset
        ));
    }
    text.push_str("----- body -----\n");
    text.push_str(&slice.text);

    // Routed through `text_result` like every other body, deliberately: the
    // envelope is built in exactly one place. `max_read_length` is what stops
    // this from re-spilling — the slice plus this header cannot reach the
    // threshold — so the general policy applies without a special case.
    text_result(READ_SPILL_TOOL_NAME, &slice.record.content_type, text)
}

// ---------------------------------------------------------------------------
// tools/call dispatch
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct ToolsCallParams {
    name: String,
    #[serde(default)]
    arguments: Value,
}

fn dispatch_tool_call(
    base: &str,
    reverse_map: &HashMap<String, (String, String)>,
    params: ToolsCallParams,
) -> Value {
    let tool = params.name.clone();
    let (wrapper_id, action_id) = match reverse_map.get(&params.name) {
        Some(pair) => pair.clone(),
        None => {
            return tool_error(
                &tool,
                format!(
                    "unknown tool '{}' (no matching wrapper action installed)",
                    params.name
                ),
            );
        }
    };

    let url = format!("{}/wrappers/{}/dispatch", base, wrapper_id);
    let body = json!({
        "action": action_id,
        "params": params.arguments,
    });

    // Use a generous timeout — wrappers do real work. The runner itself
    // bounds dispatch at ~60s by default; we add headroom on the client.
    let client = match reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(120))
        .build()
    {
        Ok(c) => c,
        Err(e) => return tool_error(&tool, format!("http client init failed: {}", e)),
    };

    let resp = match client.post(&url).json(&body).send() {
        Ok(r) => r,
        Err(e) => return tool_error(&tool, format!("dispatch HTTP error: {}", e)),
    };

    let status = resp.status();
    let parsed: Value = match resp.json() {
        Ok(v) => v,
        Err(e) => return tool_error(&tool, format!("dispatch returned non-JSON: {}", e)),
    };

    if !status.is_success() {
        // `parsed.to_string()` is the ENTIRE upstream JSON body. A failing
        // wrapper can therefore emit as much context as a succeeding one, so
        // the error path is bounded by the same policy — see `tool_error`.
        let msg = parsed
            .get("error")
            .and_then(|e| e.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| parsed.to_string());
        return tool_error(
            &tool,
            format!("dispatch {} returned {}: {}", url, status, msg),
        );
    }

    // Successful runner envelope:
    //   { "success": true, "data": { "result": <wrapper-result> } }
    let result = parsed
        .get("data")
        .and_then(|d| d.get("result"))
        .cloned()
        .unwrap_or_else(|| parsed.get("data").cloned().unwrap_or(parsed.clone()));

    // `to_string_pretty` STAYS — the plan's open question, resolved by Phase
    // 1's measurement rather than by taste. Compact serialization was expected
    // to buy headroom before a byte is spilled; it buys 0.0–0.3% (a 293,688 B
    // `export-code` result becomes 293,593 B). These bodies are dominated by
    // long string VALUES — source text, base64 — which indentation never
    // touches, so dropping it would cost every human reading an MCP log its
    // readability and return nothing measurable against the budget.
    let text = match serde_json::to_string_pretty(&result) {
        Ok(s) => s,
        Err(e) => format!("<unable to stringify result: {}>", e),
    };
    text_result(&tool, "application/json", text)
}

// ---------------------------------------------------------------------------
// Result construction + the body-size policy (plan
// 2026-08-20-runner-mcp-tool-output-spill, Phase 3 — preview + locator)
// ---------------------------------------------------------------------------

/// Body size, in bytes of `text`, above which a result is spilled to disk and
/// replaced by a preview.
///
/// **Provisional — calibrated on CONSTRUCTION, not on traffic.** Phase 1
/// instrumented this server and observed *zero* tool invocations on the box it
/// ran on, so there is no measured percentile behind this number. What there
/// is: bodies computed from real wrapper results with `serde_json` — a 12-file
/// `export-code` at 293,688 B (~73k tokens), an 8-file one at 99,058 B, and a
/// 12-file `download-component` at 90,883 B. 32 KiB sits an order of magnitude
/// below the smallest of those and comfortably above an ordinary structured
/// result, which is the separation the threshold has to make. Tune it with
/// [`SPILL_THRESHOLD_ENV`] rather than by rebuilding, and revisit it once
/// [`METRIC_PREFIX`] lines exist from a box that actually calls these tools.
const DEFAULT_SPILL_THRESHOLD_BYTES: usize = 32 * 1024;

/// Operator override for [`DEFAULT_SPILL_THRESHOLD_BYTES`], in bytes. `0`
/// disables spilling outright — every body goes back whole, exactly as it did
/// before this phase — which is the escape hatch for a consumer that turns out
/// to need entire bodies.
const SPILL_THRESHOLD_ENV: &str = "QONTINUI_MCP_SPILL_THRESHOLD_BYTES";

/// Bytes of the body shown at the head of a preview.
const PREVIEW_HEAD_BYTES: usize = 4096;

/// Bytes of the body shown at the tail of a preview. Head+tail rather than
/// head-only because for the content this server actually returns — logs,
/// diffs, source, command output — the tail is where the failure is.
const PREVIEW_TAIL_BYTES: usize = 2048;

/// A body whose longest whitespace-free run exceeds this is treated as having
/// no line structure, and gets no excerpt at all.
///
/// **The detection rule, and why it is whitespace runs.** The question a
/// preview has to answer is not "is this text?" but "can a head+tail slice
/// land on a boundary a reader can orient from?" Whitespace is the universal
/// boundary marker: logs, diffs, JSON and source all have it every few bytes,
/// while a base64 archive — `wrapper_v0__download_component` returns
/// `{format, byteLength, base64}`, and the base64 value is the whole payload —
/// is one unbroken token tens of thousands of bytes long. Slicing that yields
/// two arbitrary fragments of an opaque blob, which is worse than useless
/// because it *looks* like content. Newline density was the obvious
/// alternative and is wrong: `to_string_pretty` sprinkles newlines through the
/// base64 body's JSON structure, so density calls it line-structured. The cost
/// of this rule is a conservative false negative — one minified bundle inside
/// an otherwise readable 12-file result suppresses the excerpt for all of it —
/// which loses an excerpt but never tells a lie, and the locator still gets the
/// reader to every byte.
const MAX_OPAQUE_TOKEN_BYTES: usize = 4096;

/// The spill store for this process's session, or `None` when one could not be
/// opened. Process-global because the MCP server's lifetime *is* the AI
/// client's session — there is exactly one, and threading it through every
/// dispatch frame would buy nothing. Never set in unit tests, so the tests
/// exercise the policy through [`text_result_with`] with an explicit store.
static SPILL_STORE: OnceLock<Option<SpillStore>> = OnceLock::new();

fn spill_store() -> Option<&'static SpillStore> {
    SPILL_STORE.get().and_then(Option::as_ref)
}

/// The effective spill threshold. See [`SPILL_THRESHOLD_ENV`].
fn spill_threshold_bytes() -> usize {
    match std::env::var(SPILL_THRESHOLD_ENV) {
        Ok(raw) if !raw.trim().is_empty() => match raw.trim().parse::<usize>() {
            Ok(v) => v,
            Err(e) => {
                eprintln!(
                    "[wrappers-mcp] WARNING ignoring {}={:?} ({}) — using default {}",
                    SPILL_THRESHOLD_ENV, raw, e, DEFAULT_SPILL_THRESHOLD_BYTES
                );
                DEFAULT_SPILL_THRESHOLD_BYTES
            }
        },
        _ => DEFAULT_SPILL_THRESHOLD_BYTES,
    }
}

/// The single construction point for every agent-bound `tools/call` success
/// body in this server. See [`text_result_with`] for the policy.
fn text_result(tool: &str, content_type: &str, text: String) -> Value {
    text_result_with(
        tool,
        content_type,
        false,
        text,
        spill_store(),
        spill_threshold_bytes(),
    )
}

/// The error counterpart of [`text_result`]. Error bodies are spilled by the
/// same rule: both `dispatch_tool_call` and `dispatch_subagent_call`
/// interpolate an entire upstream JSON body into their message when the
/// response carries no `error` string, so a failure can cost as much context
/// as a success.
fn tool_error(tool: &str, msg: String) -> Value {
    eprintln!("[wrappers-mcp] tool error: {}", msg);
    text_result_with(
        tool,
        "text/plain",
        true,
        msg,
        spill_store(),
        spill_threshold_bytes(),
    )
}

/// Build the wire envelope, applying the spill policy.
///
/// Every result — wrapper dispatch, the static tools, and the error path alike
/// — is shaped `{"content":[{"type":"text","text": <String>}]}`, with
/// `isError: true` alongside for a failure. That shape used to be spelled out
/// three times; one function makes it one fact instead of three copies, and
/// gives the body-size policy exactly one place to live.
///
/// Under the threshold the envelope is byte-identical to the inline `json!`
/// literals this replaced (`text_result_envelope_is_unchanged` pins that).
/// Over it, the body is written to the spill store and the inline text becomes
/// a preview that says so — see [`spill_preview`].
///
/// **A spill that fails returns the body WHOLE**, loudly, rather than
/// truncating it or erroring: the honesty argument that rejected silent
/// truncation applies just as much when the disk is the thing that failed, and
/// an oversized truthful result beats a lost one. `isError` survives a spill —
/// the flag rides on the envelope, not on the text.
fn text_result_with(
    tool: &str,
    content_type: &str,
    is_error: bool,
    text: String,
    store: Option<&SpillStore>,
    threshold: usize,
) -> Value {
    let text = if threshold == 0 || text.len() <= threshold {
        text
    } else {
        match store {
            None => {
                eprintln!(
                    "[wrappers-mcp] WARNING {} produced {} bytes (threshold {}) but no spill \
                     store is open — returning it whole",
                    tool,
                    text.len(),
                    threshold
                );
                text
            }
            Some(store) => match store.put(tool, content_type, is_error, &text) {
                Ok(record) => {
                    let preview = spill_preview(&record, &text);
                    eprintln!(
                        "{} spill tool={} original_bytes={} preview_bytes={} spill_id={} \
                         is_error={}",
                        METRIC_PREFIX,
                        tool,
                        record.byte_len,
                        preview.len(),
                        record.id,
                        is_error
                    );
                    preview
                }
                Err(e) => {
                    eprintln!(
                        "[wrappers-mcp] WARNING spilling {} bytes for {} failed: {} — returning \
                         the body whole",
                        text.len(),
                        tool,
                        e
                    );
                    text
                }
            },
        }
    };

    let mut result = json!({
        "content": [{ "type": "text", "text": text }],
    });
    // `isError` rides in the same object as `content`. Inserting it after
    // construction rather than spelling a second `json!` literal keeps this
    // the only place the content envelope is built. The serialized bytes are
    // unchanged — `serde_json::Map` orders by key unless `preserve_order` is
    // on, and the test asserts the equality rather than assuming it, so
    // enabling that feature would fail loudly instead of silently reordering
    // the wire.
    if is_error {
        if let Some(obj) = result.as_object_mut() {
            obj.insert("isError".to_string(), Value::Bool(true));
        }
    }
    result
}

/// Render the inline stand-in for a spilled body.
///
/// The payload is **explicitly partial, not merely shorter**: it opens with a
/// warning line, states how much of the body it is showing against the true
/// byte length, and carries the locator plus the exact call that fetches the
/// rest. A model must never have to infer from length alone that it is holding
/// a fragment.
fn spill_preview(record: &SpillRecord, text: &str) -> String {
    let total = record.byte_len;
    let mut out = String::with_capacity(PREVIEW_HEAD_BYTES + PREVIEW_TAIL_BYTES + 1024);

    out.push_str(&format!(
        "!! PARTIAL RESULT — this is a PREVIEW, not the whole output. The full body is {total} \
         bytes and has been written to this server's spill store.\n\
         tool: {}\ncontent-type: {}\nspill_id: {}\n\n\
         Read the rest with this server's `{}` tool, e.g.\n  \
         {{\"spill_id\": \"{}\", \"offset\": 0, \"length\": {}}}\n\
         Repeat with the `next_offset` it reports until it says the end of the body is reached. \
         Do not treat anything below as complete.\n",
        record.tool,
        record.content_type,
        record.id,
        READ_SPILL_TOOL_NAME,
        record.id,
        max_read_length(),
    ));

    if !preview_is_sliceable(text) {
        out.push_str(&format!(
            "\n----- no excerpt -----\nThis body has no line structure (its longest run of bytes \
             without whitespace is {} — a base64 blob or a minified file looks like this), so a \
             head+tail excerpt would be two arbitrary fragments of one unbroken token. Read it by \
             range instead.\n----- end of preview -----\n",
            longest_unbroken_run(text)
        ));
        return out;
    }

    let head_end = floor_char_boundary(text, PREVIEW_HEAD_BYTES);
    let tail_start =
        ceil_char_boundary(text, text.len().saturating_sub(PREVIEW_TAIL_BYTES)).max(head_end);
    out.push_str(&format!("\n----- head: bytes 0-{head_end} -----\n"));
    out.push_str(&text[..head_end]);
    out.push_str(&format!(
        "\n----- {} bytes omitted -----\n",
        tail_start - head_end
    ));
    out.push_str(&format!(
        "----- tail: bytes {tail_start}-{} -----\n",
        text.len()
    ));
    out.push_str(&text[tail_start..]);
    out.push_str("\n----- end of preview -----\n");
    out
}

/// Whether a head+tail excerpt of this body would mean anything. See
/// [`MAX_OPAQUE_TOKEN_BYTES`] for the rule and why it is the one chosen.
fn preview_is_sliceable(text: &str) -> bool {
    longest_unbroken_run(text) <= MAX_OPAQUE_TOKEN_BYTES
}

/// Longest run of bytes containing no ASCII whitespace. One pass, no
/// allocation; on a 293 KB body this is microseconds against a call that just
/// finished a blocking HTTP round-trip.
fn longest_unbroken_run(text: &str) -> usize {
    let mut longest = 0usize;
    let mut current = 0usize;
    for b in text.as_bytes() {
        if b.is_ascii_whitespace() {
            current = 0;
        } else {
            current += 1;
            if current > longest {
                longest = current;
            }
        }
    }
    longest
}

/// Largest `i <= at` that is a UTF-8 character boundary (`str::floor_char_boundary`
/// is still unstable).
fn floor_char_boundary(text: &str, at: usize) -> usize {
    let mut i = at.min(text.len());
    while i > 0 && !text.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Smallest `i >= at` that is a UTF-8 character boundary.
fn ceil_char_boundary(text: &str, at: usize) -> usize {
    let mut i = at.min(text.len());
    while i < text.len() && !text.is_char_boundary(i) {
        i += 1;
    }
    i
}

// ---------------------------------------------------------------------------
// Result-size instrumentation (plan 2026-08-20-runner-mcp-tool-output-spill,
// Phase 1 — measure before capping)
// ---------------------------------------------------------------------------

/// Prefix for the machine-readable measurement lines. Grep this out of a
/// client's MCP server log to get the size distribution.
const METRIC_PREFIX: &str = "[wrappers-mcp] metric";

/// Emit one structured line per `tools/call` result describing how big the
/// agent-bound body is.
///
/// **Channel choice: stderr, not an HTTP counter POST — chosen on
/// robustness.** stdout is protocol-reserved (see the module doc: a single
/// stray byte breaks the framing), so the two candidates were a structured
/// stderr line or a counter POSTed to `primary_base_url()`. stderr wins:
///
/// - It is already this binary's only diagnostic channel, so it adds no
///   dependency, no new route to version across the crate boundary, and no
///   new failure mode.
/// - A POST would put a *network call on the response path*. This server
///   already treats "the runner is down" as routine (the catalog refresh
///   preserves its cache on exactly that assumption), so the measurement
///   would go missing precisely when the system is interesting — and its
///   only sane fallback would be to log to stderr anyway. The POST is
///   therefore stderr plus a way to fail.
/// - stderr cannot block on a partition, cannot recurse into dispatch, and
///   cannot corrupt the protocol because it is not stdout.
///
/// Cost is one `format!` and one write per tool call, against a call that
/// just completed a blocking HTTP round-trip — cheap enough to leave on
/// permanently. In particular it does **not** re-serialize the body: it
/// reads `text.len()` in place rather than allocating a second copy of a
/// possibly-hundreds-of-KB string.
///
/// `text_bytes` is the post-serialization size: for a wrapper dispatch the
/// text has already been through `serde_json::to_string_pretty`, so the
/// indentation is counted. The only thing it excludes is the JSON
/// string-escaping applied when the whole response is written, measured at
/// 1.00x–1.08x on realistic payloads (the 1.08x is a 13 KB body; it tends to
/// 1.00x as bodies grow), plus a ~60-byte constant JSON-RPC envelope.
///
/// `text_bytes` is always the size of what the agent ACTUALLY received. For a
/// spilled result that is the preview, not the body it stands in for — the
/// true size is on the separate `metric spill …` line
/// [`text_result_with`] emits, which carries `original_bytes` and the
/// `spill_id`. Keeping this line's meaning fixed is deliberate: it is the
/// context-cost series, and rewriting it to report a number that never entered
/// the agent's window would silently break exactly the measurement Phase 1
/// installed it for.
fn observe_tool_result(tool: &str, result: &Value) {
    let text_bytes = result
        .get("content")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .and_then(|c| c.get("text"))
        .and_then(Value::as_str)
        .map(str::len)
        .unwrap_or(0);
    let is_error = result
        .get("isError")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    // Derived, not passed: `dispatch_tool_call` is the only path that runs
    // `to_string_pretty`, and it does so only on the success arm. The two
    // static tools return plain text (the provider's analysis; a spill slice
    // plus a header), and every error body is a plain message.
    let pretty = !is_error && tool != SUBAGENT_TOOL_NAME && tool != READ_SPILL_TOOL_NAME;
    eprintln!(
        "{} tool_result tool={} text_bytes={} is_error={} pretty={}",
        METRIC_PREFIX, tool, text_bytes, is_error, pretty
    );
}

// ---------------------------------------------------------------------------
// Main loop
// ---------------------------------------------------------------------------

fn write_response(stdout: &mut impl Write, value: &Value) {
    // Each response is a single JSON line followed by a newline.
    let serialized = match serde_json::to_string(value) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[wrappers-mcp] serialize response failed: {}", e);
            return;
        }
    };
    if let Err(e) = writeln!(stdout, "{}", serialized) {
        eprintln!("[wrappers-mcp] write to stdout failed: {}", e);
        return;
    }
    let _ = stdout.flush();
}

/// Bundles the mutable tool catalog so it can travel through an
/// `Arc<Mutex<...>>` and be touched by both the stdin loop and the SSE
/// consumer thread without manual two-field locking.
struct ToolCache {
    tools: Vec<ToolEntry>,
    reverse_map: HashMap<String, (String, String)>,
}

impl ToolCache {
    fn from_tools(tools: Vec<ToolEntry>) -> Self {
        let reverse_map = build_reverse_map(&tools);
        Self { tools, reverse_map }
    }
}

fn handle_request(base: &str, cache: &Arc<Mutex<ToolCache>>, req: JsonRpcRequest) -> Option<Value> {
    let id = req.id.clone();
    match req.method.as_str() {
        "initialize" => Some(rpc_success(
            id,
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "serverInfo": { "name": SERVER_NAME, "version": SERVER_VERSION },
                // `tools.listChanged: true` advertises that we will push
                // `notifications/tools/list_changed` when the catalog
                // changes. The SSE consumer thread is the source of those
                // pushes.
                "capabilities": { "tools": { "listChanged": true } },
            }),
        )),
        "notifications/initialized" | "notifications/cancelled" => {
            // Notifications don't get a response.
            None
        }
        "tools/list" => {
            let mut guard = cache.lock().expect("tool cache mutex poisoned");
            let ToolCache { tools, reverse_map } = &mut *guard;
            refresh_tools(base, tools, reverse_map);
            Some(rpc_success(id, tools_list_payload(tools)))
        }
        "tools/call" => {
            let parsed: ToolsCallParams = match serde_json::from_value(req.params) {
                Ok(p) => p,
                Err(e) => {
                    return Some(rpc_error(
                        id,
                        ERR_INVALID_PARAMS,
                        format!("invalid tools/call params: {}", e),
                    ));
                }
            };
            // Static tools dispatch before the dynamic wrapper catalog (and
            // without touching the cache), so a wrapper action can never
            // shadow them.
            if parsed.name == SUBAGENT_TOOL_NAME {
                let result = dispatch_subagent_call(base, parsed.arguments);
                observe_tool_result(SUBAGENT_TOOL_NAME, &result);
                return Some(rpc_success(id, result));
            }
            if parsed.name == READ_SPILL_TOOL_NAME {
                let result = dispatch_read_spill(spill_store(), parsed.arguments);
                observe_tool_result(READ_SPILL_TOOL_NAME, &result);
                return Some(rpc_success(id, result));
            }
            // Observation happens here rather than inside `text_result`
            // because this is the only frame that knows the tool name;
            // every one of the three body-construction sites returns
            // through one of the two dispatch calls below.
            let tool_name = parsed.name.clone();
            let mut guard = cache.lock().expect("tool cache mutex poisoned");
            let ToolCache { tools, reverse_map } = &mut *guard;
            // If the client never re-listed, a wrapper installed since
            // startup is invisible to our cache. Refresh once before
            // surfacing "unknown tool" so the call still resolves.
            if !reverse_map.contains_key(&parsed.name) {
                refresh_tools(base, tools, reverse_map);
            }
            let result = dispatch_tool_call(base, reverse_map, parsed);
            observe_tool_result(&tool_name, &result);
            Some(rpc_success(id, result))
        }
        "ping" => Some(rpc_success(id, json!({}))),
        other => Some(rpc_error(
            id,
            ERR_METHOD_NOT_FOUND,
            format!("method '{}' is not supported", other),
        )),
    }
}

/// Validates that a `QONTINUI_RUNNER_PRIMARY_URL` value points at the local
/// machine over HTTP. Rejects anything that isn't `http://127.0.0.1:<port>`
/// or `http://localhost:<port>` (with an optional trailing slash) so a
/// poisoned env can't redirect dispatch traffic — including credentials
/// — to an attacker-controlled host. The MCP server is meant for
/// loopback IPC, so this is a tightening, not a feature loss.
fn validate_loopback_http_url(url: &str) -> bool {
    let rest = match url.strip_prefix("http://") {
        Some(r) => r,
        None => return false,
    };
    // Strip a single optional trailing slash; reject any further path / query.
    let rest = rest.strip_suffix('/').unwrap_or(rest);
    if rest.contains('/') || rest.contains('?') || rest.contains('#') || rest.contains('@') {
        return false;
    }
    // Host:port split. Port is required (otherwise we'd hit :80 implicitly,
    // which isn't where the runner lives).
    let (host, port) = match rest.rsplit_once(':') {
        Some(p) => p,
        None => return false,
    };
    if !matches!(host, "127.0.0.1" | "localhost") {
        return false;
    }
    port.parse::<u16>().is_ok() && !port.is_empty()
}

/// Resolves the primary runner's base URL. Precedence: `QONTINUI_RUNNER_PRIMARY_URL`
/// (full URL, validated to be loopback HTTP only) > `QONTINUI_PRIMARY_PORT`
/// (port) > `QONTINUI_RUNNER_API_PORT` (port, back-compat) >
/// `http://127.0.0.1:9876`.
///
/// `QONTINUI_RUNNER_PRIMARY_URL` is rejected if it isn't
/// `http://(127.0.0.1|localhost):<port>` — see `validate_loopback_http_url`.
fn primary_base_url() -> String {
    if let Ok(url) = std::env::var("QONTINUI_RUNNER_PRIMARY_URL") {
        if !url.is_empty() {
            if validate_loopback_http_url(&url) {
                return url;
            }
            eprintln!(
                "[wrappers-mcp] WARNING: ignoring QONTINUI_RUNNER_PRIMARY_URL={url:?} — must be http://(127.0.0.1|localhost):<port>; falling back to next env source",
            );
        }
    }
    if let Ok(port) = std::env::var("QONTINUI_PRIMARY_PORT") {
        if !port.is_empty() {
            return format!("http://127.0.0.1:{}", port);
        }
    }
    if let Ok(port) = std::env::var("QONTINUI_RUNNER_API_PORT") {
        if !port.is_empty() {
            return format!("http://127.0.0.1:{}", port);
        }
    }
    "http://127.0.0.1:9876".to_string()
}

fn main() {
    eprintln!("[wrappers-mcp] starting (version {})", SERVER_VERSION);

    let base = primary_base_url();
    eprintln!("[wrappers-mcp] runner base: {}", base);

    // Open the spill store once, up front, so the first oversized result does
    // not pay for directory creation and so an unusable store is reported at
    // startup rather than discovered mid-conversation. A failure here is not
    // fatal: `text_result_with` then returns oversized bodies whole.
    let _ = SPILL_STORE.set(match SpillStore::open_default() {
        Ok(store) => {
            eprintln!(
                "[wrappers-mcp] spill store: {} (session {}, threshold {} bytes)",
                store.session_dir().display(),
                store.session(),
                spill_threshold_bytes()
            );
            Some(store)
        }
        Err(e) => {
            eprintln!(
                "[wrappers-mcp] WARNING spill store unavailable: {} — oversized results will be \
                 returned whole",
                e
            );
            None
        }
    });

    let cache = Arc::new(Mutex::new(ToolCache::from_tools(fetch_tools(&base))));
    // Wrap stdout so the stdin loop and the SSE thread can both write
    // without interleaving JSON-RPC frames.
    let stdout = Arc::new(Mutex::new(std::io::stdout()));

    // Spawn the SSE consumer. It loops forever, reconnecting with backoff
    // on disconnect; the only way it stops is the process exiting.
    {
        let cache = Arc::clone(&cache);
        let stdout = Arc::clone(&stdout);
        let base = base.clone();
        thread::spawn(move || sse_consumer_loop(&base, cache, stdout));
    }

    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                eprintln!("[wrappers-mcp] stdin read error: {} (exiting)", e);
                break;
            }
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let req: JsonRpcRequest = match serde_json::from_str(trimmed) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("[wrappers-mcp] parse error: {} (line: {})", e, trimmed);
                let err = rpc_error(None, ERR_PARSE, format!("parse error: {}", e));
                let mut out = stdout.lock().expect("stdout mutex poisoned");
                write_response(&mut *out, &err);
                continue;
            }
        };
        if req.method.is_empty() {
            let err = rpc_error(req.id, ERR_INVALID_REQUEST, "method is required");
            let mut out = stdout.lock().expect("stdout mutex poisoned");
            write_response(&mut *out, &err);
            continue;
        }
        if let Some(response) = handle_request(&base, &cache, req) {
            let mut out = stdout.lock().expect("stdout mutex poisoned");
            write_response(&mut *out, &response);
        }
    }

    eprintln!("[wrappers-mcp] stdin closed, shutting down");
}

// ---------------------------------------------------------------------------
// SSE consumer
// ---------------------------------------------------------------------------

const SSE_BACKOFF_INITIAL_SECS: u64 = 1;
const SSE_BACKOFF_MAX_SECS: u64 = 30;

fn next_backoff(current: u64) -> u64 {
    (current.saturating_mul(2)).min(SSE_BACKOFF_MAX_SECS)
}

/// Long-polls `<base>/wrappers/events`. On every `data:` line received,
/// refreshes the tool cache and emits `notifications/tools/list_changed`
/// to stdout. Reconnects with exponential backoff (1s → 2s → 4s → ... →
/// 30s) on any disconnect or non-2xx response. Logs go to stderr.
///
/// We deliberately don't fight to stay connected during the AI client's
/// initialize handshake: clients typically open a single MCP session
/// per conversation, so a 1–2s delay before the first SSE attempt is
/// invisible. If the runner is genuinely down we'll keep retrying at
/// 30s intervals — cheap and the user gets a working push the moment
/// the runner comes back up.
fn sse_consumer_loop(
    base: &str,
    cache: Arc<Mutex<ToolCache>>,
    stdout: Arc<Mutex<std::io::Stdout>>,
) {
    let url = format!("{}/wrappers/events", base);
    let mut backoff = SSE_BACKOFF_INITIAL_SECS;

    loop {
        // SSE is a long-lived stream — disable per-request timeout. The
        // server's keep-alive comments will keep TCP healthy; we only
        // detect disconnect via stream EOF / read error.
        let client = match reqwest::blocking::Client::builder().timeout(None).build() {
            Ok(c) => c,
            Err(e) => {
                eprintln!(
                    "[wrappers-mcp] sse client init failed: {} (retrying in {}s)",
                    e, backoff
                );
                thread::sleep(Duration::from_secs(backoff));
                backoff = next_backoff(backoff);
                continue;
            }
        };

        let resp = match client
            .get(&url)
            .header("Accept", "text/event-stream")
            .send()
        {
            Ok(r) if r.status().is_success() => r,
            Ok(r) => {
                eprintln!(
                    "[wrappers-mcp] sse {} returned {} (retrying in {}s)",
                    url,
                    r.status(),
                    backoff
                );
                thread::sleep(Duration::from_secs(backoff));
                backoff = next_backoff(backoff);
                continue;
            }
            Err(e) => {
                eprintln!(
                    "[wrappers-mcp] sse connect failed: {} (retrying in {}s)",
                    e, backoff
                );
                thread::sleep(Duration::from_secs(backoff));
                backoff = next_backoff(backoff);
                continue;
            }
        };

        eprintln!("[wrappers-mcp] sse connected to {}", url);
        backoff = SSE_BACKOFF_INITIAL_SECS;

        let reader = std::io::BufReader::new(resp);
        for line in reader.lines() {
            match line {
                Ok(l) if is_sse_data_line(&l) => {
                    handle_sse_event(base, &cache, &stdout);
                }
                Ok(_) => {
                    // Comment (`:keep-alive`), `event:`, `id:`, blank — ignore.
                }
                Err(e) => {
                    eprintln!("[wrappers-mcp] sse stream read error: {} (reconnecting)", e);
                    break;
                }
            }
        }

        eprintln!(
            "[wrappers-mcp] sse stream closed; reconnecting in {}s",
            backoff
        );
        thread::sleep(Duration::from_secs(backoff));
        backoff = next_backoff(backoff);
    }
}

fn is_sse_data_line(line: &str) -> bool {
    line.starts_with("data:")
}

fn handle_sse_event(
    base: &str,
    cache: &Arc<Mutex<ToolCache>>,
    stdout: &Arc<Mutex<std::io::Stdout>>,
) {
    {
        let mut guard = cache.lock().expect("tool cache mutex poisoned");
        let ToolCache { tools, reverse_map } = &mut *guard;
        refresh_tools(base, tools, reverse_map);
    }
    let notification = json!({
        "jsonrpc": "2.0",
        "method": "notifications/tools/list_changed",
    });
    let mut out = stdout.lock().expect("stdout mutex poisoned");
    write_response(&mut *out, &notification);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_replaces_dashes() {
        assert_eq!(sanitize_action_id("create-component"), "create_component");
        assert_eq!(sanitize_action_id("plain"), "plain");
        assert_eq!(sanitize_action_id("a-b-c"), "a_b_c");
    }

    #[test]
    fn reverse_map_round_trip() {
        let tools = vec![ToolEntry {
            name: "wrapper_v0__create_component".to_string(),
            wrapper_id: "v0".to_string(),
            action_id: "create-component".to_string(),
            description: "x".to_string(),
            input_schema: json!({}),
        }];
        let map = build_reverse_map(&tools);
        let (w, a) = map.get("wrapper_v0__create_component").unwrap();
        assert_eq!(w, "v0");
        assert_eq!(a, "create-component");
    }

    #[test]
    fn rpc_success_envelope_shape() {
        let v = rpc_success(Some(json!(1)), json!({"ok": true}));
        assert_eq!(v["jsonrpc"], "2.0");
        assert_eq!(v["id"], json!(1));
        assert_eq!(v["result"], json!({"ok": true}));
    }

    /// Pins the Phase 1 refactor: routing the three body-construction sites
    /// through `text_result` must be byte-identical to the inline `json!`
    /// literals it replaced. The references below are those literals,
    /// verbatim — if the envelope ever changes shape or key order, this
    /// fails rather than silently changing what every MCP client parses.
    /// A spill store in a throwaway directory, so the policy tests never touch
    /// the real one. `SPILL_STORE` is deliberately never set in tests — the
    /// store travels as an explicit argument to `text_result_with`.
    fn test_store(dir: &tempfile::TempDir) -> SpillStore {
        SpillStore::open(dir.path().to_path_buf(), "test-session").expect("spill store")
    }

    /// The one text extractor the tests share.
    fn body_text(result: &Value) -> &str {
        result["content"][0]["text"].as_str().unwrap()
    }

    #[test]
    fn text_result_envelope_is_unchanged() {
        // Success arm — `dispatch_tool_call` / `dispatch_subagent_call`.
        let before = json!({ "content": [{ "type": "text", "text": "hello" }] });
        let after = text_result(
            "wrapper_v0__do_thing",
            "application/json",
            "hello".to_string(),
        );
        assert_eq!(after, before);
        assert_eq!(
            serde_json::to_string(&after).unwrap(),
            serde_json::to_string(&before).unwrap()
        );

        // Error arm — `tool_error` adds `isError` to the same envelope.
        let before_err =
            json!({ "isError": true, "content": [{ "type": "text", "text": "boom" }] });
        let after_err = tool_error("wrapper_v0__do_thing", "boom".to_string());
        assert_eq!(after_err, before_err);
        assert_eq!(
            serde_json::to_string(&after_err).unwrap(),
            serde_json::to_string(&before_err).unwrap()
        );

        // The fields the instrumentation reads must be exactly where it
        // looks for them.
        assert_eq!(after["content"][0]["text"], json!("hello"));
        assert_eq!(after_err["isError"], json!(true));
        assert!(after.get("isError").is_none());
    }

    #[test]
    fn observe_tool_result_reads_text_length_in_place() {
        // Multi-byte content: the metric is bytes, not chars.
        let body = text_result("wrapper_v0__do_thing", "text/plain", "héllo".to_string());
        assert_eq!(body_text(&body).len(), 6);
        // Smoke: must not panic on a body missing the content array.
        observe_tool_result("wrapper_v0__create_component", &body);
        observe_tool_result("analyze_with_subagent", &json!({}));
        observe_tool_result("x", &tool_error("x", "boom".to_string()));
    }

    #[test]
    fn a_body_at_or_under_the_threshold_is_returned_whole() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir);
        let body = "x".repeat(1024);
        let result = text_result_with(
            "wrapper_v0__do_thing",
            "application/json",
            false,
            body.clone(),
            Some(&store),
            1024,
        );
        assert_eq!(body_text(&result), body);
        // Nothing was written: an under-threshold body must not touch disk.
        assert_eq!(std::fs::read_dir(store.session_dir()).unwrap().count(), 0);
    }

    #[test]
    fn an_oversized_body_is_replaced_by_a_preview_that_says_it_is_partial() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir);
        // Line-structured, like a source export: head and tail are both
        // recognisable, so the excerpt is worth showing.
        let body: String = (0..4000)
            .map(|i| format!("line {i} of the export\n"))
            .collect();
        let total = body.len();
        assert!(total > 32 * 1024);

        let result = text_result_with(
            "wrapper_v0__export_code",
            "application/json",
            false,
            body.clone(),
            Some(&store),
            32 * 1024,
        );
        let text = body_text(&result);

        // Labelled partial, not merely shorter.
        assert!(text.contains("PARTIAL RESULT"));
        assert!(text.contains(&format!("{total} bytes")), "true byte length");
        assert!(
            text.contains(READ_SPILL_TOOL_NAME),
            "carries the locator tool"
        );
        assert!(text.len() < total, "preview must be smaller than the body");
        // Head and tail are both present, and the omission is stated.
        assert!(text.contains("line 0 of the export"));
        assert!(text.contains("line 3999 of the export"));
        assert!(text.contains("bytes omitted"));

        // The locator resolves to the whole body, byte for byte.
        let id = spill_id_of(text);
        let slice = store.read(&id, 0, total as u64).unwrap();
        assert_eq!(slice.text.len(), total);
        assert_eq!(slice.record.byte_len, total as u64);
        assert_eq!(slice.record.tool, "wrapper_v0__export_code");
    }

    #[test]
    fn an_opaque_body_gets_no_excerpt_and_says_why() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir);
        // `wrapper_v0__download_component`: `{format, byteLength, base64}`
        // pretty-printed. The base64 value has no whitespace at all, so the
        // structural newlines must NOT make it look sliceable.
        let body = format!(
            "{{\n  \"format\": \"zip\",\n  \"byteLength\": 68000,\n  \"base64\": \"{}\"\n}}",
            "QUJDREVGR0hJSktMTU5PUFFSU1RVVldYWVo".repeat(2600)
        );
        assert!(body.len() > 32 * 1024);

        let result = text_result_with(
            "wrapper_v0__download_component",
            "application/json",
            false,
            body.clone(),
            Some(&store),
            32 * 1024,
        );
        let text = body_text(&result);

        assert!(text.contains("PARTIAL RESULT"));
        assert!(text.contains("no excerpt"));
        assert!(text.contains("no line structure"));
        assert!(!text.contains("bytes omitted"), "no head/tail was claimed");
        // The byte length and the locator still carry the whole answer.
        assert!(text.contains(&format!("{} bytes", body.len())));
        let id = spill_id_of(text);
        assert_eq!(store.read(&id, 0, body.len() as u64).unwrap().text, body);
    }

    #[test]
    fn a_spilled_error_stays_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir);
        // The shape `dispatch_tool_call` produces when the upstream response
        // carries no `error` string: the whole JSON body interpolated in.
        let msg = format!(
            "dispatch returned 500: {}",
            "{\"detail\": \"x\"}".repeat(4000)
        );
        assert!(msg.len() > 32 * 1024);

        let result = text_result_with(
            "wrapper_v0__do_thing",
            "text/plain",
            true,
            msg.clone(),
            Some(&store),
            32 * 1024,
        );
        // The flag rides on the envelope, so spilling cannot lose it.
        assert_eq!(result["isError"], json!(true));
        let text = body_text(&result);
        assert!(text.contains("PARTIAL RESULT"));
        let id = spill_id_of(text);
        let slice = store.read(&id, 0, msg.len() as u64).unwrap();
        assert!(slice.record.is_error);
        assert_eq!(slice.text, msg);
    }

    #[test]
    fn an_oversized_body_with_no_store_is_returned_whole_not_truncated() {
        let body = "z".repeat(100_000);
        let result = text_result_with(
            "wrapper_v0__do_thing",
            "application/json",
            false,
            body.clone(),
            None,
            32 * 1024,
        );
        assert_eq!(body_text(&result), body);
    }

    #[test]
    fn a_zero_threshold_disables_spilling() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir);
        let body = "z".repeat(100_000);
        let result = text_result_with(
            "wrapper_v0__do_thing",
            "application/json",
            false,
            body.clone(),
            Some(&store),
            0,
        );
        assert_eq!(body_text(&result), body);
        assert_eq!(std::fs::read_dir(store.session_dir()).unwrap().count(), 0);
    }

    #[test]
    fn read_spill_returns_ranges_and_chains_to_the_end() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir);
        let body: String = (0..2000).map(|i| format!("row {i}\n")).collect();
        let record = store
            .put("wrapper_v0__export_code", "application/json", false, &body)
            .unwrap();

        let mut seen = String::new();
        let mut offset = 0u64;
        loop {
            let result = dispatch_read_spill(
                Some(&store),
                json!({ "spill_id": record.id, "offset": offset, "length": 700 }),
            );
            assert!(result.get("isError").is_none());
            let text = body_text(&result);
            let (header, chunk) = text.split_once("----- body -----\n").unwrap();
            assert!(header.contains(&record.id));
            seen.push_str(chunk);
            if header.contains("reaches the END") {
                break;
            }
            let next: u64 = header
                .rsplit_once("\"offset\": ")
                .unwrap()
                .1
                .trim_end_matches("}.\n")
                .parse()
                .unwrap();
            assert!(next > offset, "retrieval must make progress");
            offset = next;
        }
        assert_eq!(seen, body);
    }

    #[test]
    fn read_spill_reports_a_bad_or_missing_locator_as_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir);

        for args in [
            json!({}),
            json!({ "spill_id": "" }),
            json!({ "spill_id": "../../etc/passwd" }),
            json!({ "spill_id": "0".repeat(32) }),
        ] {
            let result = dispatch_read_spill(Some(&store), args.clone());
            assert_eq!(result["isError"], json!(true), "args {args}");
        }
        // No store at all is an honest error, not a panic.
        let result = dispatch_read_spill(None, json!({ "spill_id": "0".repeat(32) }));
        assert_eq!(result["isError"], json!(true));
    }

    #[test]
    fn read_spill_length_is_clamped_so_a_retrieval_cannot_respill() {
        // The clamp is what lets `dispatch_read_spill` route its own result
        // through `text_result` without a special case.
        assert!(max_read_length() >= MIN_READ_LENGTH);
        let threshold = spill_threshold_bytes() as u64;
        if threshold > 0 {
            assert!(
                max_read_length() + READ_LENGTH_HEADROOM
                    <= threshold.max(READ_LENGTH_HEADROOM + MIN_READ_LENGTH)
            );
        }
    }

    #[test]
    fn opaque_detection_keys_on_whitespace_runs_not_newline_density() {
        // Pretty-printed JSON around one unbroken token: newline density says
        // "line-structured", the whitespace-run rule correctly says opaque.
        let base64ish = format!("{{\n  \"base64\": \"{}\"\n}}", "A".repeat(50_000));
        assert!(!preview_is_sliceable(&base64ish));

        // Ordinary source / logs / diffs are sliceable.
        assert!(preview_is_sliceable(
            &"fn main() { println!(\"hi\"); }\n".repeat(2000)
        ));
        assert!(preview_is_sliceable("short"));

        assert_eq!(longest_unbroken_run(""), 0);
        assert_eq!(longest_unbroken_run("ab cde\tf\ngh"), 3);
        assert_eq!(longest_unbroken_run("   "), 0);
    }

    #[test]
    fn preview_slicing_never_splits_a_character() {
        // Multi-byte characters straddling both preview boundaries. The lines
        // matter: without whitespace this would (correctly) be classified
        // opaque and never take the slicing path at all.
        let body = "日本語テキスト\n".repeat(10_000); // 22 bytes per line
        assert!(body.len() > 32 * 1024);
        assert!(!body.is_char_boundary(PREVIEW_HEAD_BYTES), "boundary case");
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir);
        let result = text_result_with(
            "wrapper_v0__do_thing",
            "text/plain",
            false,
            body,
            Some(&store),
            32 * 1024,
        );
        // Reaching this point at all means both slices were valid `str`s; the
        // assertion pins that the head really was cut short of the limit.
        let text = body_text(&result);
        assert!(text.contains("head: bytes 0-4095"));
        assert!(text.contains("日本語"));
    }

    #[test]
    fn char_boundary_helpers_round_the_right_way() {
        let s = "aé日"; // 1 + 2 + 3 bytes
        assert_eq!(floor_char_boundary(s, 0), 0);
        assert_eq!(floor_char_boundary(s, 2), 1);
        assert_eq!(floor_char_boundary(s, 3), 3);
        assert_eq!(floor_char_boundary(s, 99), s.len());
        assert_eq!(ceil_char_boundary(s, 2), 3);
        assert_eq!(ceil_char_boundary(s, 4), 6);
        assert_eq!(ceil_char_boundary(s, 99), s.len());
    }

    /// Pull the `spill_id:` line out of a preview — the same read a model does.
    fn spill_id_of(preview: &str) -> String {
        preview
            .lines()
            .find_map(|l| l.strip_prefix("spill_id: "))
            .expect("preview must carry a spill_id")
            .to_string()
    }

    #[test]
    fn rpc_error_envelope_shape() {
        let v = rpc_error(Some(json!(2)), -32601, "nope");
        assert_eq!(v["error"]["code"], -32601);
        assert_eq!(v["error"]["message"], "nope");
    }

    #[test]
    fn loopback_url_validation_accepts_loopback() {
        assert!(validate_loopback_http_url("http://127.0.0.1:9876"));
        assert!(validate_loopback_http_url("http://127.0.0.1:9876/"));
        assert!(validate_loopback_http_url("http://localhost:1"));
        assert!(validate_loopback_http_url("http://localhost:65535"));
    }

    #[test]
    fn loopback_url_validation_rejects_non_loopback() {
        assert!(!validate_loopback_http_url("http://192.168.1.5:9876"));
        assert!(!validate_loopback_http_url("http://example.com:9876"));
        assert!(!validate_loopback_http_url("https://127.0.0.1:9876"));
        assert!(!validate_loopback_http_url("http://127.0.0.1:9876/api"));
        assert!(!validate_loopback_http_url("http://user@127.0.0.1:9876"));
        assert!(!validate_loopback_http_url("http://127.0.0.1"));
        assert!(!validate_loopback_http_url("http://127.0.0.1:99999"));
        assert!(!validate_loopback_http_url(""));
    }

    #[test]
    fn refresh_preserves_cache_when_runner_is_down() {
        // Point at a port that nothing's listening on. fetch_tools logs to
        // stderr and returns empty; refresh_tools should keep the existing
        // cache rather than zap it.
        let mut tools = vec![ToolEntry {
            name: "wrapper_v0__do_thing".to_string(),
            wrapper_id: "v0".to_string(),
            action_id: "do-thing".to_string(),
            description: "x".to_string(),
            input_schema: json!({}),
        }];
        let mut reverse_map = build_reverse_map(&tools);

        refresh_tools("http://127.0.0.1:1", &mut tools, &mut reverse_map);

        assert_eq!(tools.len(), 1, "cache should be preserved on empty refresh");
        assert!(reverse_map.contains_key("wrapper_v0__do_thing"));
    }

    #[test]
    fn refresh_replaces_empty_cache_with_empty_when_runner_is_down() {
        // Empty -> empty is the only allowed transition through the
        // "preserve cache" guard, so the no-op should still leave both
        // structures empty (and not panic).
        let mut tools: Vec<ToolEntry> = Vec::new();
        let mut reverse_map: HashMap<String, (String, String)> = HashMap::new();

        refresh_tools("http://127.0.0.1:1", &mut tools, &mut reverse_map);

        assert!(tools.is_empty());
        assert!(reverse_map.is_empty());
    }

    #[test]
    fn is_sse_data_line_matches_only_data_prefix() {
        assert!(is_sse_data_line("data: {\"atMs\":1}"));
        assert!(is_sse_data_line("data:nospace"));
        assert!(!is_sse_data_line(":keep-alive"));
        assert!(!is_sse_data_line("event: wrapper.changed"));
        assert!(!is_sse_data_line("id: 42"));
        assert!(!is_sse_data_line(""));
        assert!(!is_sse_data_line("retry: 1000"));
    }

    #[test]
    fn next_backoff_doubles_then_caps() {
        assert_eq!(next_backoff(1), 2);
        assert_eq!(next_backoff(2), 4);
        assert_eq!(next_backoff(8), 16);
        assert_eq!(next_backoff(16), SSE_BACKOFF_MAX_SECS);
        assert_eq!(next_backoff(SSE_BACKOFF_MAX_SECS), SSE_BACKOFF_MAX_SECS);
        // Saturating: u64::MAX shouldn't panic.
        assert_eq!(next_backoff(u64::MAX), SSE_BACKOFF_MAX_SECS);
    }

    #[test]
    fn tool_cache_from_tools_builds_reverse_map() {
        let cache = ToolCache::from_tools(vec![ToolEntry {
            name: "wrapper_v0__do_thing".to_string(),
            wrapper_id: "v0".to_string(),
            action_id: "do-thing".to_string(),
            description: "x".to_string(),
            input_schema: json!({}),
        }]);
        assert_eq!(cache.tools.len(), 1);
        let (w, a) = cache.reverse_map.get("wrapper_v0__do_thing").unwrap();
        assert_eq!(w, "v0");
        assert_eq!(a, "do-thing");
    }

    #[test]
    fn tools_list_payload_serializes_tools() {
        let tools = vec![ToolEntry {
            name: "wrapper_v0__do_thing".to_string(),
            wrapper_id: "v0".to_string(),
            action_id: "do-thing".to_string(),
            description: "a thing".to_string(),
            input_schema: json!({"type": "object"}),
        }];
        let p = tools_list_payload(&tools);
        let arr = p["tools"].as_array().unwrap();
        // Dynamic wrapper tools first, then the static built-ins.
        assert_eq!(arr.len(), 3);
        assert_eq!(arr[0]["name"], "wrapper_v0__do_thing");
        assert_eq!(arr[0]["inputSchema"], json!({"type": "object"}));
        assert_eq!(arr[1]["name"], SUBAGENT_TOOL_NAME);
        assert_eq!(arr[2]["name"], READ_SPILL_TOOL_NAME);
    }

    #[test]
    fn read_spill_tool_entry_shape() {
        let entry = read_spill_tool_entry();
        assert_eq!(entry["name"], READ_SPILL_TOOL_NAME);
        assert_eq!(entry["inputSchema"]["required"], json!(["spill_id"]));
        for field in ["spill_id", "offset", "length"] {
            assert!(
                entry["inputSchema"]["properties"][field].is_object(),
                "missing {field}"
            );
        }
    }

    #[test]
    fn subagent_tool_entry_shape() {
        let entry = subagent_tool_entry();
        assert_eq!(entry["name"], "analyze_with_subagent");
        let required: Vec<&str> = entry["inputSchema"]["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(required, vec!["provider", "prompt"]);
        assert_eq!(
            entry["inputSchema"]["properties"]["provider"]["enum"],
            json!(["pi", "deepseek"])
        );
    }

    #[test]
    fn static_tools_are_listed_even_with_empty_wrapper_catalog() {
        let p = tools_list_payload(&[]);
        let arr = p["tools"].as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["name"], SUBAGENT_TOOL_NAME);
        // The retrieval tool must be visible even here: a spill locator issued
        // before the runner went down is unreachable without it.
        assert_eq!(arr[1]["name"], READ_SPILL_TOOL_NAME);
    }
}
