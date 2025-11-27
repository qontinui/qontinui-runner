"""Data models for state detection and image extraction.

This module defines the data structures used throughout the state detection
and image extraction pipeline.
"""

from dataclasses import dataclass, field
from typing import Any, Dict, List, Optional, Tuple
import numpy as np


@dataclass
class InputEvent:
    """Represents a user input event (click, keypress, etc.)."""

    timestamp: float
    event_type: str  # 'click', 'key', 'move', etc.
    x: Optional[int] = None
    y: Optional[int] = None
    button: Optional[str] = None
    key: Optional[str] = None
    metadata: Optional[Dict[str, Any]] = None


@dataclass
class Frame:
    """Represents a single frame/screenshot with metadata."""

    image: np.ndarray  # OpenCV image (BGR format)
    timestamp: float
    frame_index: int
    file_path: Optional[str] = None
    metadata: Dict[str, Any] = field(default_factory=dict)


@dataclass
class StateImage:
    """Represents an identifying image extracted from a state.

    StateImages are persistent visual elements that help identify and
    distinguish states. They are typically UI elements like buttons,
    icons, or labels that appear consistently within a state.
    """

    name: str
    image: np.ndarray  # The extracted image region (BGR format)
    bbox: Tuple[int, int, int, int]  # (x, y, width, height)
    position_type: str  # "fixed" or "dynamic"
    position: Tuple[int, int]  # (x, y) coordinates of top-left corner
    similarity_threshold: float = 0.85  # Match threshold for this image
    context_image: Optional[np.ndarray] = None  # Larger context around the image
    source_frame_index: Optional[int] = None  # Frame this was extracted from
    extraction_method: str = "manual"  # How this image was extracted
    metadata: Dict[str, Any] = field(default_factory=dict)

    def to_dict(self) -> Dict[str, Any]:
        """Convert to JSON-serializable dictionary (excluding image data)."""
        return {
            "name": self.name,
            "bbox": self.bbox,
            "position_type": self.position_type,
            "position": self.position,
            "similarity_threshold": self.similarity_threshold,
            "source_frame_index": self.source_frame_index,
            "extraction_method": self.extraction_method,
            "metadata": self.metadata,
        }


@dataclass
class DetectedState:
    """Represents a detected application state.

    A state is a distinct visual configuration of the application,
    characterized by specific UI elements, regions, and interaction points.
    """

    name: str
    description: str
    state_images: List[StateImage]
    start_frame_index: int
    end_frame_index: int
    frame_indices: List[int] = field(default_factory=list)
    boundary: Optional[Tuple[int, int, int, int]] = None  # (x, y, width, height)
    click_locations: List[Tuple[int, int]] = field(default_factory=list)
    transitions_to: List[str] = field(default_factory=list)  # State names
    metadata: Dict[str, Any] = field(default_factory=dict)

    def to_dict(self) -> Dict[str, Any]:
        """Convert to JSON-serializable dictionary."""
        return {
            "name": self.name,
            "description": self.description,
            "state_images": [img.to_dict() for img in self.state_images],
            "start_frame_index": self.start_frame_index,
            "end_frame_index": self.end_frame_index,
            "frame_indices": self.frame_indices,
            "boundary": self.boundary,
            "click_locations": self.click_locations,
            "transitions_to": self.transitions_to,
            "metadata": self.metadata,
        }
