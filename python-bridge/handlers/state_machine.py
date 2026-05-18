"""State-machine WS bridge handlers.

Implements the runner-side response to commands routed through the
qontinui-web ``runner_command_ws`` relay. Each public coroutine takes a
JSON-serialisable payload dict (already-validated wire envelope) and
returns a JSON-serialisable response dict.

Wire contract: each handler accepts a JSON payload (already-validated
wire envelope) and returns a JSON-serialisable response dict — either
a successful ``…Response`` or an error envelope
(``invalid_payload`` / ``qontinui_exception`` / etc.). The Rust-side
dispatcher (``mcp/backend_relay.rs``) wraps the response in a
``command_response`` WS frame; web-side ``dispatch_and_wait`` correlates
by ``request_id``.

Phase 2 introduces this module + ``state_machine.discover_ui_bridge``.
Phase 3 adds ``state_machine.ui_bridge.discover`` (discover + the
web-side persistence handshake) and ``state_machine.ui_bridge.pathfind``
(multistate pathfinding over a runner-reconstructed
``UIBridgeRuntime``). Phases 4-7 extend further.
"""

from __future__ import annotations

import logging
import traceback
from typing import Any

from qontinui_schemas.commands.state_machine import (
    DiscoverUIBridgeError,
    DiscoverUIBridgeRequest,
    DiscoverUIBridgeResponse,
    UIBridgeDiscoverError,
    UIBridgeDiscoverRequest,
    UIBridgeDiscoverResponse,
    UIBridgePathfindError,
    UIBridgePathfindRequest,
    UIBridgePathfindResponse,
    UIBridgePathfindStep,
)

logger = logging.getLogger(__name__)


def _state_to_dict(state: Any) -> dict[str, Any]:
    """Serialize a ``DiscoveredState`` dataclass to a dict.

    The result uses the dataclass attribute names (``element_ids``,
    ``render_ids``, etc.) so the qontinui-web pydantic models — which
    declare those names as aliases via ``populate_by_name=True`` — can
    consume the payload directly.
    """
    return {
        "id": state.id,
        "name": state.name,
        "element_ids": list(state.element_ids),
        "render_ids": list(state.render_ids),
        "confidence": state.confidence,
        "position_zone": state.position_zone,
        "landmark_context": state.landmark_context,
        "is_global": state.is_global,
        "is_modal": state.is_modal,
    }


def _element_to_dict(element: Any) -> dict[str, Any]:
    """Serialize a ``DiscoveredElement`` dataclass to a dict."""
    return {
        "id": element.id,
        "name": element.name,
        "element_type": element.element_type,
        "render_ids": list(element.render_ids),
        "fingerprint_hash": element.fingerprint_hash,
        "position_zone": element.position_zone,
    }


async def discover_ui_bridge(payload: dict[str, Any]) -> dict[str, Any]:
    """Handle ``state_machine.discover_ui_bridge`` WS bridge command.

    Runs :class:`qontinui.discovery.state_discovery.StateDiscoveryService`
    against the supplied render-log entries (or a co-occurrence export
    if provided) and returns the discovered states + elements.

    Args:
        payload: Inbound JSON dict (wire envelope; validates against
            :class:`DiscoverUIBridgeRequest`).

    Returns:
        JSON dict; either :class:`DiscoverUIBridgeResponse`
        ``.model_dump(mode='json')`` on success or
        :class:`DiscoverUIBridgeError` ``.model_dump(mode='json')`` on
        runner-side failure.
    """
    try:
        req = DiscoverUIBridgeRequest.model_validate(payload)
    except Exception as exc:  # noqa: BLE001 - intentionally broad
        logger.exception("discover_ui_bridge: payload validation failed")
        return DiscoverUIBridgeError(
            request_id=_safe_request_id(payload),
            error="invalid_payload",
            message=f"payload validation failed: {exc!s}",
            traceback=traceback.format_exc(),
        ).model_dump(mode="json")

    try:
        # Lazy-import qontinui so importing this module is cheap when
        # the command is never invoked.
        from qontinui.discovery.state_discovery import (
            DiscoveryStrategyType,
            StateDiscoveryService,
        )

        strategy_map = {
            "auto": DiscoveryStrategyType.AUTO,
            "fingerprint": DiscoveryStrategyType.FINGERPRINT,
        }
        strategy = strategy_map.get(req.strategy, DiscoveryStrategyType.AUTO)

        service = StateDiscoveryService()
        if req.cooccurrence_export is not None:
            result = service.discover_from_export(
                req.cooccurrence_export,
                strategy=strategy,
            )
        else:
            result = service.discover_from_renders(
                req.renders,
                include_html_ids=req.include_html_ids,
                strategy=strategy,
            )

        response = DiscoverUIBridgeResponse(
            request_id=req.request_id,
            states=[_state_to_dict(s) for s in result.states],
            elements=[_element_to_dict(e) for e in result.elements],
            element_to_renders={
                k: list(v) for k, v in result.element_to_renders.items()
            },
            render_count=result.render_count,
            unique_element_count=result.unique_element_count,
            strategy_used=result.strategy_used.value,
            strategy_metadata=result.strategy_metadata or {},
        )
        return response.model_dump(mode="json")

    except Exception as exc:  # noqa: BLE001 - intentionally broad
        logger.exception("discover_ui_bridge: runner-side execution failed")
        return DiscoverUIBridgeError(
            request_id=req.request_id,
            error="qontinui_exception",
            message=str(exc),
            traceback=traceback.format_exc(),
        ).model_dump(mode="json")


_ZERO_UUID = "00000000-0000-0000-0000-000000000000"


def _safe_request_id(payload: dict[str, Any]) -> str:
    """Best-effort recovery of a UUID-shaped request_id from a payload.

    Returns the all-zero UUID when the payload's ``request_id`` is
    missing or not UUID-shaped. Used by error envelopes so that an
    invalid-payload response still validates against the response
    schema.
    """
    from uuid import UUID

    raw = payload.get("request_id")
    if not isinstance(raw, str):
        return _ZERO_UUID
    try:
        return str(UUID(raw))
    except (ValueError, AttributeError):
        return _ZERO_UUID


# =============================================================================
# Phase 3 — state_machine.ui_bridge.discover
# =============================================================================


async def ui_bridge_discover(payload: dict[str, Any]) -> dict[str, Any]:
    """Handle ``state_machine.ui_bridge.discover`` WS bridge command.

    Runs :class:`qontinui.discovery.state_discovery.StateDiscoveryService`
    against the supplied render-log entries (or a co-occurrence export
    if provided) and returns the discovered states + elements. The web
    side persists the result to ``ui_bridge_state_configs`` +
    ``ui_bridge_states`` rows after the response returns; the runner
    is stateless w.r.t. persistence.

    Args:
        payload: Inbound JSON dict (wire envelope; validates against
            :class:`UIBridgeDiscoverRequest`).

    Returns:
        JSON dict; either :class:`UIBridgeDiscoverResponse`
        ``.model_dump(mode='json')`` on success or
        :class:`UIBridgeDiscoverError` ``.model_dump(mode='json')`` on
        runner-side failure.
    """
    try:
        req = UIBridgeDiscoverRequest.model_validate(payload)
    except Exception as exc:  # noqa: BLE001 - intentionally broad
        logger.exception("ui_bridge_discover: payload validation failed")
        return UIBridgeDiscoverError(
            request_id=_safe_request_id(payload),
            error="invalid_payload",
            message=f"payload validation failed: {exc!s}",
            traceback=traceback.format_exc(),
        ).model_dump(mode="json")

    try:
        from qontinui.discovery.state_discovery import (
            DiscoveryStrategyType,
            StateDiscoveryService,
        )

        strategy_map = {
            "auto": DiscoveryStrategyType.AUTO,
            "fingerprint": DiscoveryStrategyType.FINGERPRINT,
        }
        strategy = strategy_map.get(req.strategy, DiscoveryStrategyType.AUTO)

        service = StateDiscoveryService()
        if req.cooccurrence_export is not None:
            result = service.discover_from_export(
                req.cooccurrence_export,
                strategy=strategy,
            )
        else:
            result = service.discover_from_renders(
                req.renders,
                include_html_ids=req.include_html_ids,
                strategy=strategy,
            )

        response = UIBridgeDiscoverResponse(
            request_id=req.request_id,
            states=[_state_to_dict(s) for s in result.states],
            elements=[_element_to_dict(e) for e in result.elements],
            element_to_renders={
                k: list(v) for k, v in result.element_to_renders.items()
            },
            render_count=result.render_count,
            unique_element_count=result.unique_element_count,
            strategy_used=result.strategy_used.value,
            strategy_metadata=result.strategy_metadata or {},
        )
        return response.model_dump(mode="json")

    except Exception as exc:  # noqa: BLE001 - intentionally broad
        logger.exception("ui_bridge_discover: runner-side execution failed")
        return UIBridgeDiscoverError(
            request_id=req.request_id,
            error="qontinui_exception",
            message=str(exc),
            traceback=traceback.format_exc(),
        ).model_dump(mode="json")


# =============================================================================
# Phase 3 — state_machine.ui_bridge.pathfind
# =============================================================================


class _PathfindStubClient:
    """Stub UI Bridge client for runtime construction during pathfinding.

    Pathfinding is read-only; it never executes transitions. This stub
    matches the shape used by the pre-#136 web-side endpoint
    (``qontinui-web/backend/app/api/v1/endpoints/ui_bridge_states.py``
    pathfind handler) so the multistate pathfinding code path runs as
    if a real client were attached.
    """

    def __init__(self, from_states: list[str]) -> None:
        self._from_states = from_states

    def find(self, **_kwargs: Any) -> Any:
        return type("R", (), {"elements": []})()

    def get_active_states(self) -> list[str]:
        return list(self._from_states)

    def execute_transition(self, _transition_id: str) -> Any:
        return type("R", (), {"success": True})()

    def navigate_to(self, _targets: list[str]) -> Any:
        raise NotImplementedError

    def click(self, _element_id: str, **_kwargs: Any) -> Any:
        return type("R", (), {"success": True})()

    def type(self, _element_id: str, _text: str, **_kwargs: Any) -> Any:
        return type("R", (), {"success": True})()


async def ui_bridge_pathfind(payload: dict[str, Any]) -> dict[str, Any]:
    """Handle ``state_machine.ui_bridge.pathfind`` WS bridge command.

    Reconstructs a :class:`qontinui.state_machine.ui_bridge_runtime.UIBridgeRuntime`
    from the serialised config (states + transitions) carried in the
    request, then invokes ``find_path`` with the supplied
    ``from_states`` / ``target_states``. The runner is stateless: the
    web side ships the persisted ``UIBridgeRuntimeConfig`` payload in
    each request to avoid a runner -> web -> runner config-fetch
    round-trip.

    Args:
        payload: Inbound JSON dict (wire envelope; validates against
            :class:`UIBridgePathfindRequest`).

    Returns:
        JSON dict; either :class:`UIBridgePathfindResponse`
        ``.model_dump(mode='json')`` on success (``found=True/False``
        for the no-path case) or
        :class:`UIBridgePathfindError` ``.model_dump(mode='json')`` on
        runner-side failure.
    """
    try:
        req = UIBridgePathfindRequest.model_validate(payload)
    except Exception as exc:  # noqa: BLE001 - intentionally broad
        logger.exception("ui_bridge_pathfind: payload validation failed")
        return UIBridgePathfindError(
            request_id=_safe_request_id(payload),
            error="invalid_payload",
            message=f"payload validation failed: {exc!s}",
            traceback=traceback.format_exc(),
        ).model_dump(mode="json")

    try:
        from qontinui.state_machine.ui_bridge_runtime import (
            MULTISTATE_AVAILABLE,
            UIBridgeRuntime,
            UIBridgeRuntimeConfig,
        )
        from qontinui.state_machine.ui_bridge_runtime import (
            UIBridgeState as UIBridgeStateData,
        )
        from qontinui.state_machine.ui_bridge_runtime import (
            UIBridgeTransition as UIBridgeTransitionData,
        )

        if not MULTISTATE_AVAILABLE:
            return UIBridgePathfindError(
                request_id=req.request_id,
                error="multistate_unavailable",
                message=(
                    "multistate package is not installed on this runner; "
                    "pathfinding is unavailable. Install the runner's "
                    "[pathfinding] extra to enable."
                ),
                traceback=None,
            ).model_dump(mode="json")

        stub = _PathfindStubClient(req.from_states)
        runtime_config = UIBridgeRuntimeConfig(enable_reliability_tracking=False)
        runtime = UIBridgeRuntime(stub, runtime_config)

        # Register states from the serialised config
        for state_dict in req.config.get("states", []):
            runtime.register_state(
                UIBridgeStateData(
                    id=state_dict["state_id"],
                    name=state_dict["name"],
                    element_ids=list(state_dict.get("element_ids") or []),
                )
            )

        # Register transitions from the serialised config
        for trans_dict in req.config.get("transitions", []):
            runtime.register_transition(
                UIBridgeTransitionData(
                    id=trans_dict["transition_id"],
                    name=trans_dict["name"],
                    from_states=list(trans_dict.get("from_states") or []),
                    activate_states=list(trans_dict.get("activate_states") or []),
                    exit_states=list(trans_dict.get("exit_states") or []),
                    actions=list(trans_dict.get("actions") or []),
                    path_cost=float(trans_dict.get("path_cost", 1.0)),
                    stays_visible=bool(trans_dict.get("stays_visible", False)),
                )
            )

        path = runtime.find_path(
            target_states=req.target_states,
            from_states=set(req.from_states),
        )

        if not path:
            return UIBridgePathfindResponse(
                request_id=req.request_id,
                found=False,
                steps=[],
                total_cost=0.0,
                error="No path found between specified states",
            ).model_dump(mode="json")

        # Build response steps from the path
        steps: list[UIBridgePathfindStep] = []
        total_cost = 0.0
        for trans in path.transitions_sequence:
            ui_trans = runtime._ui_transitions.get(trans.id)  # noqa: SLF001
            if ui_trans is None:
                continue
            steps.append(
                UIBridgePathfindStep(
                    transition_id=ui_trans.id,
                    transition_name=ui_trans.name,
                    from_states=list(ui_trans.from_states),
                    activate_states=list(ui_trans.activate_states),
                    exit_states=list(ui_trans.exit_states),
                    path_cost=float(ui_trans.path_cost),
                )
            )
            total_cost += float(ui_trans.path_cost)

        return UIBridgePathfindResponse(
            request_id=req.request_id,
            found=True,
            steps=steps,
            total_cost=total_cost,
            error=None,
        ).model_dump(mode="json")

    except Exception as exc:  # noqa: BLE001 - intentionally broad
        logger.exception("ui_bridge_pathfind: runner-side execution failed")
        return UIBridgePathfindError(
            request_id=req.request_id,
            error="qontinui_exception",
            message=str(exc),
            traceback=traceback.format_exc(),
        ).model_dump(mode="json")


__all__ = [
    "discover_ui_bridge",
    "ui_bridge_discover",
    "ui_bridge_pathfind",
]
