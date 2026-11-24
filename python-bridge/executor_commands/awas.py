"""AWAS (AI Web Action Standard) commands.

A mixin of ``QontinuiExecutor``. Its methods were moved verbatim from
``qontinui_executor.py`` by plan
2026-10-04-runner-python-executor-routes-118-commands-through-one-if-chain
and still read executor state through ``self``. ``COMMANDS`` maps each command
name to its handler method.
"""

import logging
from typing import Any, ClassVar

from ._host import ExecutorHost

logger = logging.getLogger(__name__)


class AwasCommands(ExecutorHost):
    """AWAS (AI Web Action Standard) commands."""

    COMMANDS: ClassVar[dict[str, str]] = {
        # AWAS (AI Web Action Standard) commands
        "awas_discover": "_handle_awas_discover",
        "awas_execute": "_handle_awas_execute",
        "awas_check_support": "_handle_awas_check_support",
        "awas_list_actions": "_handle_awas_list_actions",
        "awas_extract_elements": "_handle_awas_extract_elements",
    }

    # ============================================================================
    # AWAS (AI Web Action Standard) Handlers
    # ============================================================================

    def _get_awas_discovery_service(self):
        """Get or create the AWAS discovery service instance."""
        if not hasattr(self, "_awas_discovery_service"):
            try:
                from qontinui.awas import AwasDiscoveryService

                self._awas_discovery_service = AwasDiscoveryService()
            except ImportError:
                self._awas_discovery_service = None
        return self._awas_discovery_service

    def _get_awas_executor(self):
        """Get or create the AWAS executor instance."""
        if not hasattr(self, "_awas_executor"):
            try:
                from qontinui.awas import AwasExecutor

                self._awas_executor = AwasExecutor()
            except ImportError:
                self._awas_executor = None
        return self._awas_executor

    def _handle_awas_discover(self, params: dict[str, Any]) -> dict[str, Any]:
        """Discover AWAS manifest for a website.

        Args:
            params: Command parameters:
                - url: Base URL of the website to discover (required)
                - force_refresh: Whether to bypass cache (default: False)

        Returns:
            Dictionary with:
                - success: Whether discovery succeeded
                - manifest: Parsed AWAS manifest data (if success)
                - error: Error message (if failed)
        """
        import asyncio

        try:
            service = self._get_awas_discovery_service()
            if not service:
                return {"success": False, "error": "AWAS module not available"}

            url = params.get("url")
            if not url:
                return {"success": False, "error": "url parameter is required"}

            force_refresh = params.get("force_refresh", False)

            # Clear cache if force refresh
            if force_refresh:
                service.clear_cache(url)

            loop = self._get_or_create_async_loop()
            future = asyncio.run_coroutine_threadsafe(
                service.discover(url),
                loop,
            )
            manifest = future.result(timeout=30)

            if manifest is None:
                return {
                    "success": False,
                    "error": f"No AWAS manifest found at {url}",
                }

            # Convert manifest to dict for JSON serialization
            manifest_data = manifest.model_dump(by_alias=True, exclude_none=True)

            self.event_manager.emit_log(
                "info",
                f"Discovered AWAS manifest for {manifest.app_name} with {len(manifest.actions)} actions",
            )

            return {
                "success": True,
                "manifest": manifest_data,
                "app_name": manifest.app_name,
                "action_count": len(manifest.actions),
                "conformance_level": manifest.conformance_level.value,
            }

        except Exception as e:
            logger.exception(f"Failed to discover AWAS manifest: {e}")
            return {"success": False, "error": str(e)}

    def _handle_awas_execute(self, params: dict[str, Any]) -> dict[str, Any]:
        """Execute an AWAS action.

        Args:
            params: Command parameters:
                - url: Base URL of the website (required)
                - action_id: ID of the action to execute (required)
                - action_params: Parameters for the action (optional)
                - credentials: Authentication credentials (optional)
                - timeout_seconds: Request timeout (optional)

        Returns:
            Dictionary with:
                - success: Whether execution succeeded
                - status_code: HTTP status code
                - response_body: Response data
                - response_time_ms: Response time in milliseconds
                - error: Error message (if failed)
        """
        import asyncio

        try:
            discovery_service = self._get_awas_discovery_service()
            executor = self._get_awas_executor()

            if not discovery_service or not executor:
                return {"success": False, "error": "AWAS module not available"}

            url = params.get("url")
            action_id = params.get("action_id")
            action_params = params.get("action_params", {})
            credentials = params.get("credentials", {})
            timeout_seconds = params.get("timeout_seconds")

            if not url:
                return {"success": False, "error": "url parameter is required"}
            if not action_id:
                return {"success": False, "error": "action_id parameter is required"}

            # First discover the manifest
            loop = self._get_or_create_async_loop()
            future = asyncio.run_coroutine_threadsafe(
                discovery_service.discover(url),
                loop,
            )
            manifest = future.result(timeout=30)

            if manifest is None:
                return {"success": False, "error": f"No AWAS manifest found at {url}"}

            # Execute the action
            execute_kwargs = {
                "manifest": manifest,
                "action_id": action_id,
                "params": action_params,
                "credentials": credentials,
            }
            if timeout_seconds is not None:
                execute_kwargs["timeout_seconds"] = timeout_seconds

            future = asyncio.run_coroutine_threadsafe(
                executor.execute(**execute_kwargs),
                loop,
            )
            result = future.result(timeout=timeout_seconds or 60)

            self.event_manager.emit_log(
                "info" if result.success else "warning",
                f"AWAS action '{action_id}' {'succeeded' if result.success else 'failed'}: "
                f"status={result.status_code}, time={result.response_time_ms}ms",
            )

            return {
                "success": result.success,
                "action_id": result.action_id,
                "status_code": result.status_code,
                "response_body": result.response_body,
                "response_time_ms": result.response_time_ms,
                "error": result.error,
            }

        except Exception as e:
            logger.exception(f"Failed to execute AWAS action: {e}")
            return {"success": False, "error": str(e)}

    def _handle_awas_check_support(self, params: dict[str, Any]) -> dict[str, Any]:
        """Check if a website supports AWAS.

        Args:
            params: Command parameters:
                - url: Base URL of the website (required)

        Returns:
            Dictionary with:
                - success: Whether check completed
                - supported: Whether AWAS is supported
                - app_name: Application name (if supported)
                - action_count: Number of available actions
                - conformance_level: AWAS conformance level
        """
        import asyncio

        try:
            service = self._get_awas_discovery_service()
            if not service:
                return {"success": False, "error": "AWAS module not available"}

            url = params.get("url")
            if not url:
                return {"success": False, "error": "url parameter is required"}

            loop = self._get_or_create_async_loop()
            future = asyncio.run_coroutine_threadsafe(
                service.check_awas_support(url),
                loop,
            )
            result = future.result(timeout=30)

            self.event_manager.emit_log(
                "info",
                f"AWAS support check for {url}: {'supported' if result.get('supported') else 'not supported'}",
            )

            return {"success": True, **result}

        except Exception as e:
            logger.exception(f"Failed to check AWAS support: {e}")
            return {"success": False, "error": str(e)}

    def _handle_awas_list_actions(self, params: dict[str, Any]) -> dict[str, Any]:
        """List available AWAS actions for a website.

        Args:
            params: Command parameters:
                - url: Base URL of the website (required)
                - read_only_only: Only return read-only actions (default: False)

        Returns:
            Dictionary with:
                - success: Whether listing succeeded
                - actions: List of action summaries
                - app_name: Application name
        """
        import asyncio

        try:
            service = self._get_awas_discovery_service()
            if not service:
                return {"success": False, "error": "AWAS module not available"}

            url = params.get("url")
            if not url:
                return {"success": False, "error": "url parameter is required"}

            read_only_only = params.get("read_only_only", False)

            loop = self._get_or_create_async_loop()
            future = asyncio.run_coroutine_threadsafe(
                service.discover(url),
                loop,
            )
            manifest = future.result(timeout=30)

            if manifest is None:
                return {"success": False, "error": f"No AWAS manifest found at {url}"}

            # Get actions (filtered if requested)
            actions = manifest.get_read_only_actions() if read_only_only else manifest.actions

            # Build action summaries
            action_summaries = []
            for action in actions:
                summary = {
                    "id": action.id,
                    "name": action.name,
                    "method": action.method.value,
                    "endpoint": action.endpoint,
                    "intent": action.intent,
                    "side_effect": action.side_effect,
                    "is_read_only": action.is_read_only,
                    "parameter_count": len(action.parameters),
                }
                action_summaries.append(summary)

            self.event_manager.emit_log(
                "info",
                f"Listed {len(action_summaries)} AWAS actions for {manifest.app_name}",
            )

            return {
                "success": True,
                "app_name": manifest.app_name,
                "actions": action_summaries,
            }

        except Exception as e:
            logger.exception(f"Failed to list AWAS actions: {e}")
            return {"success": False, "error": str(e)}

    def _handle_awas_extract_elements(self, params: dict[str, Any]) -> dict[str, Any]:
        """Extract AWAS elements from HTML content.

        Args:
            params: Command parameters:
                - html: HTML content to parse (required)
                - page_url: URL of the page (optional, for context)

        Returns:
            Dictionary with:
                - success: Whether extraction succeeded
                - elements: List of extracted AWAS elements
        """
        try:
            service = self._get_awas_discovery_service()
            if not service:
                return {"success": False, "error": "AWAS module not available"}

            html = params.get("html")
            if not html:
                return {"success": False, "error": "html parameter is required"}

            page_url = params.get("page_url")

            elements = service.extract_elements(html, page_url)

            # Convert elements to dicts for JSON serialization
            element_data = []
            for elem in elements:
                element_data.append(elem.model_dump(exclude_none=True))

            self.event_manager.emit_log(
                "info",
                f"Extracted {len(elements)} AWAS elements from HTML",
            )

            return {
                "success": True,
                "elements": element_data,
                "element_count": len(elements),
            }

        except Exception as e:
            logger.exception(f"Failed to extract AWAS elements: {e}")
            return {"success": False, "error": str(e)}
