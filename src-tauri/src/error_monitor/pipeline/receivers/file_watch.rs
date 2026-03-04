//! File-watching receiver that monitors log files for new content.

use crate::error_monitor::pipeline::traits::Receiver;
use crate::error_monitor::pipeline::types::{LogRecord, SourceMeta};
use crate::error_monitor::types::{LogFormat, LogSourceConfig, ParserType, PathType};
use async_trait::async_trait;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::time::Duration;
use tokio::sync::mpsc::Sender;
use tokio_util::sync::CancellationToken;

/// Tracks position in a watched file.
#[derive(Debug, Clone)]
struct WatchedFile {
    path: PathBuf,
    position: u64,
    source_name: String,
    parser_type: ParserType,
    format: LogFormat,
}

/// Receiver that watches log files for new content via polling.
pub struct FileWatchReceiver {
    watched_files: Vec<WatchedFile>,
    poll_interval: Duration,
}

impl FileWatchReceiver {
    /// Create a new file watch receiver from log source configs.
    pub fn from_sources(sources: &[LogSourceConfig], poll_interval: Duration) -> Self {
        let mut watched = Vec::new();

        for source in sources {
            if !source.enabled {
                continue;
            }

            let paths = match source.path_type {
                PathType::File => vec![PathBuf::from(&source.path)],
                PathType::Glob => glob::glob(&source.path)
                    .map(|p| p.filter_map(|r| r.ok()).collect())
                    .unwrap_or_default(),
                PathType::Directory => std::fs::read_dir(&source.path)
                    .map(|entries| {
                        entries
                            .filter_map(|e| e.ok())
                            .filter(|e| e.path().is_file())
                            .map(|e| e.path())
                            .collect()
                    })
                    .unwrap_or_default(),
            };

            for path in paths {
                let position = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                watched.push(WatchedFile {
                    path,
                    position,
                    source_name: source.name.clone(),
                    parser_type: source.parser.clone(),
                    format: source.format.clone(),
                });
            }
        }

        Self {
            watched_files: watched,
            poll_interval,
        }
    }

    /// Read new content from a file starting at the given position.
    fn read_new_content(path: &PathBuf, position: u64) -> Result<(String, u64), String> {
        let current_size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);

        // Handle truncation (log rotation)
        let actual_position = if current_size < position { 0 } else { position };

        if current_size == actual_position {
            return Ok((String::new(), actual_position));
        }

        let mut file = std::fs::File::open(path)
            .map_err(|e| format!("Failed to open {}: {}", path.display(), e))?;
        file.seek(SeekFrom::Start(actual_position))
            .map_err(|e| format!("Failed to seek: {}", e))?;

        let mut content = String::new();
        file.read_to_string(&mut content)
            .map_err(|e| format!("Failed to read: {}", e))?;

        Ok((content, current_size))
    }
}

#[async_trait]
impl Receiver for FileWatchReceiver {
    fn name(&self) -> &str {
        "file_watch"
    }

    async fn start(&mut self, tx: Sender<Vec<LogRecord>>, cancel: CancellationToken) {
        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = tokio::time::sleep(self.poll_interval) => {
                    for watched in &mut self.watched_files {
                        match Self::read_new_content(&watched.path, watched.position) {
                            Ok((content, new_pos)) => {
                                watched.position = new_pos;
                                if content.is_empty() {
                                    continue;
                                }

                                let records: Vec<LogRecord> = content
                                    .lines()
                                    .filter(|l| !l.trim().is_empty())
                                    .map(|line| {
                                        LogRecord::new(
                                            line.to_string(),
                                            watched.source_name.clone(),
                                            SourceMeta {
                                                parser_type: watched.parser_type.clone(),
                                                format: watched.format.clone(),
                                                path: Some(watched.path.to_string_lossy().to_string()),
                                            },
                                        )
                                    })
                                    .collect();

                                if !records.is_empty() {
                                    if tx.send(records).await.is_err() {
                                        return; // Channel closed
                                    }
                                }
                            }
                            Err(e) => {
                                tracing::debug!(
                                    file = %watched.path.display(),
                                    error = %e,
                                    "Failed to read file"
                                );
                            }
                        }
                    }
                }
            }
        }
    }
}
