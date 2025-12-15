//! RAG (Retrieval-Augmented Generation) command handlers
//!
//! This module provides Tauri commands for importing and managing RAG configurations.
//! RAG configs contain pattern screenshots and element annotations for visual automation.

use crate::rag::{
    EmbeddingGenerator, ImportResult, RAGConfig, RAGStorage, ScreenshotData, SearchFilters,
    SearchResult, SemanticSearch,
};
use std::sync::Arc;
use tauri::State;
use tokio::sync::Mutex as TokioMutex;
use tracing::{info, warn};

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
