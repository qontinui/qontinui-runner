//! Tauri command surface for subagent analysis (plan
//! 2026-07-15-runner-pi-deepseek-subagent-analysis, Phase 4).
//!
//! Thin wrapper: the real work lives in `crate::subagent::execute_analysis`,
//! which blocks (subprocess / blocking HTTP), so the command hops through
//! `spawn_blocking`.

use crate::subagent::{execute_analysis, SubagentAnalysisRequest};

/// Run an analysis on a subagent (pi CLI or DeepSeek) and return the
/// analysis text.
#[tauri::command]
pub async fn analyze_with_subagent(request: SubagentAnalysisRequest) -> Result<String, String> {
    tokio::task::spawn_blocking(move || execute_analysis(&request))
        .await
        .map_err(|e| format!("subagent task join error: {}", e))?
        .map_err(|e| e.to_string())
}
