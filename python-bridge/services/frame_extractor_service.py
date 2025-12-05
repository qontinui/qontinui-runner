"""Frame Extraction Service for qontinui-runner.

This service extracts frames from video files at specific timestamps or input events.
It provides efficient frame extraction using FFmpeg with support for:
- Single frame extraction at exact timestamps
- Batch extraction for multiple timestamps (single FFmpeg pass)
- Event-based extraction with flexible filtering
- Keyframe extraction
- Regular interval extraction

The service is optimized for correlation with InputMonitorEvent data, enabling
extraction of frames at specific user interactions (clicks, keypresses, etc.).

Example:
    >>> from pathlib import Path
    >>> from services.frame_extractor_service import FrameExtractorService, EventFilter
    >>>
    >>> service = FrameExtractorService(Path("/tmp/frames"))
    >>>
    >>> # Extract single frame at timestamp
    >>> frame = service.extract_at_timestamp("video.mp4", 5.25)
    >>>
    >>> # Extract frames at filtered events (e.g., left clicks only)
    >>> filter = EventFilter(event_types=["mouse_click"], buttons=["left"], max_count=10)
    >>> frames = service.extract_at_events("video.mp4", events, filter)
    >>>
    >>> # Save extracted frame
    >>> service.save_frame(frame, "output/frame_001.png", format="png")
"""

import json
import logging
import subprocess
import sys
import tempfile
from dataclasses import dataclass, field
from pathlib import Path

from PIL import Image

from models.extracted_frame import ExtractedFrame
from models.input_event import InputMonitorEvent

# Configure logging
logger = logging.getLogger(__name__)


@dataclass
class EventFilter:
    """Filter configuration for event-based frame extraction.

    Attributes:
        event_types: List of event types to include (e.g., ["mouse_click", "key_press"])
        buttons: List of mouse buttons to include (e.g., ["left", "right", "middle"])
        keys: List of keyboard keys to include (e.g., ["enter", "space", "a"])
        include_after_delay_ms: Extract frame N milliseconds after each event (default: 0)
        max_count: Maximum number of frames to extract (None = unlimited)
        time_range: Optional (start, end) timestamp range in seconds
        modifiers: List of required modifier keys (e.g., ["ctrl", "shift"])
    """

    event_types: list[str] = field(default_factory=lambda: ["mouse_click"])
    buttons: list[str] = field(default_factory=lambda: ["left"])
    keys: list[str] | None = None
    include_after_delay_ms: int = 0
    max_count: int | None = None
    time_range: tuple[float, float] | None = None
    modifiers: list[str] | None = None

    def matches_event(self, event: InputMonitorEvent) -> bool:
        """Check if an event matches this filter.

        Args:
            event: InputMonitorEvent to check

        Returns:
            True if event matches all filter criteria
        """
        # Check event type
        if event.event_type not in self.event_types:
            return False

        # Check button for mouse events
        if event.button is not None:
            if not self.buttons or event.button not in self.buttons:
                return False

        # Check key for keyboard events
        if event.key is not None and self.keys is not None:
            if event.key not in self.keys:
                return False

        # Check modifiers
        if self.modifiers is not None:
            if not all(mod in event.modifiers for mod in self.modifiers):
                return False

        # Check time range
        if self.time_range is not None:
            start, end = self.time_range
            if not (start <= event.timestamp <= end):
                return False

        return True

    def apply_delay(self, timestamp: float) -> float:
        """Apply the configured delay to a timestamp.

        Args:
            timestamp: Original event timestamp

        Returns:
            Timestamp with delay applied (in seconds)
        """
        return timestamp + (self.include_after_delay_ms / 1000.0)


class FrameExtractorService:
    """Service for extracting frames from video files.

    This service provides multiple methods for extracting frames from video files,
    optimized for use with InputMonitorEvent data. It uses FFmpeg for efficient
    frame extraction and supports batch operations to minimize overhead.

    The service manages a storage directory for extracted frames and provides
    both in-memory (PIL Image) and persistent (saved to disk) frame access.

    Attributes:
        storage_dir: Root directory for storing extracted frames
        enabled: Whether the service is enabled
        frames_dir: Subdirectory for frame storage
    """

    def __init__(self, storage_dir: Path, enabled: bool = True) -> None:
        """Initialize the FrameExtractorService.

        Args:
            storage_dir: Root directory for storing extracted frames
            enabled: Whether the service is enabled (default: True)
        """
        self.storage_dir = storage_dir
        self.enabled = enabled
        self.frames_dir = storage_dir / "frames"

        # Create directory structure if enabled
        if self.enabled:
            self.frames_dir.mkdir(parents=True, exist_ok=True)

        logger.info(
            f"FrameExtractorService initialized: storage_dir={storage_dir}, enabled={enabled}"
        )

    def extract_at_timestamp(
        self, video_path: str, timestamp: float, frame_number: int | None = None
    ) -> ExtractedFrame | None:
        """Extract single frame at exact timestamp using FFmpeg.

        This method uses FFmpeg's seek functionality to extract a single frame
        at the specified timestamp. It's efficient for extracting individual frames
        but not optimal for extracting many frames from the same video.

        Args:
            video_path: Path to the video file
            timestamp: Timestamp in seconds (with decimal precision)
            frame_number: Optional frame number to associate with the frame

        Returns:
            ExtractedFrame with the extracted image, or None if extraction fails

        Raises:
            FileNotFoundError: If video file doesn't exist
            RuntimeError: If FFmpeg is not available
        """
        if not self.enabled:
            logger.warning("FrameExtractorService is disabled")
            return None

        video_path_obj = Path(video_path)
        if not video_path_obj.exists():
            raise FileNotFoundError(f"Video file not found: {video_path}")

        try:
            # Create temporary file for extracted frame
            with tempfile.NamedTemporaryFile(suffix=".png", delete=False) as tmp_file:
                tmp_path = tmp_file.name

            # Build FFmpeg command for single frame extraction
            # -ss before -i for faster seeking
            # -vframes 1 extracts exactly one frame
            cmd = [
                "ffmpeg",
                "-ss",
                str(timestamp),
                "-i",
                str(video_path_obj),
                "-vframes",
                "1",
                "-f",
                "image2",
                "-y",  # Overwrite output file
                tmp_path,
            ]

            logger.debug(f"Extracting frame at {timestamp}s from {video_path}")

            # Execute FFmpeg
            result = subprocess.run(
                cmd,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                creationflags=subprocess.CREATE_NO_WINDOW if sys.platform == "win32" else 0,
            )

            if result.returncode != 0:
                error_msg = result.stderr.decode("utf-8", errors="ignore")
                logger.error(f"FFmpeg extraction failed: {error_msg}")
                Path(tmp_path).unlink(missing_ok=True)
                return None

            # Load the extracted frame
            try:
                image = Image.open(tmp_path)
                # Copy to memory and close file
                image = image.copy()
            except Exception as e:
                logger.error(f"Failed to load extracted frame: {e}")
                Path(tmp_path).unlink(missing_ok=True)
                return None
            finally:
                # Clean up temporary file
                Path(tmp_path).unlink(missing_ok=True)

            # Calculate frame number if not provided
            if frame_number is None:
                fps = self._get_video_fps(video_path)
                frame_number = int(timestamp * fps)

            return ExtractedFrame(
                timestamp=timestamp,
                frame_number=frame_number,
                image=image,
                source_event=None,
                path=None,
            )

        except FileNotFoundError:
            logger.error("FFmpeg not found. Please install FFmpeg and ensure it's in PATH")
            raise RuntimeError("FFmpeg not available")
        except Exception as e:
            logger.error(f"Failed to extract frame: {type(e).__name__}: {e}")
            return None

    def extract_at_events(
        self, video_path: str, events: list[InputMonitorEvent], filter: EventFilter | None = None
    ) -> list[ExtractedFrame]:
        """Extract frames at filtered input events.

        This method filters events based on the provided criteria and extracts
        frames at the timestamps of matching events. It uses batch extraction
        for efficiency when multiple frames are needed.

        Args:
            video_path: Path to the video file
            events: List of InputMonitorEvent objects
            filter: EventFilter to apply (uses defaults if None)

        Returns:
            List of ExtractedFrame objects with source_event populated

        Raises:
            FileNotFoundError: If video file doesn't exist
        """
        if not self.enabled:
            logger.warning("FrameExtractorService is disabled")
            return []

        # Use default filter if none provided
        if filter is None:
            filter = EventFilter()

        # Filter events
        filtered_events = []
        for event in events:
            if filter.matches_event(event):
                filtered_events.append(event)

            # Check max count
            if filter.max_count is not None and len(filtered_events) >= filter.max_count:
                break

        if not filtered_events:
            logger.info("No events matched the filter criteria")
            return []

        logger.info(f"Extracting {len(filtered_events)} frames from {len(events)} events")

        # Extract timestamps with delay applied
        timestamps = [filter.apply_delay(event.timestamp) for event in filtered_events]

        # Use batch extraction for efficiency
        frames = self.extract_batch(video_path, timestamps)

        # Associate events with frames
        for frame, event in zip(frames, filtered_events, strict=False):
            frame.source_event = event.to_dict()
            frame.metadata["event_type"] = event.event_type
            frame.metadata["event_button"] = event.button
            frame.metadata["event_key"] = event.key

        return frames

    def extract_batch(self, video_path: str, timestamps: list[float]) -> list[ExtractedFrame]:
        """Batch extract frames at multiple timestamps efficiently.

        This method performs a single FFmpeg pass to extract multiple frames,
        which is significantly more efficient than extracting frames individually
        when you need multiple frames from the same video.

        Args:
            video_path: Path to the video file
            timestamps: List of timestamps in seconds

        Returns:
            List of ExtractedFrame objects in the same order as timestamps

        Raises:
            FileNotFoundError: If video file doesn't exist
        """
        if not self.enabled:
            logger.warning("FrameExtractorService is disabled")
            return []

        if not timestamps:
            return []

        video_path_obj = Path(video_path)
        if not video_path_obj.exists():
            raise FileNotFoundError(f"Video file not found: {video_path}")

        fps = self._get_video_fps(video_path)
        frames = []

        logger.info(f"Batch extracting {len(timestamps)} frames from {video_path}")

        # For batch extraction, we'll use frame numbers and the select filter
        # Convert timestamps to frame numbers
        frame_numbers = [int(ts * fps) for ts in timestamps]

        try:
            # Create temporary directory for batch extraction
            with tempfile.TemporaryDirectory() as tmp_dir:
                tmp_path = Path(tmp_dir)

                # Build select filter expression
                # select='eq(n,100)+eq(n,200)+eq(n,300)' selects frames 100, 200, 300
                select_expr = "+".join([f"eq(n,{fn})" for fn in frame_numbers])

                # Build FFmpeg command
                # Using select filter to extract specific frames
                output_pattern = str(tmp_path / "frame_%04d.png")
                cmd = [
                    "ffmpeg",
                    "-i",
                    str(video_path_obj),
                    "-vf",
                    f"select='{select_expr}'",
                    "-vsync",
                    "0",  # Don't duplicate or drop frames
                    "-f",
                    "image2",
                    "-y",
                    output_pattern,
                ]

                logger.debug(f"FFmpeg batch command: {' '.join(cmd)}")

                # Execute FFmpeg
                result = subprocess.run(
                    cmd,
                    stdout=subprocess.PIPE,
                    stderr=subprocess.PIPE,
                    creationflags=subprocess.CREATE_NO_WINDOW if sys.platform == "win32" else 0,
                )

                if result.returncode != 0:
                    error_msg = result.stderr.decode("utf-8", errors="ignore")
                    logger.error(f"FFmpeg batch extraction failed: {error_msg}")
                    return []

                # Load extracted frames
                extracted_files = sorted(tmp_path.glob("frame_*.png"))

                if len(extracted_files) != len(timestamps):
                    logger.warning(
                        f"Expected {len(timestamps)} frames but got {len(extracted_files)}"
                    )

                for i, (ts, fn) in enumerate(zip(timestamps, frame_numbers, strict=False)):
                    if i < len(extracted_files):
                        try:
                            image = Image.open(extracted_files[i])
                            # Copy to memory before file is deleted
                            image = image.copy()

                            frames.append(
                                ExtractedFrame(
                                    timestamp=ts,
                                    frame_number=fn,
                                    image=image,
                                    source_event=None,
                                    path=None,
                                )
                            )
                        except Exception as e:
                            logger.error(f"Failed to load frame {i}: {e}")
                    else:
                        logger.warning(f"Frame {i} was not extracted")

        except FileNotFoundError:
            logger.error("FFmpeg not found. Please install FFmpeg and ensure it's in PATH")
            raise RuntimeError("FFmpeg not available")
        except Exception as e:
            logger.error(f"Batch extraction failed: {type(e).__name__}: {e}")
            return []

        logger.info(f"Successfully extracted {len(frames)} frames")
        return frames

    def extract_keyframes(self, video_path: str) -> list[ExtractedFrame]:
        """Extract all keyframes (I-frames) from video.

        Keyframes are frames that don't depend on other frames for decoding.
        They're useful for getting an overview of the video content or for
        detecting scene changes.

        Args:
            video_path: Path to the video file

        Returns:
            List of ExtractedFrame objects for each keyframe

        Raises:
            FileNotFoundError: If video file doesn't exist
        """
        if not self.enabled:
            logger.warning("FrameExtractorService is disabled")
            return []

        video_path_obj = Path(video_path)
        if not video_path_obj.exists():
            raise FileNotFoundError(f"Video file not found: {video_path}")

        logger.info(f"Extracting keyframes from {video_path}")

        try:
            # Create temporary directory for keyframes
            with tempfile.TemporaryDirectory() as tmp_dir:
                tmp_path = Path(tmp_dir)
                output_pattern = str(tmp_path / "keyframe_%04d.png")

                # Build FFmpeg command to extract keyframes
                # -skip_frame nokey: Skip all frames except keyframes
                cmd = [
                    "ffmpeg",
                    "-skip_frame",
                    "nokey",
                    "-i",
                    str(video_path_obj),
                    "-vsync",
                    "0",
                    "-f",
                    "image2",
                    "-y",
                    output_pattern,
                ]

                # Execute FFmpeg
                result = subprocess.run(
                    cmd,
                    stdout=subprocess.PIPE,
                    stderr=subprocess.PIPE,
                    creationflags=subprocess.CREATE_NO_WINDOW if sys.platform == "win32" else 0,
                )

                if result.returncode != 0:
                    error_msg = result.stderr.decode("utf-8", errors="ignore")
                    logger.error(f"Keyframe extraction failed: {error_msg}")
                    return []

                # Load extracted keyframes
                fps = self._get_video_fps(video_path)
                frames = []
                extracted_files = sorted(tmp_path.glob("keyframe_*.png"))

                for i, file_path in enumerate(extracted_files):
                    try:
                        image = Image.open(file_path)
                        image = image.copy()

                        # Estimate timestamp (actual keyframe times would require parsing FFmpeg output)
                        # This is an approximation
                        timestamp = i / fps

                        frames.append(
                            ExtractedFrame(
                                timestamp=timestamp,
                                frame_number=i,
                                image=image,
                                source_event=None,
                                path=None,
                                metadata={"is_keyframe": True},
                            )
                        )
                    except Exception as e:
                        logger.error(f"Failed to load keyframe {i}: {e}")

                logger.info(f"Extracted {len(frames)} keyframes")
                return frames

        except FileNotFoundError:
            logger.error("FFmpeg not found. Please install FFmpeg and ensure it's in PATH")
            raise RuntimeError("FFmpeg not available")
        except Exception as e:
            logger.error(f"Keyframe extraction failed: {type(e).__name__}: {e}")
            return []

    def extract_at_interval(
        self, video_path: str, interval_ms: int, max_count: int | None = None
    ) -> list[ExtractedFrame]:
        """Extract frames at regular intervals.

        This method extracts frames at a fixed interval throughout the video,
        useful for sampling video content or creating thumbnails.

        Args:
            video_path: Path to the video file
            interval_ms: Interval in milliseconds between frames
            max_count: Maximum number of frames to extract (None = extract all)

        Returns:
            List of ExtractedFrame objects

        Raises:
            FileNotFoundError: If video file doesn't exist
            ValueError: If interval_ms is not positive
        """
        if not self.enabled:
            logger.warning("FrameExtractorService is disabled")
            return []

        if interval_ms <= 0:
            raise ValueError("interval_ms must be positive")

        video_path_obj = Path(video_path)
        if not video_path_obj.exists():
            raise FileNotFoundError(f"Video file not found: {video_path}")

        # Get video duration
        duration = self._get_video_duration(video_path)
        if duration is None:
            logger.error("Failed to get video duration")
            return []

        # Calculate timestamps at intervals
        interval_sec = interval_ms / 1000.0
        timestamps = []
        current = 0.0

        while current < duration:
            timestamps.append(current)
            if max_count is not None and len(timestamps) >= max_count:
                break
            current += interval_sec

        logger.info(f"Extracting {len(timestamps)} frames at {interval_ms}ms intervals")

        # Use batch extraction
        return self.extract_batch(video_path, timestamps)

    def save_frame(self, frame: ExtractedFrame, output_path: str, format: str = "png") -> str:
        """Save extracted frame to disk.

        Args:
            frame: ExtractedFrame to save
            output_path: File path where image should be saved
            format: Image format (e.g., "png", "jpg", "jpeg")

        Returns:
            Absolute path to the saved file

        Raises:
            ValueError: If frame has no image data
            IOError: If save operation fails
        """
        if not self.enabled:
            logger.warning("FrameExtractorService is disabled")
            return output_path

        if frame.image is None:
            raise ValueError("Frame has no image data to save")

        output_path_obj = Path(output_path)
        output_path_obj.parent.mkdir(parents=True, exist_ok=True)

        try:
            frame.save_image(str(output_path_obj), format=format.upper())
            logger.debug(f"Saved frame to {output_path_obj}")
            return str(output_path_obj.absolute())
        except Exception as e:
            logger.error(f"Failed to save frame: {e}")
            raise OSError(f"Failed to save frame to {output_path}: {e}")

    def save_frames_batch(
        self,
        frames: list[ExtractedFrame],
        output_dir: str,
        filename_pattern: str = "frame_{frame_number:04d}",
        format: str = "png",
    ) -> list[str]:
        """Save multiple frames to disk efficiently.

        Args:
            frames: List of ExtractedFrame objects to save
            output_dir: Directory where images should be saved
            filename_pattern: Pattern for filenames (can include {frame_number}, {timestamp})
            format: Image format (e.g., "png", "jpg")

        Returns:
            List of absolute paths to saved files

        Raises:
            ValueError: If any frame has no image data
            IOError: If save operation fails
        """
        if not self.enabled:
            logger.warning("FrameExtractorService is disabled")
            return []

        output_dir_obj = Path(output_dir)
        output_dir_obj.mkdir(parents=True, exist_ok=True)

        saved_paths = []

        for frame in frames:
            if frame.image is None:
                logger.warning(f"Skipping frame {frame.frame_number} - no image data")
                continue

            # Format filename
            filename = filename_pattern.format(
                frame_number=frame.frame_number, timestamp=frame.timestamp
            )
            filename = f"{filename}.{format.lower()}"
            output_path = output_dir_obj / filename

            try:
                frame.save_image(str(output_path), format=format.upper())
                saved_paths.append(str(output_path.absolute()))
            except Exception as e:
                logger.error(f"Failed to save frame {frame.frame_number}: {e}")

        logger.info(f"Saved {len(saved_paths)} frames to {output_dir}")
        return saved_paths

    def _get_video_fps(self, video_path: str) -> float:
        """Get the frames per second of a video file.

        Args:
            video_path: Path to the video file

        Returns:
            FPS as a float (default: 30.0 if detection fails)
        """
        try:
            cmd = [
                "ffprobe",
                "-v",
                "error",
                "-select_streams",
                "v:0",
                "-show_entries",
                "stream=r_frame_rate",
                "-of",
                "json",
                video_path,
            ]

            result = subprocess.run(
                cmd,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                creationflags=subprocess.CREATE_NO_WINDOW if sys.platform == "win32" else 0,
            )

            if result.returncode == 0:
                output = json.loads(result.stdout.decode("utf-8"))
                fps_str = output["streams"][0]["r_frame_rate"]
                # Parse fraction (e.g., "30000/1001" or "30/1")
                num, den = map(int, fps_str.split("/"))
                return num / den

        except Exception as e:
            logger.warning(f"Failed to detect video FPS: {e}")

        # Default to 30 FPS
        return 30.0

    def _get_video_duration(self, video_path: str) -> float | None:
        """Get the duration of a video file in seconds.

        Args:
            video_path: Path to the video file

        Returns:
            Duration in seconds, or None if detection fails
        """
        try:
            cmd = [
                "ffprobe",
                "-v",
                "error",
                "-select_streams",
                "v:0",
                "-show_entries",
                "format=duration",
                "-of",
                "json",
                video_path,
            ]

            result = subprocess.run(
                cmd,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                creationflags=subprocess.CREATE_NO_WINDOW if sys.platform == "win32" else 0,
            )

            if result.returncode == 0:
                output = json.loads(result.stdout.decode("utf-8"))
                return float(output["format"]["duration"])

        except Exception as e:
            logger.warning(f"Failed to detect video duration: {e}")

        return None

    def cleanup(self) -> None:
        """Clean up resources and temporary files."""
        logger.info("FrameExtractorService cleanup complete")
