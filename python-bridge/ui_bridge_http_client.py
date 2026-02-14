"""HTTP-based UIBridgeClientProtocol adapter for the Python bridge.

Makes synchronous HTTP calls to the runner's UI Bridge endpoints
(localhost:9876/ui-bridge/control/*) to interact with the target app's
UI Bridge SDK.

Since the Python bridge runs as a subprocess, synchronous HTTP calls
are appropriate — they block only this process, not the runner's async runtime.
"""

import logging
from typing import Any

import requests

logger = logging.getLogger(__name__)

# Default timeout for UI Bridge HTTP calls (seconds)
DEFAULT_TIMEOUT = 15


class ElementProxy:
    """Lightweight wrapper to give dict elements attribute access."""

    def __init__(self, data: dict) -> None:
        self._data = data
        self.id: str = data.get("id", "")
        self.tag_name: str = data.get("tagName", "")
        self.role: str = data.get("role", "")
        self.accessible_name: str = data.get("accessibleName", "")
        self.visible: bool = data.get("visible", True)
        self.bounds: dict = data.get("bounds", {})
        self.label: str = data.get("label", "")
        self.classes: list[str] = data.get("classes", [])


class FindResponse:
    """Wrapper to match the shape UIBridgeRuntime expects from find()."""

    def __init__(self, elements: list[ElementProxy]) -> None:
        self.elements = elements


class UIBridgeHTTPClient:
    """UIBridgeClientProtocol implementation using HTTP calls to the runner.

    This adapter translates the UIBridgeClientProtocol interface into HTTP
    requests against the runner's UI Bridge control endpoints.
    """

    def __init__(self, base_url: str = "http://localhost:9876") -> None:
        self.base_url = base_url.rstrip("/")
        self._session = requests.Session()

    def _request(
        self,
        method: str,
        path: str,
        json_data: dict[str, Any] | None = None,
        timeout: float = DEFAULT_TIMEOUT,
    ) -> Any:
        """Make an HTTP request to the runner UI Bridge API.

        Returns the parsed response data on success, raises on failure.
        """
        url = f"{self.base_url}{path}"
        try:
            if method == "GET":
                resp = self._session.get(url, timeout=timeout)
            elif method == "POST":
                resp = self._session.post(url, json=json_data, timeout=timeout)
            else:
                raise ValueError(f"Unsupported HTTP method: {method}")

            resp.raise_for_status()
            body = resp.json()

            if body.get("success") is False:
                error_msg = body.get("error", "Unknown error")
                raise RuntimeError(f"UI Bridge API error: {error_msg}")

            return body.get("data", body)

        except requests.ConnectionError as e:
            raise RuntimeError(
                f"Cannot connect to UI Bridge at {url}. "
                f"Is the runner running with a UI Bridge connection? Error: {e}"
            ) from e
        except requests.HTTPError as e:
            raise RuntimeError(
                f"UI Bridge HTTP error: {e.response.status_code} - {e.response.text}"
            ) from e

    def find(
        self,
        *,
        interactive_only: bool = False,
        include_hidden: bool = False,
    ) -> FindResponse:
        """Find elements in the UI via UI Bridge.

        Returns a FindResponse with .elements list of ElementProxy objects,
        matching the shape expected by UIBridgeRuntime.
        """
        params: list[str] = []
        if interactive_only:
            params.append("interactiveOnly=true")
        if include_hidden:
            params.append("includeHidden=true")

        path = "/ui-bridge/control/elements"
        if params:
            path += "?" + "&".join(params)

        data = self._request("GET", path)

        # Normalize response to list of element dicts
        if isinstance(data, dict):
            raw_elements = data.get("elements", [])
        elif isinstance(data, list):
            raw_elements = data
        else:
            raw_elements = []

        elements = [ElementProxy(e) for e in raw_elements if isinstance(e, dict)]
        return FindResponse(elements)

    def get_active_states(self) -> list[str]:
        """Get currently active state IDs from the UI Bridge snapshot.

        Takes a snapshot of the UI, extracts visible element IDs,
        and returns them as potential active state indicators.
        """
        snapshot = self._request("GET", "/ui-bridge/control/snapshot")

        # Extract element IDs from the snapshot
        active_ids: list[str] = []
        if isinstance(snapshot, dict):
            elements = snapshot.get("elements", [])
            for elem in elements:
                if isinstance(elem, dict) and elem.get("visible", True):
                    elem_id = elem.get("id") or elem.get("elementId")
                    if elem_id:
                        active_ids.append(elem_id)

        return active_ids

    def click(self, element_id: str, **kwargs: Any) -> Any:
        """Click an element by ID."""
        return self._request(
            "POST",
            f"/ui-bridge/control/element/{element_id}/action",
            {"action": "click", "params": kwargs if kwargs else None},
        )

    def type(self, element_id: str, text: str, **kwargs: Any) -> Any:
        """Type text into an element by ID."""
        params = {"text": text, **kwargs}
        return self._request(
            "POST",
            f"/ui-bridge/control/element/{element_id}/action",
            {"action": "type", "params": params},
        )

    def execute_transition(self, transition_id: str) -> Any:
        """Execute a transition by ID.

        Note: This is delegated to the UIBridgeRuntime, not the client.
        The runtime handles transition logic (action sequences, state updates).
        This method is here only for protocol completeness.
        """
        raise NotImplementedError(
            "execute_transition should be called on UIBridgeRuntime, not the client"
        )

    def navigate_to(self, target_states: list[str]) -> Any:
        """Navigate to target states.

        Note: This is delegated to the UIBridgeRuntime, not the client.
        The runtime handles pathfinding and multi-step navigation.
        This method is here only for protocol completeness.
        """
        raise NotImplementedError(
            "navigate_to should be called on UIBridgeRuntime, not the client"
        )


class ResolvingUIBridgeClient:
    """Wraps UIBridgeHTTPClient with element ID resolution for cross-session stability.

    On the first find() call, builds an ID remap from stored element snapshots
    to current elements. Subsequent click/type calls transparently resolve
    old IDs to current ones. State detection also works because old IDs are
    injected as aliases into find() results.
    """

    def __init__(self, inner: UIBridgeHTTPClient, resolver: Any) -> None:
        """Initialize with inner client and ElementResolver.

        Args:
            inner: The real HTTP client.
            resolver: ElementResolver instance for ID mapping.
        """
        self._inner = inner
        self._resolver = resolver
        self._remap_built = False

    def find(
        self,
        *,
        interactive_only: bool = False,
        include_hidden: bool = False,
    ) -> FindResponse:
        """Find elements, building ID remap on first call and injecting aliases."""
        response = self._inner.find(
            interactive_only=interactive_only,
            include_hidden=include_hidden,
        )

        if not self._remap_built:
            self._resolver.build_remap(response.elements)
            self._remap_built = True

        # Add old IDs as aliases so state detection (element_ids subset check) still works
        reverse_remap = self._resolver.get_reverse_remap()
        aliases: list[ElementProxy] = []
        for elem in response.elements:
            if elem.id in reverse_remap:
                old_id = reverse_remap[elem.id]
                alias = ElementProxy({**elem._data, "id": old_id})
                aliases.append(alias)

        if aliases:
            response.elements.extend(aliases)

        return response

    def get_active_states(self) -> list[str]:
        """Delegate to inner client."""
        return self._inner.get_active_states()

    def click(self, element_id: str, **kwargs: Any) -> Any:
        """Click with resolved element ID."""
        resolved_id = self._resolver.resolve_id(element_id)
        return self._inner.click(resolved_id, **kwargs)

    def type(self, element_id: str, text: str, **kwargs: Any) -> Any:
        """Type with resolved element ID."""
        resolved_id = self._resolver.resolve_id(element_id)
        return self._inner.type(resolved_id, text, **kwargs)

    def execute_transition(self, transition_id: str) -> Any:
        """Delegate to inner client."""
        return self._inner.execute_transition(transition_id)

    def navigate_to(self, target_states: list[str]) -> Any:
        """Delegate to inner client."""
        return self._inner.navigate_to(target_states)
