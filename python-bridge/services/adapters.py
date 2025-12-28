"""Adapters for converting between qontinui library models and runner DTOs.

This module provides adapter functions to convert between:
1. qontinui.discovery.models.StateImage (discovery/analysis) -> Runner StateImage DTO
2. qontinui.model.state.StateImage (runtime automation) -> Runner StateImage DTO

These adapters ensure clean boundaries between the library and runner while
avoiding model duplication. The runner uses thin DTOs for IPC serialization,
and these adapters handle the conversion to/from the library's richer models.

Usage:
    from services.adapters import (
        discovery_state_image_to_dto,
        runtime_state_image_to_dto,
    )

    # Convert discovery result to DTO for TypeScript
    dto = discovery_state_image_to_dto(discovery_image)

    # Convert runtime pattern to DTO
    dto = runtime_state_image_to_dto(runtime_image)
"""

from __future__ import annotations

import io
import logging
import uuid
from typing import TYPE_CHECKING, Any

import numpy as np
from PIL import Image

from models.detected_state import StateImage as StateImageDTO
from models.detected_state import UIElement as UIElementDTO

if TYPE_CHECKING:
    from qontinui.discovery.models import StateImage as DiscoveryStateImage
    from qontinui.model.state.state_image import StateImage as RuntimeStateImage

logger = logging.getLogger(__name__)


def discovery_state_image_to_dto(
    discovery_image: DiscoveryStateImage,
    generate_id: bool = False,
) -> StateImageDTO:
    """Convert a discovery StateImage to a runner DTO.

    The qontinui.discovery.models.StateImage contains pixel-level data
    from state discovery analysis. This adapter converts it to the runner's
    StateImage DTO which is optimized for IPC serialization.

    Args:
        discovery_image: StateImage from qontinui.discovery.models
        generate_id: If True, generate a new UUID for the DTO

    Returns:
        StateImageDTO suitable for serialization to TypeScript
    """
    # Convert numpy pixel_data to PNG bytes
    image_bytes: bytes | None = None
    pil_image: Image.Image | None = None

    if discovery_image.pixel_data is not None:
        try:
            # Convert BGR (OpenCV) to RGB (PIL)
            if len(discovery_image.pixel_data.shape) == 3:
                rgb_data = discovery_image.pixel_data[:, :, ::-1]
            else:
                rgb_data = discovery_image.pixel_data

            pil_image = Image.fromarray(rgb_data.astype(np.uint8))
            buffer = io.BytesIO()
            pil_image.save(buffer, format="PNG")
            image_bytes = buffer.getvalue()
        except Exception as e:
            logger.warning(f"Failed to convert pixel_data to PNG: {e}")

    # Extract position from coordinates
    position = (discovery_image.x, discovery_image.y)

    # Extract source region (bounding box)
    source_region = (
        discovery_image.x,
        discovery_image.y,
        discovery_image.width,
        discovery_image.height,
    )

    return StateImageDTO(
        id=str(uuid.uuid4()) if generate_id else discovery_image.id,
        name=discovery_image.name,
        image_data=image_bytes,
        image=pil_image,
        position=position,
        is_fixed_position=False,  # Discovery doesn't determine this
        source_region=source_region,
        confidence=discovery_image.frequency,
        similarity_threshold=0.85,  # Default threshold
        metadata={
            "pixel_hash": discovery_image.pixel_hash,
            "screenshot_ids": discovery_image.screenshot_ids,
            "dark_pixel_percentage": discovery_image.dark_pixel_percentage,
            "light_pixel_percentage": discovery_image.light_pixel_percentage,
            "mask_density": discovery_image.mask_density,
        },
    )


def runtime_state_image_to_dto(
    runtime_image: RuntimeStateImage,
    generate_id: bool = True,
) -> StateImageDTO:
    """Convert a runtime StateImage to a runner DTO.

    The qontinui.model.state.StateImage is used during automation execution
    and contains Pattern/Image objects. This adapter extracts the visual
    data and creates a serializable DTO.

    Args:
        runtime_image: StateImage from qontinui.model.state.state_image
        generate_id: If True, generate a new UUID for the DTO

    Returns:
        StateImageDTO suitable for serialization to TypeScript
    """
    image_bytes: bytes | None = None
    pil_image: Image.Image | None = None

    # Try to get image data from the runtime image
    try:
        pattern = runtime_image.get_pattern()
        if hasattr(pattern, "image") and pattern.image is not None:
            # Pattern has an Image object
            if hasattr(pattern.image, "to_pil"):
                pil_image = pattern.image.to_pil()
            elif hasattr(pattern.image, "data"):
                # Raw numpy data
                pil_image = Image.fromarray(pattern.image.data.astype(np.uint8))

            if pil_image is not None:
                buffer = io.BytesIO()
                pil_image.save(buffer, format="PNG")
                image_bytes = buffer.getvalue()
    except Exception as e:
        logger.warning(f"Failed to extract image from runtime StateImage: {e}")

    # Get position from search region if available
    position: tuple[int, int] | None = None
    source_region: tuple[int, int, int, int] | None = None

    if runtime_image._search_region is not None:
        region = runtime_image._search_region
        position = (region.x, region.y)
        source_region = (region.x, region.y, region.w, region.h)

    return StateImageDTO(
        id=str(uuid.uuid4()) if generate_id else (runtime_image.name or str(uuid.uuid4())),
        name=runtime_image.name or "Unnamed",
        image_data=image_bytes,
        image=pil_image,
        position=position,
        is_fixed_position=runtime_image._fixed,
        source_region=source_region,
        confidence=runtime_image._probability,
        similarity_threshold=runtime_image._similarity,
        metadata={
            "shared": runtime_image._shared,
            "owner_state": runtime_image.owner_state_name,
        },
    )


def dto_to_dict(dto: StateImageDTO | UIElementDTO) -> dict[str, Any]:
    """Convert a DTO to a dictionary for JSON serialization.

    This is a convenience wrapper around the DTO's to_dict() method.

    Args:
        dto: StateImageDTO or UIElementDTO instance

    Returns:
        Dictionary ready for JSON serialization
    """
    return dto.to_dict()


def numpy_to_pil(
    array: np.ndarray,
    is_bgr: bool = True,
) -> Image.Image:
    """Convert a numpy array to PIL Image.

    Args:
        array: Numpy array (OpenCV BGR or RGB format)
        is_bgr: If True, convert from BGR to RGB

    Returns:
        PIL Image object
    """
    if is_bgr and len(array.shape) == 3 and array.shape[2] == 3:
        array = array[:, :, ::-1]  # BGR to RGB
    return Image.fromarray(array.astype(np.uint8))


def pil_to_bytes(
    image: Image.Image,
    image_format: str = "PNG",
) -> bytes:
    """Convert a PIL Image to bytes.

    Args:
        image: PIL Image object
        image_format: Image format (PNG, JPEG, etc.)

    Returns:
        Image data as bytes
    """
    buffer = io.BytesIO()
    image.save(buffer, format=image_format)
    return buffer.getvalue()


def numpy_to_bytes(
    array: np.ndarray,
    is_bgr: bool = True,
    image_format: str = "PNG",
) -> bytes:
    """Convert a numpy array directly to image bytes.

    Args:
        array: Numpy array (OpenCV BGR or RGB format)
        is_bgr: If True, convert from BGR to RGB
        image_format: Image format (PNG, JPEG, etc.)

    Returns:
        Image data as bytes
    """
    pil_image = numpy_to_pil(array, is_bgr)
    return pil_to_bytes(pil_image, image_format)
