"""Accessibility-tree capture and CDP ref commands.

A mixin of ``QontinuiExecutor``. Its methods were moved verbatim from
``qontinui_executor.py`` by plan
2026-10-04-runner-python-executor-routes-118-commands-through-one-if-chain
and still read executor state through ``self``. ``COMMANDS`` maps each command
name to its handler method.
"""

import logging
from typing import Any, ClassVar

from ._host import ExecutorHost
from ._shared import AccessibilityCaptureService

logger = logging.getLogger(__name__)


class AccessibilityCommands(ExecutorHost):
    """Accessibility-tree capture and CDP ref commands."""

    COMMANDS: ClassVar[dict[str, str]] = {
        # Accessibility capture commands
        "capture_accessibility": "_handle_capture_accessibility",
        "click_ref": "_handle_click_ref",
        "fill_ref": "_handle_fill_ref",
        "focus_ref": "_handle_focus_ref",
        "get_ref": "_handle_get_ref",
        "get_accessibility_snapshot": "_handle_get_accessibility_snapshot",
        "get_accessibility_ai_context": "_handle_get_accessibility_ai_context",
        "find_accessibility_elements": "_handle_find_accessibility_elements",
        "disconnect_accessibility": "_handle_disconnect_accessibility",
        "scan_cdp_ports": "_handle_scan_cdp_ports",
        "list_browser_targets": "_handle_list_browser_targets",
        "auto_connect_accessibility": "_handle_auto_connect_accessibility",
    }

    def _get_accessibility_capture_service(self) -> AccessibilityCaptureService:
        """Get or create the accessibility capture service."""
        if self._accessibility_capture_service is None:
            self._accessibility_capture_service = AccessibilityCaptureService()
        return self._accessibility_capture_service  # type: ignore[no-any-return]

    # ============================================================================
    # Accessibility Capture Handlers
    # ============================================================================

    def _handle_capture_accessibility(self, params: dict[str, Any]) -> dict[str, Any]:
        """Capture accessibility tree from a browser via CDP.

        Args:
            params: Command parameters:
                - target: Target to capture ("auto", URL, or page index)
                - cdp_host: CDP host (default: localhost)
                - cdp_port: CDP port (default: 9222)
                - interactive_only: Only include interactive elements
                - include_hidden: Include hidden elements
                - max_depth: Maximum tree depth

        Returns:
            Dictionary with:
                - success: Whether capture succeeded
                - snapshot: Serialized AccessibilitySnapshot
                - ai_context: AI-friendly text representation
                - stats: Capture statistics
                - error: Error message if failed
        """
        import asyncio

        try:
            service = self._get_accessibility_capture_service()

            target = params.get("target", "auto")
            cdp_host = params.get("cdp_host", "localhost")
            cdp_port = params.get("cdp_port", 9222)
            interactive_only = params.get("interactive_only", False)
            include_hidden = params.get("include_hidden", False)
            max_depth = params.get("max_depth")

            loop = self._get_or_create_async_loop()
            future = asyncio.run_coroutine_threadsafe(
                service.capture_accessibility_tree(
                    target=target,
                    cdp_host=cdp_host,
                    cdp_port=cdp_port,
                    interactive_only=interactive_only,
                    include_hidden=include_hidden,
                    max_depth=max_depth,
                ),
                loop,
            )
            result = future.result(timeout=60)

            self.event_manager.emit_log(
                "info" if result.get("success") else "error",
                f"Accessibility capture: {'success' if result.get('success') else result.get('error', 'failed')}",
            )

            return result

        except Exception as e:
            logger.exception(f"Failed to capture accessibility tree: {e}")
            return {"success": False, "error": str(e)}

    def _handle_click_ref(self, params: dict[str, Any]) -> dict[str, Any]:
        """Click an element by its accessibility ref.

        Args:
            params: Command parameters:
                - ref: Reference ID (e.g., "@e3")

        Returns:
            Dictionary with success status and element info
        """
        import asyncio

        try:
            service = self._get_accessibility_capture_service()
            ref = params.get("ref")

            if not ref:
                return {"success": False, "error": "ref parameter is required"}

            loop = self._get_or_create_async_loop()
            future = asyncio.run_coroutine_threadsafe(service.click_ref(ref), loop)
            result = future.result(timeout=30)

            self.event_manager.emit_log(
                "info" if result.get("success") else "warning",
                f"Click ref {ref}: {'success' if result.get('success') else result.get('error', 'failed')}",
            )

            return result

        except Exception as e:
            logger.exception(f"Failed to click ref: {e}")
            return {"success": False, "error": str(e)}

    def _handle_fill_ref(self, params: dict[str, Any]) -> dict[str, Any]:
        """Type text into an element by its accessibility ref.

        Args:
            params: Command parameters:
                - ref: Reference ID (e.g., "@e2")
                - value: Text to type
                - clear_first: Clear existing content first

        Returns:
            Dictionary with success status and element info
        """
        import asyncio

        try:
            service = self._get_accessibility_capture_service()
            ref = params.get("ref")
            value = params.get("value", "")
            clear_first = params.get("clear_first", False)

            if not ref:
                return {"success": False, "error": "ref parameter is required"}

            loop = self._get_or_create_async_loop()
            future = asyncio.run_coroutine_threadsafe(
                service.fill_ref(ref, value, clear_first=clear_first), loop
            )
            result = future.result(timeout=30)

            self.event_manager.emit_log(
                "info" if result.get("success") else "warning",
                f"Fill ref {ref}: {'success' if result.get('success') else result.get('error', 'failed')}",
            )

            return result

        except Exception as e:
            logger.exception(f"Failed to fill ref: {e}")
            return {"success": False, "error": str(e)}

    def _handle_focus_ref(self, params: dict[str, Any]) -> dict[str, Any]:
        """Focus an element by its accessibility ref.

        Args:
            params: Command parameters:
                - ref: Reference ID (e.g., "@e5")

        Returns:
            Dictionary with success status and element info
        """
        import asyncio

        try:
            service = self._get_accessibility_capture_service()
            ref = params.get("ref")

            if not ref:
                return {"success": False, "error": "ref parameter is required"}

            loop = self._get_or_create_async_loop()
            future = asyncio.run_coroutine_threadsafe(service.focus_ref(ref), loop)
            result = future.result(timeout=30)

            self.event_manager.emit_log(
                "info" if result.get("success") else "warning",
                f"Focus ref {ref}: {'success' if result.get('success') else result.get('error', 'failed')}",
            )

            return result

        except Exception as e:
            logger.exception(f"Failed to focus ref: {e}")
            return {"success": False, "error": str(e)}

    def _handle_get_ref(self, params: dict[str, Any]) -> dict[str, Any]:
        """Get element details by its accessibility ref.

        Args:
            params: Command parameters:
                - ref: Reference ID (e.g., "@e1")

        Returns:
            Dictionary with element details or error
        """
        try:
            service = self._get_accessibility_capture_service()
            ref = params.get("ref")

            if not ref:
                return {"success": False, "error": "ref parameter is required"}

            return service.get_element_by_ref(ref)

        except Exception as e:
            logger.exception(f"Failed to get ref: {e}")
            return {"success": False, "error": str(e)}

    def _handle_get_accessibility_snapshot(self) -> dict[str, Any]:
        """Get the current cached accessibility snapshot.

        Returns:
            Dictionary with snapshot data or error
        """
        try:
            service = self._get_accessibility_capture_service()
            return service.get_current_snapshot()

        except Exception as e:
            logger.exception(f"Failed to get accessibility snapshot: {e}")
            return {"success": False, "error": str(e)}

    def _handle_get_accessibility_ai_context(self, params: dict[str, Any]) -> dict[str, Any]:
        """Get AI-friendly context from the current accessibility snapshot.

        Args:
            params: Command parameters:
                - max_elements: Maximum number of elements to include
                - interactive_only: Only include interactive elements

        Returns:
            Dictionary with AI context string
        """
        try:
            service = self._get_accessibility_capture_service()
            max_elements = params.get("max_elements", 100)
            interactive_only = params.get("interactive_only", True)

            return service.get_ai_context(
                max_elements=max_elements, interactive_only=interactive_only
            )

        except Exception as e:
            logger.exception(f"Failed to get accessibility AI context: {e}")
            return {"success": False, "error": str(e)}

    def _handle_find_accessibility_elements(self, params: dict[str, Any]) -> dict[str, Any]:
        """Find elements matching criteria in the accessibility tree.

        Args:
            params: Command parameters:
                - role: Role(s) to match
                - name: Exact name to match
                - name_contains: Partial name match
                - is_interactive: Filter by interactivity

        Returns:
            Dictionary with matching elements
        """
        import asyncio

        try:
            service = self._get_accessibility_capture_service()

            role = params.get("role")
            name = params.get("name")
            name_contains = params.get("name_contains")
            is_interactive = params.get("is_interactive")

            loop = self._get_or_create_async_loop()
            future = asyncio.run_coroutine_threadsafe(
                service.find_elements(
                    role=role,
                    name=name,
                    name_contains=name_contains,
                    is_interactive=is_interactive,
                ),
                loop,
            )
            result = future.result(timeout=30)

            return result

        except Exception as e:
            logger.exception(f"Failed to find accessibility elements: {e}")
            return {"success": False, "error": str(e)}

    def _handle_disconnect_accessibility(self) -> dict[str, Any]:
        """Disconnect from the accessibility source.

        Returns:
            Dictionary with disconnect status
        """
        import asyncio

        try:
            service = self._get_accessibility_capture_service()

            loop = self._get_or_create_async_loop()
            future = asyncio.run_coroutine_threadsafe(service.disconnect(), loop)
            result = future.result(timeout=10)

            self.event_manager.emit_log("info", "Accessibility capture disconnected")

            return result

        except Exception as e:
            logger.exception(f"Failed to disconnect accessibility: {e}")
            return {"success": False, "error": str(e)}

    def _handle_scan_cdp_ports(self, params: dict[str, Any]) -> dict[str, Any]:
        """Scan common CDP ports to find available browsers.

        Args:
            params: Command parameters:
                - host: Host to scan (default: localhost)
                - ports: List of ports to scan (default: common ports)
                - timeout: Timeout per port in seconds

        Returns:
            Dictionary with available ports and targets
        """
        import asyncio

        try:
            service = self._get_accessibility_capture_service()

            host = params.get("host", "localhost")
            ports = params.get("ports")
            timeout = params.get("timeout", 2.0)

            loop = self._get_or_create_async_loop()
            future = asyncio.run_coroutine_threadsafe(
                service.scan_cdp_ports(host, ports, timeout),
                loop,
            )
            result = future.result(timeout=30)

            self.event_manager.emit_log(
                "info",
                f"CDP port scan complete: {len(result.get('available_ports', []))} ports found",
            )

            return result

        except Exception as e:
            logger.exception(f"Failed to scan CDP ports: {e}")
            return {"success": False, "error": str(e)}

    def _handle_list_browser_targets(self, params: dict[str, Any]) -> dict[str, Any]:
        """List available browser page targets on a CDP port.

        Args:
            params: Command parameters:
                - host: CDP host (default: localhost)
                - port: CDP port (default: 9222)

        Returns:
            Dictionary with list of page targets
        """
        import asyncio

        try:
            service = self._get_accessibility_capture_service()

            host = params.get("host", "localhost")
            port = params.get("port", 9222)

            loop = self._get_or_create_async_loop()
            future = asyncio.run_coroutine_threadsafe(
                service.list_browser_targets(host, port),
                loop,
            )
            result = future.result(timeout=10)

            self.event_manager.emit_log(
                "info",
                f"Listed {result.get('count', 0)} browser targets on port {port}",
            )

            return result

        except Exception as e:
            logger.exception(f"Failed to list browser targets: {e}")
            return {"success": False, "error": str(e)}

    def _handle_auto_connect_accessibility(self, params: dict[str, Any]) -> dict[str, Any]:
        """Automatically connect to the first available CDP target.

        Args:
            params: Command parameters:
                - host: Host to scan (default: localhost)
                - preferred_ports: List of ports to try in order

        Returns:
            Dictionary with connection result and captured snapshot
        """
        import asyncio

        try:
            service = self._get_accessibility_capture_service()

            host = params.get("host", "localhost")
            preferred_ports = params.get("preferred_ports")

            loop = self._get_or_create_async_loop()
            future = asyncio.run_coroutine_threadsafe(
                service.auto_connect(host, preferred_ports),
                loop,
            )
            result = future.result(timeout=60)

            if result.get("success"):
                target = result.get("target", {})
                self.event_manager.emit_log(
                    "info",
                    f"Auto-connected to CDP on port {result.get('port')}: {target.get('title', 'Unknown')}",
                )
            else:
                self.event_manager.emit_log(
                    "warning",
                    f"Auto-connect failed: {result.get('error')}",
                )

            return result

        except Exception as e:
            logger.exception(f"Failed to auto-connect accessibility: {e}")
            return {"success": False, "error": str(e)}
