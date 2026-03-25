//! HTTP API endpoints for the knowledge acquisition system.
//!
//! Exposes search, vulnerability lookup, and dependency audit as MCP-compatible
//! HTTP endpoints that can be called by Claude sessions or external tools.

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tracing::{error, info, warn};

use crate::knowledge_acquisition::providers::osv::OsvEcosystem;
use crate::knowledge_acquisition::providers::ui_bridge_fetch::SnapshotType;
use crate::knowledge_acquisition::{
    KnowledgeAcquisition, KnowledgeDomain, KnowledgeResult, SearchProvider,
};
use crate::mcp::types::{ApiResponse, ApiState};

/// Create routes for the knowledge acquisition API.
pub fn routes() -> Router<Arc<ApiState>> {
    Router::new()
        .route("/knowledge/providers", get(list_providers))
        .route("/knowledge/search-web", post(search_web))
        .route("/knowledge/research-error", post(research_error))
        .route("/knowledge/fetch-page", post(fetch_page))
        .route("/knowledge/lookup-api-docs", post(lookup_api_docs))
        .route(
            "/knowledge/search-vulnerabilities",
            post(search_vulnerabilities),
        )
        .route("/knowledge/audit-dependencies", post(audit_dependencies))
        .route(
            "/knowledge/audit-manifest",
            post(audit_manifest),
        )
}

// ============================================================================
// Request/Response types
// ============================================================================

#[derive(Debug, Serialize)]
struct ProviderInfo {
    name: String,
    available: bool,
    requires_api_key: bool,
}

#[derive(Debug, Deserialize)]
struct SearchWebRequest {
    query: String,
    #[serde(default = "default_max_results")]
    max_results: usize,
}

#[derive(Debug, Deserialize)]
struct ResearchErrorRequest {
    error_message: String,
    #[serde(default)]
    framework: Option<String>,
    #[serde(default)]
    stack_trace: Option<String>,
}

#[derive(Debug, Deserialize)]
struct FetchPageRequest {
    #[serde(default)]
    query: Option<String>,
    #[serde(default = "default_snapshot_type")]
    snapshot_type: String,
}

#[derive(Debug, Deserialize)]
struct LookupApiDocsRequest {
    query: String,
    #[serde(default = "default_max_results")]
    max_results: usize,
}

#[derive(Debug, Deserialize)]
struct SearchVulnerabilitiesRequest {
    query: String,
}

#[derive(Debug, Deserialize)]
struct AuditDependenciesRequest {
    packages: Vec<PackageInput>,
}

#[derive(Debug, Deserialize)]
struct AuditManifestRequest {
    manifest_path: String,
}

#[derive(Debug, Deserialize)]
struct PackageInput {
    name: String,
    ecosystem: String,
    version: String,
}

#[derive(Debug, Serialize)]
struct SearchResponse {
    results: Vec<KnowledgeResult>,
    provider_used: Vec<String>,
}

#[derive(Debug, Serialize)]
struct VulnAuditResponse {
    packages: Vec<PackageVulns>,
    total_vulns: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    skipped_ecosystems: Vec<String>,
}

#[derive(Debug, Serialize)]
struct PackageVulns {
    package_name: String,
    vulns: Vec<KnowledgeResult>,
}

fn default_max_results() -> usize {
    5
}

fn default_snapshot_type() -> String {
    "summary".to_string()
}

// ============================================================================
// Handlers
// ============================================================================

/// List available search providers and their status
async fn list_providers(
    State(_state): State<Arc<ApiState>>,
) -> (StatusCode, Json<ApiResponse<Vec<ProviderInfo>>>) {
    let ka = KnowledgeAcquisition::new();
    let available = ka.available_providers();

    let providers = [
        SearchProvider::DuckDuckGo,
        SearchProvider::Tavily,
        SearchProvider::UiBridge,
        SearchProvider::Sploitus,
        SearchProvider::OsvDev,
        SearchProvider::Perplexity,
    ]
    .iter()
    .map(|p| ProviderInfo {
        name: p.as_str().to_string(),
        available: available.contains(p),
        requires_api_key: p.requires_api_key(),
    })
    .collect();

    (StatusCode::OK, Json(ApiResponse::success(providers)))
}

/// Web search (DuckDuckGo / Tavily)
async fn search_web(
    State(_state): State<Arc<ApiState>>,
    Json(req): Json<SearchWebRequest>,
) -> (StatusCode, Json<ApiResponse<SearchResponse>>) {
    info!("[knowledge_acquisition] Web search: {}", req.query);

    let ka = KnowledgeAcquisition::new();
    match ka
        .search(&req.query, KnowledgeDomain::General, req.max_results)
        .await
    {
        Ok(results) => ok_search_response(results),
        Err(e) => {
            error!("[knowledge_acquisition] Web search failed: {}", e);
            err_response(e)
        }
    }
}

/// Research an error message — compound search with escalation
async fn research_error(
    State(_state): State<Arc<ApiState>>,
    Json(req): Json<ResearchErrorRequest>,
) -> (StatusCode, Json<ApiResponse<SearchResponse>>) {
    let mut query = req.error_message.clone();
    if let Some(ref framework) = req.framework {
        query = format!("{framework} {query}");
    }
    if let Some(ref trace) = req.stack_trace {
        if let Some(line) = trace.lines().find(|l| {
            let trimmed = l.trim();
            !trimmed.is_empty()
                && !trimmed.starts_with("at ")
                && !trimmed.contains("node_modules")
        }) {
            query = format!("{query} {}", line.trim());
        }
    }

    info!(
        "[knowledge_acquisition] Research error: {}",
        query.chars().take(100).collect::<String>()
    );

    let ka = KnowledgeAcquisition::new();
    match ka
        .search(&query, KnowledgeDomain::ErrorResolution, 5)
        .await
    {
        Ok(results) => ok_search_response(results),
        Err(e) => {
            error!("[knowledge_acquisition] Error research failed: {}", e);
            err_response(e)
        }
    }
}

/// Fetch content from a UI Bridge-connected application
async fn fetch_page(
    State(_state): State<Arc<ApiState>>,
    Json(req): Json<FetchPageRequest>,
) -> (StatusCode, Json<ApiResponse<SearchResponse>>) {
    let snapshot_type = match req.snapshot_type.as_str() {
        "analyze" => SnapshotType::Analyze,
        "full" => SnapshotType::Full,
        _ => SnapshotType::Summary,
    };

    info!(
        "[knowledge_acquisition] Fetch page: type={}, query={:?}",
        req.snapshot_type, req.query
    );

    let ka = KnowledgeAcquisition::new();
    match ka
        .fetch_ui_bridge(snapshot_type, req.query.as_deref())
        .await
    {
        Ok(results) => ok_search_response(results),
        Err(e) => {
            error!("[knowledge_acquisition] Fetch page failed: {}", e);
            err_response(e)
        }
    }
}

/// Deep API documentation search (Tavily advanced, falls back to DuckDuckGo)
async fn lookup_api_docs(
    State(_state): State<Arc<ApiState>>,
    Json(req): Json<LookupApiDocsRequest>,
) -> (StatusCode, Json<ApiResponse<SearchResponse>>) {
    info!("[knowledge_acquisition] API docs lookup: {}", req.query);

    let ka = KnowledgeAcquisition::new();
    match ka.lookup_api_docs(&req.query, req.max_results).await {
        Ok(results) => ok_search_response(results),
        Err(e) => {
            error!("[knowledge_acquisition] API docs lookup failed: {}", e);
            err_response(e)
        }
    }
}

/// Search for vulnerabilities (Sploitus/OSV)
async fn search_vulnerabilities(
    State(_state): State<Arc<ApiState>>,
    Json(req): Json<SearchVulnerabilitiesRequest>,
) -> (StatusCode, Json<ApiResponse<SearchResponse>>) {
    info!(
        "[knowledge_acquisition] Vulnerability search: {}",
        req.query
    );

    let ka = KnowledgeAcquisition::new();
    match ka.search_vulnerabilities(&req.query).await {
        Ok(results) => ok_search_response(results),
        Err(e) => {
            error!("[knowledge_acquisition] Vuln search failed: {}", e);
            err_response(e)
        }
    }
}

/// Batch dependency audit via OSV.dev (pre-parsed packages)
async fn audit_dependencies(
    State(_state): State<Arc<ApiState>>,
    Json(req): Json<AuditDependenciesRequest>,
) -> (StatusCode, Json<ApiResponse<VulnAuditResponse>>) {
    info!(
        "[knowledge_acquisition] Dependency audit: {} packages",
        req.packages.len()
    );

    let ka = KnowledgeAcquisition::new();
    let (packages, skipped) = parse_package_inputs(req.packages);

    match ka.audit_dependencies(&packages).await {
        Ok(results) => ok_audit_response(results, skipped),
        Err(e) => {
            error!("[knowledge_acquisition] Dependency audit failed: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::error(e)),
            )
        }
    }
}

/// Audit dependencies by parsing a manifest file (package.json, Cargo.toml, pyproject.toml)
async fn audit_manifest(
    State(_state): State<Arc<ApiState>>,
    Json(req): Json<AuditManifestRequest>,
) -> (StatusCode, Json<ApiResponse<VulnAuditResponse>>) {
    info!(
        "[knowledge_acquisition] Manifest audit: {}",
        req.manifest_path
    );

    let packages = match parse_manifest(&req.manifest_path) {
        Ok(pkgs) => pkgs,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiResponse::error(format!(
                    "Failed to parse manifest: {e}"
                ))),
            );
        }
    };

    if packages.is_empty() {
        return (
            StatusCode::OK,
            Json(ApiResponse::success(VulnAuditResponse {
                packages: vec![],
                total_vulns: 0,
                skipped_ecosystems: vec![],
            })),
        );
    }

    info!(
        "[knowledge_acquisition] Parsed {} packages from manifest",
        packages.len()
    );

    let ka = KnowledgeAcquisition::new();
    match ka.audit_dependencies(&packages).await {
        Ok(results) => ok_audit_response(results, vec![]),
        Err(e) => {
            error!("[knowledge_acquisition] Manifest audit failed: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::error(e)),
            )
        }
    }
}

// ============================================================================
// Helpers
// ============================================================================

fn ok_search_response(
    results: Vec<KnowledgeResult>,
) -> (StatusCode, Json<ApiResponse<SearchResponse>>) {
    let providers_used: Vec<String> = results
        .iter()
        .map(|r| r.provider.as_str().to_string())
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();

    (
        StatusCode::OK,
        Json(ApiResponse::success(SearchResponse {
            results,
            provider_used: providers_used,
        })),
    )
}

fn err_response(e: String) -> (StatusCode, Json<ApiResponse<SearchResponse>>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ApiResponse::error(e)),
    )
}

fn ok_audit_response(
    results: Vec<(String, Vec<KnowledgeResult>)>,
    skipped: Vec<String>,
) -> (StatusCode, Json<ApiResponse<VulnAuditResponse>>) {
    let total_vulns: usize = results.iter().map(|(_, v)| v.len()).sum();
    let packages: Vec<PackageVulns> = results
        .into_iter()
        .map(|(name, vulns)| PackageVulns {
            package_name: name,
            vulns,
        })
        .collect();

    (
        StatusCode::OK,
        Json(ApiResponse::success(VulnAuditResponse {
            packages,
            total_vulns,
            skipped_ecosystems: skipped,
        })),
    )
}

/// Convert PackageInput list to typed tuples, tracking skipped ecosystems
fn parse_package_inputs(
    inputs: Vec<PackageInput>,
) -> (Vec<(String, OsvEcosystem, String)>, Vec<String>) {
    let mut packages = Vec::new();
    let mut skipped = Vec::new();

    for p in inputs {
        match p.ecosystem.to_lowercase().as_str() {
            "npm" => packages.push((p.name, OsvEcosystem::Npm, p.version)),
            "pypi" | "pip" => packages.push((p.name, OsvEcosystem::PyPI, p.version)),
            "crates.io" | "cargo" | "rust" => {
                packages.push((p.name, OsvEcosystem::CratesIo, p.version))
            }
            "go" | "golang" => packages.push((p.name, OsvEcosystem::Go, p.version)),
            "maven" | "java" => packages.push((p.name, OsvEcosystem::Maven, p.version)),
            "nuget" | "dotnet" => packages.push((p.name, OsvEcosystem::NuGet, p.version)),
            other => {
                warn!(
                    "[knowledge_acquisition] Skipping package '{}': unsupported ecosystem '{}'",
                    p.name, other
                );
                skipped.push(format!("{}@{} ({})", p.name, p.version, other));
            }
        }
    }

    (packages, skipped)
}

/// Parse a manifest file and extract (name, ecosystem, version) tuples
fn parse_manifest(path: &str) -> Result<Vec<(String, OsvEcosystem, String)>, String> {
    let content =
        std::fs::read_to_string(path).map_err(|e| format!("Cannot read {path}: {e}"))?;
    let filename = std::path::Path::new(path)
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or("");

    match filename {
        "package.json" => parse_package_json(&content),
        "Cargo.toml" => parse_cargo_toml(&content),
        "pyproject.toml" => parse_pyproject_toml(&content),
        _ => Err(format!(
            "Unsupported manifest type: {filename}. Supported: package.json, Cargo.toml, pyproject.toml"
        )),
    }
}

/// Parse package.json dependencies
fn parse_package_json(content: &str) -> Result<Vec<(String, OsvEcosystem, String)>, String> {
    let json: serde_json::Value =
        serde_json::from_str(content).map_err(|e| format!("Invalid JSON: {e}"))?;

    let mut packages = Vec::new();

    for key in ["dependencies", "devDependencies"] {
        if let Some(deps) = json.get(key).and_then(|v| v.as_object()) {
            for (name, version) in deps {
                if let Some(ver) = version.as_str() {
                    // Strip semver prefixes (^, ~, >=, etc.)
                    let clean = ver.trim_start_matches(|c: char| !c.is_ascii_digit());
                    if !clean.is_empty() {
                        packages.push((name.clone(), OsvEcosystem::Npm, clean.to_string()));
                    }
                }
            }
        }
    }

    Ok(packages)
}

/// Parse Cargo.toml dependencies
fn parse_cargo_toml(content: &str) -> Result<Vec<(String, OsvEcosystem, String)>, String> {
    let toml: toml::Value =
        toml::from_str(content).map_err(|e| format!("Invalid TOML: {e}"))?;

    let mut packages = Vec::new();

    for key in ["dependencies", "dev-dependencies", "build-dependencies"] {
        if let Some(deps) = toml.get(key).and_then(|v| v.as_table()) {
            for (name, value) in deps {
                let version = match value {
                    toml::Value::String(v) => Some(v.clone()),
                    toml::Value::Table(t) => {
                        t.get("version").and_then(|v| v.as_str()).map(|s| s.to_string())
                    }
                    _ => None,
                };
                if let Some(ver) = version {
                    let clean = ver.trim_start_matches(|c: char| !c.is_ascii_digit());
                    if !clean.is_empty() {
                        packages.push((name.clone(), OsvEcosystem::CratesIo, clean.to_string()));
                    }
                }
            }
        }
    }

    Ok(packages)
}

/// Parse pyproject.toml dependencies
fn parse_pyproject_toml(content: &str) -> Result<Vec<(String, OsvEcosystem, String)>, String> {
    let toml: toml::Value =
        toml::from_str(content).map_err(|e| format!("Invalid TOML: {e}"))?;

    let mut packages = Vec::new();

    // PEP 621: [project.dependencies]
    if let Some(deps) = toml
        .get("project")
        .and_then(|p| p.get("dependencies"))
        .and_then(|d| d.as_array())
    {
        for dep in deps {
            if let Some(spec) = dep.as_str() {
                if let Some((name, version)) = parse_pep508_spec(spec) {
                    packages.push((name, OsvEcosystem::PyPI, version));
                }
            }
        }
    }

    // Poetry: [tool.poetry.dependencies]
    if let Some(deps) = toml
        .get("tool")
        .and_then(|t| t.get("poetry"))
        .and_then(|p| p.get("dependencies"))
        .and_then(|d| d.as_table())
    {
        for (name, value) in deps {
            if name == "python" {
                continue;
            }
            let version = match value {
                toml::Value::String(v) => Some(v.clone()),
                toml::Value::Table(t) => {
                    t.get("version").and_then(|v| v.as_str()).map(|s| s.to_string())
                }
                _ => None,
            };
            if let Some(ver) = version {
                let clean = ver
                    .trim_start_matches('^')
                    .trim_start_matches('~')
                    .trim_start_matches(">=");
                if !clean.is_empty() {
                    packages.push((name.clone(), OsvEcosystem::PyPI, clean.to_string()));
                }
            }
        }
    }

    Ok(packages)
}

/// Parse a PEP 508 dependency spec like "requests>=2.28.0" into (name, version)
fn parse_pep508_spec(spec: &str) -> Option<(String, String)> {
    // Split on first version operator
    let operators = [">=", "<=", "==", "~=", "!=", ">", "<"];
    for op in operators {
        if let Some(pos) = spec.find(op) {
            let name = spec[..pos].trim().to_string();
            let version = spec[pos + op.len()..].trim().to_string();
            // Take only the first version in a range (e.g., ">=2.28,<3" -> "2.28")
            let version = version.split(',').next().unwrap_or("").trim().to_string();
            if !name.is_empty() && !version.is_empty() {
                return Some((name, version));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_package_json() {
        let json = r#"{
            "dependencies": {
                "express": "^4.18.2",
                "lodash": "~4.17.21"
            },
            "devDependencies": {
                "jest": "^29.0.0"
            }
        }"#;
        let pkgs = parse_package_json(json).unwrap();
        assert_eq!(pkgs.len(), 3);
        assert!(pkgs.iter().any(|(n, _, v)| n == "express" && v == "4.18.2"));
        assert!(pkgs.iter().any(|(n, _, v)| n == "lodash" && v == "4.17.21"));
        assert!(pkgs.iter().any(|(n, _, v)| n == "jest" && v == "29.0.0"));
    }

    #[test]
    fn test_parse_cargo_toml() {
        let toml = r#"
[dependencies]
serde = "1.0"
reqwest = { version = "0.13", features = ["json"] }

[dev-dependencies]
tokio = { version = "1.0", features = ["full"] }
"#;
        let pkgs = parse_cargo_toml(toml).unwrap();
        assert_eq!(pkgs.len(), 3);
        assert!(pkgs.iter().any(|(n, _, v)| n == "serde" && v == "1.0"));
        assert!(pkgs.iter().any(|(n, _, v)| n == "reqwest" && v == "0.13"));
    }

    #[test]
    fn test_parse_pyproject_toml_pep621() {
        let toml = r#"
[project]
dependencies = [
    "requests>=2.28.0",
    "click==8.1.7",
]
"#;
        let pkgs = parse_pyproject_toml(toml).unwrap();
        assert_eq!(pkgs.len(), 2);
        assert!(pkgs.iter().any(|(n, _, v)| n == "requests" && v == "2.28.0"));
        assert!(pkgs.iter().any(|(n, _, v)| n == "click" && v == "8.1.7"));
    }

    #[test]
    fn test_parse_pyproject_toml_poetry() {
        let toml = r#"
[tool.poetry.dependencies]
python = "^3.11"
fastapi = "^0.100.0"
"#;
        let pkgs = parse_pyproject_toml(toml).unwrap();
        assert_eq!(pkgs.len(), 1); // python excluded
        assert!(pkgs.iter().any(|(n, _, v)| n == "fastapi" && v == "0.100.0"));
    }

    #[test]
    fn test_parse_pep508_spec() {
        assert_eq!(
            parse_pep508_spec("requests>=2.28.0"),
            Some(("requests".to_string(), "2.28.0".to_string()))
        );
        assert_eq!(
            parse_pep508_spec("click==8.1.7"),
            Some(("click".to_string(), "8.1.7".to_string()))
        );
        assert_eq!(
            parse_pep508_spec("numpy>=1.24,<2"),
            Some(("numpy".to_string(), "1.24".to_string()))
        );
        assert_eq!(parse_pep508_spec("bare-package"), None);
    }

    #[test]
    fn test_parse_package_inputs_with_skipped() {
        let inputs = vec![
            PackageInput {
                name: "lodash".to_string(),
                ecosystem: "npm".to_string(),
                version: "4.17.21".to_string(),
            },
            PackageInput {
                name: "unknown-pkg".to_string(),
                ecosystem: "rubygems".to_string(),
                version: "1.0".to_string(),
            },
        ];
        let (pkgs, skipped) = parse_package_inputs(inputs);
        assert_eq!(pkgs.len(), 1);
        assert_eq!(skipped.len(), 1);
        assert!(skipped[0].contains("rubygems"));
    }
}
