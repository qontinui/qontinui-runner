//! HTTP endpoint for Restate service callbacks.
//!
//! Starts an HTTP server on the configured service endpoint port (default: 9080)
//! that the Restate server calls into when executing workflow invocations.
//!
//! This module is gated behind `#[cfg(feature = "restate")]` because it depends
//! on the `restate-sdk` crate.

#![cfg(feature = "restate")]

use std::sync::Arc;

use restate_sdk::prelude::*;
use tracing::{error, info};

use crate::config_storage::ConfigStorage;
use crate::AppState;

use super::service::{self, QontinuiWorkflowImpl, WorkflowStateObjectImpl};

/// Start the Restate service HTTP endpoint.
///
/// This endpoint is where the Restate server sends invocation requests.
/// It binds both the `QontinuiWorkflow` (Workflow) and `WorkflowStateObject`
/// (Virtual Object) services to a single HTTP server.
///
/// # Arguments
/// * `port` - The port to listen on (default: 9080 from RestateSettings)
/// * `app_state` - Shared application state for workflow execution
/// * `config_storage` - Configuration storage for step executors
///
/// # Errors
/// Returns an error string if the server fails to start or bind.
pub async fn start_restate_endpoint(
    port: u16,
    app_state: Arc<AppState>,
    config_storage: Arc<tokio::sync::Mutex<ConfigStorage>>,
) -> Result<(), String> {
    // Initialize global state before starting the endpoint
    service::init_global_state(app_state, config_storage);

    info!(port = port, "Starting Restate service HTTP endpoint");

    let endpoint = Endpoint::builder()
        .bind(QontinuiWorkflowImpl.serve())
        .bind(WorkflowStateObjectImpl.serve())
        .build();

    info!(
        port = port,
        "Restate endpoint built, starting HTTP server with QontinuiWorkflow and WorkflowStateObject"
    );

    HttpServer::new(endpoint)
        .listen_and_serve(
            format!("0.0.0.0:{}", port)
                .parse()
                .map_err(|e| format!("Invalid address: {}", e))?,
        )
        .await
        .map_err(|e| {
            let msg = format!("Restate HTTP endpoint failed: {}", e);
            error!("{}", msg);
            msg
        })?;

    Ok(())
}
