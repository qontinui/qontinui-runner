"""Extraction commands: web, Playwright collection, UI-TARS, vision and page analysis.

A mixin of ``QontinuiExecutor``. Its methods were moved verbatim from
``qontinui_executor.py`` by plan
2026-10-04-runner-python-executor-routes-118-commands-through-one-if-chain
and still read executor state through ``self``. ``COMMANDS`` maps each command
name to its handler method.
"""

import traceback
from typing import Any, ClassVar

from event_manager import EventType

from ._host import ExecutorHost
from ._shared import (
    PlaywrightCollectorService,
    TestAnalysisService,
    UITarsExtractionService,
    VisionExtractionService,
    WebExtractionService,
    get_uitars_extraction_service,
)


class ExtractionCommands(ExecutorHost):
    """Extraction commands: web, Playwright collection, UI-TARS, vision and page analysis."""

    COMMANDS: ClassVar[dict[str, str]] = {
        # Web extraction commands
        "start_web_extraction": "_handle_start_web_extraction",
        "stop_web_extraction": "_handle_stop_web_extraction",
        "get_extraction_status": "_handle_get_extraction_status",
        # Playwright State Collector commands
        "start_playwright_collection": "_handle_start_playwright_collection",
        "get_playwright_collection_status": "_handle_get_playwright_collection_status",
        "get_playwright_collection_results": "_handle_get_playwright_collection_results",
        "stop_playwright_collection": "_handle_stop_playwright_collection",
        # UI-TARS extraction commands
        "start_uitars_extraction": "_handle_start_uitars_extraction",
        "stop_uitars_extraction": "_handle_stop_uitars_extraction",
        "get_uitars_extraction_status": "_handle_get_uitars_extraction_status",
        "get_uitars_extraction_results": "_handle_get_uitars_extraction_results",
        # Vision extraction commands
        "run_vision_extraction": "_handle_run_vision_extraction",
        # Page analysis commands for AI-powered test generation
        "analyze_page_playwright": "_handle_analyze_page_playwright",
        "analyze_page_playwright_script": "_handle_analyze_page_playwright_script",
        "analyze_page_vision": "_handle_analyze_page_vision",
    }

    def _get_web_extraction_service(self) -> WebExtractionService:
        """Get or create the web extraction service."""
        if self._web_extraction_service is None:
            self._web_extraction_service = WebExtractionService(
                event_manager=self.event_manager,
            )
        return self._web_extraction_service  # type: ignore[no-any-return]

    def _get_vision_extraction_service(self) -> VisionExtractionService:
        """Get or create the vision extraction service."""
        if self._vision_extraction_service is None:
            self._vision_extraction_service = VisionExtractionService(
                event_manager=self.event_manager,
            )
        return self._vision_extraction_service  # type: ignore[no-any-return]

    def _get_test_analysis_service(self) -> TestAnalysisService:
        """Get or create the test analysis service for AI-powered test generation."""
        if self._test_analysis_service is None:
            self._test_analysis_service = TestAnalysisService(
                event_manager=self.event_manager,
            )
        return self._test_analysis_service  # type: ignore[no-any-return]

    def _handle_analyze_page_playwright(self, params: dict[str, Any]) -> dict[str, Any]:
        """
        Analyze page via Playwright CDP for AI test generation.

        Connects to an existing browser via Chrome DevTools Protocol,
        captures a screenshot, and extracts DOM elements with their
        bounding boxes, text content, and CSS selectors.

        Args:
            params: Configuration with:
                - cdp_port: CDP port number (default: 9222)

        Returns:
            Dict with:
                - success: Whether analysis succeeded
                - analysis: PageAnalysis data if success
                - error: Error message if failed
        """
        import asyncio
        import sys

        cdp_port = params.get("cdp_port", 9222)

        print(
            f"[info    ] EXECUTOR: _handle_analyze_page_playwright called with cdp_port={cdp_port}",
            file=sys.stderr,
            flush=True,
        )

        try:
            service = self._get_test_analysis_service()

            # Run async analysis using the async loop
            loop = self._get_or_create_async_loop()
            future = asyncio.run_coroutine_threadsafe(
                service.analyze_via_playwright(cdp_port=cdp_port),
                loop,
            )
            result = future.result(timeout=60)  # 60 second timeout for CDP connection

            if result.get("success"):
                return {
                    "success": True,
                    "analysis": result.get("data"),  # Return the actual analysis data
                }
            else:
                return {
                    "success": False,
                    "error": result.get("error", "Unknown error during Playwright analysis"),
                }

        except TimeoutError:
            return {
                "success": False,
                "error": f"Timeout connecting to browser via CDP on port {cdp_port}. Make sure a browser is running with --remote-debugging-port={cdp_port}",
            }
        except Exception as e:
            import traceback

            return {
                "success": False,
                "error": f"Playwright analysis failed: {e}",
                "traceback": traceback.format_exc(),
            }

    def _handle_analyze_page_playwright_script(self, params: dict[str, Any]) -> dict[str, Any]:
        """
        Analyze page by running a Playwright script.

        Executes the provided Playwright script to navigate to a page,
        then captures DOM elements with bounding boxes, text content,
        and CSS selectors.

        Args:
            params: Configuration with:
                - script: Playwright TypeScript/JavaScript code

        Returns:
            Dict with:
                - success: Whether analysis succeeded
                - analysis: PageAnalysis data if success
                - error: Error message if failed
        """
        import asyncio
        import sys

        script = params.get("script", "")

        print(
            f"[info    ] EXECUTOR: _handle_analyze_page_playwright_script called with script ({len(script)} chars)",
            file=sys.stderr,
            flush=True,
        )

        if not script.strip():
            return {
                "success": False,
                "error": "Playwright script is required",
            }

        try:
            service = self._get_test_analysis_service()

            # Run async analysis using the async loop
            loop = self._get_or_create_async_loop()
            future = asyncio.run_coroutine_threadsafe(
                service.analyze_via_playwright_script(script=script),
                loop,
            )
            result = future.result(timeout=120)  # 2 minute timeout for script execution

            if result.get("success"):
                return {
                    "success": True,
                    "analysis": result.get("data"),
                }
            else:
                return {
                    "success": False,
                    "error": result.get("error", "Unknown error during Playwright script analysis"),
                }

        except TimeoutError:
            return {
                "success": False,
                "error": "Timeout running Playwright script. Check for infinite loops or slow navigation.",
            }
        except Exception as e:
            import traceback

            return {
                "success": False,
                "error": f"Playwright script analysis failed: {e}",
                "traceback": traceback.format_exc(),
            }

    def _handle_analyze_page_vision(self, params: dict[str, Any]) -> dict[str, Any]:
        """
        Analyze page via Qontinui Vision for AI test generation.

        Captures a screenshot from the specified monitor and runs
        OCR, edge detection, and SAM3 segmentation to detect UI elements.

        Args:
            params: Configuration with:
                - monitor_index: Monitor index to capture (default: 0)
                - screenshot_base64: Optional pre-captured screenshot

        Returns:
            Dict with:
                - success: Whether analysis succeeded
                - analysis: PageAnalysis data if success
                - error: Error message if failed
        """
        import sys

        monitor_index = params.get("monitor_index", 0)
        screenshot_base64 = params.get("screenshot_base64")

        print(
            f"[info    ] EXECUTOR: _handle_analyze_page_vision called with monitor_index={monitor_index}",
            file=sys.stderr,
            flush=True,
        )

        try:
            service = self._get_test_analysis_service()

            # Vision analysis is synchronous
            result = service.analyze_via_vision(
                screenshot_base64=screenshot_base64,
                monitor_index=monitor_index,
            )

            if result.get("success"):
                return {
                    "success": True,
                    "analysis": result.get("data"),  # Return the actual analysis data
                }
            else:
                return {
                    "success": False,
                    "error": result.get("error", "Unknown error during Vision analysis"),
                }

        except Exception as e:
            import traceback

            return {
                "success": False,
                "error": f"Vision analysis failed: {e}",
                "traceback": traceback.format_exc(),
            }

    def _handle_run_vision_extraction(self, params: dict[str, Any]) -> dict[str, Any]:
        """
        Handle run vision extraction command.

        Runs SAM3, Edge Detection, and/or OCR on a screenshot.

        Args:
            params: Extraction configuration with:
                - screenshot: Base64-encoded image OR file path
                - techniques: List of techniques ["edge", "sam3", "ocr"]
                - Edge detection config: canny_low, canny_high, min_contour_area
                - SAM3 config: points_per_side, pred_iou_thresh, stability_score_thresh
                - OCR config: ocr_engine, ocr_languages, ocr_confidence_threshold
                - Fusion config: iou_threshold

        Returns:
            Dict with extraction results.
        """
        import sys

        print(
            f"[info    ] EXECUTOR: _handle_run_vision_extraction called with {len(params.get('screenshot', ''))} char screenshot",
            file=sys.stderr,
            flush=True,
        )

        try:
            service = self._get_vision_extraction_service()

            # Run extraction (synchronous since VisionExtractionService.extract is sync)
            config = params.get("config", params)  # Support both nested and flat config
            result = service.extract(config)

            if result.get("success"):
                self.event_manager.emit_event(
                    EventType.EXTRACTION_STARTED,
                    {
                        "extraction_id": result.get("extraction_id"),
                        "technique": "vision",
                        "techniques_run": result.get("techniques_run", []),
                    },
                )

            print(
                f"[info    ] EXECUTOR: vision_extraction result: success={result.get('success')}, "
                f"edge={len(result.get('edge_results', []))}, "
                f"sam3={len(result.get('sam3_results', []))}, "
                f"ocr={len(result.get('ocr_results', []))}",
                file=sys.stderr,
                flush=True,
            )
            return result

        except Exception as e:
            print(
                f"[error   ] EXECUTOR: Failed to run vision extraction: {e}",
                file=sys.stderr,
                flush=True,
            )
            print(
                f"[error   ] EXECUTOR: Traceback: {traceback.format_exc()}",
                file=sys.stderr,
                flush=True,
            )
            self.event_manager.emit_log("error", f"Failed to run vision extraction: {e}")
            return {"success": False, "error": str(e)}

    def _handle_start_web_extraction(self, params: dict[str, Any]) -> dict[str, Any]:
        """Handle start web extraction command."""
        import asyncio
        import sys

        print(
            f"[info    ] EXECUTOR: _handle_start_web_extraction called with params: {params}",
            file=sys.stderr,
            flush=True,
        )

        try:
            service = self._get_web_extraction_service()

            # Get dedicated event loop running in background thread
            loop = self._get_or_create_async_loop()

            # Schedule the async operation on the background loop using run_coroutine_threadsafe
            # This is required because we may be called from within an already-running event loop
            config = params.get("config", params)  # Support both nested and flat config
            print(
                f"[info    ] EXECUTOR: Starting extraction with config: {config}",
                file=sys.stderr,
                flush=True,
            )

            future = asyncio.run_coroutine_threadsafe(service.start_extraction(config), loop)
            # Wait for result with timeout (extraction may take a while to initialize)
            result = future.result(timeout=60)

            if result.get("success"):
                self.event_manager.emit_event(
                    EventType.EXTRACTION_STARTED,
                    {
                        "extraction_id": result.get("extraction_id"),
                        "config": config,
                    },
                )

            print(
                f"[info    ] EXECUTOR: start_extraction result: {result}",
                file=sys.stderr,
                flush=True,
            )
            return result

        except Exception as e:
            print(
                f"[error   ] EXECUTOR: Failed to start extraction: {e}",
                file=sys.stderr,
                flush=True,
            )

            print(
                f"[error   ] EXECUTOR: Traceback: {traceback.format_exc()}",
                file=sys.stderr,
                flush=True,
            )
            self.event_manager.emit_log("error", f"Failed to start extraction: {e}")
            return {"success": False, "error": str(e)}

    def _handle_stop_web_extraction(self) -> dict[str, Any]:
        """Handle stop web extraction command."""
        import asyncio

        try:
            if self._web_extraction_service is None:
                return {"success": False, "error": "No extraction in progress"}

            # Get dedicated event loop running in background thread
            loop = self._get_or_create_async_loop()

            # Schedule the async operation on the background loop using run_coroutine_threadsafe
            future = asyncio.run_coroutine_threadsafe(
                self._web_extraction_service.stop_extraction(), loop
            )
            result = future.result(timeout=30)
            return result  # type: ignore[no-any-return]

        except Exception as e:
            self.event_manager.emit_log("error", f"Failed to stop extraction: {e}")
            return {"success": False, "error": str(e)}

    def _handle_get_extraction_status(self) -> dict[str, Any]:
        """Handle get extraction status command."""
        try:
            if self._web_extraction_service is None:
                return {
                    "success": True,
                    "is_running": False,
                    "extraction_id": None,
                }

            status = self._web_extraction_service.get_status()
            return {"success": True, **status}

        except Exception as e:
            self.event_manager.emit_log("error", f"Failed to get extraction status: {e}")
            return {"success": False, "error": str(e)}

    def _get_playwright_collector_service(self) -> PlaywrightCollectorService:
        """Get or create the Playwright collector service."""
        if self._playwright_collector_service is None:
            self._playwright_collector_service = PlaywrightCollectorService(
                event_manager=self.event_manager,
            )
        return self._playwright_collector_service  # type: ignore[no-any-return]

    def _handle_start_playwright_collection(self, params: dict[str, Any]) -> dict[str, Any]:
        """Handle start Playwright collection command."""
        import sys

        print(
            f"[info    ] EXECUTOR: _handle_start_playwright_collection called with params: {params}",
            file=sys.stderr,
            flush=True,
        )

        try:
            service = self._get_playwright_collector_service()
            result = service.start_collection(params)

            print(
                f"[info    ] EXECUTOR: start_playwright_collection result: {result}",
                file=sys.stderr,
                flush=True,
            )
            return result

        except Exception as e:
            self.event_manager.emit_log("error", f"Failed to start Playwright collection: {e}")
            import traceback

            traceback.print_exc(file=sys.stderr)
            return {"success": False, "error": str(e)}

    def _handle_get_playwright_collection_status(self, params: dict[str, Any]) -> dict[str, Any]:
        """Handle get Playwright collection status command."""
        try:
            if self._playwright_collector_service is None:
                return {
                    "success": True,
                    "status": "idle",
                    "job_id": None,
                }

            job_id = params.get("job_id")
            result: dict[str, Any] = self._playwright_collector_service.get_job_status(job_id)
            return result

        except Exception as e:
            self.event_manager.emit_log("error", f"Failed to get Playwright collection status: {e}")
            return {"success": False, "error": str(e)}

    def _handle_get_playwright_collection_results(self, params: dict[str, Any]) -> dict[str, Any]:
        """Handle get Playwright collection results command."""
        try:
            if self._playwright_collector_service is None:
                return {"success": False, "error": "No Playwright collection service"}

            job_id = params.get("job_id")
            result: dict[str, Any] = self._playwright_collector_service.get_results(job_id)
            return result

        except Exception as e:
            self.event_manager.emit_log(
                "error", f"Failed to get Playwright collection results: {e}"
            )
            return {"success": False, "error": str(e)}

    def _handle_stop_playwright_collection(self) -> dict[str, Any]:
        """Handle stop Playwright collection command."""
        try:
            if self._playwright_collector_service is None:
                return {"success": False, "error": "No collection in progress"}

            result: dict[str, Any] = self._playwright_collector_service.stop_collection()
            return result

        except Exception as e:
            self.event_manager.emit_log("error", f"Failed to stop Playwright collection: {e}")
            return {"success": False, "error": str(e)}

    # =========================================================================
    # UI-TARS Extraction Handlers
    # =========================================================================

    def _get_uitars_extraction_service(self) -> UITarsExtractionService:
        """Get or create the UI-TARS extraction service."""
        if self._uitars_extraction_service is None:
            self._uitars_extraction_service = get_uitars_extraction_service(
                emit_log_fn=self.event_manager.emit_log,
                emit_event_fn=self._emit_event_wrapper,
            )
        return self._uitars_extraction_service

    def _handle_start_uitars_extraction(self, params: dict[str, Any]) -> dict[str, Any]:
        """Handle start UI-TARS extraction command."""
        import sys

        print(
            f"[info    ] EXECUTOR: _handle_start_uitars_extraction called with params: {params}",
            file=sys.stderr,
            flush=True,
        )

        try:
            service = self._get_uitars_extraction_service()

            # Check if UI-TARS is available
            if not service.is_available():
                self.event_manager.emit_log(
                    "warning",
                    "[UI-TARS] UI-TARS not available, will run simulated extraction",
                )

            # Start extraction
            config = params.get("config", params)
            result = service.start_extraction(config)

            print(
                f"[info    ] EXECUTOR: start_uitars_extraction result: {result}",
                file=sys.stderr,
                flush=True,
            )
            return result

        except Exception as e:
            self.event_manager.emit_log("error", f"Failed to start UI-TARS extraction: {e}")
            import traceback

            traceback.print_exc(file=sys.stderr)
            return {"success": False, "error": str(e)}

    def _handle_stop_uitars_extraction(self) -> dict[str, Any]:
        """Handle stop UI-TARS extraction command."""
        try:
            if self._uitars_extraction_service is None:
                return {"success": False, "error": "No UI-TARS extraction in progress"}

            return self._uitars_extraction_service.stop_extraction()

        except Exception as e:
            self.event_manager.emit_log("error", f"Failed to stop UI-TARS extraction: {e}")
            return {"success": False, "error": str(e)}

    def _handle_get_uitars_extraction_status(self) -> dict[str, Any]:
        """Handle get UI-TARS extraction status command."""
        try:
            if self._uitars_extraction_service is None:
                return {
                    "success": True,
                    "status": "idle",
                    "current_step": 0,
                    "max_steps": 0,
                    "elapsed_seconds": 0,
                    "states_discovered": 0,
                    "transitions_discovered": 0,
                    "uitars_available": False,
                }

            status = self._uitars_extraction_service.get_status()
            return {"success": True, **status}

        except Exception as e:
            self.event_manager.emit_log("error", f"Failed to get UI-TARS extraction status: {e}")
            return {"success": False, "error": str(e)}

    def _handle_get_uitars_extraction_results(self) -> dict[str, Any]:
        """Handle get UI-TARS extraction results command."""
        try:
            if self._uitars_extraction_service is None:
                return {
                    "success": True,
                    "states": [],
                    "transitions": [],
                    "total_steps": 0,
                    "total_screenshots": 0,
                    "exploration_time_seconds": 0,
                }

            results = self._uitars_extraction_service.get_results()
            return {"success": True, **results}

        except Exception as e:
            self.event_manager.emit_log("error", f"Failed to get UI-TARS extraction results: {e}")
            return {"success": False, "error": str(e)}
