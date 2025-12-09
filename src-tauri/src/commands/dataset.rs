//! Dataset packaging commands
//!
//! This module handles dataset packaging operations:
//! - Scanning local storage for images matching a manifest
//! - Packaging annotation exports with local images into YOLO format
//! - Splitting datasets into train/val/test sets

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tauri::State;
use tracing::{error, info, warn};
use zip::ZipArchive;

use super::{AppState, CommandResponse};

/// Image metadata from the manifest
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageMetadata {
    pub filename: String,
    pub image_hash: String,
    pub width: u32,
    pub height: u32,
    pub action_id: Option<String>,
    pub action_type: Option<String>,
    pub active_states: Option<Vec<String>>,
    pub timestamp: Option<String>,
}

/// Image manifest from annotation export
#[derive(Debug, Deserialize)]
pub struct ImageManifest {
    pub version: String,
    pub dataset_id: String,
    pub dataset_name: String,
    pub export_date: String,
    pub total_images: usize,
    pub images: Vec<ImageMetadata>,
}

/// Result of scanning local images
#[derive(Debug, Serialize)]
pub struct ScanResult {
    pub total_in_manifest: usize,
    pub matched: Vec<MatchedImage>,
    pub unmatched: Vec<ImageMetadata>,
    pub screenshot_base_path: String,
}

/// A matched image with its local path
#[derive(Debug, Serialize)]
pub struct MatchedImage {
    pub metadata: ImageMetadata,
    pub local_path: String,
    pub hash_verified: bool,
}

/// Configuration for dataset packaging
#[derive(Debug, Deserialize)]
pub struct PackageConfig {
    pub manifest_path: String,
    pub annotation_zip_path: String,
    pub output_path: String,
    pub train_split: f32,
    pub val_split: f32,
    pub test_split: f32,
    pub random_seed: u64,
}

/// Result of packaging operation
#[derive(Debug, Serialize)]
pub struct PackageResult {
    pub success: bool,
    pub output_path: String,
    pub train_count: usize,
    pub val_count: usize,
    pub test_count: usize,
    pub classes: Vec<String>,
}

/// Calculate SHA256 hash of a file
fn calculate_file_hash(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 8192];

    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }

    Ok(format!("{:x}", hasher.finalize()))
}

/// Scan local storage for images matching the manifest.
///
/// Reads the image_manifest.json from the annotation export and searches
/// local screenshot storage for matching images.
///
/// # Arguments
/// * `manifest_path` - Path to the image_manifest.json file
/// * `state` - Application state containing the local storage service
///
/// # Returns
/// * `Ok(CommandResponse)` - Success with scan results
/// * `Err(String)` - Error if scan operation fails
#[tauri::command]
pub fn scan_local_images(
    manifest_path: String,
    state: State<Arc<AppState>>,
) -> Result<CommandResponse, String> {
    info!("Scanning local images for manifest: {}", manifest_path);

    // Read and parse manifest
    let manifest_content = fs::read_to_string(&manifest_path).map_err(|e| {
        error!("Failed to read manifest: {}", e);
        format!("Failed to read manifest: {}", e)
    })?;

    let manifest: ImageManifest = serde_json::from_str(&manifest_content).map_err(|e| {
        error!("Failed to parse manifest: {}", e);
        format!("Failed to parse manifest: {}", e)
    })?;

    info!(
        "Manifest loaded: dataset={}, total_images={}",
        manifest.dataset_name, manifest.total_images
    );

    // Get screenshot base path from storage
    let storage = state.local_storage.lock().unwrap();
    let screenshot_path = &storage.config.screenshot_path;

    let mut matched: Vec<MatchedImage> = Vec::new();
    let mut unmatched: Vec<ImageMetadata> = Vec::new();

    // Build a map of all local images
    let mut local_images: HashMap<String, PathBuf> = HashMap::new();
    if screenshot_path.exists() {
        for session_entry in fs::read_dir(screenshot_path).map_err(|e| {
            error!("Failed to read screenshot directory: {}", e);
            format!("Failed to read screenshot directory: {}", e)
        })? {
            let session_entry =
                session_entry.map_err(|e| format!("Failed to read entry: {}", e))?;
            let session_path = session_entry.path();

            if session_path.is_dir() {
                for img_entry in fs::read_dir(&session_path)
                    .map_err(|e| format!("Failed to read session directory: {}", e))?
                {
                    let img_entry =
                        img_entry.map_err(|e| format!("Failed to read entry: {}", e))?;
                    let img_path = img_entry.path();

                    if img_path.is_file() {
                        if let Some(filename) = img_path.file_name() {
                            let filename_str = filename.to_string_lossy().to_string();
                            local_images.insert(filename_str, img_path);
                        }
                    }
                }
            }
        }
    }

    info!("Found {} local images", local_images.len());

    // Match manifest images with local storage
    for img_meta in manifest.images {
        if let Some(local_path) = local_images.get(&img_meta.filename) {
            // Found by filename, optionally verify hash
            let hash_verified = if !img_meta.image_hash.is_empty() {
                match calculate_file_hash(local_path) {
                    Ok(calculated_hash) => {
                        let hash_match = calculated_hash == img_meta.image_hash;
                        if !hash_match {
                            warn!(
                                "Hash mismatch for {}: expected {}, got {}",
                                img_meta.filename, img_meta.image_hash, calculated_hash
                            );
                        }
                        hash_match
                    }
                    Err(e) => {
                        warn!("Failed to calculate hash for {}: {}", img_meta.filename, e);
                        false
                    }
                }
            } else {
                false
            };

            matched.push(MatchedImage {
                metadata: img_meta,
                local_path: local_path.to_string_lossy().to_string(),
                hash_verified,
            });
        } else {
            // Not found in local storage
            unmatched.push(img_meta);
        }
    }

    let result = ScanResult {
        total_in_manifest: manifest.total_images,
        matched,
        unmatched,
        screenshot_base_path: screenshot_path.to_string_lossy().to_string(),
    };

    info!(
        "Scan complete: matched={}, unmatched={}",
        result.matched.len(),
        result.unmatched.len()
    );

    Ok(CommandResponse {
        success: true,
        message: Some(format!(
            "Found {}/{} images",
            result.matched.len(),
            result.total_in_manifest
        )),
        data: Some(serde_json::to_value(&result).map_err(|e| {
            error!("Failed to serialize scan result: {}", e);
            format!("Failed to serialize scan result: {}", e)
        })?),
    })
}

/// Package dataset with annotations and images into YOLO format.
///
/// Creates a complete YOLO dataset structure with:
/// - images/ and labels/ directories
/// - train/val/test splits
/// - data.yaml configuration
/// - classes.txt file
///
/// # Arguments
/// * `config` - Packaging configuration
/// * `state` - Application state (unused but kept for consistency)
///
/// # Returns
/// * `Ok(CommandResponse)` - Success with packaging results
/// * `Err(String)` - Error if packaging fails
#[tauri::command]
pub fn package_dataset(
    config: PackageConfig,
    _state: State<Arc<AppState>>,
) -> Result<CommandResponse, String> {
    info!("Packaging dataset to: {}", config.output_path);

    // Validate splits
    let total_split = config.train_split + config.val_split + config.test_split;
    if (total_split - 1.0).abs() > 0.001 {
        return Err(format!(
            "Split percentages must sum to 1.0, got {}",
            total_split
        ));
    }

    // Read manifest
    let manifest_content = fs::read_to_string(&config.manifest_path).map_err(|e| {
        error!("Failed to read manifest: {}", e);
        format!("Failed to read manifest: {}", e)
    })?;

    let manifest: ImageManifest = serde_json::from_str(&manifest_content).map_err(|e| {
        error!("Failed to parse manifest: {}", e);
        format!("Failed to parse manifest: {}", e)
    })?;

    // Open annotation ZIP
    let zip_file = fs::File::open(&config.annotation_zip_path).map_err(|e| {
        error!("Failed to open annotation ZIP: {}", e);
        format!("Failed to open annotation ZIP: {}", e)
    })?;

    let mut archive = ZipArchive::new(zip_file).map_err(|e| {
        error!("Failed to read ZIP archive: {}", e);
        format!("Failed to read ZIP archive: {}", e)
    })?;

    // Create output directory structure
    let output_path = Path::new(&config.output_path);
    fs::create_dir_all(output_path).map_err(|e| {
        error!("Failed to create output directory: {}", e);
        format!("Failed to create output directory: {}", e)
    })?;

    let images_dir = output_path.join("images");
    let labels_dir = output_path.join("labels");

    for split in &["train", "val", "test"] {
        fs::create_dir_all(images_dir.join(split))
            .map_err(|e| format!("Failed to create images/{} directory: {}", split, e))?;
        fs::create_dir_all(labels_dir.join(split))
            .map_err(|e| format!("Failed to create labels/{} directory: {}", split, e))?;
    }

    // Extract all annotations from ZIP to a temporary map
    let mut annotations: HashMap<String, String> = HashMap::new();
    let mut classes_set: std::collections::HashSet<String> = std::collections::HashSet::new();

    for i in 0..archive.len() {
        let mut file = archive
            .by_index(i)
            .map_err(|e| format!("Failed to read ZIP entry {}: {}", i, e))?;

        let filename = file.name().to_string();
        if filename.ends_with(".txt") && filename.contains("labels/") {
            let mut content = String::new();
            file.read_to_string(&mut content)
                .map_err(|e| format!("Failed to read annotation file {}: {}", filename, e))?;

            // Extract class IDs from YOLO format annotations
            for line in content.lines() {
                if let Some(class_id) = line.split_whitespace().next() {
                    classes_set.insert(class_id.to_string());
                }
            }

            // Store annotation with just the filename
            if let Some(name) = Path::new(&filename).file_name() {
                annotations.insert(name.to_string_lossy().to_string(), content);
            }
        }
    }

    info!("Extracted {} annotations from ZIP", annotations.len());

    // Get screenshot base path from storage
    let storage = _state.local_storage.lock().unwrap();
    let screenshot_path = &storage.config.screenshot_path;

    // Build a map of all local images (same as scan_local_images)
    let mut local_images: HashMap<String, PathBuf> = HashMap::new();
    if screenshot_path.exists() {
        for session_entry in fs::read_dir(screenshot_path)
            .map_err(|e| format!("Failed to read screenshot directory: {}", e))?
        {
            let session_entry =
                session_entry.map_err(|e| format!("Failed to read entry: {}", e))?;
            let session_path = session_entry.path();

            if session_path.is_dir() {
                for img_entry in fs::read_dir(&session_path)
                    .map_err(|e| format!("Failed to read session directory: {}", e))?
                {
                    let img_entry =
                        img_entry.map_err(|e| format!("Failed to read entry: {}", e))?;
                    let img_path = img_entry.path();

                    if img_path.is_file() {
                        if let Some(filename) = img_path.file_name() {
                            let filename_str = filename.to_string_lossy().to_string();
                            local_images.insert(filename_str, img_path);
                        }
                    }
                }
            }
        }
    }

    // Prepare matched images
    let mut matched_items: Vec<(PathBuf, String)> = Vec::new();
    for img_meta in &manifest.images {
        if let Some(local_path) = local_images.get(&img_meta.filename) {
            // Get corresponding annotation
            let annotation_filename = img_meta.filename.replace(".png", ".txt");
            if let Some(annotation) = annotations.get(&annotation_filename) {
                matched_items.push((local_path.clone(), annotation.clone()));
            } else {
                warn!("No annotation found for image: {}", img_meta.filename);
            }
        }
    }

    if matched_items.is_empty() {
        return Err("No matched images with annotations found".to_string());
    }

    info!(
        "Found {} matched image-annotation pairs",
        matched_items.len()
    );

    // Shuffle items using random seed
    use rand::seq::SliceRandom;
    use rand::SeedableRng;
    let mut rng = rand::rngs::StdRng::seed_from_u64(config.random_seed);
    matched_items.shuffle(&mut rng);

    // Calculate split indices
    let total = matched_items.len();
    let train_end = (total as f32 * config.train_split).round() as usize;
    let val_end = train_end + (total as f32 * config.val_split).round() as usize;

    let train_items = &matched_items[..train_end];
    let val_items = &matched_items[train_end..val_end];
    let test_items = &matched_items[val_end..];

    // Copy images and annotations
    let copy_split = |items: &[(PathBuf, String)], split: &str| -> Result<(), String> {
        for (img_path, annotation) in items {
            let filename = img_path
                .file_name()
                .ok_or_else(|| "Invalid image filename".to_string())?
                .to_string_lossy()
                .to_string();

            // Copy image
            let dest_img = images_dir.join(split).join(&filename);
            fs::copy(img_path, &dest_img)
                .map_err(|e| format!("Failed to copy image {}: {}", filename, e))?;

            // Write annotation
            let annotation_filename = filename.replace(".png", ".txt");
            let dest_label = labels_dir.join(split).join(&annotation_filename);
            fs::write(&dest_label, annotation).map_err(|e| {
                format!("Failed to write annotation {}: {}", annotation_filename, e)
            })?;
        }
        Ok(())
    };

    copy_split(train_items, "train")?;
    copy_split(val_items, "val")?;
    copy_split(test_items, "test")?;

    info!(
        "Copied images: train={}, val={}, test={}",
        train_items.len(),
        val_items.len(),
        test_items.len()
    );

    // Create classes.txt
    let mut classes: Vec<String> = classes_set.into_iter().collect();
    classes.sort_by(|a, b| {
        a.parse::<u32>()
            .unwrap_or(u32::MAX)
            .cmp(&b.parse::<u32>().unwrap_or(u32::MAX))
    });
    let classes_content = classes.join("\n");
    fs::write(output_path.join("classes.txt"), &classes_content)
        .map_err(|e| format!("Failed to write classes.txt: {}", e))?;

    // Create data.yaml
    let data_yaml = format!(
        r#"# YOLO Dataset Configuration
# Generated by Qontinui Runner

path: {}
train: images/train
val: images/val
test: images/test

nc: {}
names: [{}]
"#,
        output_path.to_string_lossy(),
        classes.len(),
        classes
            .iter()
            .map(|c| format!("'{}'", c))
            .collect::<Vec<_>>()
            .join(", ")
    );

    fs::write(output_path.join("data.yaml"), &data_yaml)
        .map_err(|e| format!("Failed to write data.yaml: {}", e))?;

    let result = PackageResult {
        success: true,
        output_path: output_path.to_string_lossy().to_string(),
        train_count: train_items.len(),
        val_count: val_items.len(),
        test_count: test_items.len(),
        classes,
    };

    info!("Dataset packaged successfully: {} images total", total);

    Ok(CommandResponse {
        success: true,
        message: Some(format!(
            "Dataset packaged successfully: {} images total",
            total
        )),
        data: Some(serde_json::to_value(&result).map_err(|e| {
            error!("Failed to serialize package result: {}", e);
            format!("Failed to serialize package result: {}", e)
        })?),
    })
}
