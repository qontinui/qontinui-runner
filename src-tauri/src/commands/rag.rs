//! RAG (Retrieval-Augmented Generation) command handlers
//!
//! This module provides Tauri commands for importing and managing RAG configurations.
//! RAG configs contain pattern screenshots and element annotations for visual automation.

use crate::auth::AuthManager;
use crate::rag::{
    EmbeddingGenerator, EmbeddingStatus, ImportResult, QontinuiConfig, RAGConfig, RAGStorage,
    ScreenshotData, SearchFilters, SearchResult, SemanticSearch,
};
use base64::Engine as _;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, State};
use tokio::sync::Mutex as TokioMutex;
use tracing::{error, info, warn};

/// Get API base URL for qontinui-web backend
fn get_api_base_url() -> String {
    std::env::var("QONTINUI_API_URL").unwrap_or_else(|_| {
        if cfg!(debug_assertions) {
            "http://localhost:8000".to_string()
        } else {
            "https://qontinui.io".to_string()
        }
    })
}

/// Send embedding results to the web backend
///
/// This function reads the embeddings.json file and sends the results
/// to the web backend for storage.
async fn send_embeddings_to_web(project_id: &str) -> Result<(), String> {
    // Get the embeddings file path
    let home = dirs::home_dir().ok_or("Could not determine home directory")?;
    let embeddings_path = home
        .join(".qontinui")
        .join("rag")
        .join(project_id)
        .join("embeddings")
        .join("embeddings.json");

    if !embeddings_path.exists() {
        return Err(format!(
            "Embeddings file not found: {}",
            embeddings_path.display()
        ));
    }

    // Read embeddings file
    let embeddings_content = std::fs::read_to_string(&embeddings_path)
        .map_err(|e| format!("Failed to read embeddings file: {}", e))?;

    let embeddings_data: Value = serde_json::from_str(&embeddings_content)
        .map_err(|e| format!("Failed to parse embeddings JSON: {}", e))?;

    // Extract elements and group by state_image_id
    let elements = embeddings_data
        .get("elements")
        .and_then(|e| e.as_array())
        .ok_or("No elements found in embeddings")?;

    // Group embeddings by state_image_id (web stores embeddings per stateImage)
    let mut state_image_embeddings: HashMap<String, Value> = HashMap::new();

    for element in elements {
        let state_image_id = element
            .get("state_image_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        if state_image_id.is_empty() {
            continue;
        }

        // Extract embeddings from the element
        let image_embedding = element.get("image_embedding").cloned();
        let text_embedding = element.get("text_embedding").cloned();
        let ocr_text = element.get("ocr_text").cloned();
        let ocr_confidence = element.get("ocr_confidence").cloned();

        // Only keep first result per state_image_id (multiple patterns may reference same stateImage)
        if !state_image_embeddings.contains_key(state_image_id) {
            state_image_embeddings.insert(
                state_image_id.to_string(),
                serde_json::json!({
                    "state_image_id": state_image_id,
                    "success": true,
                    "image_embedding": image_embedding,
                    "text_embedding": text_embedding,
                    "ocr_text": ocr_text,
                    "ocr_confidence": ocr_confidence,
                    "error": null
                }),
            );
        }
    }

    let results: Vec<Value> = state_image_embeddings.into_values().collect();
    let successful = results.len();

    if results.is_empty() {
        info!("No embeddings to send to web backend");
        return Ok(());
    }

    // Get auth token
    let auth_manager = AuthManager::new();
    let access_token = auth_manager
        .get_access_token()
        .map_err(|e| format!("Failed to get access token: {}", e))?;

    // Build request payload
    let request_body = serde_json::json!({
        "project_id": project_id,
        "results": results,
        "total_processed": successful,
        "successful": successful,
        "failed": 0
    });

    // Send to web backend
    let api_url = get_api_base_url();
    let endpoint = format!("{}/api/v1/rag/{}/embedding-results", api_url, project_id);

    info!(
        "Sending {} embedding results to web backend: {}",
        successful, endpoint
    );

    let client = reqwest::Client::new();
    let response = client
        .post(&endpoint)
        .header("Authorization", format!("Bearer {}", access_token))
        .header("Content-Type", "application/json")
        .json(&request_body)
        .send()
        .await
        .map_err(|e| format!("Failed to send embeddings to web: {}", e))?;

    if response.status().is_success() {
        let response_body: Value = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse response: {}", e))?;

        info!(
            "Successfully sent embeddings to web backend: {:?}",
            response_body
        );
        Ok(())
    } else {
        let status = response.status();
        let error_body = response.text().await.unwrap_or_default();
        Err(format!(
            "Web backend returned error {}: {}",
            status, error_body
        ))
    }
}

use super::CommandResponse;

/// Shared state for RAG operations
pub struct RAGState {
    pub storage: Arc<TokioMutex<RAGStorage>>,
    pub embedding_generator: Arc<TokioMutex<EmbeddingGenerator>>,
    pub semantic_search: Arc<TokioMutex<SemanticSearch>>,
}

impl RAGState {
    pub fn new() -> Result<Self, String> {
        let storage =
            RAGStorage::new().map_err(|e| format!("Failed to initialize RAG storage: {}", e))?;
        let embedding_generator = EmbeddingGenerator::new()
            .map_err(|e| format!("Failed to initialize embedding generator: {}", e))?;
        let semantic_search = SemanticSearch::new()
            .map_err(|e| format!("Failed to initialize semantic search: {}", e))?;

        Ok(Self {
            storage: Arc::new(TokioMutex::new(storage)),
            embedding_generator: Arc::new(TokioMutex::new(embedding_generator)),
            semantic_search: Arc::new(TokioMutex::new(semantic_search)),
        })
    }
}

/// Import a RAG configuration with screenshots
///
/// This command:
/// 1. Validates the RAG configuration
/// 2. Saves the config to ~/.qontinui/rag/{project_id}/
/// 3. Saves screenshots to ~/.qontinui/rag/{project_id}/screenshots/
/// 4. Triggers embedding generation (background)
///
/// # Arguments
/// * `config` - RAG configuration metadata
/// * `screenshots` - Screenshot data (base64-encoded PNG images)
/// * `state` - RAG state containing storage and embedding generator
///
/// # Returns
/// * `Ok(CommandResponse)` - Success with import result
/// * `Err(String)` - Error message if import fails
#[tauri::command]
pub async fn import_rag_config(
    config: RAGConfig,
    screenshots: Vec<ScreenshotData>,
    state: State<'_, Arc<RAGState>>,
) -> Result<CommandResponse, String> {
    info!(
        "Importing RAG config: project_id={}, screenshots={}",
        config.project_id,
        screenshots.len()
    );

    // Validate configuration
    if config.project_id.is_empty() {
        return Err("Project ID cannot be empty".to_string());
    }

    if config.screenshots.len() != screenshots.len() {
        return Err(format!(
            "Screenshot count mismatch: config has {}, data has {}",
            config.screenshots.len(),
            screenshots.len()
        ));
    }

    let project_id = config.project_id.clone();
    let screenshot_count = screenshots.len();
    let element_count = config.total_element_count();

    // Save configuration
    let storage = state.storage.lock().await;
    #[allow(deprecated)]
    let storage_path = storage
        .save_config(&config)
        .map_err(|e| format!("Failed to save config: {}", e))?;

    // Save screenshots
    let saved_count = storage
        .save_screenshots(&project_id, &screenshots)
        .map_err(|e| format!("Failed to save screenshots: {}", e))?;

    let storage_path_str = storage_path.to_string_lossy().to_string();
    drop(storage); // Release lock before starting async task

    if saved_count != screenshot_count {
        warn!(
            "Screenshot count mismatch: expected {}, saved {}",
            screenshot_count, saved_count
        );
    }

    // Trigger embedding generation in background
    info!(
        "Starting background embedding generation for project_id={}",
        project_id
    );
    let embedding_generator = state.embedding_generator.lock().await;
    let _progress_rx = embedding_generator.generate_embeddings_async(project_id.clone());
    drop(embedding_generator);

    // Note: We don't wait for embeddings to complete - they run in the background
    // Clients can poll get_rag_embedding_status to check progress

    let result = ImportResult {
        success: true,
        project_id: project_id.clone(),
        message: format!(
            "Successfully imported RAG config with {} screenshots and {} elements. Embedding generation started.",
            saved_count, element_count
        ),
        screenshot_count: saved_count,
        element_count,
        storage_path: storage_path_str,
    };

    Ok(CommandResponse {
        success: true,
        message: Some(result.message.clone()),
        data: Some(serde_json::to_value(result).unwrap()),
    })
}

/// Get the embedding generation status for a project
///
/// # Arguments
/// * `project_id` - Project ID to check
/// * `state` - RAG state
///
/// # Returns
/// * `Ok(CommandResponse)` - Success with embedding status
/// * `Err(String)` - Error message if check fails
#[tauri::command]
pub async fn get_rag_embedding_status(
    project_id: String,
    state: State<'_, Arc<RAGState>>,
) -> Result<CommandResponse, String> {
    info!("Checking embedding status for project_id={}", project_id);

    let embedding_generator = state.embedding_generator.lock().await;

    // Get progress from state if available (includes in-progress tracking)
    if let Some(progress) = embedding_generator.get_progress(&project_id) {
        let status_str = match &progress.status {
            crate::rag::EmbeddingStatus::NotStarted => "not_started",
            crate::rag::EmbeddingStatus::InProgress(_) => "in_progress",
            crate::rag::EmbeddingStatus::Completed => "completed",
            crate::rag::EmbeddingStatus::Failed(_) => "failed",
        };

        let mut data = serde_json::json!({
            "status": status_str,
            "message": progress.message,
        });

        // Add optional fields if present
        if let Some(percent) = progress.percent {
            data["percent"] = serde_json::json!(percent);
        }
        if let Some(elements_processed) = progress.elements_processed {
            data["elements_processed"] = serde_json::json!(elements_processed);
        }
        if let Some(total_elements) = progress.total_elements {
            data["total_elements"] = serde_json::json!(total_elements);
        }

        return Ok(CommandResponse {
            success: true,
            message: Some(progress.message),
            data: Some(data),
        });
    }

    // Fallback to file-based check (for completed/not started)
    let status = embedding_generator.check_status(&project_id);

    let status_str = match &status {
        crate::rag::EmbeddingStatus::NotStarted => "not_started",
        crate::rag::EmbeddingStatus::InProgress(pct) => {
            return Ok(CommandResponse {
                success: true,
                message: Some(format!("Embedding generation in progress: {}%", pct)),
                data: Some(serde_json::json!({
                    "status": "in_progress",
                    "percent": pct
                })),
            })
        }
        crate::rag::EmbeddingStatus::Completed => "completed",
        crate::rag::EmbeddingStatus::Failed(_) => "failed",
    };

    let message = match &status {
        crate::rag::EmbeddingStatus::Failed(err) => {
            Some(format!("Embedding generation failed: {}", err))
        }
        crate::rag::EmbeddingStatus::Completed => {
            Some("Embeddings generated successfully".to_string())
        }
        _ => None,
    };

    Ok(CommandResponse {
        success: true,
        message,
        data: Some(serde_json::json!({
            "status": status_str
        })),
    })
}

/// Search RAG elements by query (legacy label-based search)
///
/// NOTE: This is a legacy implementation using simple label matching.
/// For semantic search with embeddings, use `search_rag_elements_semantic` instead.
///
/// # Arguments
/// * `project_id` - Project ID to search in
/// * `query` - Search query string
/// * `filters` - Optional search filters
/// * `state` - RAG state
///
/// # Returns
/// * `Ok(CommandResponse)` - Success with search results
/// * `Err(String)` - Error message if search fails
#[tauri::command]
pub async fn search_rag_elements(
    project_id: String,
    query: String,
    filters: Option<SearchFilters>,
    state: State<'_, Arc<RAGState>>,
) -> Result<CommandResponse, String> {
    info!(
        "Searching RAG elements (label-based): project_id={}, query={}",
        project_id, query
    );

    let storage = state.storage.lock().await;

    // Load config
    let config = storage
        .load_config(&project_id)
        .map_err(|e| format!("Failed to load config: {}", e))?;

    // Simple label-based search
    let mut results = Vec::new();
    let query_lower = query.to_lowercase();

    for (screenshot_id, elements) in &config.elements {
        for element in elements {
            // Apply filters
            if let Some(ref f) = filters {
                if let Some(ref element_type) = f.element_type {
                    if element.element_type.as_ref() != Some(element_type) {
                        continue;
                    }
                }

                if let Some(ref state_id) = f.state_id {
                    let screenshot = config.screenshots.iter().find(|s| s.id == *screenshot_id);
                    if let Some(ss) = screenshot {
                        if ss.state_id.as_ref() != Some(state_id) {
                            continue;
                        }
                    }
                }
            }

            // Simple label matching
            if element.label.to_lowercase().contains(&query_lower) {
                results.push(SearchResult {
                    element_id: element.id.clone(),
                    label: element.label.clone(),
                    screenshot_id: screenshot_id.clone(),
                    bbox: element.bbox.clone(),
                    similarity: 1.0, // Placeholder - would be actual similarity score with embeddings
                    metadata: element.metadata.clone(),
                });
            }
        }
    }

    // Apply limit filter
    if let Some(ref f) = filters {
        if let Some(limit) = f.limit {
            results.truncate(limit);
        }
    }

    info!("Found {} matching elements", results.len());

    Ok(CommandResponse {
        success: true,
        message: Some(format!("Found {} matching elements", results.len())),
        data: Some(serde_json::to_value(results).unwrap()),
    })
}

/// Search RAG elements using semantic similarity (vector search)
///
/// This command uses embeddings and Qdrant vector database for semantic search.
/// It finds elements that are semantically similar to the query, not just exact matches.
///
/// # Arguments
/// * `project_id` - Project ID to search in
/// * `query` - Search query string
/// * `limit` - Maximum number of results (default: 10)
/// * `min_similarity` - Optional minimum similarity score 0.0-1.0 (default: 0.0)
/// * `state` - RAG state
///
/// # Returns
/// * `Ok(CommandResponse)` - Success with search results
/// * `Err(String)` - Error message if search fails
#[tauri::command]
pub async fn search_rag_elements_semantic(
    project_id: String,
    query: String,
    limit: Option<usize>,
    min_similarity: Option<f32>,
    state: State<'_, Arc<RAGState>>,
) -> Result<CommandResponse, String> {
    info!(
        "Semantic search: project_id={}, query='{}', limit={:?}, min_similarity={:?}",
        project_id, query, limit, min_similarity
    );

    let limit = limit.unwrap_or(10);

    // Validate min_similarity
    if let Some(score) = min_similarity {
        if !(0.0..=1.0).contains(&score) {
            return Err("min_similarity must be between 0.0 and 1.0".to_string());
        }
    }

    // Clone the Arc before spawning blocking task
    let semantic_search_arc = state.semantic_search.clone();

    let results = tokio::task::spawn_blocking({
        let project_id = project_id.clone();
        let query = query.clone();

        move || {
            // Lock inside the blocking task
            let runtime = tokio::runtime::Runtime::new().unwrap();
            let semantic_search = runtime.block_on(semantic_search_arc.lock());
            semantic_search.search(&project_id, &query, limit, min_similarity)
        }
    })
    .await
    .map_err(|e| format!("Task join error: {}", e))?
    .map_err(|e| match e {
        crate::rag::SearchError::EmbeddingsNotFound(msg) => {
            format!(
                "Embeddings not found. Please generate embeddings first. Details: {}",
                msg
            )
        }
        _ => format!("Semantic search failed: {}", e),
    })?;

    info!("Semantic search found {} results", results.len());

    Ok(CommandResponse {
        success: true,
        message: Some(format!(
            "Found {} semantically similar elements",
            results.len()
        )),
        data: Some(serde_json::to_value(results).unwrap()),
    })
}

/// List all RAG configurations
///
/// # Arguments
/// * `state` - RAG state
///
/// # Returns
/// * `Ok(CommandResponse)` - Success with list of RAG config summaries
/// * `Err(String)` - Error message if listing fails
#[tauri::command]
pub async fn list_rag_configs(state: State<'_, Arc<RAGState>>) -> Result<CommandResponse, String> {
    info!("Listing RAG configurations");

    let storage = state.storage.lock().await;
    let summaries = storage
        .list_configs()
        .map_err(|e| format!("Failed to list configs: {}", e))?;

    info!("Found {} RAG configurations", summaries.len());

    Ok(CommandResponse {
        success: true,
        message: Some(format!("Found {} configurations", summaries.len())),
        data: Some(serde_json::to_value(summaries).unwrap()),
    })
}

/// Delete a RAG configuration and all associated data
///
/// # Arguments
/// * `project_id` - Project ID to delete
/// * `state` - RAG state
///
/// # Returns
/// * `Ok(CommandResponse)` - Success message
/// * `Err(String)` - Error message if deletion fails
#[tauri::command]
pub async fn delete_rag_config(
    project_id: String,
    state: State<'_, Arc<RAGState>>,
) -> Result<CommandResponse, String> {
    info!("Deleting RAG config: project_id={}", project_id);

    let storage = state.storage.lock().await;
    storage
        .delete_config(&project_id)
        .map_err(|e| format!("Failed to delete config: {}", e))?;

    info!("Successfully deleted RAG config: project_id={}", project_id);

    Ok(CommandResponse {
        success: true,
        message: Some(format!("Successfully deleted RAG config: {}", project_id)),
        data: None,
    })
}

/// Get RAG configuration details
///
/// # Arguments
/// * `project_id` - Project ID to get details for
/// * `state` - RAG state
///
/// # Returns
/// * `Ok(CommandResponse)` - Success with config details
/// * `Err(String)` - Error message if not found
#[tauri::command]
pub async fn get_rag_config(
    project_id: String,
    state: State<'_, Arc<RAGState>>,
) -> Result<CommandResponse, String> {
    info!("Getting RAG config: project_id={}", project_id);

    let storage = state.storage.lock().await;
    let config = storage
        .load_config(&project_id)
        .map_err(|e| format!("Failed to load config: {}", e))?;

    Ok(CommandResponse {
        success: true,
        message: Some(format!("Successfully loaded config for {}", project_id)),
        data: Some(serde_json::to_value(config).unwrap()),
    })
}

/// Get storage usage for a RAG project
///
/// # Arguments
/// * `project_id` - Project ID to check
/// * `state` - RAG state
///
/// # Returns
/// * `Ok(CommandResponse)` - Success with storage usage info
/// * `Err(String)` - Error message if check fails
#[tauri::command]
pub async fn get_rag_storage_usage(
    project_id: String,
    state: State<'_, Arc<RAGState>>,
) -> Result<CommandResponse, String> {
    info!("Getting storage usage for project_id={}", project_id);

    let storage = state.storage.lock().await;
    let usage = storage
        .get_storage_usage(&project_id)
        .map_err(|e| format!("Failed to get storage usage: {}", e))?;

    Ok(CommandResponse {
        success: true,
        message: Some(format!("Total storage: {}", usage.total_size())),
        data: Some(serde_json::json!({
            "project_id": usage.project_id,
            "total_bytes": usage.total_bytes,
            "total_size": usage.total_size(),
            "image_count": usage.image_count,
            "image_bytes": usage.image_bytes,
            "image_size": crate::rag::storage::StorageUsage::format_bytes(usage.image_bytes),
            "config_bytes": usage.config_bytes,
            "config_size": crate::rag::storage::StorageUsage::format_bytes(usage.config_bytes),
            "embeddings_bytes": usage.embeddings_bytes,
            "embeddings_size": crate::rag::storage::StorageUsage::format_bytes(usage.embeddings_bytes),
        })),
    })
}

/// Start RAG processing (embedding generation) for a project
///
/// This command triggers embedding generation and emits progress events
/// to the frontend via Tauri events. The frontend can listen to:
/// - `rag-progress`: Progress updates during processing
/// - `rag-completion`: Final results when processing completes
///
/// # Arguments
/// * `project_id` - Project ID to process
/// * `config` - Optional QontinuiConfig to save before processing (for configs loaded from file)
/// * `app_handle` - Tauri app handle for emitting events
/// * `state` - RAG state
///
/// # Returns
/// * `Ok(CommandResponse)` - Processing started successfully
/// * `Err(String)` - Error message if start fails
#[tauri::command]
pub async fn start_rag_processing(
    project_id: String,
    config: Option<QontinuiConfig>,
    app_handle: AppHandle,
    state: State<'_, Arc<RAGState>>,
) -> Result<CommandResponse, String> {
    info!("Starting RAG processing for project_id={}", project_id);

    // Check if config exists, if not and config is provided, save it
    let storage = state.storage.lock().await;
    let config_exists = storage.config_exists(&project_id);

    if !config_exists {
        if let Some(ref cfg) = config {
            info!(
                "Config not found in RAG storage, saving provided config for project_id={}",
                project_id
            );

            // Save the config
            storage
                .save_qontinui_config(&project_id, cfg)
                .map_err(|e| format!("Failed to save config: {}", e))?;

            // Also save images to disk for embedding generation
            let images_path = storage.get_images_path(&project_id);
            std::fs::create_dir_all(&images_path)
                .map_err(|e| format!("Failed to create images directory: {}", e))?;

            for image in &cfg.images {
                let image_path = images_path.join(format!("{}.png", image.id));
                if let Ok(image_data) =
                    base64::engine::general_purpose::STANDARD.decode(&image.data)
                {
                    if let Err(e) = std::fs::write(&image_path, &image_data) {
                        warn!("Failed to save image {}: {}", image.id, e);
                    } else {
                        info!("Saved image: {:?}", image_path);
                    }
                }
            }
        } else {
            drop(storage);
            return Err(format!("Project not found: {}", project_id));
        }
    }
    drop(storage);

    // Start embedding generation
    let embedding_generator = state.embedding_generator.lock().await;
    let mut progress_rx = embedding_generator.generate_embeddings_async(project_id.clone());
    drop(embedding_generator);

    // Spawn task to listen for progress and emit events
    let app_handle_clone = app_handle.clone();
    let project_id_clone = project_id.clone();

    tokio::spawn(async move {
        while let Some(progress) = progress_rx.recv().await {
            // Convert status to string
            let status_str = match &progress.status {
                EmbeddingStatus::NotStarted => "not_started",
                EmbeddingStatus::InProgress(_) => "in_progress",
                EmbeddingStatus::Completed => "completed",
                EmbeddingStatus::Failed(_) => "failed",
            };

            // Build event payload
            let mut payload = serde_json::json!({
                "project_id": project_id_clone,
                "status": status_str,
                "message": progress.message,
            });

            if let Some(percent) = progress.percent {
                payload["percent"] = serde_json::json!(percent);
            }
            if let Some(elements_processed) = progress.elements_processed {
                payload["elements_processed"] = serde_json::json!(elements_processed);
            }
            if let Some(total_elements) = progress.total_elements {
                payload["total_elements"] = serde_json::json!(total_elements);
            }

            // Add error message if failed
            if let EmbeddingStatus::Failed(err) = &progress.status {
                payload["error"] = serde_json::json!(err);
            }

            // Emit progress event
            if let Err(e) = app_handle_clone.emit("rag-progress", &payload) {
                error!("Failed to emit rag-progress event: {}", e);
            }

            // If completed or failed, emit completion event and send to web
            if matches!(
                progress.status,
                EmbeddingStatus::Completed | EmbeddingStatus::Failed(_)
            ) {
                let is_success = matches!(progress.status, EmbeddingStatus::Completed);

                // If successful, send embeddings to web backend
                let mut web_sync_success = false;
                let mut web_sync_error: Option<String> = None;

                if is_success {
                    info!(
                        "Embedding generation completed, sending results to web for project_id={}",
                        project_id_clone
                    );

                    match send_embeddings_to_web(&project_id_clone).await {
                        Ok(()) => {
                            info!(
                                "Successfully synced embeddings to web for project_id={}",
                                project_id_clone
                            );
                            web_sync_success = true;
                        }
                        Err(e) => {
                            warn!(
                                "Failed to sync embeddings to web for project_id={}: {}",
                                project_id_clone, e
                            );
                            web_sync_error = Some(e);
                        }
                    }
                }

                let completion_payload = serde_json::json!({
                    "project_id": project_id_clone,
                    "success": is_success,
                    "total_processed": progress.elements_processed.unwrap_or(0),
                    "successful": if is_success {
                        progress.elements_processed.unwrap_or(0)
                    } else { 0 },
                    "failed": if matches!(progress.status, EmbeddingStatus::Failed(_)) {
                        progress.total_elements.unwrap_or(0)
                    } else { 0 },
                    "results": [],
                    "web_sync_success": web_sync_success,
                    "web_sync_error": web_sync_error,
                });

                if let Err(e) = app_handle_clone.emit("rag-completion", &completion_payload) {
                    error!("Failed to emit rag-completion event: {}", e);
                }

                break;
            }
        }
    });

    Ok(CommandResponse {
        success: true,
        message: Some(format!(
            "RAG processing started for project: {}",
            project_id
        )),
        data: None,
    })
}
