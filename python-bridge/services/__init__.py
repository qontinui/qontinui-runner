"""Services for the Python bridge."""

from .ai_test_generator import AiTestGeneratorService
from .frame_extractor_service import EventFilter, FrameExtractorService
from .input_monitor_service import InputMonitorService
from .screenshot_service import ScreenshotService
from .state_detection_service import LocalStateDetectionService
from .test_analysis_service import TestAnalysisService
from .unified_data_collector import UnifiedDataCollector
from .video_capture_service import VideoCaptureService


def __getattr__(name: str):
    """Lazy import for IntegrationTestingService to avoid triggering qontinui HAL init at startup."""
    if name == "IntegrationTestingService":
        from .integration_testing_service import IntegrationTestingService

        return IntegrationTestingService
    raise AttributeError(f"module {__name__!r} has no attribute {name!r}")


__all__ = [
    "ScreenshotService",
    "UnifiedDataCollector",
    "LocalStateDetectionService",
    "VideoCaptureService",
    "InputMonitorService",
    "FrameExtractorService",
    "EventFilter",
    "TestAnalysisService",
    "AiTestGeneratorService",
    "IntegrationTestingService",
]
