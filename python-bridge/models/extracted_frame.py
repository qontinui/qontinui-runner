"""ExtractedFrame data model for video capture architecture.

This module defines the ExtractedFrame model, which represents a single frame
extracted from a video capture session. It includes timing information, frame data,
and optional references to the input event that triggered the capture.

The ExtractedFrame serves as the foundational data unit for state detection and
transition analysis in the qontinui-runner capture workflow.
"""

from dataclasses import dataclass, field
from typing import Optional, Dict, Any
from PIL import Image


@dataclass
class ExtractedFrame:
    """Represents a single frame extracted from video capture.

    This model captures both the visual data (as a PIL Image or file path) and
    the temporal context (timestamp, frame number) of a captured frame. It can
    optionally reference the input event that triggered the capture, enabling
    correlation between user actions and visual changes.

    Attributes:
        timestamp: Unix timestamp (seconds) when the frame was captured
        frame_number: Sequential frame number in the capture session (0-indexed)
        image: PIL Image object containing the frame data (None if using path)
        source_event: Optional reference to the InputEvent that triggered this capture
        path: Optional file path to the saved frame image (if persisted to disk)
        metadata: Additional arbitrary data for extensibility
    """

    timestamp: float
    frame_number: int
    image: Optional[Image.Image] = None
    source_event: Optional[Dict[str, Any]] = None  # Serialized InputEvent
    path: Optional[str] = None
    metadata: Dict[str, Any] = field(default_factory=dict)

    def __post_init__(self):
        """Validate that either image or path is provided."""
        if self.image is None and self.path is None:
            raise ValueError("ExtractedFrame must have either 'image' or 'path' specified")

    @property
    def has_image_data(self) -> bool:
        """Check if frame has image data loaded in memory.

        Returns:
            True if image is loaded as PIL Image, False if only path is available
        """
        return self.image is not None

    @property
    def has_source_event(self) -> bool:
        """Check if frame has an associated input event.

        Returns:
            True if source_event is provided and not empty
        """
        return self.source_event is not None and len(self.source_event) > 0

    def load_image(self) -> Image.Image:
        """Load the image from path if not already loaded.

        Returns:
            PIL Image object

        Raises:
            ValueError: If both image and path are None
            FileNotFoundError: If path is specified but file doesn't exist
            IOError: If image file cannot be loaded
        """
        if self.image is not None:
            return self.image

        if self.path is None:
            raise ValueError("Cannot load image: both 'image' and 'path' are None")

        try:
            self.image = Image.open(self.path)
            return self.image
        except FileNotFoundError:
            raise FileNotFoundError(f"Image file not found: {self.path}")
        except Exception as e:
            raise IOError(f"Failed to load image from {self.path}: {e}")

    def to_dict(self) -> Dict[str, Any]:
        """Convert to dictionary for serialization.

        The image data itself is not serialized - only the path is included.
        If you need to serialize the image, save it to disk first and set the path.

        Returns:
            Dictionary containing all serializable fields
        """
        return {
            "timestamp": self.timestamp,
            "frame_number": self.frame_number,
            "source_event": self.source_event,
            "path": self.path,
            "has_image_data": self.has_image_data,
            "metadata": self.metadata,
        }

    @classmethod
    def from_dict(cls, data: Dict[str, Any]) -> "ExtractedFrame":
        """Create ExtractedFrame from dictionary.

        Args:
            data: Dictionary containing frame data (from to_dict or JSON)

        Returns:
            New ExtractedFrame instance

        Raises:
            KeyError: If required fields are missing
            ValueError: If data is invalid
        """
        return cls(
            timestamp=data["timestamp"],
            frame_number=data["frame_number"],
            image=None,  # Image data not serialized
            source_event=data.get("source_event"),
            path=data.get("path"),
            metadata=data.get("metadata", {}),
        )

    def save_image(self, path: str, format: Optional[str] = None) -> None:
        """Save the frame image to disk.

        Args:
            path: File path where image should be saved
            format: Image format (e.g., "PNG", "JPEG"). If None, inferred from path

        Raises:
            ValueError: If image is not loaded
            IOError: If image cannot be saved
        """
        if self.image is None:
            raise ValueError("Cannot save image: no image data loaded")

        try:
            self.image.save(path, format=format)
            self.path = path
        except Exception as e:
            raise IOError(f"Failed to save image to {path}: {e}")

    def __repr__(self) -> str:
        """String representation for debugging."""
        return (
            f"ExtractedFrame(frame_number={self.frame_number}, "
            f"timestamp={self.timestamp:.3f}, "
            f"has_image={self.has_image_data}, "
            f"has_event={self.has_source_event}, "
            f"path={self.path})"
        )
