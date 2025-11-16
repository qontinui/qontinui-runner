use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{error, info, warn};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageConfig {
    pub screenshot_path: PathBuf,
    pub video_path: PathBuf,
    pub max_screenshot_mb: u64,
    pub max_video_mb: u64,
    pub auto_cleanup: bool,
}

impl Default for StorageConfig {
    fn default() -> Self {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        let base_path = home.join("qontinui");

        Self {
            screenshot_path: base_path.join("screenshots"),
            video_path: base_path.join("videos"),
            max_screenshot_mb: 500,
            max_video_mb: 500,
            auto_cleanup: true,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct StorageUsage {
    pub screenshots_mb: f64,
    pub videos_mb: f64,
    pub screenshot_count: usize,
    pub video_count: usize,
}

pub struct LocalStorage {
    pub config: StorageConfig,
}

impl LocalStorage {
    pub fn new(config: StorageConfig) -> Result<Self> {
        // Create directories if they don't exist
        fs::create_dir_all(&config.screenshot_path)
            .context("Failed to create screenshot directory")?;
        fs::create_dir_all(&config.video_path).context("Failed to create video directory")?;

        // Create logs directory
        let logs_path = config
            .screenshot_path
            .parent()
            .unwrap_or(Path::new("."))
            .join("logs");
        fs::create_dir_all(&logs_path).context("Failed to create logs directory")?;

        info!(
            "Initialized local storage: screenshots={}, videos={}",
            config.screenshot_path.display(),
            config.video_path.display()
        );

        Ok(Self { config })
    }

    pub fn with_default_config() -> Result<Self> {
        Self::new(StorageConfig::default())
    }

    /// Save a screenshot to disk
    pub fn save_screenshot(
        &self,
        session_id: &str,
        screenshot_id: &str,
        data: &[u8],
    ) -> Result<PathBuf> {
        // Create session directory
        let session_dir = self.config.screenshot_path.join(session_id);
        fs::create_dir_all(&session_dir)
            .context(format!("Failed to create session directory: {:?}", session_dir))?;

        // Create file path
        let file_path = session_dir.join(format!("{}.png", screenshot_id));

        // Check if we need cleanup before saving
        if self.config.auto_cleanup {
            self.cleanup_if_needed("screenshots", data.len() as u64)?;
        }

        // Write file
        fs::write(&file_path, data)
            .context(format!("Failed to write screenshot to {:?}", file_path))?;

        info!(
            "Saved screenshot: session={}, id={}, size={} bytes",
            session_id,
            screenshot_id,
            data.len()
        );

        Ok(file_path)
    }

    /// Save a video to disk
    pub fn save_video(&self, session_id: &str, data: &[u8]) -> Result<PathBuf> {
        // Create file path with timestamp
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let file_path = self
            .config
            .video_path
            .join(format!("{}_{}.mp4", session_id, timestamp));

        // Check if we need cleanup before saving
        if self.config.auto_cleanup {
            self.cleanup_if_needed("videos", data.len() as u64)?;
        }

        // Write file
        fs::write(&file_path, data)
            .context(format!("Failed to write video to {:?}", file_path))?;

        info!(
            "Saved video: session={}, size={} bytes",
            session_id,
            data.len()
        );

        Ok(file_path)
    }

    /// Get storage usage
    pub fn get_storage_usage(&self) -> Result<StorageUsage> {
        let screenshots_info = self.calculate_directory_size(&self.config.screenshot_path)?;
        let videos_info = self.calculate_directory_size(&self.config.video_path)?;

        Ok(StorageUsage {
            screenshots_mb: screenshots_info.0 as f64 / (1024.0 * 1024.0),
            videos_mb: videos_info.0 as f64 / (1024.0 * 1024.0),
            screenshot_count: screenshots_info.1,
            video_count: videos_info.1,
        })
    }

    /// Delete sessions older than specified days
    pub fn delete_old_sessions(&self, storage_type: &str, older_than_days: u32) -> Result<()> {
        let base_path = match storage_type {
            "screenshots" => &self.config.screenshot_path,
            "videos" => &self.config.video_path,
            _ => return Err(anyhow::anyhow!("Invalid storage type: {}", storage_type)),
        };

        let cutoff_time = SystemTime::now()
            - std::time::Duration::from_secs(older_than_days as u64 * 24 * 60 * 60);

        let mut deleted_count = 0;
        let mut deleted_size = 0u64;

        if base_path.exists() {
            if storage_type == "screenshots" {
                // For screenshots, delete session directories
                for entry in fs::read_dir(base_path)? {
                    let entry = entry?;
                    let path = entry.path();

                    if path.is_dir() {
                        if let Ok(metadata) = fs::metadata(&path) {
                            if let Ok(modified) = metadata.modified() {
                                if modified < cutoff_time {
                                    let size = self.calculate_directory_size(&path)?.0;
                                    if let Err(e) = fs::remove_dir_all(&path) {
                                        warn!("Failed to delete directory {:?}: {}", path, e);
                                    } else {
                                        deleted_count += 1;
                                        deleted_size += size;
                                        info!("Deleted old session: {:?}", path);
                                    }
                                }
                            }
                        }
                    }
                }
            } else {
                // For videos, delete individual files
                for entry in fs::read_dir(base_path)? {
                    let entry = entry?;
                    let path = entry.path();

                    if path.is_file() {
                        if let Ok(metadata) = fs::metadata(&path) {
                            if let Ok(modified) = metadata.modified() {
                                if modified < cutoff_time {
                                    let size = metadata.len();
                                    if let Err(e) = fs::remove_file(&path) {
                                        warn!("Failed to delete file {:?}: {}", path, e);
                                    } else {
                                        deleted_count += 1;
                                        deleted_size += size;
                                        info!("Deleted old video: {:?}", path);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        info!(
            "Cleanup complete: deleted {} items, freed {:.2} MB",
            deleted_count,
            deleted_size as f64 / (1024.0 * 1024.0)
        );

        Ok(())
    }

    /// Clear all storage
    pub fn clear_all_storage(&self) -> Result<()> {
        let mut total_deleted = 0u64;

        // Clear screenshots
        if self.config.screenshot_path.exists() {
            let size = self.calculate_directory_size(&self.config.screenshot_path)?.0;
            fs::remove_dir_all(&self.config.screenshot_path)?;
            fs::create_dir_all(&self.config.screenshot_path)?;
            total_deleted += size;
            info!("Cleared screenshots directory");
        }

        // Clear videos
        if self.config.video_path.exists() {
            let size = self.calculate_directory_size(&self.config.video_path)?.0;
            fs::remove_dir_all(&self.config.video_path)?;
            fs::create_dir_all(&self.config.video_path)?;
            total_deleted += size;
            info!("Cleared videos directory");
        }

        info!(
            "Cleared all storage: freed {:.2} MB",
            total_deleted as f64 / (1024.0 * 1024.0)
        );

        Ok(())
    }

    /// Calculate directory size and file count (recursive)
    fn calculate_directory_size(&self, path: &Path) -> Result<(u64, usize)> {
        let mut total_size = 0u64;
        let mut file_count = 0usize;

        if !path.exists() {
            return Ok((0, 0));
        }

        for entry in fs::read_dir(path)? {
            let entry = entry?;
            let path = entry.path();

            if path.is_file() {
                if let Ok(metadata) = fs::metadata(&path) {
                    total_size += metadata.len();
                    file_count += 1;
                }
            } else if path.is_dir() {
                let (dir_size, dir_count) = self.calculate_directory_size(&path)?;
                total_size += dir_size;
                file_count += dir_count;
            }
        }

        Ok((total_size, file_count))
    }

    /// Cleanup old files if needed to make space
    fn cleanup_if_needed(&self, storage_type: &str, new_file_size: u64) -> Result<()> {
        let usage = self.get_storage_usage()?;

        let (current_mb, max_mb) = match storage_type {
            "screenshots" => (usage.screenshots_mb, self.config.max_screenshot_mb),
            "videos" => (usage.videos_mb, self.config.max_video_mb),
            _ => return Ok(()),
        };

        let new_file_mb = new_file_size as f64 / (1024.0 * 1024.0);
        let projected_mb = current_mb + new_file_mb;

        if projected_mb > max_mb as f64 {
            warn!(
                "Storage limit will be exceeded for {}: current={:.2} MB, new file={:.2} MB, limit={} MB",
                storage_type, current_mb, new_file_mb, max_mb
            );

            // Try to free up space by deleting old files (30 days)
            self.delete_old_sessions(storage_type, 30)?;

            // Check again
            let usage = self.get_storage_usage()?;
            let current_mb = match storage_type {
                "screenshots" => usage.screenshots_mb,
                "videos" => usage.videos_mb,
                _ => 0.0,
            };

            if current_mb + new_file_mb > max_mb as f64 {
                warn!(
                    "Still not enough space after cleanup. Current: {:.2} MB, Need: {:.2} MB, Limit: {} MB",
                    current_mb, new_file_mb, max_mb
                );
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    #[test]
    fn test_storage_creation() {
        let temp_dir = TempDir::new().unwrap();
        let config = StorageConfig {
            screenshot_path: temp_dir.path().join("screenshots"),
            video_path: temp_dir.path().join("videos"),
            max_screenshot_mb: 100,
            max_video_mb: 100,
            auto_cleanup: true,
        };

        let storage = LocalStorage::new(config).unwrap();
        assert!(storage.config.screenshot_path.exists());
        assert!(storage.config.video_path.exists());
    }

    #[test]
    fn test_save_screenshot() {
        let temp_dir = TempDir::new().unwrap();
        let config = StorageConfig {
            screenshot_path: temp_dir.path().join("screenshots"),
            video_path: temp_dir.path().join("videos"),
            max_screenshot_mb: 100,
            max_video_mb: 100,
            auto_cleanup: false,
        };

        let storage = LocalStorage::new(config).unwrap();
        let data = b"fake screenshot data";

        let path = storage
            .save_screenshot("session_123", "screenshot_001", data)
            .unwrap();

        assert!(path.exists());
        let saved_data = fs::read(&path).unwrap();
        assert_eq!(saved_data, data);
    }

    #[test]
    fn test_storage_usage() {
        let temp_dir = TempDir::new().unwrap();
        let config = StorageConfig {
            screenshot_path: temp_dir.path().join("screenshots"),
            video_path: temp_dir.path().join("videos"),
            max_screenshot_mb: 100,
            max_video_mb: 100,
            auto_cleanup: false,
        };

        let storage = LocalStorage::new(config).unwrap();

        // Save some test data
        let data = vec![0u8; 1024 * 1024]; // 1 MB
        storage
            .save_screenshot("session_1", "screenshot_1", &data)
            .unwrap();

        let usage = storage.get_storage_usage().unwrap();
        assert!(usage.screenshots_mb >= 1.0);
        assert_eq!(usage.screenshot_count, 1);
    }
}
