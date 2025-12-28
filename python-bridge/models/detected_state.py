"""DetectedState data models for video capture architecture.

This module defines the DetectedState, StateImage, and UIElement models used
for representing application states detected from video capture analysis.

A DetectedState represents a distinct visual state of an application, containing:
- Representative frames that exemplify the state
- StateImages: Persistent visual elements that identify the state
- UIElements: Interactive elements with bounding boxes and labels
"""

import base64
from dataclasses import dataclass, field
from io import BytesIO
from typing import Any

from PIL import Image


@dataclass
class StateImage:
    """Represents a visual element that helps identify a state.

    StateImages are persistent visual elements that appear consistently within
    a state. They can be used for state recognition and can have fixed or
    variable positions within the state.

    Attributes:
        id: Unique identifier for this state image
        name: Human-readable name/label for the image
        image_data: Raw image bytes (PNG, JPEG, etc.) or None if using PIL Image
        image: PIL Image object (alternative to image_data)
        position: Optional (x, y) coordinates if image has fixed position
        is_fixed_position: Whether the image always appears at the same position
        source_region: Optional (x, y, w, h) defining where image was extracted from
        confidence: Confidence score for this image (0.0 to 1.0)
        similarity_threshold: Minimum similarity required for matching (0.0 to 1.0)
        metadata: Additional arbitrary data for extensibility
    """

    id: str
    name: str
    image_data: bytes | None = None
    image: Image.Image | None = None
    position: tuple[int, int] | None = None
    is_fixed_position: bool = False
    source_region: tuple[int, int, int, int] | None = None
    confidence: float = 0.0
    similarity_threshold: float = 0.85
    metadata: dict[str, Any] = field(default_factory=dict)

    def __post_init__(self):
        """Validate that either image_data or image is provided."""
        if self.image_data is None and self.image is None:
            raise ValueError("StateImage must have either 'image_data' or 'image' specified")

    @property
    def has_fixed_location(self) -> bool:
        """Check if this image has a fixed position.

        Returns:
            True if is_fixed_position is True and position is provided
        """
        return self.is_fixed_position and self.position is not None

    def get_image(self) -> Image.Image:
        """Get the PIL Image object, loading from bytes if necessary.

        Returns:
            PIL Image object

        Raises:
            ValueError: If neither image nor image_data is available
        """
        if self.image is not None:
            return self.image

        if self.image_data is None:
            raise ValueError("No image data available")

        try:
            self.image = Image.open(BytesIO(self.image_data))
            return self.image
        except Exception as e:
            raise OSError(f"Failed to load image from bytes: {e}") from e

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for serialization.

        Returns:
            Dictionary containing all serializable fields
        """
        result = {
            "id": self.id,
            "name": self.name,
            "position": self.position,
            "is_fixed_position": self.is_fixed_position,
            "source_region": self.source_region,
            "confidence": self.confidence,
            "similarity_threshold": self.similarity_threshold,
            "metadata": self.metadata,
        }

        # Encode image_data as base64 if present
        if self.image_data is not None:
            result["image_data_base64"] = base64.b64encode(self.image_data).decode("utf-8")
        elif self.image is not None:
            # Convert PIL Image to bytes if needed
            buffer = BytesIO()
            self.image.save(buffer, format="PNG")
            result["image_data_base64"] = base64.b64encode(buffer.getvalue()).decode("utf-8")

        return result

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "StateImage":
        """Create StateImage from dictionary.

        Args:
            data: Dictionary containing state image data

        Returns:
            New StateImage instance
        """
        # Decode base64 image data if present
        image_data = None
        if "image_data_base64" in data:
            image_data = base64.b64decode(data["image_data_base64"])

        return cls(
            id=data["id"],
            name=data["name"],
            image_data=image_data,
            image=None,
            position=tuple(data["position"]) if data.get("position") else None,
            is_fixed_position=data.get("is_fixed_position", False),
            source_region=tuple(data["source_region"]) if data.get("source_region") else None,
            confidence=data.get("confidence", 0.0),
            similarity_threshold=data.get("similarity_threshold", 0.85),
            metadata=data.get("metadata", {}),
        )


@dataclass
class UIElement:
    """Represents an interactive UI element detected in a state.

    UIElements are clickable or interactive components like buttons, text fields,
    checkboxes, etc. They include bounding box information for locating the
    element on screen.

    Attributes:
        id: Unique identifier for this UI element
        element_type: Type of element ("button", "text_field", "checkbox", etc.)
        bounding_box: (x, y, w, h) defining element's screen location
        label: Optional text label or description of the element
        confidence: Detection confidence score (0.0 to 1.0)
        clickable: Whether this element can be clicked
        interactable: Whether this element supports user interaction
        metadata: Additional arbitrary data for extensibility
    """

    id: str
    element_type: str
    bounding_box: tuple[int, int, int, int]
    label: str | None = None
    confidence: float = 0.0
    clickable: bool = True
    interactable: bool = True
    metadata: dict[str, Any] = field(default_factory=dict)

    @property
    def center(self) -> tuple[int, int]:
        """Calculate the center point of the element's bounding box.

        Returns:
            (x, y) coordinates of the center point
        """
        x, y, w, h = self.bounding_box
        return (x + w // 2, y + h // 2)

    @property
    def area(self) -> int:
        """Calculate the area of the element's bounding box.

        Returns:
            Area in square pixels
        """
        _, _, w, h = self.bounding_box
        return w * h

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for serialization.

        Returns:
            Dictionary containing all serializable fields
        """
        return {
            "id": self.id,
            "element_type": self.element_type,
            "bounding_box": self.bounding_box,
            "label": self.label,
            "confidence": self.confidence,
            "clickable": self.clickable,
            "interactable": self.interactable,
            "center": self.center,
            "area": self.area,
            "metadata": self.metadata,
        }

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "UIElement":
        """Create UIElement from dictionary.

        Args:
            data: Dictionary containing UI element data

        Returns:
            New UIElement instance
        """
        return cls(
            id=data["id"],
            element_type=data["element_type"],
            bounding_box=tuple(data["bounding_box"]),
            label=data.get("label"),
            confidence=data.get("confidence", 0.0),
            clickable=data.get("clickable", True),
            interactable=data.get("interactable", True),
            metadata=data.get("metadata", {}),
        )


@dataclass
class DetectedState:
    """Represents a distinct application state detected from video analysis.

    A DetectedState corresponds to a specific visual configuration of the
    application, characterized by persistent visual elements (StateImages)
    and interactive components (UIElements). States are identified by
    analyzing frame sequences and detecting visual consistency.

    Attributes:
        id: Unique identifier for this state
        name: Human-readable name for the state
        representative_frame: Frame number that best represents this state
        frame_range: (start, end) frame numbers where this state is active
        similarity_score: Average similarity score across frames (0.0 to 1.0)
        images: List of StateImage objects that identify this state
        elements: List of UIElement objects detected in this state
        confidence: Overall confidence in state detection (0.0 to 1.0)
        description: Optional human-readable description of the state
        metadata: Additional arbitrary data for extensibility
    """

    id: str
    name: str
    representative_frame: int
    frame_range: tuple[int, int]
    similarity_score: float
    images: list[StateImage] = field(default_factory=list)
    elements: list[UIElement] = field(default_factory=list)
    confidence: float = 0.0
    description: str | None = None
    metadata: dict[str, Any] = field(default_factory=dict)

    @property
    def frame_count(self) -> int:
        """Calculate the number of frames in this state's range.

        Returns:
            Number of frames (end - start + 1)
        """
        start, end = self.frame_range
        return end - start + 1

    @property
    def num_images(self) -> int:
        """Get the number of StateImages in this state.

        Returns:
            Count of state images
        """
        return len(self.images)

    @property
    def num_elements(self) -> int:
        """Get the number of UIElements in this state.

        Returns:
            Count of UI elements
        """
        return len(self.elements)

    def get_image_by_id(self, image_id: str) -> StateImage | None:
        """Find a StateImage by its ID.

        Args:
            image_id: ID of the image to find

        Returns:
            StateImage if found, None otherwise
        """
        for image in self.images:
            if image.id == image_id:
                return image
        return None

    def get_element_by_id(self, element_id: str) -> UIElement | None:
        """Find a UIElement by its ID.

        Args:
            element_id: ID of the element to find

        Returns:
            UIElement if found, None otherwise
        """
        for element in self.elements:
            if element.id == element_id:
                return element
        return None

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for serialization.

        Returns:
            Dictionary containing all serializable fields
        """
        return {
            "id": self.id,
            "name": self.name,
            "representative_frame": self.representative_frame,
            "frame_range": self.frame_range,
            "similarity_score": self.similarity_score,
            "confidence": self.confidence,
            "description": self.description,
            "images": [img.to_dict() for img in self.images],
            "elements": [elem.to_dict() for elem in self.elements],
            "num_images": self.num_images,
            "num_elements": self.num_elements,
            "frame_count": self.frame_count,
            "metadata": self.metadata,
        }

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "DetectedState":
        """Create DetectedState from dictionary.

        Args:
            data: Dictionary containing state data

        Returns:
            New DetectedState instance
        """
        return cls(
            id=data["id"],
            name=data["name"],
            representative_frame=data["representative_frame"],
            frame_range=tuple(data["frame_range"]),
            similarity_score=data["similarity_score"],
            confidence=data.get("confidence", 0.0),
            description=data.get("description"),
            images=[StateImage.from_dict(img) for img in data.get("images", [])],
            elements=[UIElement.from_dict(elem) for elem in data.get("elements", [])],
            metadata=data.get("metadata", {}),
        )

    def __repr__(self) -> str:
        """String representation for debugging."""
        return (
            f"DetectedState(id={self.id}, name={self.name}, "
            f"frames={self.frame_range}, images={self.num_images}, "
            f"elements={self.num_elements}, similarity={self.similarity_score:.3f})"
        )
