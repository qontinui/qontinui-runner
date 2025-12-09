use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use tracing::{error, info, warn};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoRecordingConfig {
    pub enabled: bool,
    #[serde(rename = "startDelaySeconds")]
    pub start_delay_seconds: u32,
    #[serde(rename = "maxDurationSeconds")]
    pub max_duration_seconds: u32,
    pub fps: u32,
    pub quality: String,
    pub codec: String,
}

impl Default for VideoRecordingConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            start_delay_seconds: 0,
            max_duration_seconds: 300, // 5 minutes
            fps: 15,
            quality: "medium".to_string(),
            codec: "h264".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct VideoRecordingStatus {
    pub recording: bool,
    pub duration: u64,
    pub path: String,
}

struct RecordingState {
    process: Option<Child>,
    start_time: Option<Instant>,
    output_path: Option<PathBuf>,
    config: VideoRecordingConfig,
}

pub struct VideoRecordingService {
    state: Arc<Mutex<RecordingState>>,
}

impl VideoRecordingService {
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(RecordingState {
                process: None,
                start_time: None,
                output_path: None,
                config: VideoRecordingConfig::default(),
            })),
        }
    }

    /// Start recording after configured delay
    pub fn start_recording(
        &self,
        session_id: &str,
        config: VideoRecordingConfig,
    ) -> Result<(), String> {
        let mut state = self.state.lock().map_err(|e| e.to_string())?;

        // Check if already recording
        if state.process.is_some() {
            return Err("Recording already in progress".to_string());
        }

        // Validate configuration
        if config.start_delay_seconds > 60 {
            return Err("Start delay cannot exceed 60 seconds".to_string());
        }

        if config.fps < 15 || config.fps > 30 {
            return Err("FPS must be between 15 and 30".to_string());
        }

        info!(
            "Starting video recording for session: {} with delay: {}s",
            session_id, config.start_delay_seconds
        );

        // Generate output filename
        let timestamp = chrono::Local::now().format("%Y%m%d_%H%M%S");
        let output_path = self
            .get_output_directory()
            .join(format!("qontinui_{}_{}.mp4", session_id, timestamp));

        // Store config and prepare state
        state.config = config.clone();
        state.output_path = Some(output_path.clone());

        // Clone for thread
        let state_clone = self.state.clone();
        let delay = config.start_delay_seconds;
        let output_path_clone = output_path.clone();

        // Spawn thread to handle delay and start recording
        thread::spawn(move || {
            if delay > 0 {
                info!("Waiting {}s before starting recording...", delay);
                thread::sleep(Duration::from_secs(delay as u64));
            }

            if let Err(e) = Self::start_ffmpeg_recording(&state_clone, output_path_clone) {
                error!("Failed to start FFmpeg recording: {}", e);
            }
        });

        Ok(())
    }

    /// Stop the current recording
    pub fn stop_recording(&self) -> Result<String, String> {
        let mut state = self.state.lock().map_err(|e| e.to_string())?;

        if state.process.is_none() {
            return Err("No recording in progress".to_string());
        }

        info!("Stopping video recording");

        // Kill the FFmpeg process gracefully
        if let Some(mut process) = state.process.take() {
            // Send 'q' to FFmpeg stdin to gracefully quit
            if let Some(stdin) = process.stdin.as_mut() {
                use std::io::Write;
                let _ = stdin.write_all(b"q");
                let _ = stdin.flush();
            }

            // Wait for process to exit (with timeout)
            let timeout = Duration::from_secs(5);
            let start = Instant::now();

            loop {
                match process.try_wait() {
                    Ok(Some(_)) => {
                        info!("FFmpeg process exited successfully");
                        break;
                    }
                    Ok(None) => {
                        if start.elapsed() > timeout {
                            warn!("FFmpeg did not exit gracefully, killing process");
                            let _ = process.kill();
                            break;
                        }
                        thread::sleep(Duration::from_millis(100));
                    }
                    Err(e) => {
                        error!("Error waiting for FFmpeg: {}", e);
                        let _ = process.kill();
                        break;
                    }
                }
            }
        }

        let output_path = state
            .output_path
            .take()
            .ok_or("No output path set".to_string())?
            .to_string_lossy()
            .to_string();

        state.start_time = None;

        info!("Recording stopped. Video saved to: {}", output_path);
        Ok(output_path)
    }

    /// Get current recording status
    pub fn get_status(&self) -> Result<VideoRecordingStatus, String> {
        let state = self.state.lock().map_err(|e| e.to_string())?;

        let recording = state.process.is_some();
        let duration = if let Some(start_time) = state.start_time {
            start_time.elapsed().as_secs()
        } else {
            0
        };
        let path = state
            .output_path
            .as_ref()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();

        Ok(VideoRecordingStatus {
            recording,
            duration,
            path,
        })
    }

    /// Internal method to start FFmpeg recording process
    fn start_ffmpeg_recording(
        state: &Arc<Mutex<RecordingState>>,
        output_path: PathBuf,
    ) -> Result<(), String> {
        let mut state_guard = state.lock().map_err(|e| e.to_string())?;
        let config = state_guard.config.clone();

        // Build FFmpeg command based on platform
        let ffmpeg_args = Self::build_ffmpeg_args(&config, &output_path)?;

        info!("Starting FFmpeg with args: {:?}", ffmpeg_args);

        // Start FFmpeg process
        let mut command = Command::new("ffmpeg");
        command
            .args(&ffmpeg_args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());

        let process = command.spawn().map_err(|e| {
            format!(
                "Failed to start FFmpeg: {}. Make sure FFmpeg is installed and in PATH.",
                e
            )
        })?;

        state_guard.process = Some(process);
        state_guard.start_time = Some(Instant::now());

        info!("FFmpeg recording started successfully");

        // If max duration is set, schedule auto-stop
        if config.max_duration_seconds > 0 {
            let state_clone = state.clone();
            let duration = config.max_duration_seconds;
            thread::spawn(move || {
                thread::sleep(Duration::from_secs(duration as u64));
                info!("Max duration reached, stopping recording");

                let service = VideoRecordingService { state: state_clone };

                if let Err(e) = service.stop_recording() {
                    error!("Failed to auto-stop recording: {}", e);
                }
            });
        }

        Ok(())
    }

    /// Build FFmpeg command arguments based on platform and configuration
    fn build_ffmpeg_args(
        config: &VideoRecordingConfig,
        output_path: &Path,
    ) -> Result<Vec<String>, String> {
        let mut args = Vec::new();

        // Overwrite output file without asking
        args.push("-y".to_string());

        // Platform-specific screen capture settings
        #[cfg(target_os = "windows")]
        {
            // Use gdigrab for Windows screen capture
            args.push("-f".to_string());
            args.push("gdigrab".to_string());
            args.push("-framerate".to_string());
            args.push(config.fps.to_string());
            args.push("-i".to_string());
            args.push("desktop".to_string());
        }

        #[cfg(target_os = "macos")]
        {
            // Use avfoundation for macOS screen capture
            args.push("-f".to_string());
            args.push("avfoundation".to_string());
            args.push("-framerate".to_string());
            args.push(config.fps.to_string());
            args.push("-i".to_string());
            args.push("1".to_string()); // Screen capture device
        }

        #[cfg(target_os = "linux")]
        {
            // Use x11grab for Linux screen capture
            args.push("-f".to_string());
            args.push("x11grab".to_string());
            args.push("-framerate".to_string());
            args.push(config.fps.to_string());
            args.push("-i".to_string());
            args.push(":0.0".to_string()); // Default X11 display
        }

        // Codec settings
        args.push("-c:v".to_string());
        args.push(Self::get_codec_name(&config.codec));

        // Quality settings (CRF for h264/h265)
        let crf = match config.quality.as_str() {
            "low" => "28",    // Lower quality, smaller file
            "medium" => "23", // Balanced
            "high" => "18",   // Higher quality, larger file
            _ => "23",        // Default to medium
        };
        args.push("-crf".to_string());
        args.push(crf.to_string());

        // Preset for encoding speed
        args.push("-preset".to_string());
        args.push("ultrafast".to_string()); // Prioritize performance during recording

        // Pixel format
        args.push("-pix_fmt".to_string());
        args.push("yuv420p".to_string());

        // Output file
        args.push(output_path.to_string_lossy().to_string());

        Ok(args)
    }

    /// Get FFmpeg codec name from config
    fn get_codec_name(codec: &str) -> String {
        match codec {
            "h264" => "libx264".to_string(),
            "h265" => "libx265".to_string(),
            "vp9" => "libvpx-vp9".to_string(),
            _ => "libx264".to_string(), // Default to h264
        }
    }

    /// Get output directory for recordings
    fn get_output_directory(&self) -> PathBuf {
        // Use user's videos directory or temp directory as fallback
        if let Some(videos_dir) = dirs::video_dir() {
            let qontinui_dir = videos_dir.join("qontinui-recordings");
            // Create directory if it doesn't exist
            let _ = std::fs::create_dir_all(&qontinui_dir);
            qontinui_dir
        } else {
            std::env::temp_dir()
        }
    }
}

impl Drop for VideoRecordingService {
    fn drop(&mut self) {
        // Ensure recording is stopped when service is dropped
        if let Ok(state) = self.state.lock() {
            if state.process.is_some() {
                drop(state); // Release lock before calling stop_recording
                let _ = self.stop_recording();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = VideoRecordingConfig::default();
        assert_eq!(config.enabled, false);
        assert_eq!(config.start_delay_seconds, 0);
        assert_eq!(config.max_duration_seconds, 300);
        assert_eq!(config.fps, 15);
        assert_eq!(config.quality, "medium");
        assert_eq!(config.codec, "h264");
    }

    #[test]
    fn test_service_creation() {
        let service = VideoRecordingService::new();
        let status = service.get_status().unwrap();
        assert_eq!(status.recording, false);
        assert_eq!(status.duration, 0);
    }
}
