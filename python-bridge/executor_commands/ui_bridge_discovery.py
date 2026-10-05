"""UI Bridge exploration and state-discovery commands.

A mixin of ``QontinuiExecutor``. Its methods were moved verbatim from
``qontinui_executor.py`` by plan
2026-10-04-runner-python-executor-routes-118-commands-through-one-if-chain
and still read executor state through ``self``. ``COMMANDS`` maps each command
name to its handler method.
"""

from typing import Any, ClassVar

from services.ui_bridge_explorer_service import UIBridgeExplorerService

from ._host import ExecutorHost
from ._shared import QONTINUI_AVAILABLE


class UiBridgeDiscoveryCommands(ExecutorHost):
    """UI Bridge exploration and state-discovery commands."""

    COMMANDS: ClassVar[dict[str, str]] = {
        # UI Bridge exploration commands
        "start_ui_bridge_exploration": "_handle_start_ui_bridge_exploration",
        "get_ui_bridge_exploration_status": "_handle_get_ui_bridge_exploration_status",
        "get_ui_bridge_exploration_results": "_handle_get_ui_bridge_exploration_results",
        "stop_ui_bridge_exploration": "_handle_stop_ui_bridge_exploration",
        # UI Bridge state discovery from render logs
        "discover_states_from_renders": "_handle_discover_states_from_renders",
        # UI Bridge state discovery from fingerprint co-occurrence data
        "discover_states_from_fingerprints": "_handle_discover_states_from_fingerprints",
        # UI Bridge automatic exploration
        "run_ui_bridge_exploration": "_handle_run_ui_bridge_exploration",
    }

    # =========================================================================
    # UI Bridge Exploration Handlers
    # =========================================================================

    def _get_ui_bridge_explorer_service(self) -> UIBridgeExplorerService:
        """Get or create the UI Bridge explorer service."""
        if self._ui_bridge_explorer_service is None:
            self._ui_bridge_explorer_service = UIBridgeExplorerService(
                event_manager=self.event_manager,
            )
        return self._ui_bridge_explorer_service

    def _handle_start_ui_bridge_exploration(self, params: dict[str, Any]) -> dict[str, Any]:
        """Handle start UI Bridge exploration command.

        Starts an exploration job in the background and returns a job ID.
        Use get_ui_bridge_exploration_status and get_ui_bridge_exploration_results
        to poll for progress and retrieve results.
        """
        import sys

        print(
            f"[info    ] EXECUTOR: _handle_start_ui_bridge_exploration called with params: {params}",
            file=sys.stderr,
            flush=True,
        )

        try:
            service = self._get_ui_bridge_explorer_service()
            result = service.start_exploration(params)

            if result.get("success"):
                self.event_manager.emit_log(
                    "info",
                    f"[UI Bridge] Started exploration job: {result.get('job_id')}",
                )

            return result

        except Exception as e:
            import traceback

            self.event_manager.emit_log("error", f"[UI Bridge] Start exploration failed: {e}")
            traceback.print_exc(file=sys.stderr)
            return {"success": False, "error": str(e)}

    def _handle_get_ui_bridge_exploration_status(self, params: dict[str, Any]) -> dict[str, Any]:
        """Handle get UI Bridge exploration status command.

        Returns the current status of an exploration job.
        """
        import sys

        print(
            f"[info    ] EXECUTOR: _handle_get_ui_bridge_exploration_status called with params: {params}",
            file=sys.stderr,
            flush=True,
        )

        try:
            service = self._get_ui_bridge_explorer_service()
            job_id = params.get("job_id")
            return service.get_job_status(job_id)

        except Exception as e:
            import traceback

            self.event_manager.emit_log("error", f"[UI Bridge] Get exploration status failed: {e}")
            traceback.print_exc(file=sys.stderr)
            return {"success": False, "error": str(e)}

    def _handle_get_ui_bridge_exploration_results(self, params: dict[str, Any]) -> dict[str, Any]:
        """Handle get UI Bridge exploration results command.

        Returns the results of a completed exploration job.
        """
        import sys

        print(
            f"[info    ] EXECUTOR: _handle_get_ui_bridge_exploration_results called with params: {params}",
            file=sys.stderr,
            flush=True,
        )

        try:
            service = self._get_ui_bridge_explorer_service()
            job_id = params.get("job_id")
            return service.get_results(job_id)

        except Exception as e:
            import traceback

            self.event_manager.emit_log("error", f"[UI Bridge] Get exploration results failed: {e}")
            traceback.print_exc(file=sys.stderr)
            return {"success": False, "error": str(e)}

    def _handle_stop_ui_bridge_exploration(self, params: dict[str, Any]) -> dict[str, Any]:
        """Handle stop UI Bridge exploration command.

        Stops the current exploration job.
        """
        import sys

        print(
            "[info    ] EXECUTOR: _handle_stop_ui_bridge_exploration called",
            file=sys.stderr,
            flush=True,
        )

        try:
            service = self._get_ui_bridge_explorer_service()
            return service.stop_exploration()

        except Exception as e:
            import traceback

            self.event_manager.emit_log("error", f"[UI Bridge] Stop exploration failed: {e}")
            traceback.print_exc(file=sys.stderr)
            return {"success": False, "error": str(e)}

    def _handle_discover_states_from_renders(self, params: dict[str, Any]) -> dict[str, Any]:
        """Handle discover states from render logs command.

        Runs co-occurrence analysis on existing render logs to discover states.
        This is separate from exploration - it only analyzes provided render data.

        Args:
            params: Command parameters:
                - render_logs: Array of DOM snapshot render log entries

        Returns:
            Dictionary with:
                - success: Whether discovery succeeded
                - data: UIBridgeStateDiscoveryResult with states, elements, etc.
                - error: Error message (if failed)
        """
        import sys

        print(
            "[info    ] EXECUTOR: _handle_discover_states_from_renders called",
            file=sys.stderr,
            flush=True,
        )

        try:
            if not QONTINUI_AVAILABLE:
                return {
                    "success": False,
                    "error": "Qontinui library not available",
                }

            # Get render logs from params
            render_logs = params.get("render_logs", [])

            if not render_logs:
                return {
                    "success": False,
                    "error": "No render_logs provided",
                }

            self.event_manager.emit_log(
                "info",
                f"[UI Bridge] Starting state discovery on {len(render_logs)} render logs",
            )

            # Import and call the qontinui library function
            from qontinui.discovery.ui_bridge_adapter import (
                discover_states_from_renders,
            )

            result = discover_states_from_renders(render_logs)

            self.event_manager.emit_log(
                "info",
                f"[UI Bridge] State discovery complete: {len(result.states)} states, "
                f"{result.unique_element_count} unique elements from {result.render_count} renders",
            )

            return {
                "success": True,
                "data": result.to_dict(),
            }

        except Exception as e:
            import traceback

            self.event_manager.emit_log(
                "error", f"[UI Bridge] Discover states from renders failed: {e}"
            )
            traceback.print_exc(file=sys.stderr)
            return {"success": False, "error": str(e)}

    def _handle_discover_states_from_fingerprints(self, params: dict[str, Any]) -> dict[str, Any]:
        """Handle discover states from fingerprint co-occurrence data.

        Uses the FingerprintStateDiscovery class to analyze element fingerprints
        collected across multiple page captures and discover states based on
        which elements consistently appear together.

        Args:
            params: Command parameters:
                - cooccurrence_export: CooccurrenceExport data from UI Bridge capture session
                    - sessionId: Unique session identifier
                    - presenceMatrix: Array of capture snapshots with fingerprint data
                    - transitions: Array of action transitions between captures
                    - fingerprintStats: Statistics for each fingerprint hash
                    - stateCandidates: Pre-computed state candidates (optional)
                - config: Optional discovery configuration
                    - minCooccurrenceRate: Minimum co-occurrence rate (default: 0.95)

        Returns:
            Dictionary with:
                - success: Whether discovery succeeded
                - data: Discovered states and transitions
                    - states: Array of DiscoveredFingerprintState objects
                    - transitions: Array of state transitions
                    - statistics: Discovery statistics
                - error: Error message (if failed)
        """
        import sys

        print(
            "[info    ] EXECUTOR: _handle_discover_states_from_fingerprints called",
            file=sys.stderr,
            flush=True,
        )

        try:
            if not QONTINUI_AVAILABLE:
                return {
                    "success": False,
                    "error": "Qontinui library not available",
                }

            # Get co-occurrence export from params (as raw dict)
            cooccurrence_export = params.get("cooccurrence_export")

            if not cooccurrence_export:
                return {
                    "success": False,
                    "error": "No cooccurrence_export provided",
                }

            # Get optional config
            user_config = params.get("config", {})
            min_cooccurrence_rate = user_config.get("minCooccurrenceRate", 0.95)

            self.event_manager.emit_log(
                "info",
                f"[UI Bridge] Starting fingerprint state discovery "
                f"(min_cooccurrence_rate={min_cooccurrence_rate})",
            )

            # Import the FingerprintStateDiscovery class
            from qontinui.state_machine.fingerprint_state_discovery import (
                FingerprintStateDiscovery,
                FingerprintStateDiscoveryConfig,
            )

            # Log input data summary
            presence_matrix = cooccurrence_export.get("presenceMatrix", [])
            transitions_data = cooccurrence_export.get("transitions", [])
            fingerprint_stats = cooccurrence_export.get("fingerprintStats", {})

            self.event_manager.emit_log(
                "info",
                f"[UI Bridge] Input data: "
                f"{len(presence_matrix)} captures, "
                f"{len(transitions_data)} transitions, "
                f"{len(fingerprint_stats)} unique fingerprints",
            )

            # Create discovery config
            config = FingerprintStateDiscoveryConfig(
                min_cooccurrence_rate=min_cooccurrence_rate,
            )

            # Create discovery instance
            discovery = FingerprintStateDiscovery(config=config)

            # Load data from raw dict (the method handles parsing internally)
            discovery.load_cooccurrence_export(cooccurrence_export)

            # Run discovery
            discovered_states = discovery.discover_states()

            self.event_manager.emit_log(
                "info",
                f"[UI Bridge] Fingerprint state discovery complete: "
                f"{len(discovered_states)} states discovered",
            )

            # Get raw transitions
            raw_transitions = discovery.get_transitions()

            # Map raw transitions to state transitions
            # We need to find which state each transition's fingerprints belong to
            state_transitions = self._map_transitions_to_states(raw_transitions, discovered_states)

            # Get statistics
            stats = discovery.get_statistics()

            # Build result
            result = {
                "states": [
                    {
                        "stateId": state.state_id,
                        "name": state.name,
                        "fingerprintHashes": list(state.fingerprint_hashes),
                        "elementIds": list(state.element_ids),
                        "positionZone": state.position_zone,
                        "landmarkContext": state.landmark_context,
                        "isGlobal": state.is_global,
                        "isModal": state.is_modal,
                        "repeatPatternCount": state.repeat_pattern_count,
                        "confidence": state.confidence,
                        "observationCount": state.observation_count,
                    }
                    for state in discovered_states
                ],
                "transitions": state_transitions,
                "statistics": {
                    "totalCaptures": stats.get("total_captures", 0),
                    "totalTransitions": stats.get("total_transitions", 0),
                    "uniqueFingerprints": stats.get("total_fingerprints", 0),
                    "discoveredStates": stats.get("discovered_states", 0),
                    "globalStates": stats.get("global_states", 0),
                    "modalStates": stats.get("modal_states", 0),
                    "discoveredTransitions": len(state_transitions),
                },
            }

            return {
                "success": True,
                "data": result,
            }

        except Exception as e:
            import traceback

            self.event_manager.emit_log(
                "error", f"[UI Bridge] Discover states from fingerprints failed: {e}"
            )
            traceback.print_exc(file=sys.stderr)
            return {"success": False, "error": str(e)}

    def _map_transitions_to_states(
        self,
        raw_transitions: list[Any],
        discovered_states: list[Any],
    ) -> list[dict[str, Any]]:
        """Map raw transitions to state transitions.

        Args:
            raw_transitions: List of TransitionRecord objects
            discovered_states: List of DiscoveredFingerprintState objects

        Returns:
            List of state transition dictionaries
        """
        # Build a mapping from fingerprint hash to state
        fp_to_state: dict[str, str] = {}
        for state in discovered_states:
            for fp_hash in state.fingerprint_hashes:
                fp_to_state[fp_hash] = state.state_id

        # Track unique state transitions
        transition_counts: dict[tuple[str, str, str], int] = {}

        for t in raw_transitions:
            # Find which states the appeared/disappeared fingerprints belong to
            from_states: set[str] = set()
            to_states: set[str] = set()

            for fp_hash in t.disappeared_fingerprints:
                if fp_hash in fp_to_state:
                    from_states.add(fp_to_state[fp_hash])

            for fp_hash in t.appeared_fingerprints:
                if fp_hash in fp_to_state:
                    to_states.add(fp_to_state[fp_hash])

            # Create transitions for each from/to state pair
            for from_state in from_states:
                for to_state in to_states:
                    if from_state != to_state:
                        key = (from_state, to_state, t.action_type)
                        transition_counts[key] = transition_counts.get(key, 0) + 1

        # Convert to result format
        result = []
        for (from_state, to_state, action_type), count in transition_counts.items():
            result.append(
                {
                    "fromStateId": from_state,
                    "toStateId": to_state,
                    "actionType": action_type,
                    "count": count,
                }
            )

        return result

    def _handle_run_ui_bridge_exploration(self, params: dict[str, Any]) -> dict[str, Any]:
        """Handle automatic UI Bridge exploration.

        Uses the UIBridgeExplorer to systematically explore interactive elements
        on a web page via the browser extension, building co-occurrence data
        for fingerprint-based state discovery.

        Args:
            params: Command parameters:
                - runner_url: URL of the qontinui-runner (default: http://127.0.0.1:{QONTINUI_PORT})
                - config: Optional exploration configuration
                    - max_depth: Maximum navigation depth (default: 2)
                    - max_elements_per_page: Max elements per page (default: 20)
                    - max_total_elements: Max total elements (default: 100)
                    - action_delay_ms: Delay between actions (default: 500)
                    - blocked_keywords: Keywords to skip
                    - safe_keywords: Safe keywords
                    - capture_screenshots: Whether to capture screenshots

        Returns:
            Dictionary with:
                - success: Whether exploration succeeded
                - data: Exploration results
                    - exploration_id: Unique ID
                    - elements_discovered: Count of discovered elements
                    - elements_explored: Count of explored elements
                    - errors: List of errors
                    - state_discovery_result: Discovered states
                    - cooccurrence_export: Raw export data
                - error: Error message (if failed)
        """
        import asyncio
        import sys

        print(
            "[info    ] EXECUTOR: _handle_run_ui_bridge_exploration called",
            file=sys.stderr,
            flush=True,
        )

        try:
            if not QONTINUI_AVAILABLE:
                return {
                    "success": False,
                    "error": "Qontinui library not available",
                }

            # Get parameters
            runner_url = params.get("runner_url", self._get_runner_api_base())
            user_config = params.get("config", {})

            self.event_manager.emit_log(
                "info",
                f"[UI Bridge] Starting automatic exploration (runner_url={runner_url})",
            )

            # Import explorer
            from qontinui.discovery.target_connection import ExplorationConfig
            from qontinui.discovery.ui_bridge_explorer import (
                UIBridgeExplorer,
            )

            # Build exploration config
            config = ExplorationConfig(
                target_type="extension",  # Use extension connection
                connection_url=runner_url,
                max_depth=user_config.get("max_depth", 2),
                max_elements_per_page=user_config.get("max_elements_per_page", 20),
                max_total_elements=user_config.get("max_total_elements", 100),
                action_delay_ms=user_config.get("action_delay_ms", 500),
                blocked_keywords=user_config.get("blocked_keywords", []),
                safe_keywords=user_config.get("safe_keywords", []),
                capture_screenshots=user_config.get("capture_screenshots", False),
                record_render_logs=True,
            )

            # Progress callback to emit events
            def on_progress(
                message: str,
                elements_discovered: int,
                elements_explored: int,
                current_element: str | None,
            ) -> bool:
                self.event_manager.emit_log(
                    "info",
                    f"[Exploration] {message} "
                    f"(discovered={elements_discovered}, explored={elements_explored})",
                )
                return True  # Continue exploration

            # Run exploration in event loop
            async def run_exploration():
                async with UIBridgeExplorer(config, on_progress=on_progress) as explorer:
                    return await explorer.explore()

            # Get or create event loop
            try:
                loop = asyncio.get_event_loop()
            except RuntimeError:
                loop = asyncio.new_event_loop()
                asyncio.set_event_loop(loop)

            result = loop.run_until_complete(run_exploration())

            self.event_manager.emit_log(
                "info",
                f"[UI Bridge] Exploration complete: "
                f"{result.elements_discovered} discovered, "
                f"{result.elements_explored} explored, "
                f"{len(result.errors)} errors",
            )

            # Build response
            response_data: dict[str, Any] = {
                "exploration_id": result.exploration_id,
                "elements_discovered": result.elements_discovered,
                "elements_explored": result.elements_explored,
                "errors": result.errors,
            }

            # Include state discovery result if available
            if result.state_discovery_result:
                response_data["state_discovery_result"] = result.state_discovery_result.to_dict()

            # Include cooccurrence export if available
            if result.cooccurrence_export:
                response_data["cooccurrence_export"] = result.cooccurrence_export

            return {
                "success": True,
                "data": response_data,
            }

        except Exception as e:
            import traceback

            self.event_manager.emit_log("error", f"[UI Bridge] Exploration failed: {e}")
            traceback.print_exc(file=sys.stderr)
            return {"success": False, "error": str(e)}
