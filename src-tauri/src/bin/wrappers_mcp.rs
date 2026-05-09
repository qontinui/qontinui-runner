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
//!
//! If a refresh returns empty while we previously had tools, the existing
//! cache is preserved — almost always a transient runner-down blip rather
//! than the user truly uninstalling every wrapper.

use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

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
    let arr: Vec<Value> = tools
        .iter()
        .map(|t| {
            json!({
                "name": t.name,
                "description": t.description,
                "inputSchema": t.input_schema,
            })
        })
        .collect();
    json!({ "tools": arr })
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
    let (wrapper_id, action_id) = match reverse_map.get(&params.name) {
        Some(pair) => pair.clone(),
        None => {
            return tool_error(format!(
                "unknown tool '{}' (no matching wrapper action installed)",
                params.name
            ));
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
        Err(e) => return tool_error(format!("http client init failed: {}", e)),
    };

    let resp = match client.post(&url).json(&body).send() {
        Ok(r) => r,
        Err(e) => return tool_error(format!("dispatch HTTP error: {}", e)),
    };

    let status = resp.status();
    let parsed: Value = match resp.json() {
        Ok(v) => v,
        Err(e) => return tool_error(format!("dispatch returned non-JSON: {}", e)),
    };

    if !status.is_success() {
        let msg = parsed
            .get("error")
            .and_then(|e| e.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| parsed.to_string());
        return tool_error(format!("dispatch {} returned {}: {}", url, status, msg));
    }

    // Successful runner envelope:
    //   { "success": true, "data": { "result": <wrapper-result> } }
    let result = parsed
        .get("data")
        .and_then(|d| d.get("result"))
        .cloned()
        .unwrap_or_else(|| parsed.get("data").cloned().unwrap_or(parsed.clone()));

    let text = match serde_json::to_string_pretty(&result) {
        Ok(s) => s,
        Err(e) => format!("<unable to stringify result: {}>", e),
    };
    json!({
        "content": [{ "type": "text", "text": text }],
    })
}

fn tool_error(msg: String) -> Value {
    eprintln!("[wrappers-mcp] tool error: {}", msg);
    json!({
        "isError": true,
        "content": [{ "type": "text", "text": msg }],
    })
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

fn handle_request(
    base: &str,
    tools: &mut Vec<ToolEntry>,
    reverse_map: &mut HashMap<String, (String, String)>,
    req: JsonRpcRequest,
) -> Option<Value> {
    let id = req.id.clone();
    match req.method.as_str() {
        "initialize" => Some(rpc_success(
            id,
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "serverInfo": { "name": SERVER_NAME, "version": SERVER_VERSION },
                "capabilities": { "tools": {} },
            }),
        )),
        "notifications/initialized" | "notifications/cancelled" => {
            // Notifications don't get a response.
            None
        }
        "tools/list" => {
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
            // If the client never re-listed, a wrapper installed since
            // startup is invisible to our cache. Refresh once before
            // surfacing "unknown tool" so the call still resolves.
            if !reverse_map.contains_key(&parsed.name) {
                refresh_tools(base, tools, reverse_map);
            }
            let result = dispatch_tool_call(base, reverse_map, parsed);
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

    let mut tools = fetch_tools(&base);
    let mut reverse_map = build_reverse_map(&tools);

    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut stdout = stdout.lock();

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
                write_response(&mut stdout, &err);
                continue;
            }
        };
        if req.method.is_empty() {
            let err = rpc_error(req.id, ERR_INVALID_REQUEST, "method is required");
            write_response(&mut stdout, &err);
            continue;
        }
        if let Some(response) = handle_request(&base, &mut tools, &mut reverse_map, req) {
            write_response(&mut stdout, &response);
        }
    }

    eprintln!("[wrappers-mcp] stdin closed, shutting down");
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
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["name"], "wrapper_v0__do_thing");
        assert_eq!(arr[0]["inputSchema"], json!({"type": "object"}));
    }
}
