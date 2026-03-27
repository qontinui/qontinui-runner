//! API Surface Scanner — static analysis of the qontinui codebase to map every
//! endpoint, Tauri command, PgDb method, Clorinde query, Python event, and their
//! interconnections. Detects orphaned (uncalled) endpoints.

use axum::{extract::State, routing::post, Json, Router};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::info;

use crate::mcp::types::{ApiResponse, ApiState};

// ─── Types ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiSurface {
    pub tauri_commands: Vec<TauriCommand>,
    pub mcp_routes: Vec<McpRoute>,
    pub ui_bridge_routes: Vec<UiBridgeRoute>,
    pub python_events: Vec<PythonEvent>,
    pub pg_methods: Vec<PgMethod>,
    pub clorinde_queries: Vec<ClorindeQuery>,
    pub db_tables: Vec<DbTable>,
    pub connections: Vec<ApiConnection>,
    pub orphans: Vec<OrphanedEndpoint>,
    pub summary: ScanSummary,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TauriCommand {
    pub name: String,
    pub file: String,
    pub line: u32,
    pub parameters: Vec<String>,
    pub return_type: String,
    pub callers: Vec<Caller>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpRoute {
    pub method: String,
    pub path: String,
    pub handler: String,
    pub file: String,
    pub line: u32,
    pub callers: Vec<Caller>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiBridgeRoute {
    pub path: String,
    pub method: String,
    pub file: String,
    pub line: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PythonEvent {
    pub name: String,
    pub value: String,
    pub file: String,
    pub line: u32,
    pub intercepted_by: Vec<Caller>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PgMethod {
    pub name: String,
    pub file: String,
    pub line: u32,
    pub clorinde_query: Option<String>,
    pub callers: Vec<Caller>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClorindeQuery {
    pub name: String,
    pub file: String,
    pub line: u32,
    pub sql_preview: String,
    pub table: Option<String>,
    pub wrapper: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DbTable {
    pub name: String,
    pub file: String,
    pub line: u32,
    pub column_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiConnection {
    pub from_type: String,
    pub from_name: String,
    pub to_type: String,
    pub to_name: String,
    pub connection_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrphanedEndpoint {
    pub endpoint_type: String,
    pub name: String,
    pub file: String,
    pub line: u32,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Caller {
    pub file: String,
    pub line: u32,
    pub context: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanSummary {
    pub total_tauri_commands: usize,
    pub total_mcp_routes: usize,
    pub total_pg_methods: usize,
    pub total_clorinde_queries: usize,
    pub total_db_tables: usize,
    pub total_python_events: usize,
    pub total_connections: usize,
    pub total_orphans: usize,
    pub scan_duration_ms: u64,
}

// ─── Routes ──────────────────────────────────────────────────────────────────

pub fn routes() -> Router<Arc<ApiState>> {
    Router::new().route("/api-surface/scan", post(handle_scan))
}

async fn handle_scan(
    State(_state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<ApiSurface>>, (axum::http::StatusCode, Json<ApiResponse<()>>)> {
    let start = std::time::Instant::now();

    // Resolve the project root: walk up from current exe or CWD to find src-tauri/,
    // or fall back to the known dev path.
    let project_root = find_project_root()
        .unwrap_or_else(|| PathBuf::from("D:/qontinui-root/qontinui-runner"));

    let src_tauri = project_root.join("src-tauri");
    let src_frontend = project_root.join("src");
    let python_bridge = project_root.join("python-bridge");

    info!("API Surface scan starting — project_root={}", project_root.display());

    // Run the file-system scan on a blocking thread to avoid holding the async executor
    let surface = tokio::task::spawn_blocking(move || {
        scan_codebase(&project_root, &src_tauri, &src_frontend, &python_bridge, start)
    })
    .await
    .map_err(|e| {
        (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiResponse {
                success: false,
                data: None,
                error: Some(format!("Scan task failed: {}", e)),
                error_detail: None,
            }),
        )
    })?;

    Ok(Json(ApiResponse {
        success: true,
        data: Some(surface),
        error: None,
        error_detail: None,
    }))
}

// ─── Project root discovery ──────────────────────────────────────────────────

fn find_project_root() -> Option<PathBuf> {
    // Try walking up from the current exe location
    if let Ok(exe) = std::env::current_exe() {
        let mut cur = exe.clone();
        for _ in 0..8 {
            if !cur.pop() { break; }
            if cur.join("src-tauri").join("Cargo.toml").exists() && cur.join("src").exists() {
                return Some(cur);
            }
        }
    }
    // Try CWD
    if let Ok(cwd) = std::env::current_dir() {
        let mut cur = cwd;
        for _ in 0..6 {
            if cur.join("src-tauri").join("Cargo.toml").exists() && cur.join("src").exists() {
                return Some(cur);
            }
            if !cur.pop() { break; }
        }
    }
    // Fallback: check common dev locations
    for candidate in &[
        "D:/qontinui-root/qontinui-runner",
        "/d/qontinui-root/qontinui-runner",
    ] {
        let p = PathBuf::from(candidate);
        if p.join("src-tauri").join("Cargo.toml").exists() {
            return Some(p);
        }
    }
    None
}

// ─── Core scanner ────────────────────────────────────────────────────────────

fn scan_codebase(
    project_root: &Path,
    src_tauri: &Path,
    src_frontend: &Path,
    python_bridge: &Path,
    start: std::time::Instant,
) -> ApiSurface {
    // 1. Scan all source types
    let tauri_commands = scan_tauri_commands(src_tauri);
    let mcp_routes = scan_mcp_routes(src_tauri);
    let ui_bridge_routes = scan_ui_bridge_routes(project_root);
    let python_events = scan_python_events(python_bridge);
    let pg_methods = scan_pg_methods(src_tauri);
    let clorinde_queries = scan_clorinde_queries(src_tauri);
    let db_tables = scan_db_tables(src_tauri);

    // 2. Resolve connections
    let mut connections = Vec::new();
    let mut tauri_commands = tauri_commands;
    let mut mcp_routes = mcp_routes;
    let mut pg_methods = pg_methods;
    let mut clorinde_queries = clorinde_queries;
    let mut python_events = python_events;

    // Frontend → Tauri command callers
    resolve_tauri_callers(src_frontend, &mut tauri_commands);

    // Frontend → MCP route callers
    resolve_mcp_callers(src_frontend, &mut mcp_routes);

    // Tauri command → PgDb method calls
    resolve_command_to_pg(&mut connections, src_tauri, &tauri_commands, &mut pg_methods);

    // PgDb → Clorinde query calls
    resolve_pg_to_clorinde(&mut connections, &mut pg_methods, &mut clorinde_queries, src_tauri);

    // Python event → Rust intercepts
    resolve_event_intercepts(src_tauri, &mut python_events, &mut connections);

    // Frontend invoke → Tauri connections
    for cmd in &tauri_commands {
        for caller in &cmd.callers {
            connections.push(ApiConnection {
                from_type: "frontend".into(),
                from_name: caller.file.clone(),
                to_type: "tauri_command".into(),
                to_name: cmd.name.clone(),
                connection_type: "invokes".into(),
            });
        }
    }

    // Frontend fetch → MCP connections
    for route in &mcp_routes {
        for caller in &route.callers {
            connections.push(ApiConnection {
                from_type: "frontend".into(),
                from_name: caller.file.clone(),
                to_type: "mcp_route".into(),
                to_name: route.path.clone(),
                connection_type: "fetches".into(),
            });
        }
    }

    // 3. Detect orphans
    let orphans = detect_orphans(
        &tauri_commands,
        &mcp_routes,
        &pg_methods,
        &clorinde_queries,
        &python_events,
    );

    let summary = ScanSummary {
        total_tauri_commands: tauri_commands.len(),
        total_mcp_routes: mcp_routes.len(),
        total_pg_methods: pg_methods.len(),
        total_clorinde_queries: clorinde_queries.len(),
        total_db_tables: db_tables.len(),
        total_python_events: python_events.len(),
        total_connections: connections.len(),
        total_orphans: orphans.len(),
        scan_duration_ms: start.elapsed().as_millis() as u64,
    };

    info!(
        "API Surface scan complete: {} commands, {} routes, {} PgDb methods, {} queries, {} tables, {} events, {} connections, {} orphans in {}ms",
        summary.total_tauri_commands, summary.total_mcp_routes, summary.total_pg_methods,
        summary.total_clorinde_queries, summary.total_db_tables, summary.total_python_events,
        summary.total_connections, summary.total_orphans, summary.scan_duration_ms
    );

    ApiSurface {
        tauri_commands,
        mcp_routes,
        ui_bridge_routes,
        python_events,
        pg_methods,
        clorinde_queries,
        db_tables,
        connections,
        orphans,
        summary,
    }
}

// ─── Tauri command scanner ───────────────────────────────────────────────────

fn scan_tauri_commands(src_tauri: &Path) -> Vec<TauriCommand> {
    let mut commands = Vec::new();
    let commands_dir = src_tauri.join("src").join("commands");
    let src_dir = src_tauri.join("src");

    for dir in &[&commands_dir, &src_dir] {
        if !dir.exists() {
            continue;
        }
        walk_rs_files(dir, &mut |path, content| {
            // Skip commands dir when scanning src to avoid double-counting
            if **dir == src_dir && path.starts_with(&commands_dir) {
                return;
            }
            let lines: Vec<&str> = content.lines().collect();
            let rel = relative_path(src_tauri, path);

            for (i, line) in lines.iter().enumerate() {
                if line.contains("#[tauri::command]") {
                    // Next non-attribute, non-empty line should be the fn signature
                    if let Some(sig) = find_fn_signature(&lines, i + 1) {
                        let (name, params, ret) = parse_fn_signature(&sig);
                        commands.push(TauriCommand {
                            name,
                            file: rel.clone(),
                            line: (i + 1) as u32,
                            parameters: params,
                            return_type: ret,
                            callers: Vec::new(),
                        });
                    }
                }
            }
        });
    }

    commands
}

// ─── MCP route scanner ──────────────────────────────────────────────────────

fn scan_mcp_routes(src_tauri: &Path) -> Vec<McpRoute> {
    let mut routes = Vec::new();
    let mcp_dir = src_tauri.join("src").join("mcp");
    if !mcp_dir.exists() {
        return routes;
    }

    walk_rs_files(&mcp_dir, &mut |path, content| {
        let lines: Vec<&str> = content.lines().collect();
        let rel = relative_path(src_tauri, path);

        for (i, line) in lines.iter().enumerate() {
            // Match .route("/path", get/post/put/delete(handler))
            if let Some(caps) = parse_route_line(line) {
                routes.push(McpRoute {
                    method: caps.0,
                    path: caps.1,
                    handler: caps.2,
                    file: rel.clone(),
                    line: (i + 1) as u32,
                    callers: Vec::new(),
                });
            }
        }
    });

    routes
}

fn parse_route_line(line: &str) -> Option<(String, String, String)> {
    let trimmed = line.trim();
    if !trimmed.contains(".route(") {
        return None;
    }

    // Extract path: .route("/some/path", method(handler))
    let after_route = trimmed.split(".route(").nth(1)?;
    let path_start = after_route.find('"')? + 1;
    let path_end = after_route[path_start..].find('"')? + path_start;
    let path = after_route[path_start..path_end].to_string();

    // Extract method and handler
    let rest = &after_route[path_end + 1..];
    let method_handler = rest.trim().trim_start_matches(',').trim();

    let method = if method_handler.starts_with("get(") || method_handler.starts_with("get_service(") {
        "GET"
    } else if method_handler.starts_with("post(") {
        "POST"
    } else if method_handler.starts_with("put(") {
        "PUT"
    } else if method_handler.starts_with("delete(") {
        "DELETE"
    } else if method_handler.starts_with("patch(") {
        "PATCH"
    } else {
        return None;
    };

    let handler_start = method_handler.find('(')? + 1;
    let handler_end = method_handler[handler_start..].find(')').unwrap_or(method_handler.len() - handler_start) + handler_start;
    let handler = method_handler[handler_start..handler_end].trim().to_string();

    Some((method.to_string(), path, handler))
}

// ─── UI Bridge route scanner ────────────────────────────────────────────────

fn scan_ui_bridge_routes(project_root: &Path) -> Vec<UiBridgeRoute> {
    let mut routes = Vec::new();
    let types_file = project_root
        .join("ui-bridge-server")
        .join("src")
        .join("types.ts");

    if !types_file.exists() {
        return routes;
    }

    if let Ok(content) = std::fs::read_to_string(&types_file) {
        let lines: Vec<&str> = content.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            let trimmed = line.trim();
            // Look for route definitions like: "/some-path": { method: "POST", ... }
            if trimmed.starts_with('"') || trimmed.starts_with('\'') {
                if let Some(path) = extract_quoted_string(trimmed) {
                    if path.starts_with('/') {
                        let method = if trimmed.contains("POST") {
                            "POST"
                        } else if trimmed.contains("PUT") {
                            "PUT"
                        } else {
                            "GET"
                        };
                        routes.push(UiBridgeRoute {
                            path,
                            method: method.to_string(),
                            file: "ui-bridge-server/src/types.ts".to_string(),
                            line: (i + 1) as u32,
                        });
                    }
                }
            }
        }
    }

    routes
}

// ─── Python event scanner ───────────────────────────────────────────────────

fn scan_python_events(python_bridge: &Path) -> Vec<PythonEvent> {
    let mut events = Vec::new();
    let event_file = python_bridge.join("event_manager.py");

    if !event_file.exists() {
        return events;
    }

    if let Ok(content) = std::fs::read_to_string(&event_file) {
        let lines: Vec<&str> = content.lines().collect();
        let mut in_enum = false;

        for (i, line) in lines.iter().enumerate() {
            let trimmed = line.trim();
            if trimmed.starts_with("class EventType") {
                in_enum = true;
                continue;
            }
            if in_enum {
                // End of enum: non-empty, non-comment line that is not indented
                if !trimmed.is_empty()
                    && !line.starts_with(' ')
                    && !line.starts_with('\t')
                    && !trimmed.starts_with('#')
                    && !trimmed.starts_with("class EventType")
                {
                    in_enum = false;
                    continue;
                }
                // Check for ENUM_NAME = "value" pattern
                if let Some((name, value)) = parse_python_enum_entry(trimmed) {
                    events.push(PythonEvent {
                        name,
                        value,
                        file: "python-bridge/event_manager.py".to_string(),
                        line: (i + 1) as u32,
                        intercepted_by: Vec::new(),
                    });
                }
            }
        }
    }

    events
}

fn parse_python_enum_entry(line: &str) -> Option<(String, String)> {
    let parts: Vec<&str> = line.splitn(2, '=').collect();
    if parts.len() != 2 {
        return None;
    }
    let name = parts[0].trim().to_string();
    let value_raw = parts[1].trim();
    let value = extract_quoted_string(value_raw)?;
    Some((name, value))
}

// ─── PgDb method scanner ────────────────────────────────────────────────────

fn scan_pg_methods(src_tauri: &Path) -> Vec<PgMethod> {
    let mut methods = Vec::new();
    let pg_dir = src_tauri.join("src").join("database").join("pg");
    if !pg_dir.exists() {
        return methods;
    }

    walk_rs_files(&pg_dir, &mut |path, content| {
        let lines: Vec<&str> = content.lines().collect();
        let rel = relative_path(src_tauri, path);
        let mut in_impl = false;
        let mut brace_depth: i32 = 0;

        for (i, line) in lines.iter().enumerate() {
            let trimmed = line.trim();

            if trimmed.starts_with("impl PgDb") {
                in_impl = true;
                brace_depth = 0;
            }

            if in_impl {
                // Track brace depth to detect end of impl block
                for ch in trimmed.chars() {
                    if ch == '{' { brace_depth += 1; }
                    if ch == '}' { brace_depth -= 1; }
                }
                if brace_depth <= 0 && in_impl && !trimmed.starts_with("impl") {
                    in_impl = false;
                    continue;
                }
            }

            if in_impl && trimmed.starts_with("pub async fn ") {
                let fn_name = trimmed
                    .trim_start_matches("pub async fn ")
                    .split('(')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_string();
                if !fn_name.is_empty() {
                    methods.push(PgMethod {
                        name: fn_name,
                        file: rel.clone(),
                        line: (i + 1) as u32,
                        clorinde_query: None,
                        callers: Vec::new(),
                    });
                }
            }
        }
    });

    methods
}

// ─── Clorinde query scanner ─────────────────────────────────────────────────

fn scan_clorinde_queries(src_tauri: &Path) -> Vec<ClorindeQuery> {
    let mut queries = Vec::new();
    let queries_dir = src_tauri.join("queries");
    if !queries_dir.exists() {
        return queries;
    }

    if let Ok(entries) = std::fs::read_dir(&queries_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().map_or(true, |e| e != "sql") {
                continue;
            }
            if let Ok(content) = std::fs::read_to_string(&path) {
                let lines: Vec<&str> = content.lines().collect();
                let rel = format!("queries/{}", path.file_name().unwrap_or_default().to_string_lossy());

                for (i, line) in lines.iter().enumerate() {
                    let trimmed = line.trim();
                    if trimmed.starts_with("--!") {
                        let after = trimmed.trim_start_matches("--!").trim();
                        let fn_name = after.split_whitespace().next().unwrap_or("").to_string();
                        if fn_name.is_empty() {
                            continue;
                        }

                        // Collect SQL lines until next --! or end
                        let mut sql = String::new();
                        for j in (i + 1)..lines.len() {
                            if lines[j].trim().starts_with("--!") {
                                break;
                            }
                            if !lines[j].trim().starts_with("--") {
                                if !sql.is_empty() {
                                    sql.push(' ');
                                }
                                sql.push_str(lines[j].trim());
                            }
                        }
                        let sql_preview = if sql.len() > 200 {
                            format!("{}...", &sql[..200])
                        } else {
                            sql.clone()
                        };

                        // Extract table from SQL (FROM/INTO/UPDATE tablename)
                        let table = extract_table_from_sql(&sql);

                        queries.push(ClorindeQuery {
                            name: fn_name,
                            file: rel.clone(),
                            line: (i + 1) as u32,
                            sql_preview,
                            table,
                            wrapper: None,
                        });
                    }
                }
            }
        }
    }

    queries
}

fn extract_table_from_sql(sql: &str) -> Option<String> {
    let upper = sql.to_uppercase();
    for keyword in &["FROM ", "INTO ", "UPDATE ", "JOIN "] {
        if let Some(idx) = upper.find(keyword) {
            let rest = &sql[idx + keyword.len()..];
            let table = rest
                .trim()
                .split(|c: char| !c.is_alphanumeric() && c != '_')
                .next()
                .unwrap_or("")
                .to_string();
            if !table.is_empty()
                && !table.eq_ignore_ascii_case("SET")
                && !table.eq_ignore_ascii_case("VALUES")
                && !table.eq_ignore_ascii_case("SELECT")
            {
                return Some(table);
            }
        }
    }
    None
}

// ─── DB table scanner ───────────────────────────────────────────────────────

fn scan_db_tables(src_tauri: &Path) -> Vec<DbTable> {
    let mut tables = Vec::new();

    for schema_name in &["schema.pg.sql", "schema.sql"] {
        let schema_file = src_tauri.join(schema_name);
        if !schema_file.exists() {
            continue;
        }
        if let Ok(content) = std::fs::read_to_string(&schema_file) {
            let lines: Vec<&str> = content.lines().collect();
            for (i, line) in lines.iter().enumerate() {
                let upper = line.to_uppercase();
                if upper.contains("CREATE TABLE") {
                    let table_name = extract_create_table_name(line);
                    if let Some(name) = table_name {
                        // Count columns until closing paren
                        let mut col_count = 0u32;
                        for j in (i + 1)..lines.len() {
                            let col_line = lines[j].trim();
                            if col_line.starts_with(')') {
                                break;
                            }
                            if !col_line.is_empty()
                                && !col_line.starts_with("--")
                                && !col_line.to_uppercase().starts_with("CONSTRAINT")
                                && !col_line.to_uppercase().starts_with("PRIMARY KEY")
                                && !col_line.to_uppercase().starts_with("UNIQUE")
                                && !col_line.to_uppercase().starts_with("CHECK")
                                && !col_line.to_uppercase().starts_with("FOREIGN")
                            {
                                col_count += 1;
                            }
                        }
                        tables.push(DbTable {
                            name,
                            file: schema_name.to_string(),
                            line: (i + 1) as u32,
                            column_count: col_count,
                        });
                    }
                }
            }
        }
    }

    tables
}

fn extract_create_table_name(line: &str) -> Option<String> {
    let upper = line.to_uppercase();
    let idx = upper.find("CREATE TABLE")?;
    let rest = &line[idx + "CREATE TABLE".len()..];
    let rest = rest.trim();
    // Skip IF NOT EXISTS
    let rest = if rest.to_uppercase().starts_with("IF NOT EXISTS") {
        rest["IF NOT EXISTS".len()..].trim()
    } else {
        rest
    };
    let name = rest
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .next()
        .unwrap_or("")
        .to_string();
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

// ─── Connection resolvers ───────────────────────────────────────────────────

fn resolve_tauri_callers(src_frontend: &Path, commands: &mut [TauriCommand]) {
    if !src_frontend.exists() {
        return;
    }

    // Build a map of command names for quick lookup
    let command_names: Vec<String> = commands.iter().map(|c| c.name.clone()).collect();

    walk_ts_files(src_frontend, &mut |path, content| {
        let lines: Vec<&str> = content.lines().collect();
        let rel = relative_path(src_frontend.parent().unwrap_or(src_frontend), path);

        for (i, line) in lines.iter().enumerate() {
            // Look for invoke("command_name" or invoke('command_name'
            if line.contains("invoke(") || line.contains("invoke<") {
                for (cmd_idx, cmd_name) in command_names.iter().enumerate() {
                    if line.contains(&format!("\"{}\"", cmd_name))
                        || line.contains(&format!("'{}'", cmd_name))
                    {
                        commands[cmd_idx].callers.push(Caller {
                            file: rel.clone(),
                            line: (i + 1) as u32,
                            context: line.trim().chars().take(120).collect(),
                        });
                    }
                }
            }
        }
    });
}

fn resolve_mcp_callers(src_frontend: &Path, routes: &mut [McpRoute]) {
    if !src_frontend.exists() {
        return;
    }

    let route_paths: Vec<String> = routes.iter().map(|r| r.path.clone()).collect();

    walk_ts_files(src_frontend, &mut |path, content| {
        let lines: Vec<&str> = content.lines().collect();
        let rel = relative_path(src_frontend.parent().unwrap_or(src_frontend), path);

        for (i, line) in lines.iter().enumerate() {
            if line.contains("fetch(") || line.contains("getApiBase()") || line.contains("apiBase") {
                for (route_idx, route_path) in route_paths.iter().enumerate() {
                    if line.contains(route_path) {
                        routes[route_idx].callers.push(Caller {
                            file: rel.clone(),
                            line: (i + 1) as u32,
                            context: line.trim().chars().take(120).collect(),
                        });
                    }
                }
            }
        }
    });
}

fn resolve_command_to_pg(
    connections: &mut Vec<ApiConnection>,
    src_tauri: &Path,
    commands: &[TauriCommand],
    pg_methods: &mut [PgMethod],
) {
    let pg_method_names: Vec<String> = pg_methods.iter().map(|m| m.name.clone()).collect();

    // Scan all Rust source directories where Tauri commands may be implemented
    let src_dir = src_tauri.join("src");
    if !src_dir.exists() {
        return;
    }

    // Collect found connections first (borrow checker friendly)
    let mut found: Vec<(String, String, String, u32)> = Vec::new(); // (cmd_name, method_name, file, line)

    walk_rs_files(&src_dir, &mut |path, content| {
        let lines: Vec<&str> = content.lines().collect();
        let rel = relative_path(src_tauri, path);

        // Track which command we're in; reset at each new function definition
        let mut current_command: Option<String> = None;

        for (i, line) in lines.iter().enumerate() {
            let trimmed = line.trim();

            // Reset command context at any new function boundary
            if trimmed.starts_with("pub async fn ") || trimmed.starts_with("pub fn ") || trimmed.starts_with("async fn ") || trimmed.starts_with("fn ") {
                let fn_name = trimmed
                    .split("fn ")
                    .nth(1)
                    .unwrap_or("")
                    .split('(')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_string();
                if commands.iter().any(|c| c.name == fn_name) {
                    current_command = Some(fn_name);
                } else {
                    current_command = None;
                }
            }

            if let Some(ref cmd_name) = current_command {
                // Look for pg.method_name( or pg_db.method_name(
                for method_name in &pg_method_names {
                    if trimmed.contains(&format!(".{}(", method_name)) {
                        found.push((cmd_name.clone(), method_name.clone(), rel.clone(), (i + 1) as u32));
                    }
                }
            }
        }
    });

    // Apply found connections and populate PgMethod.callers
    for (cmd_name, method_name, file, line) in found {
        connections.push(ApiConnection {
            from_type: "tauri_command".into(),
            from_name: cmd_name.clone(),
            to_type: "pg_method".into(),
            to_name: method_name.clone(),
            connection_type: "calls".into(),
        });
        if let Some(method) = pg_methods.iter_mut().find(|m| m.name == method_name) {
            method.callers.push(Caller {
                file,
                line,
                context: format!("Called by command: {}", cmd_name),
            });
        }
    }
}

fn resolve_pg_to_clorinde(
    connections: &mut Vec<ApiConnection>,
    pg_methods: &mut [PgMethod],
    clorinde_queries: &mut [ClorindeQuery],
    src_tauri: &Path,
) {
    let pg_dir = src_tauri.join("src").join("database").join("pg");
    if !pg_dir.exists() {
        return;
    }

    // Build owned query name list for scanning (avoids borrowing clorinde_queries)
    let query_names: Vec<String> = clorinde_queries.iter().map(|q| q.name.clone()).collect();
    let method_names: Vec<String> = pg_methods.iter().map(|m| m.name.clone()).collect();

    // Collect found links: (pg_method_name, clorinde_query_name)
    let mut found: Vec<(String, String)> = Vec::new();

    walk_rs_files(&pg_dir, &mut |_path, content| {
        let lines: Vec<&str> = content.lines().collect();
        let mut current_method: Option<String> = None;

        for line in &lines {
            let trimmed = line.trim();

            if trimmed.starts_with("pub async fn ") {
                let fn_name = trimmed
                    .trim_start_matches("pub async fn ")
                    .split('(')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_string();
                if method_names.iter().any(|m| m == &fn_name) {
                    current_method = Some(fn_name);
                } else {
                    current_method = None;
                }
            }

            // Look for qontinui_db::queries::module::function_name()
            if trimmed.contains("qontinui_db::queries::") || trimmed.contains("queries::") {
                if let Some(ref method_name) = current_method {
                    for query_name in &query_names {
                        if trimmed.contains(&format!("{}(", query_name)) {
                            found.push((method_name.clone(), query_name.clone()));
                        }
                    }
                }
            }
        }
    });

    // Apply found links
    for (method_name, query_name) in found {
        if let Some(method) = pg_methods.iter_mut().find(|m| m.name == method_name) {
            method.clorinde_query = Some(query_name.clone());
        }
        if let Some(query) = clorinde_queries.iter_mut().find(|q| q.name == query_name) {
            query.wrapper = Some(method_name.clone());
        }
        connections.push(ApiConnection {
            from_type: "pg_method".into(),
            from_name: method_name,
            to_type: "clorinde_query".into(),
            to_name: query_name,
            connection_type: "calls".into(),
        });
    }
}

fn resolve_event_intercepts(
    src_tauri: &Path,
    events: &mut [PythonEvent],
    connections: &mut Vec<ApiConnection>,
) {
    let executor_dir = src_tauri.join("src").join("executor");
    let mcp_dir = src_tauri.join("src").join("mcp");

    for dir in &[&executor_dir, &mcp_dir] {
        if !dir.exists() {
            continue;
        }
        walk_rs_files(dir, &mut |path, content| {
            let lines: Vec<&str> = content.lines().collect();
            let rel = relative_path(src_tauri, path);

            for (i, line) in lines.iter().enumerate() {
                for event in events.iter_mut() {
                    if line.contains(&format!("\"{}\"", event.value)) {
                        event.intercepted_by.push(Caller {
                            file: rel.clone(),
                            line: (i + 1) as u32,
                            context: line.trim().chars().take(120).collect(),
                        });
                        connections.push(ApiConnection {
                            from_type: "python_event".into(),
                            from_name: event.value.clone(),
                            to_type: "rust_handler".into(),
                            to_name: rel.clone(),
                            connection_type: "intercepts".into(),
                        });
                    }
                }
            }
        });
    }
}

// ─── Orphan detection ────────────────────────────────────────────────────────

fn detect_orphans(
    tauri_commands: &[TauriCommand],
    mcp_routes: &[McpRoute],
    pg_methods: &[PgMethod],
    clorinde_queries: &[ClorindeQuery],
    python_events: &[PythonEvent],
) -> Vec<OrphanedEndpoint> {
    let mut orphans = Vec::new();

    for cmd in tauri_commands {
        if cmd.callers.is_empty() {
            orphans.push(OrphanedEndpoint {
                endpoint_type: "tauri_command".into(),
                name: cmd.name.clone(),
                file: cmd.file.clone(),
                line: cmd.line,
                reason: "No frontend callers found (no invoke() references)".into(),
            });
        }
    }

    for route in mcp_routes {
        if route.callers.is_empty() {
            orphans.push(OrphanedEndpoint {
                endpoint_type: "mcp_route".into(),
                name: format!("{} {}", route.method, route.path),
                file: route.file.clone(),
                line: route.line,
                reason: "No frontend callers found (no fetch() references)".into(),
            });
        }
    }

    for method in pg_methods {
        if method.callers.is_empty() {
            orphans.push(OrphanedEndpoint {
                endpoint_type: "pg_method".into(),
                name: method.name.clone(),
                file: method.file.clone(),
                line: method.line,
                reason: "No Tauri command or MCP handler calls this PgDb method".into(),
            });
        }
    }

    for query in clorinde_queries {
        if query.wrapper.is_none() {
            orphans.push(OrphanedEndpoint {
                endpoint_type: "clorinde_query".into(),
                name: query.name.clone(),
                file: query.file.clone(),
                line: query.line,
                reason: "No PgDb wrapper method found".into(),
            });
        }
    }

    for event in python_events {
        if event.intercepted_by.is_empty() {
            orphans.push(OrphanedEndpoint {
                endpoint_type: "python_event".into(),
                name: event.value.clone(),
                file: event.file.clone(),
                line: event.line,
                reason: "Not intercepted by any Rust handler".into(),
            });
        }
    }

    orphans
}

// ─── File walking utilities ─────────────────────────────────────────────────

fn walk_rs_files(dir: &Path, callback: &mut impl FnMut(&Path, &str)) {
    walk_files_with_ext(dir, "rs", callback);
}

fn walk_ts_files(dir: &Path, callback: &mut impl FnMut(&Path, &str)) {
    walk_files_with_ext(dir, "ts", callback);
    walk_files_with_ext(dir, "tsx", callback);
}

fn walk_files_with_ext(dir: &Path, ext: &str, callback: &mut impl FnMut(&Path, &str)) {
    let walker = walkdir::WalkDir::new(dir)
        .max_depth(10)
        .into_iter()
        .filter_entry(|e| {
            let name = e.file_name().to_string_lossy();
            // Skip node_modules, target, .git, dist
            !matches!(
                name.as_ref(),
                "node_modules" | "target" | ".git" | "dist" | "build" | ".next"
            )
        });

    for entry in walker.flatten() {
        let path = entry.path();
        if path.is_file() && path.extension().map_or(false, |e| e == ext) {
            if let Ok(content) = std::fs::read_to_string(path) {
                callback(path, &content);
            }
        }
    }
}

fn relative_path(base: &Path, path: &Path) -> String {
    path.strip_prefix(base)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

// ─── Signature parsing helpers ──────────────────────────────────────────────

fn find_fn_signature(lines: &[&str], start: usize) -> Option<String> {
    let mut sig = String::new();
    for i in start..lines.len() {
        let trimmed = lines[i].trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with("//") {
            continue;
        }
        sig.push_str(trimmed);
        sig.push(' ');
        if trimmed.contains('{') || trimmed.ends_with('{') {
            break;
        }
    }
    if sig.contains("fn ") {
        Some(sig)
    } else {
        None
    }
}

fn parse_fn_signature(sig: &str) -> (String, Vec<String>, String) {
    // Extract fn name
    let name = sig
        .split("fn ")
        .nth(1)
        .unwrap_or("")
        .split('(')
        .next()
        .unwrap_or("")
        .trim()
        .to_string();

    // Extract parameters
    let params_raw = sig
        .split('(')
        .nth(1)
        .unwrap_or("")
        .split(')')
        .next()
        .unwrap_or("");

    let params: Vec<String> = params_raw
        .split(',')
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect();

    // Extract return type
    let return_type = if let Some(idx) = sig.find("->") {
        sig[idx + 2..]
            .split('{')
            .next()
            .unwrap_or("")
            .trim()
            .to_string()
    } else {
        "()".to_string()
    };

    (name, params, return_type)
}

fn extract_quoted_string(s: &str) -> Option<String> {
    let quote = if s.contains('"') { '"' } else if s.contains('\'') { '\'' } else { return None };
    let start = s.find(quote)? + 1;
    let end = s[start..].find(quote)? + start;
    Some(s[start..end].to_string())
}
