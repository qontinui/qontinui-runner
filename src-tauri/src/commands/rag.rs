//! RAG (Retrieval-Augmented Generation) command handlers
//!
//! This module provides Tauri commands for importing and managing RAG configurations.
//! RAG configs contain pattern screenshots and element annotations for visual automation.

use crate::auth::AuthManager;
use crate::event_system::EventEmitter;
use crate::rag::{
    EmbeddingGenerator, EmbeddingStatus, ImportResult, QontinuiConfig, RAGStorage, SearchFilters,
    SearchResult, SemanticSearch,
};
use base64::Engine as _;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};
use tauri::{AppHandle, State};
use tokio::sync::Mutex as TokioMutex;
use tracing::{info, warn};

use crate::api_config::get_api_base_url;

/// Send embedding results to the web backend
///
/// This function reads the embeddings.json file and sends the results
/// to the web backend for storage.
pub async fn send_embeddings_to_web(project_id: &str) -> Result<(), String> {
    // Get the project directory
    let home = dirs::home_dir().ok_or("Could not determine home directory")?;
    let project_dir = home.join(".qontinui").join("rag").join(project_id);

    let embeddings_path = project_dir.join("embeddings").join("embeddings.json");
    let config_path = project_dir.join("config.json");

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

    // Read config file to get pattern -> stateImage mapping
    let config_content = std::fs::read_to_string(&config_path)
        .map_err(|e| format!("Failed to read config file: {}", e))?;

    let config_data: Value = serde_json::from_str(&config_content)
        .map_err(|e| format!("Failed to parse config JSON: {}", e))?;

    // Build pattern_id -> state_image_id mapping from config
    let mut pattern_to_state_image: HashMap<String, String> = HashMap::new();

    if let Some(states) = config_data.get("states").and_then(|s| s.as_array()) {
        for state in states {
            if let Some(state_images) = state.get("stateImages").and_then(|si| si.as_array()) {
                for state_image in state_images {
                    let state_image_id =
                        state_image.get("id").and_then(|v| v.as_str()).unwrap_or("");

                    if let Some(patterns) = state_image.get("patterns").and_then(|p| p.as_array()) {
                        for pattern in patterns {
                            if let Some(pattern_id) = pattern.get("id").and_then(|v| v.as_str()) {
                                pattern_to_state_image
                                    .insert(pattern_id.to_string(), state_image_id.to_string());
                            }
                        }
                    }
                }
            }
        }
    }

    info!(
        "Built pattern to stateImage mapping: {} patterns",
        pattern_to_state_image.len()
    );

    // Extract elements and group by state_image_id
    let elements = embeddings_data
        .get("elements")
        .and_then(|e| e.as_array())
        .ok_or("No elements found in embeddings")?;

    // Group embeddings by state_image_id (web stores embeddings per stateImage)
    let mut state_image_embeddings: HashMap<String, Value> = HashMap::new();

    for element in elements {
        // The element id is the pattern id (e.g., "pattern_123")
        let pattern_id = element.get("id").and_then(|v| v.as_str()).unwrap_or("");

        // Look up the state_image_id from the mapping
        let state_image_id = pattern_to_state_image
            .get(pattern_id)
            .map(|s| s.as_str())
            .unwrap_or("");

        if state_image_id.is_empty() {
            warn!("No stateImage mapping found for pattern: {}", pattern_id);
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

    /// Create a degraded RAGState that will return errors on use
    ///
    /// This is used when RAG initialization fails but we want the runner to continue.
    /// All RAG features will be disabled and will return errors when called.
    pub fn new_degraded() -> Self {
        warn!("Creating degraded RAGState - all RAG features will be disabled");
        Self {
            storage: Arc::new(TokioMutex::new(RAGStorage::new_degraded())),
            embedding_generator: Arc::new(TokioMutex::new(EmbeddingGenerator::new_degraded())),
            semantic_search: Arc::new(TokioMutex::new(SemanticSearch::new_degraded())),
        }
    }
}

/// Import a QontinuiConfig for RAG processing
///
/// This command:
/// 1. Validates the configuration
/// 2. Saves the config to ~/.qontinui/rag/{project_id}/
/// 3. Extracts and saves images from config.images[]
/// 4. Triggers embedding generation (background)
///
/// # Arguments
/// * `project_id` - Project ID for storage
/// * `config` - QontinuiConfig with images and states
/// * `state` - RAG state containing storage and embedding generator
///
/// # Returns
/// * `Ok(CommandResponse)` - Success with import result
/// * `Err(String)` - Error message if import fails
#[tauri::command]
pub async fn import_rag_config(
    project_id: String,
    config: QontinuiConfig,
    state: State<'_, Arc<RAGState>>,
) -> Result<CommandResponse, String> {
    info!(
        "Importing RAG config: project_id={}, images={}, states={}",
        project_id,
        config.images.len(),
        config.states.len()
    );

    // Validate configuration
    if project_id.is_empty() {
        return Err("Project ID cannot be empty".to_string());
    }

    let image_count = config.images.len();
    let pattern_count = config.pattern_count();

    // Save configuration
    let storage = state.storage.lock().await;
    let storage_path = storage
        .save_qontinui_config(&project_id, &config)
        .map_err(|e| format!("Failed to save config: {}", e))?;

    // Save images from config
    let referenced_ids = config.referenced_image_ids();
    let saved_count = storage
        .save_images_from_config(&project_id, &config.images, &referenced_ids)
        .map_err(|e| format!("Failed to save images: {}", e))?;

    let storage_path_str = storage_path.to_string_lossy().to_string();
    drop(storage); // Release lock before starting async task

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
            "Successfully imported RAG config with {} images ({} saved) and {} patterns. Embedding generation started.",
            image_count, saved_count, pattern_count
        ),
        screenshot_count: saved_count,
        element_count: pattern_count,
        storage_path: storage_path_str,
    };

    Ok(CommandResponse {
        success: true,
        message: Some(result.message.clone()),
        data: serde_json::to_value(result).ok(),
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

/// Search RAG elements by query (name-based search in patterns)
///
/// NOTE: This is a simple name-matching implementation.
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
        "Searching RAG elements (name-based): project_id={}, query={}",
        project_id, query
    );

    let storage = state.storage.lock().await;

    // Load config
    let config = storage
        .load_qontinui_config(&project_id)
        .map_err(|e| format!("Failed to load config: {}", e))?;

    // Simple name-based search through states and patterns
    let mut results = Vec::new();
    let query_lower = query.to_lowercase();

    for state_obj in &config.states {
        // Apply state filter
        if let Some(ref f) = filters {
            if let Some(ref filter_state_id) = f.state_id {
                if &state_obj.id != filter_state_id {
                    continue;
                }
            }
        }

        for state_image in &state_obj.state_images {
            // Search in stateImage name
            if state_image.name.to_lowercase().contains(&query_lower) {
                results.push(SearchResult {
                    element_id: state_image.id.clone(),
                    label: state_image.name.clone(),
                    screenshot_id: state_obj.id.clone(),
                    bbox: crate::rag::BoundingBox {
                        x: 0,
                        y: 0,
                        width: 0,
                        height: 0,
                    },
                    similarity: 1.0,
                    metadata: None,
                });
            }

            // Search in pattern names
            for pattern in &state_image.patterns {
                let pattern_name = pattern.name.as_deref().unwrap_or(&pattern.id);
                if pattern_name.to_lowercase().contains(&query_lower) {
                    results.push(SearchResult {
                        element_id: pattern.id.clone(),
                        label: pattern_name.to_string(),
                        screenshot_id: pattern.image_id.clone(),
                        bbox: crate::rag::BoundingBox {
                            x: 0,
                            y: 0,
                            width: 0,
                            height: 0,
                        },
                        similarity: 1.0,
                        metadata: None,
                    });
                }
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
        data: serde_json::to_value(results).ok(),
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

    // Lock the semantic search and perform the search
    // Note: search() is a CPU-bound operation that doesn't need spawn_blocking
    let semantic_search = state.semantic_search.lock().await;
    let results = semantic_search
        .search(&project_id, &query, limit, min_similarity)
        .map_err(|e| match e {
            crate::rag::SearchError::EmbeddingsNotFound(msg) => {
                format!(
                    "Embeddings not found. Please generate embeddings first. Details: {}",
                    msg
                )
            }
            _ => format!("Semantic search failed: {}", e),
        })?;

    let results_for_response = results;

    Ok(CommandResponse {
        success: true,
        message: Some(format!(
            "Found {} semantically similar results",
            results_for_response.len()
        )),
        data: serde_json::to_value(results_for_response).ok(),
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
        data: serde_json::to_value(summaries).ok(),
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
        .load_qontinui_config(&project_id)
        .map_err(|e| format!("Failed to load config: {}", e))?;

    Ok(CommandResponse {
        success: true,
        message: Some(format!("Successfully loaded config for {}", project_id)),
        data: serde_json::to_value(config).ok(),
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

    // Spawn task to listen for progress and emit events via EventEmitter
    let emitter = EventEmitter::new(app_handle.clone());
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

            // Extract error message if failed
            let error_msg = if let EmbeddingStatus::Failed(err) = &progress.status {
                Some(err.clone())
            } else {
                None
            };

            // Emit progress event via EventEmitter
            // Note: Using raw emit for now since AppEvent::RagProgress has a different structure
            // that includes the error field. This demonstrates compatibility with existing payloads.
            let payload = serde_json::json!({
                "project_id": project_id_clone,
                "status": status_str,
                "message": progress.message,
                "percent": progress.percent,
                "elements_processed": progress.elements_processed,
                "total_elements": progress.total_elements,
                "error": error_msg,
            });
            emitter.emit_raw_or_error("rag-progress", &payload);

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

                // Emit completion event via EventEmitter
                let total_processed = progress.elements_processed.unwrap_or(0) as i32;
                let successful = if is_success { total_processed } else { 0 };
                let failed = if matches!(progress.status, EmbeddingStatus::Failed(_)) {
                    progress.total_elements.unwrap_or(0) as i32
                } else {
                    0
                };

                emitter.rag_completion_with_sync(
                    &project_id_clone,
                    is_success,
                    total_processed,
                    successful,
                    failed,
                    web_sync_success,
                    web_sync_error,
                );

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

pub fn plugin() -> TauriPlugin<tauri::Wry> {
    PluginBuilder::<tauri::Wry>::new("qontinui_rag")
        .invoke_handler(tauri::generate_handler![
            import_rag_config,
            get_rag_embedding_status,
            search_rag_elements,
            search_rag_elements_semantic,
            list_rag_configs,
            delete_rag_config,
            get_rag_config,
            get_rag_storage_usage,
            start_rag_processing,
        ])
        .build()
}
