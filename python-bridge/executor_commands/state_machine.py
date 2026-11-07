"""UI Bridge state-machine commands: generate, load, ``sm_*`` queries, clear.

A mixin of ``QontinuiExecutor``. Its methods were moved verbatim from
``qontinui_executor.py`` by plan
2026-10-04-runner-python-executor-routes-118-commands-through-one-if-chain
and still read executor state through ``self``. ``COMMANDS`` maps each command
name to its handler method.
"""

import dataclasses
import sys
from typing import Any, ClassVar

from ._host import ExecutorHost


class StateMachineCommands(ExecutorHost):
    """UI Bridge state-machine commands: generate, load, ``sm_*`` queries, clear."""

    COMMANDS: ClassVar[dict[str, str]] = {
        "generate_state_machine": "_handle_generate_state_machine",
        # UI Bridge State Machine commands
        "load_state_machine": "_handle_load_state_machine",
        "get_state_machine_status": "_handle_get_state_machine_status",
        "sm_execute_transition": "_handle_sm_execute_transition",
        "sm_navigate_to_states": "_handle_sm_navigate_to_states",
        "sm_get_active_states": "_handle_sm_get_active_states",
        "sm_get_available_transitions": "_handle_sm_get_available_transitions",
        "sm_get_permitted_triggers": "_handle_sm_get_permitted_triggers",
        "sm_get_blocked_triggers": "_handle_sm_get_blocked_triggers",
        "sm_get_mermaid_diagram": "_handle_sm_get_mermaid_diagram",
        "clear_state_machine": "_handle_clear_state_machine",
    }

    def _handle_generate_state_machine(self, params: dict[str, Any]) -> dict[str, Any]:
        """Generate a state machine configuration from approved templates.

        Uses the qontinui library's ClickToStateMachineBuilder to convert
        user-approved template candidates into a state machine configuration.

        Args:
            params: Command parameters:
                - approved_templates: List of approved template dictionaries (required)
                - grouping_method: How to group templates into states. Options:
                    - "state_hints": Use template's state_hint field (default)
                    - "user_assignments": Use explicit state_assignments mapping
                    - "co_occurrence": Group by which templates appear together
                    - "single_state": Put all templates in one state
                    - "one_per_template": Each template becomes its own state
                - state_assignments: Dict mapping state_name to list of template IDs
                    (required if grouping_method="user_assignments")
                - session_id: Session ID for metadata (optional)
                - video_path: Path to video file used for capture (optional, but
                    REQUIRED for co_occurrence grouping to analyze which templates
                    appear together)
                - co_occurrence_sample_interval: For co_occurrence grouping, check
                    every N frames (default: 30, ~1 per second at 30fps)
                - output_path: Path to write the config JSON file (optional)

        Returns:
            Dictionary with:
                - success: Whether generation succeeded
                - state_machine: The generated state machine configuration dict
                - states_count: Number of states in the generated config
                - state_images_count: Total number of state images
                - transitions_count: Number of transitions
                - output_path: Path where config was saved (if output_path provided)
                - error: Error message (if failed)
        """
        from pathlib import Path

        approved_templates_data = params.get("approved_templates")
        if not approved_templates_data:
            return {"success": False, "error": "approved_templates is required"}

        if not isinstance(approved_templates_data, list):
            return {"success": False, "error": "approved_templates must be a list"}

        grouping_method = params.get("grouping_method", "state_hints")
        state_assignments = params.get("state_assignments")
        session_id = params.get("session_id", "")
        video_path = params.get("video_path")
        output_path = params.get("output_path")
        co_occurrence_sample_interval = params.get("co_occurrence_sample_interval", 30)

        # Validate grouping method
        valid_methods = [
            "state_hints",
            "user_assignments",
            "co_occurrence",
            "single_state",
            "one_per_template",
        ]
        if grouping_method not in valid_methods:
            return {
                "success": False,
                "error": f"Invalid grouping_method. Must be one of: {valid_methods}",
            }

        # Validate state_assignments if using user_assignments method
        if grouping_method == "user_assignments" and not state_assignments:
            return {
                "success": False,
                "error": "state_assignments is required when grouping_method='user_assignments'",
            }

        # Validate video_path if using co_occurrence method
        if grouping_method == "co_occurrence" and not video_path:
            return {
                "success": False,
                "error": "video_path is required when grouping_method='co_occurrence'. "
                "The video is analyzed to determine which templates appear together.",
            }

        try:
            # Import qontinui library modules
            from qontinui.discovery.click_analysis import (
                ApprovedTemplate,
                ClickToStateMachineBuilder,
            )

            # Convert dictionaries to ApprovedTemplate objects
            approved_templates: list[ApprovedTemplate] = []
            for template_data in approved_templates_data:
                try:
                    template = ApprovedTemplate.from_dict(template_data)
                    approved_templates.append(template)
                except Exception as e:
                    self.event_manager.emit_log(
                        "warning",
                        f"Failed to parse template {template_data.get('id', 'unknown')}: {e}",
                    )
                    continue

            if not approved_templates:
                return {
                    "success": False,
                    "error": "No valid templates could be parsed from approved_templates",
                }

            self.event_manager.emit_log(
                "info",
                f"Building state machine from {len(approved_templates)} approved templates "
                f"using {grouping_method} grouping",
            )

            # Emit progress event
            self.event_manager.emit_event_wrapper(
                "state_machine_generation_started",
                {
                    "template_count": len(approved_templates),
                    "grouping_method": grouping_method,
                    "session_id": session_id,
                },
            )

            # Build the state machine
            builder = ClickToStateMachineBuilder()
            result = builder.build_from_templates(
                templates=approved_templates,
                grouping_method=grouping_method,
                state_assignments=state_assignments,
                session_id=session_id,
                video_path=Path(video_path) if video_path else None,
                co_occurrence_sample_interval=co_occurrence_sample_interval,
            )

            # Convert to dictionary for JSON serialization
            state_machine_dict = result.to_dict()

            self.event_manager.emit_log(
                "info",
                f"Generated state machine: {result.state_count} states, "
                f"{result.state_image_count} state images, {result.transition_count} transitions",
            )

            # Write to file if output_path provided
            if output_path:
                import json

                output_file = Path(output_path)
                output_file.parent.mkdir(parents=True, exist_ok=True)
                with open(output_file, "w") as f:
                    json.dump(state_machine_dict, f, indent=2)
                self.event_manager.emit_log("info", f"State machine config saved to: {output_path}")

            # Emit completion event
            self.event_manager.emit_event_wrapper(
                "state_machine_generation_completed",
                {
                    "states_count": result.state_count,
                    "state_images_count": result.state_image_count,
                    "transitions_count": result.transition_count,
                    "session_id": session_id,
                    "output_path": output_path,
                },
            )

            return {
                "success": True,
                "state_machine": state_machine_dict,
                "states_count": result.state_count,
                "state_images_count": result.state_image_count,
                "transitions_count": result.transition_count,
                "output_path": output_path,
            }

        except ImportError as e:
            self.event_manager.emit_log("error", f"Failed to import qontinui library: {e}")
            return {"success": False, "error": f"qontinui library not available: {e}"}
        except Exception as e:
            self.event_manager.emit_log("error", f"Failed to generate state machine: {e}")
            import traceback

            traceback.print_exc(file=sys.stderr)
            return {"success": False, "error": str(e)}

    # =========================================================================
    # UI Bridge State Machine handlers
    # =========================================================================

    @staticmethod
    def _normalize_sm_config(config_data: dict[str, Any]) -> dict[str, Any]:
        """Normalize state machine config from CRUD API format to from_dict format.

        The CRUD API returns states/transitions as lists with database fields
        (state_id, extra_metadata, config_id, etc). UIBridgeRuntime.from_dict
        expects dicts keyed by ID with UIBridgeState/UIBridgeTransition fields.
        """
        result: dict[str, Any] = {}

        # Transform states: list -> dict keyed by state_id
        states = config_data.get("states", {})
        if isinstance(states, list):
            result["states"] = {}
            for s in states:
                sid = s.get("state_id", s.get("id", ""))
                result["states"][sid] = {
                    "id": sid,
                    "name": s.get("name", sid),
                    "element_ids": s.get("element_ids", []),
                    "metadata": s.get("extra_metadata", s.get("metadata", {})),
                }
        else:
            result["states"] = states

        # Transform transitions: list -> dict keyed by transition_id
        transitions = config_data.get("transitions", {})
        if isinstance(transitions, list):
            result["transitions"] = {}
            for t in transitions:
                tid = t.get("transition_id", t.get("id", ""))
                result["transitions"][tid] = {
                    "id": tid,
                    "name": t.get("name", tid),
                    "from_states": t.get("from_states", []),
                    "activate_states": t.get("activate_states", []),
                    "exit_states": t.get("exit_states", []),
                    "actions": t.get("actions", []),
                    "path_cost": t.get("path_cost", 1.0),
                    "stays_visible": t.get("stays_visible", False),
                    "metadata": t.get("extra_metadata", t.get("metadata", {})),
                }
        else:
            result["transitions"] = transitions

        if "config" in config_data:
            result["config"] = config_data["config"]

        return result

    def _handle_load_state_machine(self, params: dict[str, Any]) -> dict[str, Any]:
        """Load a UI Bridge state machine configuration.

        Creates a UIBridgeRuntime from the exported config and stores it
        in memory for subsequent operations. Persists to SQLite for
        auto-reload on restart.

        Args:
            params: Must contain "config" key with the exported state machine JSON.
        """
        config_data = params.get("config")
        if not config_data:
            return {"success": False, "error": "config is required"}

        try:
            from qontinui.state_machine.ui_bridge_runtime import UIBridgeRuntime

            from element_resolver import ElementResolver
            from ui_bridge_http_client import (
                ResolvingUIBridgeClient,
                UIBridgeHTTPClient,
            )

            persistence = self._get_sm_persistence()

            # Create HTTP client with resolving wrapper
            inner_client = UIBridgeHTTPClient(self._get_runner_api_base())
            resolver = ElementResolver(persistence)
            client = ResolvingUIBridgeClient(inner_client, resolver)

            # Normalize CRUD API format to from_dict format if needed
            normalized = self._normalize_sm_config(config_data)
            runtime = UIBridgeRuntime.from_dict(normalized, client)
            self._ui_bridge_runtime = runtime
            self._element_resolver = resolver

            # Persist config for auto-reload on restart
            persistence.save_config(config_data)

            # Capture element snapshots for cross-session ID mapping
            try:
                elements = inner_client.find().elements
                resolver.capture_snapshots(elements)
            except Exception:
                pass  # UI Bridge may not be connected yet

            stats = runtime.get_statistics()
            self.event_manager.emit_log(
                "info",
                f"State machine loaded: {stats['states']['registered']} states, "
                f"{stats['transitions']['registered']} transitions",
            )
            return {"success": True, "statistics": stats}

        except ImportError as e:
            return {"success": False, "error": f"Required library not available: {e}"}
        except Exception as e:
            self.event_manager.emit_log("error", f"Failed to load state machine: {e}")
            return {"success": False, "error": str(e)}

    def _handle_get_state_machine_status(self) -> dict[str, Any]:
        """Get the status of the loaded state machine."""
        if self._ui_bridge_runtime is None:
            return {
                "success": True,
                "loaded": False,
                "message": "No state machine loaded",
            }

        try:
            stats = self._ui_bridge_runtime.get_statistics()
            return {
                "success": True,
                "loaded": True,
                "statistics": stats,
            }
        except Exception as e:
            return {"success": False, "error": str(e)}

    def _handle_sm_execute_transition(self, params: dict[str, Any]) -> dict[str, Any]:
        """Execute a specific transition by ID."""
        if self._ui_bridge_runtime is None:
            return {"success": False, "error": "No state machine loaded"}

        transition_id = params.get("transition_id")
        if not transition_id:
            return {"success": False, "error": "transition_id is required"}

        try:
            result = self._ui_bridge_runtime.execute_transition(transition_id)
            # The runtime reports a failed transition (an unregistered id, a
            # failed action) by RETURNING a result whose ``success`` is false,
            # not by raising; reporting that as success made every caller read
            # a failed transition as passed.
            if isinstance(result, dict):
                ok = result.get("success") is True
                error = result.get("error")
                payload: Any = result
            else:
                ok = getattr(result, "success", None) is True
                error = getattr(result, "error", None)
                payload = (
                    dataclasses.asdict(result)
                    if dataclasses.is_dataclass(result) and not isinstance(result, type)
                    else {"completed": ok}
                )
                if result is None:
                    error = "runtime returned no result"
            if not ok:
                return {
                    "success": False,
                    "transition_id": transition_id,
                    "error": error or f"Transition {transition_id} failed",
                    "result": payload,
                }
            return {"success": True, "transition_id": transition_id, "result": payload}
        except Exception as e:
            self.event_manager.emit_log(
                "error", f"Failed to execute transition {transition_id}: {e}"
            )
            return {"success": False, "error": str(e)}

    def _handle_sm_navigate_to_states(self, params: dict[str, Any]) -> dict[str, Any]:
        """Navigate to target states using pathfinding."""
        if self._ui_bridge_runtime is None:
            return {"success": False, "error": "No state machine loaded"}

        target_states = params.get("target_states")
        if not target_states:
            return {"success": False, "error": "target_states is required"}

        try:
            result = self._ui_bridge_runtime.navigate_to(target_states)
            return {
                "success": True,
                "target_states": target_states,
                "result": result if isinstance(result, dict) else {"completed": True},
            }
        except Exception as e:
            self.event_manager.emit_log(
                "error", f"Failed to navigate to states {target_states}: {e}"
            )
            return {"success": False, "error": str(e)}

    def _handle_sm_get_active_states(self) -> dict[str, Any]:
        """Get currently active states from the runtime."""
        if self._ui_bridge_runtime is None:
            return {"success": False, "error": "No state machine loaded"}

        try:
            active_states = self._ui_bridge_runtime.get_active_states()
            return {
                "success": True,
                "active_states": active_states,
            }
        except Exception as e:
            return {"success": False, "error": str(e)}

    def _handle_sm_get_available_transitions(self) -> dict[str, Any]:
        """Get transitions available from the current state."""
        if self._ui_bridge_runtime is None:
            return {"success": False, "error": "No state machine loaded"}

        try:
            transitions = self._ui_bridge_runtime.get_available_transitions()
            # Convert transition objects to dicts for JSON serialization
            transition_list: list[Any] = []
            for t in transitions:
                if hasattr(t, "__dict__"):
                    transition_list.append(
                        {
                            "id": getattr(t, "id", None),
                            "name": getattr(t, "name", None),
                            "from_states": getattr(t, "from_states", []),
                            "activate_states": getattr(t, "activate_states", []),
                            "exit_states": getattr(t, "exit_states", []),
                        }
                    )
                elif isinstance(t, dict):
                    transition_list.append(t)
                else:
                    transition_list.append(str(t))

            return {
                "success": True,
                "transitions": transition_list,
            }
        except Exception as e:
            return {"success": False, "error": str(e)}

    def _handle_sm_get_permitted_triggers(self, params: dict[str, Any]) -> dict[str, Any]:
        """Return transitions currently permitted from a (hypothetical) active set.

        Accepts optional ``active_state_ids`` list to query a hypothetical
        state configuration without disturbing the runtime's active set.
        """
        if self._ui_bridge_runtime is None:
            return {"success": False, "error": "No state machine loaded"}

        active_state_ids = params.get("active_state_ids") if params else None
        try:
            triggers = self._ui_bridge_runtime.get_permitted_triggers(active_state_ids)
            return {
                "success": True,
                "permitted_triggers": triggers,
            }
        except Exception as e:
            return {"success": False, "error": str(e)}

    def _handle_sm_get_blocked_triggers(self, params: dict[str, Any]) -> dict[str, Any]:
        """Return transitions currently blocked, each annotated with a reason."""
        if self._ui_bridge_runtime is None:
            return {"success": False, "error": "No state machine loaded"}

        active_state_ids = params.get("active_state_ids") if params else None
        try:
            triggers = self._ui_bridge_runtime.get_blocked_triggers(active_state_ids)
            return {
                "success": True,
                "blocked_triggers": triggers,
            }
        except Exception as e:
            return {"success": False, "error": str(e)}

    def _handle_sm_get_mermaid_diagram(self, params: dict[str, Any]) -> dict[str, Any]:
        """Return a Mermaid ``stateDiagram-v2`` source for the loaded machine.

        Accepts optional ``active_state_ids`` list to highlight a hypothetical
        active-state configuration. When absent or empty, the runtime's
        current active set is highlighted.
        """
        if self._ui_bridge_runtime is None:
            return {"success": False, "error": "No state machine loaded"}

        active_state_ids = params.get("active_state_ids") if params else None
        try:
            diagram = self._ui_bridge_runtime.get_mermaid_diagram(active_state_ids)
            return {
                "success": True,
                "diagram": diagram,
            }
        except Exception as e:
            return {"success": False, "error": str(e)}

    def _handle_clear_state_machine(self) -> dict[str, Any]:
        """Clear the loaded state machine and persisted data."""
        was_loaded = self._ui_bridge_runtime is not None
        self._ui_bridge_runtime = None
        self._element_resolver = None

        # Clear persisted data
        try:
            persistence = self._get_sm_persistence()
            persistence.clear()
        except Exception:
            pass

        self.event_manager.emit_log(
            "info",
            "State machine cleared" if was_loaded else "No state machine was loaded",
        )
        return {"success": True, "was_loaded": was_loaded}
