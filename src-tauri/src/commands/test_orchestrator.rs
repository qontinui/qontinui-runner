//! Test Orchestrator Commands
//!
//! Tauri commands for AI-driven multi-step API test orchestration.

use crate::commands::{AppState, CommandResponse};
use crate::test_orchestrator::{
    create_orchestration_plan, generate_test_from_execution, GenerateTestRequest,
    OrchestrationExecutionResult, TestOrchestrationPlan, TestOrchestrationRequest,
    TestOrchestrator,
};
use std::sync::Arc;
use tauri::State;
use tracing::{error, info};

/// Create an orchestration plan from selected API requests
///
/// Uses AI to analyze the selected requests and create an execution plan
/// with variable chaining between steps.
#[tauri::command]
pub async fn plan_test_orchestration(
    request: TestOrchestrationRequest,
    state: State<'_, Arc<AppState>>,
) -> Result<CommandResponse, String> {
    info!(
        "Planning test orchestration with {} requests",
        request.request_ids.len()
    );

    // Get saved API requests from database
    let saved_requests_json = state
        .pg_db
        .list_saved_api_requests()
        .await
        .map_err(|e| format!("Failed to list saved requests: {}", e))?;
    let saved_requests: Vec<crate::saved_api_requests::SavedApiRequest> = saved_requests_json
        .into_iter()
        .filter_map(|v| serde_json::from_value(v).ok())
        .collect();

    // Extract DoctorHandle for AI health monitoring
    let doctor_handle = state.doctor_handle.lock().await.clone();

    // Create the plan using AI
    match create_orchestration_plan(&request, &saved_requests, doctor_handle.as_ref()) {
        Ok(plan) => Ok(CommandResponse {
            success: true,
            message: Some(format!(
                "Created orchestration plan with {} steps",
                plan.steps.len()
            )),
            data: Some(serde_json::to_value(&plan).unwrap_or_default()),
        }),
        Err(e) => {
            error!("Failed to create orchestration plan: {}", e);
            Ok(CommandResponse {
                success: false,
                message: Some(format!("Failed to create plan: {}", e)),
                data: None,
            })
        }
    }
}

/// Execute an orchestration plan
///
/// Runs the HTTP requests in order, chaining variables between steps.
#[tauri::command]
pub async fn execute_test_orchestration(
    plan: TestOrchestrationPlan,
    state: State<'_, Arc<AppState>>,
) -> Result<CommandResponse, String> {
    info!("Executing orchestration plan: {}", plan.id);

    // Get saved API requests from database
    let saved_requests_json = state
        .pg_db
        .list_saved_api_requests()
        .await
        .map_err(|e| format!("Failed to list saved requests: {}", e))?;
    let saved_requests: Vec<crate::saved_api_requests::SavedApiRequest> = saved_requests_json
        .into_iter()
        .filter_map(|v| serde_json::from_value(v).ok())
        .collect();

    // Create orchestrator and execute
    let mut orchestrator = TestOrchestrator::new();

    match orchestrator.execute_plan(&plan, &saved_requests).await {
        Ok(result) => {
            let success_msg = if result.success {
                format!(
                    "Executed {} steps successfully in {}ms",
                    result.step_results.len(),
                    result.total_duration_ms
                )
            } else {
                format!(
                    "Execution failed at step {} after {}ms",
                    result.failed_at_step.unwrap_or(0),
                    result.total_duration_ms
                )
            };

            Ok(CommandResponse {
                success: result.success,
                message: Some(success_msg),
                data: Some(serde_json::to_value(&result).unwrap_or_default()),
            })
        }
        Err(e) => {
            error!("Failed to execute orchestration plan: {}", e);
            Ok(CommandResponse {
                success: false,
                message: Some(format!("Execution failed: {}", e)),
                data: None,
            })
        }
    }
}

/// Generate a verification test from orchestration execution results
///
/// Uses AI to generate test code that verifies the expected behavior
/// based on the executed requests and responses.
#[tauri::command]
pub async fn generate_test_from_orchestration(
    execution_result: OrchestrationExecutionResult,
    plan: TestOrchestrationPlan,
    test_type: String,
    additional_instructions: Option<String>,
    state: State<'_, Arc<AppState>>,
) -> Result<CommandResponse, String> {
    info!("Generating {} test from orchestration execution", test_type);

    let doctor_handle = state.doctor_handle.lock().await.clone();

    let request = GenerateTestRequest {
        execution_result,
        plan,
        test_type: test_type.clone(),
        additional_instructions,
    };

    match generate_test_from_execution(&request, doctor_handle.as_ref()) {
        Ok(test) => Ok(CommandResponse {
            success: true,
            message: Some(format!("Generated {} test: {}", test_type, test.name)),
            data: Some(serde_json::to_value(&test).unwrap_or_default()),
        }),
        Err(e) => {
            error!("Failed to generate test: {}", e);
            Ok(CommandResponse {
                success: false,
                message: Some(format!("Failed to generate test: {}", e)),
                data: None,
            })
        }
    }
}

/// Get all saved API requests for the orchestrator UI
///
/// Returns the list of saved API requests that can be selected for orchestration.
#[tauri::command]
pub async fn get_saved_requests_for_orchestration(
    state: State<'_, Arc<AppState>>,
) -> Result<CommandResponse, String> {
    info!("Getting saved API requests for orchestration");

    let requests_result: Result<serde_json::Value, String> = state.pg_db.list_saved_api_requests().await.map(|v| serde_json::to_value(&v).unwrap_or_default());
    match requests_result {
        Ok(requests_json) => Ok(CommandResponse {
            success: true,
            message: Some(format!("Found {} saved requests", requests_json.as_array().map(|a| a.len()).unwrap_or(0))),
            data: Some(requests_json),
        }),
        Err(e) => {
            error!("Failed to list saved requests: {}", e);
            Ok(CommandResponse {
                success: false,
                message: Some(format!("Failed to list requests: {}", e)),
                data: None,
            })
        }
    }
}

/// Save an orchestration plan for later reuse
#[tauri::command]
pub async fn save_orchestration_plan(
    plan: TestOrchestrationPlan,
    state: State<'_, Arc<AppState>>,
) -> Result<CommandResponse, String> {
    info!("Saving orchestration plan: {}", plan.id);

    // Serialize plan to JSON Value
    let plan_value =
        serde_json::to_value(&plan).map_err(|e| format!("Failed to serialize plan: {}", e))?;

    // Store as a setting
    let key = format!("orchestration_plan_{}", plan.id);
    let save_err = state.pg_db.set_setting(&key, &plan_value).await.err();
    if let Some(e) = save_err {
        error!("Failed to save orchestration plan: {}", e);
        return Ok(CommandResponse {
            success: false,
            message: Some(format!("Failed to save plan: {}", e)),
            data: None,
        });
    }

    Ok(CommandResponse {
        success: true,
        message: Some("Orchestration plan saved".to_string()),
        data: None,
    })
}

/// List saved orchestration plans
#[tauri::command]
pub async fn list_orchestration_plans(
    state: State<'_, Arc<AppState>>,
) -> Result<CommandResponse, String> {
    info!("Listing saved orchestration plans");

    // Get all settings
    let all_settings = state.pg_db.get_all_settings().await
    .map_err(|e| format!("Failed to get settings: {}", e))?;

    // Filter for orchestration plans
    let plans: Vec<TestOrchestrationPlan> = if let serde_json::Value::Object(map) = all_settings {
        map.iter()
            .filter(|(key, _)| key.starts_with("orchestration_plan_"))
            .filter_map(|(_, value)| serde_json::from_value(value.clone()).ok())
            .collect()
    } else {
        Vec::new()
    };

    Ok(CommandResponse {
        success: true,
        message: Some(format!("Found {} saved plans", plans.len())),
        data: Some(serde_json::to_value(&plans).unwrap_or_default()),
    })
}

/// Delete an orchestration plan
#[tauri::command]
pub async fn delete_orchestration_plan(
    plan_id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<CommandResponse, String> {
    info!("Deleting orchestration plan: {}", plan_id);

    // Set the value to null to effectively delete the setting
    let key = format!("orchestration_plan_{}", plan_id);
    let del_err = state.pg_db.set_setting(&key, &serde_json::Value::Null).await.err();
    if let Some(e) = del_err {
        error!("Failed to delete orchestration plan: {}", e);
        return Ok(CommandResponse {
            success: false,
            message: Some(format!("Failed to delete plan: {}", e)),
            data: None,
        });
    }

    Ok(CommandResponse {
        success: true,
        message: Some("Orchestration plan deleted".to_string()),
        data: None,
    })
}
