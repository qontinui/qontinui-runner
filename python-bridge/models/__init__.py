"""Models for the Python bridge.

This module provides data models for the qontinui-runner python-bridge,
including video capture architecture models for state detection and transition
analysis.
"""

# Core execution models
from .action_execution_record import ActionExecutionRecord
from .detected_state import DetectedState, StateImage, UIElement

# Video capture architecture models
from .extracted_frame import ExtractedFrame
from .historical_capture import HistoricalCapture
from .input_event import InputMonitorEvent
from .processing_result import ProcessingLog, ProcessingResult, ProcessingStep

# Legacy/compatibility models
from .state_models import Frame, InputEvent
from .transition import Transition

# Transition analysis helper (if still needed)
try:
    from .transition import StateChangePoint
except ImportError:
    StateChangePoint = None

__all__ = [
    # Core execution models
    "ActionExecutionRecord",
    "HistoricalCapture",
    # Video capture architecture models
    "ExtractedFrame",
    "DetectedState",
    "StateImage",
    "UIElement",
    "Transition",
    "ProcessingResult",
    "ProcessingLog",
    "ProcessingStep",
    # Legacy/compatibility models
    "InputEvent",
    "InputMonitorEvent",
    "Frame",
]

# Add StateChangePoint to exports if available
if StateChangePoint is not None:
    __all__.append("StateChangePoint")
