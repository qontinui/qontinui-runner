"""GUI config pipeline commands and their UI Bridge HTTP helpers.

A mixin of ``QontinuiExecutor``. Its methods were moved verbatim from
``qontinui_executor.py`` by plan
2026-10-04-runner-python-executor-routes-118-commands-through-one-if-chain
and still read executor state through ``self``. ``COMMANDS`` maps each command
name to its handler method.
"""

from typing import Any, ClassVar

from ._host import ExecutorHost
from ._shared import QONTINUI_AVAILABLE


class GuiConfigCommands(ExecutorHost):
    """GUI config pipeline commands and their UI Bridge HTTP helpers."""

    COMMANDS: ClassVar[dict[str, str]] = {
        # GUI Config Pipeline commands
        "gui_config_capture_elements": "_handle_gui_config_capture_elements",
        "gui_config_build": "_handle_gui_config_build",
        "gui_config_capture_multi_state": "_handle_gui_config_capture_multi_state",
    }

    # =========================================================================
    # GUI Config Pipeline commands
    # =========================================================================

    def _fetch_ui_bridge_snapshot(self, api_port: int) -> dict[str, Any] | None:
        """Fetch a UI Bridge snapshot via HTTP.

        Returns the snapshot dict (with 'elements' key) or None on failure.
        On failure, logs the error and returns None.
        """
        import http.client
        import json as _json
        import sys

        try:
            conn = http.client.HTTPConnection("127.0.0.1", api_port, timeout=30)
            conn.request("GET", "/ui-bridge/control/snapshot")
            resp = conn.getresponse()
            body = resp.read().decode("utf-8")
            conn.close()
            if resp.status != 200:
                print(
                    f"[warn    ] EXECUTOR: UI Bridge snapshot returned status {resp.status}",
                    file=sys.stderr,
                    flush=True,
                )
                return None
            snapshot_response = _json.loads(body)
        except Exception as e:
            print(
                f"[warn    ] EXECUTOR: Failed to fetch UI Bridge snapshot: {e}",
                file=sys.stderr,
                flush=True,
            )
            return None

        # The HTTP endpoint wraps in {success, data: {elements, ...}}
        snapshot = snapshot_response.get("data", snapshot_response)
        element_count = len(snapshot.get("elements", []))
        print(
            f"[info    ] EXECUTOR: Got {element_count} elements from UI Bridge",
            file=sys.stderr,
            flush=True,
        )
        return snapshot

    def _handle_gui_config_capture_elements(self, params: dict[str, Any]) -> dict[str, Any]:
        """Extract element images using UI Bridge DOM capture.

        Captures element images directly from the webview DOM via html2canvas,
        bypassing MSS screen capture entirely. This produces correct images
        regardless of window z-order (other windows can cover the runner).

        Args:
            params:
                - api_port: Runner API port for UI Bridge
                - category_filter: Optional list of categories to include
                - min_element_size: Minimum element dimension in pixels (default 4)
                - scale_factor: DPI scale factor (default 1.0, used for filtering only)
                - padding: Extra pixels around each crop (default 0, unused with DOM capture)

        Returns:
            Dictionary with element_images mapping and metadata.
        """
        import sys

        try:
            if not QONTINUI_AVAILABLE:
                return {"success": False, "error": "Qontinui library not available"}

            from qontinui.discovery.element_image_pipeline import (
                ElementImagePipeline,
                ExtractionConfig,
            )

            api_port = params.get("api_port", 9876)
            snapshot = self._fetch_ui_bridge_snapshot(api_port)
            if snapshot is None:
                return {"success": False, "error": "Failed to fetch UI Bridge snapshot"}
            if not snapshot.get("elements"):
                return {"success": False, "error": "UI Bridge snapshot has no elements"}

            # Capture element images directly from the DOM
            captures = self._fetch_ui_bridge_element_captures(api_port)
            if captures is None:
                return {
                    "success": False,
                    "error": "Failed to capture element images via UI Bridge",
                }

            # Build config (filters still apply)
            cat_filter = params.get("category_filter")
            config = ExtractionConfig(
                min_element_size=params.get("min_element_size", 4),
                padding=params.get("padding", 0),
                scale_factor=params.get("scale_factor", 1.0),
                category_filter=set(cat_filter) if cat_filter else None,
            )

            pipeline = ElementImagePipeline(config)
            result = pipeline.extract_from_captures(snapshot, captures)

            # Build response — element images as a dict
            element_images = {}
            for img in result.images:
                element_images[img.element_id] = {
                    "base64_png": img.base64_png,
                    "width": img.width,
                    "height": img.height,
                    "sha256": img.sha256,
                    "label": img.label,
                    "type": img.element_type,
                    "bbox": list(img.bbox),
                }

            return {
                "success": True,
                "element_count": len(result.images),
                "skipped_count": len(result.skipped),
                "screenshot_size": [result.screenshot_width, result.screenshot_height],
                "viewport_size": [result.viewport_width, result.viewport_height],
                "element_images": element_images,
                "skipped": result.skipped,
            }

        except Exception as e:
            print(
                f"[error   ] EXECUTOR: gui_config_capture_elements failed: {e}",
                file=sys.stderr,
                flush=True,
            )
            import traceback

            traceback.print_exc(file=sys.stderr)
            return {"success": False, "error": str(e)}

    def _handle_gui_config_build(self, params: dict[str, Any]) -> dict[str, Any]:
        """Build a QontinuiConfig from element images and state/transition definitions.

        Args:
            params:
                - name: Config name
                - states: List of state dicts with id, name, element_ids
                - transitions: List of transition dicts
                - element_images: Dict of element_id -> image data
                - description: Optional description
                - similarity: Default similarity threshold (default 0.85)

        Returns:
            Dictionary with the complete QontinuiConfig.
        """
        import sys

        try:
            if not QONTINUI_AVAILABLE:
                return {"success": False, "error": "Qontinui library not available"}

            import base64
            import io

            from PIL import Image
            from qontinui.discovery.element_image_pipeline import (
                ElementRect,
                ExtractedElementImage,
            )
            from qontinui.state_machine.config_bridge import (
                ConfigBridge,
                UIBridgeStateInput,
                UIBridgeTransitionInput,
            )

            name = params.get("name", "Untitled Config")
            states_raw = params.get("states", [])
            transitions_raw = params.get("transitions", [])
            element_images_raw = params.get("element_images", {})
            description = params.get("description", "")
            similarity = float(params.get("similarity", 0.85))

            if not states_raw:
                return {"success": False, "error": "states is required"}
            if not element_images_raw:
                return {"success": False, "error": "element_images is required"}

            # Convert raw states
            ui_states = [
                UIBridgeStateInput(
                    id=s["id"],
                    name=s["name"],
                    element_ids=s.get("element_ids", []),
                    description=s.get("description", ""),
                    is_initial=s.get("is_initial", False),
                    is_final=s.get("is_final", False),
                )
                for s in states_raw
            ]

            # Convert raw transitions
            ui_transitions = [
                UIBridgeTransitionInput(
                    id=t["id"],
                    name=t["name"],
                    from_states=t.get("from_states", []),
                    activate_states=t.get("activate_states", []),
                    exit_states=t.get("exit_states", []),
                    stays_visible=t.get("stays_visible", False),
                )
                for t in transitions_raw
            ]

            # Reconstruct ExtractedElementImage objects
            all_element_images: dict[str, ExtractedElementImage] = {}
            for eid, data in element_images_raw.items():
                b64 = data.get("base64_png", "")
                if not b64:
                    continue
                img = Image.open(io.BytesIO(base64.b64decode(b64)))
                bbox = data.get("bbox", [0, 0, img.width, img.height])
                all_element_images[eid] = ExtractedElementImage(
                    element_id=eid,
                    label=data.get("label", eid),
                    element_type=data.get("type", "unknown"),
                    image=img,
                    bbox=(int(bbox[0]), int(bbox[1]), int(bbox[2]), int(bbox[3])),
                    base64_png=b64,
                    sha256=data.get("sha256", ""),
                    viewport_rect=ElementRect(
                        x=0, y=0, width=float(img.width), height=float(img.height)
                    ),
                )

            # Group images by state
            state_images: dict[str, list[ExtractedElementImage]] = {}
            for s in ui_states:
                state_images[s.id] = [
                    all_element_images[eid] for eid in s.element_ids if eid in all_element_images
                ]

            # Build config
            bridge = ConfigBridge(default_similarity=similarity)
            config = bridge.build_config(
                name=name,
                states=ui_states,
                transitions=ui_transitions,
                state_images=state_images,
                description=description,
            )

            return {
                "success": True,
                "config": config,
                "stats": {
                    "image_count": len(config.get("images", [])),
                    "state_count": len(config.get("states", [])),
                    "transition_count": len(config.get("transitions", [])),
                },
            }

        except Exception as e:
            print(
                f"[error   ] EXECUTOR: gui_config_build failed: {e}",
                file=sys.stderr,
                flush=True,
            )
            import traceback

            traceback.print_exc(file=sys.stderr)
            return {"success": False, "error": str(e)}

    def _handle_gui_config_capture_multi_state(self, params: dict[str, Any]) -> dict[str, Any]:
        """Orchestrate multi-state GUI config capture using UI Bridge DOM capture.

        Walks through a sequence of interactions, capturing element images
        directly from the DOM via html2canvas at each step. Diffs element sets
        to find only NEW elements per state, and builds a complete QontinuiConfig.

        No MSS screen capture is used — element images come from the live DOM,
        so other windows covering the runner do not affect the result.

        Args:
            params:
                - api_port: Runner API port for UI Bridge
                - name: Config name
                - interactions: List of {action_type, target, state_name, wait_seconds}
                - min_element_size: Minimum element dimension (default 4)
                - description: Optional config description
                - similarity: Default similarity threshold (default 0.85)
                - scale_factor: DPI scale (default 1.0, used for filtering only)

        Returns:
            Dictionary with complete QontinuiConfig and capture stats.
        """
        import sys

        try:
            if not QONTINUI_AVAILABLE:
                return {"success": False, "error": "Qontinui library not available"}

            import time

            from qontinui.discovery.element_image_pipeline import (
                ElementImagePipeline,
                ExtractedElementImage,
                ExtractionConfig,
            )
            from qontinui.state_machine.config_bridge import (
                ConfigBridge,
                UIBridgeStateInput,
                UIBridgeTransitionInput,
            )

            api_port = params.get("api_port", 9876)
            scale = params.get("scale_factor", 1.0)
            interactions = params.get("interactions", [])
            min_size = params.get("min_element_size", 4)
            name = params.get("name", "Untitled Multi-State Config")
            description = params.get("description", "")
            similarity = float(params.get("similarity", 0.85))

            if not interactions:
                return {"success": False, "error": "interactions list is required"}

            config = ExtractionConfig(
                min_element_size=min_size,
                padding=0,
                scale_factor=scale,
            )
            pipeline = ElementImagePipeline(config)

            seen_ids: set[str] = set()
            ui_states: list[UIBridgeStateInput] = []
            ui_transitions: list[UIBridgeTransitionInput] = []
            state_images: dict[str, list[ExtractedElementImage]] = {}

            for i, interaction in enumerate(interactions):
                action_type = interaction.get("action_type", "initial")
                target = interaction.get("target")
                state_name = interaction.get("state_name", f"State {i}")
                wait_seconds = float(interaction.get("wait_seconds", 1.0))

                print(
                    f"[info    ] EXECUTOR: Multi-state step {i}: {action_type}"
                    f" target={target} state={state_name}",
                    file=sys.stderr,
                    flush=True,
                )

                # 1. Perform action (if not initial)
                if action_type != "initial" and target:
                    self._perform_ui_bridge_action(api_port, action_type, target)
                    time.sleep(wait_seconds)

                # 2. Fetch UI Bridge snapshot
                snapshot = self._fetch_ui_bridge_snapshot(api_port)
                if snapshot is None:
                    continue
                elements = snapshot.get("elements", [])
                if not elements:
                    print(
                        f"[warn    ] EXECUTOR: No elements at step {i}",
                        file=sys.stderr,
                        flush=True,
                    )
                    continue

                # 3. Diff: find only NEW element IDs
                current_ids: set[str] = set()
                for el in elements:
                    eid = el.get("id", "")
                    rect = el.get("state", {}).get("rect", {})
                    w = rect.get("width", 0)
                    h = rect.get("height", 0)
                    visible = el.get("state", {}).get("visible", True)
                    if visible and w >= min_size and h >= min_size and eid:
                        current_ids.add(eid)

                new_ids = current_ids - seen_ids
                seen_ids |= current_ids

                if not new_ids:
                    print(
                        f"[info    ] EXECUTOR: No new elements at step {i}, skipping",
                        file=sys.stderr,
                        flush=True,
                    )
                    continue

                print(
                    f"[info    ] EXECUTOR: Step {i}: {len(new_ids)} new elements"
                    f" (total seen: {len(seen_ids)})",
                    file=sys.stderr,
                    flush=True,
                )

                # 4. Capture only the new elements via UI Bridge DOM capture
                captures = self._fetch_ui_bridge_element_captures(
                    api_port, element_ids=list(new_ids)
                )
                if captures is None:
                    print(
                        f"[warn    ] EXECUTOR: DOM capture failed at step {i}, skipping",
                        file=sys.stderr,
                        flush=True,
                    )
                    continue

                # 5. Filter snapshot to only new elements, run pipeline
                filtered_elements = [el for el in elements if el.get("id", "") in new_ids]
                filtered_snapshot = {**snapshot, "elements": filtered_elements}

                result = pipeline.extract_from_captures(filtered_snapshot, captures)

                # 6. Record state
                state_id = f"state-{i}"
                ui_states.append(
                    UIBridgeStateInput(
                        id=state_id,
                        name=state_name,
                        element_ids=list(new_ids),
                        is_initial=(i == 0),
                    )
                )
                state_images[state_id] = result.images

                # 7. Record transition (if not initial and we have a previous state)
                if action_type != "initial" and len(ui_states) >= 2:
                    prev_state = ui_states[-2]
                    ui_transitions.append(
                        UIBridgeTransitionInput(
                            id=f"transition-{i}",
                            name=f"{action_type.title()} {target or ''}".strip(),
                            from_states=[prev_state.id],
                            activate_states=[state_id],
                            exit_states=[],
                            actions=[
                                {
                                    "type": action_type,
                                    "target": target,
                                }
                            ],
                        )
                    )

            if not ui_states:
                return {
                    "success": False,
                    "error": "No states captured — no new elements found at any step",
                }

            # 8. Build final config
            bridge = ConfigBridge(default_similarity=similarity)
            gui_config = bridge.build_config(
                name=name,
                states=ui_states,
                transitions=ui_transitions,
                state_images=state_images,
                description=description,
            )

            print(
                f"[info    ] EXECUTOR: Multi-state capture complete: "
                f"{len(ui_states)} states, {len(ui_transitions)} transitions",
                file=sys.stderr,
                flush=True,
            )

            return {
                "success": True,
                "config": gui_config,
                "stats": {
                    "state_count": len(ui_states),
                    "transition_count": len(ui_transitions),
                    "image_count": len(gui_config.get("images", [])),
                    "total_elements_seen": len(seen_ids),
                    "interactions_processed": len(interactions),
                },
            }

        except Exception as e:
            print(
                f"[error   ] EXECUTOR: gui_config_capture_multi_state failed: {e}",
                file=sys.stderr,
                flush=True,
            )
            import traceback

            traceback.print_exc(file=sys.stderr)
            return {"success": False, "error": str(e)}

    def _perform_ui_bridge_action(self, api_port: int, action_type: str, target: str) -> None:
        """Perform a UI action via the runner's UI Bridge HTTP API.

        Args:
            api_port: Runner API port
            action_type: Action to perform (click, scroll, etc.)
            target: Element ID to act on
        """
        import http.client
        import json as _json
        import sys

        try:
            conn = http.client.HTTPConnection("127.0.0.1", api_port, timeout=10)
            conn.request(
                "POST",
                f"/ui-bridge/control/element/{target}/action",
                _json.dumps({"action": action_type}).encode(),
                {"Content-Type": "application/json"},
            )
            resp = conn.getresponse()
            resp.read()
            conn.close()
            print(
                f"[info    ] EXECUTOR: UI action {action_type} on {target}: {resp.status}",
                file=sys.stderr,
                flush=True,
            )
        except Exception as e:
            print(
                f"[warn    ] EXECUTOR: UI action failed: {action_type} on {target}: {e}",
                file=sys.stderr,
                flush=True,
            )

    def _fetch_ui_bridge_element_captures(
        self,
        api_port: int,
        element_ids: list[str] | None = None,
    ) -> dict[str, dict[str, Any]] | None:
        """Capture element images via the UI Bridge (DOM-based, no screen capture).

        Uses html2canvas in the frontend to render each element directly from
        the DOM. This produces correct images regardless of window z-order.

        Args:
            api_port: Runner API port
            element_ids: Optional list of element IDs to capture.
                If None, captures all visible elements.

        Returns:
            Dict mapping element_id to {base64_png, width, height}, or None on failure.
        """
        import http.client
        import json as _json
        import sys

        try:
            body = {}
            if element_ids is not None:
                body["element_ids"] = element_ids

            conn = http.client.HTTPConnection("127.0.0.1", api_port, timeout=30)
            conn.request(
                "POST",
                "/ui-bridge/control/capture-element-images",
                _json.dumps(body).encode(),
                {"Content-Type": "application/json"},
            )
            resp = conn.getresponse()
            raw = resp.read().decode("utf-8")
            conn.close()

            if resp.status != 200:
                print(
                    f"[warn    ] EXECUTOR: UI Bridge capture returned status {resp.status}",
                    file=sys.stderr,
                    flush=True,
                )
                return None

            response = _json.loads(raw)
            data = response.get("data", response)
            captures = data.get("captures", {})
            count = len(captures)
            print(
                f"[info    ] EXECUTOR: Got {count} element captures from UI Bridge",
                file=sys.stderr,
                flush=True,
            )
            return captures

        except Exception as e:
            print(
                f"[warn    ] EXECUTOR: Failed to capture element images via UI Bridge: {e}",
                file=sys.stderr,
                flush=True,
            )
            return None
