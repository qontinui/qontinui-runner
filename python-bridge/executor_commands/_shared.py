"""Module-level state shared by ``qontinui_executor.py`` and the command mixins.

This module is the single owner of two import-time checks that used to sit at the top
of ``qontinui_executor.py``:

- the all-or-nothing import of the services that may need the heavy ML stack (torch,
  easyocr, ...), leaving ``None`` placeholders for all of them when any one fails, and
- the ``qontinui`` library availability check (``QONTINUI_AVAILABLE``).

Neither value is reassigned after import. ``qontinui_executor.py`` imports
``executor_commands`` at the point these blocks used to run, so the import order is
unchanged; it still prints the "library not available" event itself, from
``QONTINUI_IMPORT_ERROR``. Mixins must never import ``qontinui_executor``: it runs as
``__main__``, so importing it by name would execute the whole module a second time.
"""

import logging
import traceback
from pathlib import Path

logger = logging.getLogger(__name__)

# What ``Path(__file__).parent.parent.parent`` resolved to inside qontinui_executor.py
# (the directory above the runner checkout, where ``.dev-logs`` lives). This file sits
# one directory deeper, in the same bundle layout both from source and when frozen.
RUNNER_PARENT_DIR = Path(__file__).parent.parent.parent.parent

# Services that may require heavy ML dependencies (torch, easyocr, etc.)
# Import them gracefully so the executor can start without the full ML stack.
_ML_SERVICES_AVAILABLE = True
try:
    from gui_automation import GUIAutomation
    from services.accessibility_capture_service import (
        AccessibilityCaptureService,
    )
    from services.ai_builder_generator import AiBuilderGeneratorService
    from services.ai_shell_command_generator import (
        AiShellCommandGeneratorService,
    )
    from services.ai_test_generator import AiTestGeneratorService
    from services.input_monitor_service import InputMonitorService
    from services.integration_testing_service import (
        IntegrationTestingService,
    )
    from services.model_manager import get_model_manager
    from services.pattern_matching_service import (
        get_pattern_matching_service,
    )
    from services.playwright_collector_service import (
        PlaywrightCollectorService,
    )
    from services.screenshot_service import ScreenshotService
    from services.test_analysis_service import TestAnalysisService
    from services.uitars_extraction_service import (
        UITarsExtractionService,
        get_uitars_extraction_service,
    )
    from services.unified_data_collector import UnifiedDataCollector
    from services.vision_extraction_service import VisionExtractionService
    from services.web_extraction_service import WebExtractionService
except ImportError as _ml_import_err:
    _ML_SERVICES_AVAILABLE = False
    _ml_import_error_msg = f"{type(_ml_import_err).__name__}: {_ml_import_err}"
    logger.warning(f"ML services not available (non-fatal): {_ml_import_error_msg}")
    # Provide None placeholders for unavailable services
    GUIAutomation = None  # type: ignore[assignment, misc]
    AccessibilityCaptureService = None  # type: ignore[assignment, misc]
    AiBuilderGeneratorService = None  # type: ignore[assignment, misc]
    AiShellCommandGeneratorService = None  # type: ignore[assignment, misc]
    AiTestGeneratorService = None  # type: ignore[assignment, misc]
    InputMonitorService = None  # type: ignore[assignment, misc]
    IntegrationTestingService = None  # type: ignore[assignment, misc]
    get_model_manager = None  # type: ignore[assignment, misc]
    get_pattern_matching_service = None  # type: ignore[assignment, misc]
    PlaywrightCollectorService = None  # type: ignore[assignment, misc]
    ScreenshotService = None  # type: ignore[assignment, misc]
    TestAnalysisService = None  # type: ignore[assignment, misc]
    UITarsExtractionService = None  # type: ignore[assignment, misc]
    get_uitars_extraction_service = None  # type: ignore[assignment, misc]
    UnifiedDataCollector = None  # type: ignore[assignment, misc]
    VisionExtractionService = None  # type: ignore[assignment, misc]
    WebExtractionService = None  # type: ignore[assignment, misc]

# Check if qontinui library is available. Every use of ``navigation_api`` and
# ``get_settings`` is guarded by ``QONTINUI_AVAILABLE``; the ``None`` placeholders only
# let ``qontinui_executor.py`` import the names unconditionally.
# (error details, traceback) of the failed import; empty when the library is available.
QONTINUI_IMPORT_ERROR: tuple[str, str] = ("", "")
try:
    from qontinui import navigation_api
    from qontinui.config import get_settings

    QONTINUI_AVAILABLE = True
except ImportError as e:
    QONTINUI_AVAILABLE = False
    QONTINUI_IMPORT_ERROR = (f"{type(e).__name__}: {str(e)}", traceback.format_exc())
    navigation_api = None  # type: ignore[assignment]
    get_settings = None  # type: ignore[assignment]

__all__ = [
    "QONTINUI_AVAILABLE",
    "QONTINUI_IMPORT_ERROR",
    "RUNNER_PARENT_DIR",
    "AccessibilityCaptureService",
    "AiBuilderGeneratorService",
    "AiShellCommandGeneratorService",
    "AiTestGeneratorService",
    "GUIAutomation",
    "InputMonitorService",
    "IntegrationTestingService",
    "PlaywrightCollectorService",
    "ScreenshotService",
    "TestAnalysisService",
    "UITarsExtractionService",
    "UnifiedDataCollector",
    "VisionExtractionService",
    "WebExtractionService",
    "get_model_manager",
    "get_pattern_matching_service",
    "get_settings",
    "get_uitars_extraction_service",
    "navigation_api",
]
