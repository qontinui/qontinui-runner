//! Subagent analysis HTTP API (plan
//! 2026-07-15-runner-pi-deepseek-subagent-analysis, Phase 4).
//!
//! `POST /subagent/analyze` carries `subagent::execute_analysis` on the
//! runner's local HTTP surface. The `analyze_with_subagent` static MCP tool
//! in `bin/wrappers_mcp.rs` POSTs here — that pairing is what puts the
//! analysis into a Claude session as a native tool result (plan D2).

use axum::{response::Json, routing::post, Router};
use std::sync::Arc;
use tracing::info;

use crate::mcp::types::{ApiResponse, ApiState};
use crate::subagent::{execute_analysis, SubagentAnalysisRequest};
use qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked;

pub fn routes() -> Router<Arc<ApiState>> {
    Router::new().route("/subagent/analyze", post(analyze_handler))
}

/// POST /subagent/analyze — body is a `SubagentAnalysisRequest`; `data` in
/// the success envelope is the analysis text.
async fn analyze_handler(
    Json(request): Json<SubagentAnalysisRequest>,
) -> Json<ApiResponse<String>> {
    info!(
        "POST /subagent/analyze: provider={:?}, files={}",
        request.provider,
        request.file_refs.len()
    );
    match spawn_blocking_tracked(move || execute_analysis(&request)).await {
        Ok(Ok(text)) => Json(ApiResponse::success(text)),
        Ok(Err(e)) => Json(ApiResponse::error(e.to_string())),
        Err(e) => Json(ApiResponse::error(format!(
            "subagent task join error: {}",
            e
        ))),
    }
}
