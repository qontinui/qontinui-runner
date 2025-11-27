"""Analysis module for qontinui-runner.

This module provides tools for analyzing captured data, including:
- StateImage extraction from frames
- Contour detection and UI element identification
- Position analysis (fixed vs. dynamic)
- State boundary detection using visual clustering
- Transition point identification
- Transition analysis and auto-generation
"""

from .image_extractor import (
    ImageExtractionConfig,
    StateImageExtractor,
    save_state_image,
    load_state_image,
)
from .state_boundary_detector import (
    StateBoundaryConfig,
    StateBoundaryDetector,
    FrameFeatures,
    TransitionPoint,
)
from .transition_analyzer import (
    TransitionAnalyzer,
    AutoTransitionBuilder,
    CaptureSession,
)

__all__ = [
    "ImageExtractionConfig",
    "StateImageExtractor",
    "save_state_image",
    "load_state_image",
    "StateBoundaryConfig",
    "StateBoundaryDetector",
    "FrameFeatures",
    "TransitionPoint",
    "TransitionAnalyzer",
    "AutoTransitionBuilder",
    "CaptureSession",
]
