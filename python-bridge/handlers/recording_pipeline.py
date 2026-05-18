"""Recording-pipeline WS bridge handlers.

Implements the runner-side response to long-running
``recording_pipeline.{process,process_with_playbook,merge}`` commands
routed through the qontinui-web ``runner_command_ws`` relay.

Wire contract: each handler takes a JSON payload (already-validated
wire envelope) and returns a JSON-serialisable response dict. The
response carries the final terminal-event shape
(:class:`ProcessRecordingResult` from
``qontinui_schemas.commands.recording_pipeline``) directly in the
``command_response`` frame; the web side filters by ``request_id`` via
the existing :meth:`CommandRelayService.dispatch_and_wait` machinery.

Per Phase 4's plan §"Per the plan, progress streaming is the design
goal. ... If the architecture changes are larger than a single-phase
scope, document the deviation in your report + ship the simpler
alternative (no progress; final result only)":

- This implementation ships the **simpler alternative**: the handler
  runs the pipeline synchronously inside the dispatcher subprocess and
  returns ONE combined ``recording_pipeline_result``-shaped response
  via the standard one-shot dispatch reply path.
- Granular progress events
  (:class:`qontinui_schemas.commands.recording_pipeline.ProcessRecordingProgress`)
  are defined in the schema (so callers can adopt them later) but the
  runner does not currently emit any. The web side persists only the
  final result.
- Deferred follow-up: extend
  ``src-tauri/src/mcp/ws_bridge_dispatch.rs`` with an event-streaming
  mode (publishing zero or more ``recording_pipeline_progress`` frames
  on ``runner:responses:{runner_id}`` between the request and the
  terminal response) once an empirical UX need surfaces.

Phase 4 of plan ``plans/2026-05-17-web-runner-ws-bridge-plan-b.md``.
"""

from __future__ import annotations

import logging
import traceback
from typing import Any

from qontinui_schemas.commands.recording_pipeline import (
    DispatchAck,
    MergeRecordingRequest,
    ProcessRecordingRequest,
    ProcessRecordingResult,
    ProcessRecordingWithPlaybookRequest,
    RecordingPipelineError,
)

logger = logging.getLogger(__name__)


_ZERO_UUID = "00000000-0000-0000-0000-000000000000"


def _safe_request_id(payload: dict[str, Any]) -> str:
    """Best-effort recovery of a UUID-shaped ``request_id`` from a payload."""
    from uuid import UUID

    raw = payload.get("request_id")
    if not isinstance(raw, str):
        return _ZERO_UUID
    try:
        return str(UUID(raw))
    except (ValueError, AttributeError):
        return _ZERO_UUID


def _safe_run_id(payload: dict[str, Any]) -> str:
    """Best-effort recovery of a UUID-shaped ``run_id`` from a payload."""
    from uuid import UUID

    raw = payload.get("run_id")
    if not isinstance(raw, str):
        return _ZERO_UUID
    try:
        return str(UUID(raw))
    except (ValueError, AttributeError):
        return _ZERO_UUID


def _ui_state_to_dict(state: Any) -> dict[str, Any]:
    """Serialize a ``UIBridgeState`` dataclass to a dict.

    Mirrors the dataclass fields at
    ``qontinui/src/qontinui/state_machine/ui_bridge_runtime.py:183``
    plus a derived ``element_count`` field that the legacy web-side
    response model surfaces directly (lets the web handler short-circuit
    a recompute).
    """
    return {
        "id": state.id,
        "name": state.name,
        "element_ids": list(state.element_ids),
        "element_count": len(state.element_ids),
        "blocking": state.blocking,
        "blocks": list(state.blocks) if state.blocks else [],
        "group": state.group,
        "path_cost": state.path_cost,
        "metadata": dict(state.metadata) if state.metadata else {},
    }


def _ui_transition_to_dict(transition: Any) -> dict[str, Any]:
    """Serialize a ``UIBridgeTransition`` dataclass to a dict.

    Mirrors the dataclass fields at
    ``qontinui/src/qontinui/state_machine/ui_bridge_runtime.py:211``.
    """
    return {
        "id": transition.id,
        "name": transition.name,
        "from_states": list(transition.from_states),
        "activate_states": list(transition.activate_states),
        "exit_states": list(transition.exit_states),
        "actions": list(transition.actions) if transition.actions else [],
        "activate_groups": list(transition.activate_groups)
        if transition.activate_groups
        else [],
        "exit_groups": list(transition.exit_groups) if transition.exit_groups else [],
        "path_cost": transition.path_cost,
        "stays_visible": transition.stays_visible,
        "metadata": dict(transition.metadata) if transition.metadata else {},
    }


def _result_to_dict(result: Any, *, playbook_content: str | None = None) -> dict[str, Any]:
    """Serialize a ``RecordingPipelineResult`` to the wire-payload dict.

    Mirrors the dataclass at
    ``qontinui/src/qontinui/state_machine/recording_pipeline.py:70``,
    plus an optional ``playbook_content`` field populated by the
    ``process_with_playbook`` handler.
    """
    payload: dict[str, Any] = {
        "session_id": result.session_id,
        "state_count": result.state_count,
        "transition_count": result.transition_count,
        "global_state_count": result.global_state_count,
        "modal_state_count": result.modal_state_count,
        "states": [_ui_state_to_dict(s) for s in result.states],
        "transitions": [_ui_transition_to_dict(t) for t in result.transitions],
    }
    if playbook_content is not None:
        payload["playbook_content"] = playbook_content
    return payload


def _build_config(config_overrides: dict[str, Any]) -> Any:
    """Build a ``RecordingPipelineConfig`` from the request overrides.

    Forces ``persist=False`` because web is responsible for PG
    persistence; the runner stays stateless w.r.t. storage.
    """
    from qontinui.state_machine import RecordingPipelineConfig  # type: ignore[attr-defined]

    config = RecordingPipelineConfig()
    for key, value in (config_overrides or {}).items():
        if hasattr(config, key):
            setattr(config, key, value)
    config.persist = False
    return config


# =============================================================================
# recording_pipeline.process
# =============================================================================


async def process_recording(payload: dict[str, Any]) -> dict[str, Any]:
    """Handle ``recording_pipeline.process`` WS bridge command.

    Runs :meth:`qontinui.state_machine.RecordingPipeline.process_recording`
    against the supplied ``recording_export`` and returns a
    :class:`ProcessRecordingResult`-shaped wire envelope.

    Args:
        payload: Inbound JSON dict (wire envelope; validates against
            :class:`ProcessRecordingRequest`).

    Returns:
        JSON dict; either a :class:`ProcessRecordingResult`
        ``.model_dump(mode='json')`` (status=completed on success,
        status=failed on runtime failure) or a
        :class:`RecordingPipelineError` envelope for payload-validation
        failure (where ``run_id`` may not be recoverable).
    """
    try:
        req = ProcessRecordingRequest.model_validate(payload)
    except Exception as exc:  # noqa: BLE001 - intentionally broad
        logger.exception("process_recording: payload validation failed")
        return RecordingPipelineError(
            command="recording_pipeline.process",
            request_id=_safe_request_id(payload),
            run_id=_safe_run_id(payload),
            error="invalid_payload",
            message=f"payload validation failed: {exc!s}",
            traceback=traceback.format_exc(),
        ).model_dump(mode="json")

    try:
        # Lazy-import qontinui so importing this module is cheap when
        # the command is never invoked.
        from qontinui.state_machine import RecordingPipeline  # type: ignore[attr-defined]

        config = _build_config(req.config)
        pipeline = RecordingPipeline(persistence=None, config=config)
        result = pipeline.process_recording(req.recording_export)

        wire_result = ProcessRecordingResult(
            run_id=req.run_id,
            command="recording_pipeline.process",
            status="completed",
            result=_result_to_dict(result),
        )
        # Wrap as a command_response envelope keyed by request_id so the
        # web-side dispatch_and_wait correlates correctly.
        return {
            "type": "command_response",
            "command": "recording_pipeline.process",
            "request_id": str(req.request_id),
            **wire_result.model_dump(mode="json"),
        }

    except Exception as exc:  # noqa: BLE001 - intentionally broad
        logger.exception("process_recording: runner-side execution failed")
        wire_result = ProcessRecordingResult(
            run_id=req.run_id,
            command="recording_pipeline.process",
            status="failed",
            error={
                "error": "qontinui_exception",
                "message": str(exc),
                "traceback": traceback.format_exc(),
            },
        )
        return {
            "type": "command_response",
            "command": "recording_pipeline.process",
            "request_id": str(req.request_id),
            **wire_result.model_dump(mode="json"),
        }


# =============================================================================
# recording_pipeline.process_with_playbook
# =============================================================================


async def process_with_playbook(payload: dict[str, Any]) -> dict[str, Any]:
    """Handle ``recording_pipeline.process_with_playbook`` WS bridge command.

    Runs :meth:`RecordingPipeline.process_recording` then
    :func:`qontinui.state_machine.playbook_generator.generate_playbook`
    over the same result. The returned ``result.result`` dict carries
    an additional ``playbook_content`` field with the generated
    markdown body.
    """
    try:
        req = ProcessRecordingWithPlaybookRequest.model_validate(payload)
    except Exception as exc:  # noqa: BLE001 - intentionally broad
        logger.exception("process_with_playbook: payload validation failed")
        return RecordingPipelineError(
            command="recording_pipeline.process_with_playbook",
            request_id=_safe_request_id(payload),
            run_id=_safe_run_id(payload),
            error="invalid_payload",
            message=f"payload validation failed: {exc!s}",
            traceback=traceback.format_exc(),
        ).model_dump(mode="json")

    try:
        from qontinui.state_machine import RecordingPipeline  # type: ignore[attr-defined]
        from qontinui.state_machine.playbook_generator import (  # type: ignore[attr-defined]
            generate_playbook,
        )

        config = _build_config(req.config)
        pipeline = RecordingPipeline(persistence=None, config=config)
        result = pipeline.process_recording(req.recording_export)

        playbook_content = generate_playbook(
            states=result.states,
            transitions=result.transitions,
            variables=req.variables,
            interactions=req.recording_export.get("transitions", []),
            app_name=req.app_name,
            app_url=req.app_url,
        )

        wire_result = ProcessRecordingResult(
            run_id=req.run_id,
            command="recording_pipeline.process_with_playbook",
            status="completed",
            result=_result_to_dict(result, playbook_content=playbook_content),
        )
        return {
            "type": "command_response",
            "command": "recording_pipeline.process_with_playbook",
            "request_id": str(req.request_id),
            **wire_result.model_dump(mode="json"),
        }

    except Exception as exc:  # noqa: BLE001 - intentionally broad
        logger.exception("process_with_playbook: runner-side execution failed")
        wire_result = ProcessRecordingResult(
            run_id=req.run_id,
            command="recording_pipeline.process_with_playbook",
            status="failed",
            error={
                "error": "qontinui_exception",
                "message": str(exc),
                "traceback": traceback.format_exc(),
            },
        )
        return {
            "type": "command_response",
            "command": "recording_pipeline.process_with_playbook",
            "request_id": str(req.request_id),
            **wire_result.model_dump(mode="json"),
        }


# =============================================================================
# recording_pipeline.merge
# =============================================================================


def _dict_to_ui_state(d: dict[str, Any]) -> Any:
    """Reconstruct a ``UIBridgeState`` dataclass from a wire dict."""
    from qontinui.state_machine.ui_bridge_runtime import UIBridgeState  # type: ignore[attr-defined]

    return UIBridgeState(
        id=d["id"],
        name=d.get("name", d["id"]),
        element_ids=list(d.get("element_ids", [])),
        blocking=bool(d.get("blocking", False)),
        blocks=list(d.get("blocks", [])),
        group=d.get("group"),
        path_cost=float(d.get("path_cost", 1.0)),
        metadata=dict(d.get("metadata", {})),
    )


def _dict_to_ui_transition(d: dict[str, Any]) -> Any:
    """Reconstruct a ``UIBridgeTransition`` dataclass from a wire dict."""
    from qontinui.state_machine.ui_bridge_runtime import UIBridgeTransition  # type: ignore[attr-defined]

    return UIBridgeTransition(
        id=d["id"],
        name=d.get("name", d["id"]),
        from_states=list(d.get("from_states", [])),
        activate_states=list(d.get("activate_states", [])),
        exit_states=list(d.get("exit_states", [])),
        actions=list(d.get("actions", [])),
        activate_groups=list(d.get("activate_groups", [])),
        exit_groups=list(d.get("exit_groups", [])),
        path_cost=float(d.get("path_cost", 1.0)),
        stays_visible=bool(d.get("stays_visible", False)),
        metadata=dict(d.get("metadata", {})),
    )


async def merge_recording(payload: dict[str, Any]) -> dict[str, Any]:
    """Handle ``recording_pipeline.merge`` WS bridge command.

    Runs :meth:`RecordingPipeline.merge_recording` over the new
    recording's export + the supplied prior states + transitions
    (web fetched them from PG and forwarded them so the runner stays
    stateless w.r.t. persistence).
    """
    try:
        req = MergeRecordingRequest.model_validate(payload)
    except Exception as exc:  # noqa: BLE001 - intentionally broad
        logger.exception("merge_recording: payload validation failed")
        return RecordingPipelineError(
            command="recording_pipeline.merge",
            request_id=_safe_request_id(payload),
            run_id=_safe_run_id(payload),
            error="invalid_payload",
            message=f"payload validation failed: {exc!s}",
            traceback=traceback.format_exc(),
        ).model_dump(mode="json")

    try:
        from qontinui.state_machine import RecordingPipeline  # type: ignore[attr-defined]

        config = _build_config(req.config)
        pipeline = RecordingPipeline(persistence=None, config=config)

        existing_states = [_dict_to_ui_state(s) for s in req.existing_states]
        existing_transitions = [
            _dict_to_ui_transition(t) for t in req.existing_transitions
        ]

        result = pipeline.merge_recording(
            export_data=req.recording_export,
            existing_states=existing_states,
            existing_transitions=existing_transitions,
        )

        wire_result = ProcessRecordingResult(
            run_id=req.run_id,
            command="recording_pipeline.merge",
            status="completed",
            result=_result_to_dict(result),
        )
        return {
            "type": "command_response",
            "command": "recording_pipeline.merge",
            "request_id": str(req.request_id),
            **wire_result.model_dump(mode="json"),
        }

    except Exception as exc:  # noqa: BLE001 - intentionally broad
        logger.exception("merge_recording: runner-side execution failed")
        wire_result = ProcessRecordingResult(
            run_id=req.run_id,
            command="recording_pipeline.merge",
            status="failed",
            error={
                "error": "qontinui_exception",
                "message": str(exc),
                "traceback": traceback.format_exc(),
            },
        )
        return {
            "type": "command_response",
            "command": "recording_pipeline.merge",
            "request_id": str(req.request_id),
            **wire_result.model_dump(mode="json"),
        }


# Silence "imported but unused" for the DispatchAck type — re-exported
# for the future event-streaming follow-up referenced in the module
# docstring (the runner-side equivalent will emit a DispatchAck on
# command receipt, before the long-running pipeline starts).
_ = DispatchAck


__all__ = [
    "process_recording",
    "process_with_playbook",
    "merge_recording",
]
