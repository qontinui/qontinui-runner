"""Accessibility Capture Service.

This service provides accessibility tree capture and ref-based interaction
capabilities for the qontinui-runner. It wraps the qontinui HAL accessibility
capture interface with a service pattern suitable for IPC communication.

Key features:
- Capture accessibility trees from browsers via CDP
- Ref-based element selection (@e1, @e2, etc.)
- AI-friendly context generation for Claude prompts
- Async operations for non-blocking capture
"""

import asyncio
import logging
from typing import Any

from qontinui.hal.implementations.accessibility import CDPAccessibilityCapture
from qontinui.hal.interfaces import IAccessibilityCapture
from qontinui_schemas.accessibility import (
    AccessibilityBackend,
    AccessibilityCaptureOptions,
    AccessibilityConfig,
    AccessibilityNode,
    AccessibilitySelector,
    AccessibilitySnapshot,
)

logger = logging.getLogger(__name__)


class AccessibilityCaptureService:
    """Service for accessibility tree capture and ref-based interaction.

    This service follows the Single Responsibility Principle by exclusively
    handling accessibility-related operations:
    - Connecting to accessibility sources (browsers via CDP)
    - Capturing accessibility tree snapshots
    - Finding and interacting with elements by ref
    - Generating AI-friendly context for prompts

    The service maintains state between captures, allowing ref-based
    interactions without re-capturing the tree each time.

    Example:
        >>> service = AccessibilityCaptureService()
        >>> result = await service.capture_accessibility_tree(cdp_port=9222)
        >>> print(result["ai_context"])  # AI-friendly text
        >>> await service.click_ref("@e3")
    """

    def __init__(self) -> None:
        """Initialize the AccessibilityCaptureService."""
        self._capture: IAccessibilityCapture | None = None
        self._current_snapshot: AccessibilitySnapshot | None = None
        self._is_connected: bool = False

    async def capture_accessibility_tree(
        self,
        target: str = "auto",
        cdp_host: str = "localhost",
        cdp_port: int = 9222,
        interactive_only: bool = False,
        include_hidden: bool = False,
        max_depth: int | None = None,
    ) -> dict[str, Any]:
        """Capture the accessibility tree from a browser.

        This method:
        1. Connects to the browser via CDP (if not already connected)
        2. Captures the full accessibility tree
        3. Assigns refs to nodes (@e1, @e2, etc.)
        4. Generates AI-friendly context
        5. Returns structured result for IPC

        Args:
            target: Target to capture: "auto", page URL, or index.
            cdp_host: CDP host (default: localhost).
            cdp_port: CDP port (default: 9222).
            interactive_only: Only include interactive elements in refs.
            include_hidden: Include hidden elements in capture.
            max_depth: Maximum tree depth to capture.

        Returns:
            Dictionary with:
            - success: Whether capture succeeded
            - snapshot: Serialized AccessibilitySnapshot
            - ai_context: AI-friendly text representation
            - stats: Capture statistics (node counts, etc.)
            - error: Error message if failed
        """
        try:
            # Create capture instance if not exists
            if self._capture is None:
                config = AccessibilityConfig(
                    backend=AccessibilityBackend.CDP,
                    cdp_host=cdp_host,
                    cdp_port=cdp_port,
                    interactive_only=interactive_only,
                    include_hidden=include_hidden,
                    max_depth=max_depth,
                )
                self._capture = CDPAccessibilityCapture(config)

            # Connect if not connected
            if not self._is_connected:
                target_spec = None if target == "auto" else target
                connected = await self._capture.connect(
                    target=target_spec,
                    host=cdp_host,
                    port=cdp_port,
                )
                if not connected:
                    return {
                        "success": False,
                        "error": f"Failed to connect to CDP at {cdp_host}:{cdp_port}",
                    }
                self._is_connected = True

            # Capture tree
            options = AccessibilityCaptureOptions(
                target=target,
                config=AccessibilityConfig(
                    interactive_only=interactive_only,
                    include_hidden=include_hidden,
                    max_depth=max_depth,
                ),
            )
            snapshot = await self._capture.capture_tree(options)
            self._current_snapshot = snapshot

            # Generate AI context
            ai_context = self._capture.to_ai_context(
                snapshot,
                max_elements=100,
                interactive_only=True,
            )

            # Serialize snapshot for IPC
            snapshot_dict = snapshot.model_dump()

            return {
                "success": True,
                "snapshot": snapshot_dict,
                "ai_context": ai_context,
                "stats": {
                    "total_nodes": snapshot.total_nodes,
                    "interactive_nodes": snapshot.interactive_nodes,
                    "backend": snapshot.backend.value,
                    "url": snapshot.url,
                    "title": snapshot.title,
                },
            }

        except Exception as e:
            logger.exception("Failed to capture accessibility tree")
            return {
                "success": False,
                "error": str(e),
            }

    async def click_ref(self, ref: str) -> dict[str, Any]:
        """Click an element by its ref.

        Args:
            ref: Reference ID (e.g., "@e3")

        Returns:
            Dictionary with:
            - success: Whether click succeeded
            - ref: The ref that was clicked
            - element_name: Name of clicked element
            - element_role: Role of clicked element
            - error: Error message if failed
        """
        if not self._capture or not self._is_connected:
            return {
                "success": False,
                "error": "Not connected to accessibility source",
            }

        try:
            node = await self._capture.get_node_by_ref(ref)
            if not node:
                return {
                    "success": False,
                    "error": f"Element not found: {ref}",
                }

            clicked = await self._capture.click_by_ref(ref)

            return {
                "success": clicked,
                "ref": ref,
                "element_name": node.name,
                "element_role": node.role.value,
                "error": None if clicked else "Click action failed",
            }

        except Exception as e:
            logger.exception(f"Failed to click ref: {ref}")
            return {
                "success": False,
                "error": str(e),
            }

    async def fill_ref(self, ref: str, value: str, clear_first: bool = False) -> dict[str, Any]:
        """Type text into an element by its ref.

        Args:
            ref: Reference ID (e.g., "@e2")
            value: Text to type
            clear_first: Clear existing content before typing

        Returns:
            Dictionary with:
            - success: Whether typing succeeded
            - ref: The ref that was typed into
            - value: The text that was typed
            - element_name: Name of element
            - error: Error message if failed
        """
        if not self._capture or not self._is_connected:
            return {
                "success": False,
                "error": "Not connected to accessibility source",
            }

        try:
            node = await self._capture.get_node_by_ref(ref)
            if not node:
                return {
                    "success": False,
                    "error": f"Element not found: {ref}",
                }

            typed = await self._capture.type_by_ref(ref, value, clear_first=clear_first)

            return {
                "success": typed,
                "ref": ref,
                "value": value,
                "element_name": node.name,
                "element_role": node.role.value,
                "error": None if typed else "Type action failed",
            }

        except Exception as e:
            logger.exception(f"Failed to fill ref: {ref}")
            return {
                "success": False,
                "error": str(e),
            }

    async def focus_ref(self, ref: str) -> dict[str, Any]:
        """Focus an element by its ref.

        Args:
            ref: Reference ID (e.g., "@e5")

        Returns:
            Dictionary with:
            - success: Whether focus succeeded
            - ref: The ref that was focused
            - element_name: Name of focused element
            - error: Error message if failed
        """
        if not self._capture or not self._is_connected:
            return {
                "success": False,
                "error": "Not connected to accessibility source",
            }

        try:
            node = await self._capture.get_node_by_ref(ref)
            if not node:
                return {
                    "success": False,
                    "error": f"Element not found: {ref}",
                }

            focused = await self._capture.focus_by_ref(ref)

            return {
                "success": focused,
                "ref": ref,
                "element_name": node.name,
                "element_role": node.role.value,
                "error": None if focused else "Focus action failed",
            }

        except Exception as e:
            logger.exception(f"Failed to focus ref: {ref}")
            return {
                "success": False,
                "error": str(e),
            }

    def get_element_by_ref(self, ref: str) -> dict[str, Any]:
        """Get element details by ref (synchronous, from cached snapshot).

        Args:
            ref: Reference ID (e.g., "@e1")

        Returns:
            Dictionary with element details or error
        """
        if not self._current_snapshot:
            return {
                "success": False,
                "error": "No snapshot available. Call capture_accessibility_tree first.",
            }

        node = self._current_snapshot.get_node_by_ref(ref)
        if not node:
            return {
                "success": False,
                "error": f"Element not found: {ref}",
            }

        return {
            "success": True,
            "element": node.model_dump(),
        }

    def get_current_snapshot(self) -> dict[str, Any]:
        """Get the current cached snapshot.

        Returns:
            Dictionary with snapshot data or error
        """
        if not self._current_snapshot:
            return {
                "success": False,
                "error": "No snapshot available. Call capture_accessibility_tree first.",
            }

        return {
            "success": True,
            "snapshot": self._current_snapshot.model_dump(),
            "stats": {
                "total_nodes": self._current_snapshot.total_nodes,
                "interactive_nodes": self._current_snapshot.interactive_nodes,
                "backend": self._current_snapshot.backend.value,
                "url": self._current_snapshot.url,
                "title": self._current_snapshot.title,
            },
        }

    def get_ai_context(self, max_elements: int = 100, interactive_only: bool = True) -> dict[str, Any]:
        """Get AI-friendly context from the current snapshot.

        Args:
            max_elements: Maximum number of elements to include
            interactive_only: Only include interactive elements

        Returns:
            Dictionary with AI context string
        """
        if not self._current_snapshot or not self._capture:
            return {
                "success": False,
                "error": "No snapshot available. Call capture_accessibility_tree first.",
            }

        ai_context = self._capture.to_ai_context(
            self._current_snapshot,
            max_elements=max_elements,
            interactive_only=interactive_only,
        )

        return {
            "success": True,
            "ai_context": ai_context,
        }

    async def find_elements(
        self,
        role: str | list[str] | None = None,
        name: str | None = None,
        name_contains: str | None = None,
        is_interactive: bool | None = None,
    ) -> dict[str, Any]:
        """Find elements matching criteria.

        Args:
            role: Role(s) to match
            name: Exact name to match
            name_contains: Partial name match
            is_interactive: Filter by interactivity

        Returns:
            Dictionary with matching elements
        """
        if not self._capture or not self._current_snapshot:
            return {
                "success": False,
                "error": "No snapshot available. Call capture_accessibility_tree first.",
            }

        try:
            from qontinui_schemas.accessibility import AccessibilityRole

            # Build selector
            selector_role = None
            if role:
                if isinstance(role, str):
                    selector_role = AccessibilityRole(role)
                else:
                    selector_role = [AccessibilityRole(r) for r in role]

            selector = AccessibilitySelector(
                role=selector_role,
                name=name,
                name_contains=name_contains,
                is_interactive=is_interactive,
            )

            nodes = await self._capture.find_nodes(selector)

            return {
                "success": True,
                "count": len(nodes),
                "elements": [node.model_dump() for node in nodes],
            }

        except Exception as e:
            logger.exception("Failed to find elements")
            return {
                "success": False,
                "error": str(e),
            }

    async def disconnect(self) -> dict[str, Any]:
        """Disconnect from the accessibility source.

        Returns:
            Dictionary with disconnect status
        """
        try:
            if self._capture:
                await self._capture.disconnect()
            self._is_connected = False
            self._current_snapshot = None
            return {"success": True}
        except Exception as e:
            logger.exception("Failed to disconnect")
            return {
                "success": False,
                "error": str(e),
            }

    @property
    def is_connected(self) -> bool:
        """Check if connected to an accessibility source."""
        return self._is_connected

    @property
    def has_snapshot(self) -> bool:
        """Check if a snapshot is available."""
        return self._current_snapshot is not None

    async def scan_cdp_ports(
        self,
        host: str = "localhost",
        ports: list[int] | None = None,
        timeout: float = 2.0,
    ) -> dict[str, Any]:
        """Scan common CDP ports to find available browsers.

        This helps users discover which browsers are available for
        accessibility capture without manually specifying ports.

        Args:
            host: Host to scan (default: localhost)
            ports: Ports to scan (default: common debugging ports)
            timeout: Timeout per port in seconds

        Returns:
            Dictionary with:
            - success: Whether scan completed
            - available_ports: List of ports with CDP servers
            - targets: Dict mapping port to list of page targets
        """
        import aiohttp

        if ports is None:
            # Common CDP debugging ports
            ports = [9222, 9223, 9224, 9225, 9226, 9229, 9230]

        available_ports: list[int] = []
        targets_by_port: dict[int, list[dict[str, Any]]] = {}

        async def check_port(port: int) -> tuple[int, list[dict[str, Any]] | None]:
            try:
                async with aiohttp.ClientSession() as session:
                    url = f"http://{host}:{port}/json"
                    async with asyncio.wait_for(
                        session.get(url),
                        timeout=timeout,
                    ) as resp:
                        if resp.status == 200:
                            targets = await resp.json()
                            # Filter to page targets
                            page_targets = [
                                {
                                    "title": t.get("title", ""),
                                    "url": t.get("url", ""),
                                    "type": t.get("type", ""),
                                }
                                for t in targets
                                if t.get("type") == "page"
                            ]
                            return port, page_targets
            except Exception:
                pass
            return port, None

        try:
            # Scan all ports concurrently
            tasks = [check_port(port) for port in ports]
            results = await asyncio.gather(*tasks)

            for port, targets in results:
                if targets is not None:
                    available_ports.append(port)
                    targets_by_port[port] = targets

            return {
                "success": True,
                "available_ports": available_ports,
                "targets": targets_by_port,
            }

        except Exception as e:
            logger.exception("Failed to scan CDP ports")
            return {
                "success": False,
                "error": str(e),
            }

    async def list_browser_targets(
        self,
        host: str = "localhost",
        port: int = 9222,
    ) -> dict[str, Any]:
        """List available browser page targets on a CDP port.

        Args:
            host: CDP host
            port: CDP port

        Returns:
            Dictionary with:
            - success: Whether listing succeeded
            - targets: List of page targets with title, url
        """
        import aiohttp

        try:
            async with aiohttp.ClientSession() as session:
                url = f"http://{host}:{port}/json"
                async with session.get(url) as resp:
                    if resp.status != 200:
                        return {
                            "success": False,
                            "error": f"CDP server returned {resp.status}",
                        }
                    all_targets = await resp.json()

            # Filter to page targets and format
            page_targets = [
                {
                    "title": t.get("title", ""),
                    "url": t.get("url", ""),
                    "id": t.get("id", ""),
                    "description": t.get("description", ""),
                }
                for t in all_targets
                if t.get("type") == "page"
            ]

            return {
                "success": True,
                "count": len(page_targets),
                "targets": page_targets,
            }

        except Exception as e:
            logger.exception("Failed to list browser targets")
            return {
                "success": False,
                "error": str(e),
            }

    def compare_snapshots(
        self,
        old_snapshot: dict[str, Any],
        new_snapshot: dict[str, Any],
    ) -> dict[str, Any]:
        """Compare two accessibility snapshots and return differences.

        Useful for detecting UI changes between captures, such as:
        - New elements appearing
        - Elements disappearing
        - Element name/value changes
        - State changes (focused, checked, etc.)

        Args:
            old_snapshot: Previous snapshot dict
            new_snapshot: Current snapshot dict

        Returns:
            Dictionary with:
            - success: Whether comparison completed
            - added: List of refs for new elements
            - removed: List of refs for removed elements
            - changed: List of {ref, changes} for modified elements
            - summary: Human-readable summary
        """
        try:
            old_nodes: dict[str, dict] = {}
            new_nodes: dict[str, dict] = {}

            def collect_nodes(node: dict, collector: dict) -> None:
                ref = node.get("ref", "")
                if ref:
                    collector[ref] = {
                        "role": node.get("role"),
                        "name": node.get("name"),
                        "value": node.get("value"),
                        "is_interactive": node.get("is_interactive"),
                        "state": node.get("state", {}),
                    }
                for child in node.get("children", []):
                    collect_nodes(child, collector)

            if old_snapshot.get("root"):
                collect_nodes(old_snapshot["root"], old_nodes)
            if new_snapshot.get("root"):
                collect_nodes(new_snapshot["root"], new_nodes)

            old_refs = set(old_nodes.keys())
            new_refs = set(new_nodes.keys())

            added = list(new_refs - old_refs)
            removed = list(old_refs - new_refs)
            changed = []

            # Check for changes in common elements
            for ref in old_refs & new_refs:
                old = old_nodes[ref]
                new = new_nodes[ref]
                changes = {}

                if old.get("name") != new.get("name"):
                    changes["name"] = {"old": old.get("name"), "new": new.get("name")}
                if old.get("value") != new.get("value"):
                    changes["value"] = {"old": old.get("value"), "new": new.get("value")}

                # Compare state
                old_state = old.get("state", {})
                new_state = new.get("state", {})
                state_changes = {}
                for key in {"is_focused", "is_disabled", "is_checked", "is_expanded"}:
                    if old_state.get(key) != new_state.get(key):
                        state_changes[key] = {"old": old_state.get(key), "new": new_state.get(key)}

                if state_changes:
                    changes["state"] = state_changes

                if changes:
                    changed.append({"ref": ref, "changes": changes})

            summary = f"Added: {len(added)}, Removed: {len(removed)}, Changed: {len(changed)}"

            return {
                "success": True,
                "added": added,
                "removed": removed,
                "changed": changed,
                "summary": summary,
            }

        except Exception as e:
            logger.exception("Failed to compare snapshots")
            return {
                "success": False,
                "error": str(e),
            }

    async def auto_connect(
        self,
        host: str = "localhost",
        preferred_ports: list[int] | None = None,
    ) -> dict[str, Any]:
        """Automatically connect to the first available CDP target.

        Scans common ports and connects to the first available page.

        Args:
            host: Host to scan
            preferred_ports: Ports to try in order (default: common ports)

        Returns:
            Dictionary with:
            - success: Whether connection succeeded
            - port: Port that was connected to
            - target: Target info (title, url)
        """
        if preferred_ports is None:
            preferred_ports = [9222, 9223, 9224, 9229]

        # Scan ports
        scan_result = await self.scan_cdp_ports(host, preferred_ports)
        if not scan_result.get("success"):
            return scan_result

        available = scan_result.get("available_ports", [])
        if not available:
            return {
                "success": False,
                "error": "No CDP servers found on common ports",
            }

        # Connect to first available port
        port = available[0]
        targets = scan_result.get("targets", {}).get(port, [])

        result = await self.capture_accessibility_tree(
            cdp_host=host,
            cdp_port=port,
        )

        if result.get("success"):
            target_info = targets[0] if targets else {"title": "Unknown", "url": ""}
            return {
                "success": True,
                "port": port,
                "target": target_info,
                "snapshot": result.get("snapshot"),
                "ai_context": result.get("ai_context"),
            }
        else:
            return result
