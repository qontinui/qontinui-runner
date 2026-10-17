"""GUI config pipeline command: build a QontinuiConfig from element images.

A mixin of ``QontinuiExecutor``. Its methods were moved verbatim from
``qontinui_executor.py`` by plan
2026-10-04-runner-python-executor-routes-118-commands-through-one-if-chain
and still read executor state through ``self``. ``COMMANDS`` maps each command
name to its handler method.

The element-capture commands (``gui_config_capture_elements``,
``gui_config_capture_multi_state``) and their UI Bridge HTTP helpers were
deleted: they POSTed ``/ui-bridge/control/capture-element-images``, whose
html2canvas frontend handler qontinui-runner#2242 removed. The runner now
answers ``/gui-config/capture-elements`` and ``/gui-config/capture-multi-state``
with 410 Gone itself and never dispatches them here.
"""

from typing import Any, ClassVar

from ._host import ExecutorHost
from ._shared import QONTINUI_AVAILABLE


class GuiConfigCommands(ExecutorHost):
    """GUI config pipeline command: build a QontinuiConfig from element images."""

    COMMANDS: ClassVar[dict[str, str]] = {
        # GUI Config Pipeline commands
        "gui_config_build": "_handle_gui_config_build",
    }

    # =========================================================================
    # GUI Config Pipeline commands
    # =========================================================================

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
