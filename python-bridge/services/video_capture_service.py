"""Video Capture Service for qontinui-runner.

This service replaces screenshot capture with continuous video recording using FFmpeg.
It provides dual output streams: full quality local recording and compressed cloud stream.

The service handles:
- Platform-specific screen capture (gdigrab for Windows, avfoundation for macOS, x11grab for Linux)
- H.264 video encoding with configurable quality settings
- Frame timestamp metadata for correlation with input events
- Dual output: local full-quality + compressed stream for cloud upload
- Proper resource cleanup and error handling

Directory Structure:
    session_dir/
        recordings/
            session-{session_id}-local.mp4    (full quality)
            session-{session_id}-stream.mp4   (compressed)
            session-{session_id}-metadata.json
"""

import json
import logging
import platform
import subprocess
import sys
import threading
import time
from dataclasses import dataclass, field
from datetime import datetime
from pathlib import Path
from typing import Any

# Configure logging
logger = logging.getLogger(__name__)


@dataclass
class VideoCaptureConfig:
    """Configuration for video capture settings.

    Attributes:
        fps: Frames per second for recording (default: 30)
        codec: Video codec to use (default: h264)
        quality: Quality preset - affects CRF value (default: high)
                CRF 18 for local, CRF 28 for cloud
        resolution: Resolution setting (default: native)
        audio: Whether to capture audio (default: False)
    """

    fps: int = 30
    codec: str = "h264"
    quality: str = "high"  # high, medium, low
    resolution: str = "native"
    audio: bool = False

    def get_crf_value(self, stream_type: str = "local") -> int:
        """Get CRF value based on quality and stream type.

        Args:
            stream_type: Either "local" or "stream"

        Returns:
            CRF value (lower = better quality, higher file size)
        """
        quality_map = {
            "high": {"local": 18, "stream": 28},
            "medium": {"local": 23, "stream": 30},
            "low": {"local": 28, "stream": 32},
        }
        return quality_map.get(self.quality, quality_map["high"])[stream_type]


@dataclass
class CompressedStreamConfig:
    """Configuration for compressed stream output.

    Attributes:
        fps: Frames per second for stream (default: 10)
        resolution: Target resolution as (width, height) tuple (default: 1280x720)
        bitrate: Target bitrate (default: 1M)
    """

    fps: int = 10
    resolution: tuple[int, int] = (1280, 720)
    bitrate: str = "1M"


@dataclass
class VideoRecordingSession:
    """Represents an active video recording session.

    Attributes:
        session_id: Unique identifier for the recording session
        start_time: Unix timestamp when recording started
        local_output_path: Path to local full-quality output file
        stream_output_path: Path to compressed stream output file
        metadata_path: Path to metadata JSON file
        process: FFmpeg subprocess handle
        frame_timestamps: List of frame timestamps for correlation
        config: Video capture configuration used
    """

    session_id: str
    start_time: float
    local_output_path: Path
    stream_output_path: Path
    metadata_path: Path
    process: subprocess.Popen | None = None
    frame_timestamps: list[float] = field(default_factory=list)
    config: VideoCaptureConfig | None = None


class VideoCaptureService:
    """Service for continuous video recording using FFmpeg.

    This service manages video recording sessions with dual output:
    1. Local full-quality recording (CRF 18, native resolution)
    2. Compressed stream for cloud upload (CRF 28, reduced resolution/fps)

    The service handles platform-specific capture methods and maintains
    frame timestamp metadata for synchronization with input events.

    Example:
        >>> from pathlib import Path
        >>> service = VideoCaptureService(Path("/tmp/recordings"))
        >>> config = VideoCaptureConfig(fps=30, quality="high")
        >>> success = service.start_recording("session-001", config)
        >>> if success:
        ...     # Recording is active
        ...     time.sleep(10)
        ...     video_path = service.stop_recording()
        ...     print(f"Recording saved to: {video_path}")
    """

    def __init__(self, storage_dir: Path, enabled: bool = True) -> None:
        """Initialize the VideoCaptureService.

        Args:
            storage_dir: Root directory for storing video recordings
            enabled: Whether the service is enabled (default: True)
        """
        self.storage_dir = storage_dir
        self.enabled = enabled
        self.recordings_dir = storage_dir / "recordings"
        self._current_session: VideoRecordingSession | None = None
        self._recording_lock = threading.Lock()
        self._timestamp_thread: threading.Thread | None = None
        self._stop_timestamp_tracking = threading.Event()

        # Create directory structure if enabled
        if self.enabled:
            self.recordings_dir.mkdir(parents=True, exist_ok=True)

        # Detect platform for capture method
        self._platform = platform.system()
        logger.info(f"VideoCaptureService initialized for platform: {self._platform}")

    def start_recording(
        self,
        session_id: str,
        config: VideoCaptureConfig | None = None,
        stream_config: CompressedStreamConfig | None = None,
    ) -> bool:
        """Start a new video recording session.

        This method initiates FFmpeg with dual output streams:
        - Local: Full quality recording for local storage
        - Stream: Compressed recording optimized for cloud upload

        Args:
            session_id: Unique identifier for this recording session
            config: Video capture configuration (uses defaults if None)
            stream_config: Compressed stream configuration (uses defaults if None)

        Returns:
            True if recording started successfully, False otherwise
        """
        if not self.enabled:
            logger.warning("VideoCaptureService is disabled")
            return False

        with self._recording_lock:
            if self._current_session is not None:
                logger.error("Cannot start recording: session already active")
                return False

            # Use default configs if not provided
            config = config or VideoCaptureConfig()
            stream_config = stream_config or CompressedStreamConfig()

            try:
                # Create output paths
                local_path = self.recordings_dir / f"session-{session_id}-local.mp4"
                stream_path = self.recordings_dir / f"session-{session_id}-stream.mp4"
                metadata_path = self.recordings_dir / f"session-{session_id}-metadata.json"

                # Build FFmpeg command
                ffmpeg_cmd = self._build_ffmpeg_command(
                    local_path, stream_path, config, stream_config
                )

                logger.info(f"Starting FFmpeg recording for session {session_id}")
                logger.debug(f"FFmpeg command: {' '.join(ffmpeg_cmd)}")

                # Start FFmpeg process
                process = subprocess.Popen(
                    ffmpeg_cmd,
                    stdout=subprocess.PIPE,
                    stderr=subprocess.PIPE,
                    stdin=subprocess.PIPE,
                    creationflags=subprocess.CREATE_NO_WINDOW if sys.platform == "win32" else 0,
                )

                # Wait a moment to check if process started successfully
                time.sleep(0.5)
                if process.poll() is not None:
                    # Process terminated immediately - capture error
                    _, stderr = process.communicate()
                    error_msg = stderr.decode("utf-8", errors="ignore")
                    logger.error(f"FFmpeg failed to start: {error_msg}")
                    return False

                # Create recording session
                self._current_session = VideoRecordingSession(
                    session_id=session_id,
                    start_time=time.time(),
                    local_output_path=local_path,
                    stream_output_path=stream_path,
                    metadata_path=metadata_path,
                    process=process,
                    config=config,
                )

                # Start timestamp tracking thread
                self._start_timestamp_tracking()

                # Write initial metadata
                self._write_metadata()

                logger.info(f"Recording started successfully for session {session_id}")
                return True

            except FileNotFoundError:
                logger.error("FFmpeg not found. Please install FFmpeg and ensure it's in PATH")
                return False
            except Exception as e:
                logger.error(f"Failed to start recording: {type(e).__name__}: {e}")
                import traceback

                traceback.print_exc(file=sys.stderr)
                return False

    def stop_recording(self) -> str | None:
        """Stop the current recording session.

        This method gracefully stops the FFmpeg process and finalizes the recording.
        It returns the path to the local full-quality video file.

        Returns:
            Path to the local video file, or None if no recording was active
        """
        with self._recording_lock:
            if self._current_session is None:
                logger.warning("No active recording session to stop")
                return None

            try:
                session = self._current_session
                logger.info(f"Stopping recording for session {session.session_id}")

                # Stop timestamp tracking
                self._stop_timestamp_tracking.set()
                if self._timestamp_thread and self._timestamp_thread.is_alive():
                    self._timestamp_thread.join(timeout=2.0)

                # Send 'q' to FFmpeg to gracefully stop
                if session.process and session.process.poll() is None:
                    try:
                        if session.process.stdin is not None:
                            session.process.stdin.write(b"q")
                            session.process.stdin.flush()
                    except Exception as e:
                        logger.warning(f"Failed to send quit command to FFmpeg: {e}")

                    # Wait for process to finish (with timeout)
                    try:
                        session.process.wait(timeout=5.0)
                    except subprocess.TimeoutExpired:
                        logger.warning("FFmpeg did not stop gracefully, terminating")
                        session.process.terminate()
                        session.process.wait(timeout=2.0)

                # Update final metadata
                self._write_metadata(end_time=time.time())

                local_path = str(session.local_output_path)

                # Clear current session
                self._current_session = None
                self._stop_timestamp_tracking.clear()

                logger.info(f"Recording stopped. Output: {local_path}")
                return local_path

            except Exception as e:
                logger.error(f"Error stopping recording: {type(e).__name__}: {e}")
                import traceback

                traceback.print_exc(file=sys.stderr)
                return None

    def get_frame_timestamp(self, frame_number: int) -> float | None:
        """Get the timestamp for a specific frame number.

        This method allows correlating video frames with input events
        by providing the timestamp when a particular frame was captured.

        Args:
            frame_number: Frame number (0-based index)

        Returns:
            Unix timestamp for the frame, or None if invalid frame number
        """
        if self._current_session is None:
            return None

        if 0 <= frame_number < len(self._current_session.frame_timestamps):
            return self._current_session.frame_timestamps[frame_number]

        return None

    def is_recording(self) -> bool:
        """Check if a recording session is currently active.

        Returns:
            True if recording is active, False otherwise
        """
        with self._recording_lock:
            if self._current_session is None:
                return False

            # Check if FFmpeg process is still running
            if self._current_session.process:
                return self._current_session.process.poll() is None

            return False

    def get_current_session_info(self) -> dict[str, Any] | None:
        """Get information about the current recording session.

        Returns:
            Dictionary with session information, or None if no active session
        """
        with self._recording_lock:
            if self._current_session is None:
                return None

            session = self._current_session
            return {
                "session_id": session.session_id,
                "start_time": session.start_time,
                "duration": time.time() - session.start_time,
                "frame_count": len(session.frame_timestamps),
                "local_output": str(session.local_output_path),
                "stream_output": str(session.stream_output_path),
                "is_running": self.is_recording(),
            }

    def cleanup(self) -> None:
        """Clean up resources and stop any active recording."""
        if self.is_recording():
            logger.info("Cleaning up: stopping active recording")
            self.stop_recording()

    def _build_ffmpeg_command(
        self,
        local_path: Path,
        stream_path: Path,
        config: VideoCaptureConfig,
        stream_config: CompressedStreamConfig,
    ) -> list[str]:
        """Build the FFmpeg command for dual-output recording.

        Args:
            local_path: Output path for local full-quality file
            stream_path: Output path for compressed stream file
            config: Video capture configuration
            stream_config: Compressed stream configuration

        Returns:
            List of command arguments for FFmpeg
        """
        cmd = ["ffmpeg"]

        # Platform-specific input configuration
        if self._platform == "Windows":
            # Windows: gdigrab
            cmd.extend(["-f", "gdigrab", "-framerate", str(config.fps), "-i", "desktop"])
        elif self._platform == "Darwin":
            # macOS: avfoundation
            # Note: May require screen recording permissions
            cmd.extend(
                [
                    "-f",
                    "avfoundation",
                    "-framerate",
                    str(config.fps),
                    "-i",
                    "1:none",  # Capture screen 1, no audio
                ]
            )
        elif self._platform == "Linux":
            # Linux: x11grab
            cmd.extend(
                ["-f", "x11grab", "-framerate", str(config.fps), "-i", ":0.0"]  # Display :0.0
            )
        else:
            raise RuntimeError(f"Unsupported platform: {self._platform}")

        # Audio configuration
        if config.audio:
            # Platform-specific audio input would go here
            # For now, skip audio implementation
            pass

        # Common encoding settings
        local_crf = config.get_crf_value("local")
        stream_crf = config.get_crf_value("stream")

        # Local output: full quality
        cmd.extend(
            [
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",  # Faster encoding for real-time
                "-crf",
                str(local_crf),
                "-pix_fmt",
                "yuv420p",
                str(local_path),
            ]
        )

        # Compressed stream output: lower quality/resolution
        stream_width, stream_height = stream_config.resolution
        cmd.extend(
            [
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-crf",
                str(stream_crf),
                "-vf",
                f"fps={stream_config.fps},scale={stream_width}:{stream_height}",
                "-b:v",
                stream_config.bitrate,
                "-pix_fmt",
                "yuv420p",
                str(stream_path),
            ]
        )

        return cmd

    def _start_timestamp_tracking(self) -> None:
        """Start background thread to track frame timestamps."""
        self._stop_timestamp_tracking.clear()
        self._timestamp_thread = threading.Thread(target=self._track_timestamps, daemon=True)
        self._timestamp_thread.start()

    def _track_timestamps(self) -> None:
        """Background thread function to track frame timestamps.

        This thread records timestamps at the configured FPS rate to maintain
        frame-to-timestamp correlation for event synchronization.
        """
        if self._current_session is None:
            return

        fps = self._current_session.config.fps if self._current_session.config else 30
        frame_interval = 1.0 / fps

        logger.debug(f"Starting timestamp tracking at {fps} FPS")

        while not self._stop_timestamp_tracking.is_set():
            if self._current_session:
                timestamp = time.time()
                self._current_session.frame_timestamps.append(timestamp)

            # Sleep until next frame
            time.sleep(frame_interval)

        logger.debug("Timestamp tracking stopped")

    def _write_metadata(self, end_time: float | None = None) -> None:
        """Write session metadata to JSON file.

        Args:
            end_time: Optional end time for final metadata update
        """
        if self._current_session is None:
            return

        session = self._current_session

        metadata = {
            "session_id": session.session_id,
            "start_time": session.start_time,
            "start_time_iso": datetime.fromtimestamp(session.start_time).isoformat(),
            "platform": self._platform,
            "local_output": str(session.local_output_path),
            "stream_output": str(session.stream_output_path),
            "frame_count": len(session.frame_timestamps),
            "config": {
                "fps": session.config.fps if session.config else 30,
                "codec": session.config.codec if session.config else "h264",
                "quality": session.config.quality if session.config else "high",
                "resolution": session.config.resolution if session.config else "native",
                "audio": session.config.audio if session.config else False,
            },
        }

        if end_time is not None:
            metadata["end_time"] = end_time
            metadata["end_time_iso"] = datetime.fromtimestamp(end_time).isoformat()
            metadata["duration_seconds"] = end_time - session.start_time

        try:
            session.metadata_path.write_text(
                json.dumps(metadata, indent=2, ensure_ascii=False), encoding="utf-8"
            )
        except Exception as e:
            logger.error(f"Failed to write metadata: {e}")
