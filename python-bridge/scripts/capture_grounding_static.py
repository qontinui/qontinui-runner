#!/usr/bin/env python3
"""Static grounding-data capture via UI Bridge + mss.

Connects to a running qontinui-web dev server's UI Bridge API, navigates
to component gallery routes, captures screenshots at multiple viewport
widths, extracts element bounding boxes from the UI Bridge control
snapshot, and writes GroundingRecord JSONL for grounding-model fine-tuning.

Usage::

    # Start qontinui-web dev server first:
    cd qontinui-web/frontend && npm run dev

    # Open http://localhost:3001/dev/grounding in a browser so UI Bridge
    # SDK connects, then run:
    python scripts/capture_grounding_static.py

    # Custom output directory:
    QONTINUI_EXPORT_DIR=~/datasets/grounding python scripts/capture_grounding_static.py

Requires: requests, mss, Pillow, numpy
"""

from __future__ import annotations

import argparse
import io
import logging
import os
import sys
import time
from datetime import datetime, timezone
from pathlib import Path

import numpy as np
import requests
from PIL import Image

# ---------------------------------------------------------------------------
# Path setup — add qontinui-train to sys.path for grounding_record imports
# ---------------------------------------------------------------------------
_SCRIPT_DIR = Path(__file__).resolve().parent
_BRIDGE_DIR = _SCRIPT_DIR.parent  # python-bridge/
_ROOT = _BRIDGE_DIR.parent.parent  # qontinui-root/
_TRAIN_ROOT = _ROOT / "qontinui-train"

if str(_TRAIN_ROOT) not in sys.path:
    sys.path.insert(0, str(_TRAIN_ROOT))

from qontinui_train.export.grounding_record import (  # noqa: E402
    GroundingElement,
    GroundingJSONLWriter,
    GroundingRecord,
)

logging.basicConfig(
    level=logging.INFO,
    format="%(asctime)s [%(levelname)s] %(message)s",
)
logger = logging.getLogger(__name__)

# ---------------------------------------------------------------------------
# Configuration
# ---------------------------------------------------------------------------

DEFAULT_UI_BRIDGE_URL = "http://localhost:3001/api/ui-bridge"
DEFAULT_OUTPUT_DIR = "dataset"

ROUTES = ["/dev/grounding", "/visual"]

VIEWPORTS = [1920, 1440, 768, 375]

# HTTP timeout for UI Bridge calls (seconds)
REQUEST_TIMEOUT = 20

# Settle time after navigation or viewport change (seconds)
SETTLE_DELAY = 1.0


# ---------------------------------------------------------------------------
# UI Bridge HTTP helpers
# ---------------------------------------------------------------------------

class UIBridgeClient:
    """Minimal HTTP client for UI Bridge control endpoints."""

    def __init__(self, base_url: str) -> None:
        self.base_url = base_url.rstrip("/")
        self._session = requests.Session()

    def _get(self, path: str) -> dict:
        url = f"{self.base_url}{path}"
        resp = self._session.get(url, timeout=REQUEST_TIMEOUT)
        resp.raise_for_status()
        return resp.json()

    def _post(self, path: str, data: dict | None = None) -> dict:
        url = f"{self.base_url}{path}"
        resp = self._session.post(url, json=data or {}, timeout=REQUEST_TIMEOUT)
        resp.raise_for_status()
        return resp.json()

    def health_check(self) -> bool:
        """Check if UI Bridge is reachable."""
        try:
            resp = self._get("/health")
            return resp.get("success", False) or resp.get("status") == "ok"
        except Exception:
            return False

    def navigate(self, url: str) -> dict:
        """Navigate the connected browser to a URL path."""
        return self._post("/control/page/navigate", {"url": url})

    def set_viewport_constraints(self, width: int) -> dict:
        """Apply CSS viewport width constraints."""
        return self._post("/control/viewport-constraints", {"width": width})

    def restore_viewport(self) -> dict:
        """Remove CSS viewport constraints."""
        return self._post("/control/viewport-constraints", {"restore": True})

    def get_control_snapshot(self) -> dict:
        """Get full control snapshot with elements and viewport."""
        body = self._get("/control/snapshot")
        return body.get("data", body)


# ---------------------------------------------------------------------------
# Screenshot capture (mss — same pattern as trajectory_logger.py)
# ---------------------------------------------------------------------------

def capture_screen() -> tuple[bytes, int, int]:
    """Capture the primary monitor as PNG bytes. Returns (png_bytes, width, height)."""
    import mss

    with mss.mss() as sct:
        mon = sct.monitors[1]
        shot = sct.grab(mon)
        arr = np.array(shot)[:, :, :3]  # BGRA → BGR

    h, w = arr.shape[:2]
    img = Image.fromarray(arr[..., ::-1])  # BGR → RGB for PIL
    buf = io.BytesIO()
    img.save(buf, format="PNG")
    return buf.getvalue(), w, h


# ---------------------------------------------------------------------------
# Element extraction (UI Bridge snapshot → GroundingElement)
# ---------------------------------------------------------------------------

def snapshot_to_elements(snapshot: dict) -> list[GroundingElement]:
    """Convert UI Bridge control snapshot elements to GroundingElements."""
    elements = []
    for el in snapshot.get("elements", []):
        state = el.get("state", {})
        rect = state.get("rect", {})

        x = int(rect.get("x", 0))
        y = int(rect.get("y", 0))
        w = int(rect.get("width", 0))
        h = int(rect.get("height", 0))

        if w <= 0 or h <= 0:
            continue

        # Determine interactability
        category = el.get("category")
        is_fallback = el.get("_domFallback", False)
        interactable = category == "interactive" or is_fallback or bool(el.get("actions"))

        elements.append(
            GroundingElement(
                role=el.get("type", "unknown"),
                text=el.get("label"),
                bbox=(x, y, w, h),
                interactable=interactable,
            )
        )

    return elements


# ---------------------------------------------------------------------------
# Main capture loop
# ---------------------------------------------------------------------------

def run_capture(
    ui_bridge_url: str,
    output_dir: Path,
    routes: list[str],
    viewports: list[int],
) -> None:
    client = UIBridgeClient(ui_bridge_url)
    writer = GroundingJSONLWriter(output_dir)

    logger.info("UI Bridge: %s", ui_bridge_url)
    logger.info("Output: %s", output_dir)

    # Health check
    if not client.health_check():
        logger.error(
            "UI Bridge not reachable at %s. "
            "Is qontinui-web running and is the page open in a browser?",
            ui_bridge_url,
        )
        sys.exit(1)

    logger.info("UI Bridge connected")

    total_written = 0

    for route in routes:
        logger.info("Navigating to %s", route)
        try:
            client.navigate(route)
        except Exception as e:
            logger.warning("Navigation to %s failed: %s — skipping", route, e)
            continue

        time.sleep(SETTLE_DELAY)

        for vp_width in viewports:
            logger.info("  Viewport: %dpx", vp_width)

            # Apply viewport constraint
            try:
                client.set_viewport_constraints(vp_width)
            except Exception as e:
                logger.warning("  Failed to set viewport %d: %s — skipping", vp_width, e)
                continue

            time.sleep(SETTLE_DELAY)

            try:
                # Get elements from UI Bridge
                snapshot = client.get_control_snapshot()
                elements = snapshot_to_elements(snapshot)

                # Capture screenshot
                png_bytes, screen_w, screen_h = capture_screen()

                # Build record
                record = GroundingRecord(
                    image_hash="",  # computed by writer
                    image_path="",  # computed by writer
                    viewport_width=vp_width,
                    viewport_height=screen_h,
                    elements=elements,
                    action=None,
                    source="static",
                    timestamp=datetime.now(timezone.utc).isoformat(),
                )

                written = writer.write(record, png_bytes)
                if written:
                    total_written += 1
                    logger.info(
                        "    Written: %d elements, hash=%s",
                        len(elements),
                        record.image_hash,
                    )
                else:
                    logger.info("    Deduped (identical screenshot)")

            except Exception as e:
                logger.warning("    Capture failed: %s", e)

            finally:
                # Restore viewport
                try:
                    client.restore_viewport()
                except Exception:
                    pass

    logger.info("Done. %d records written to %s", total_written, output_dir / "grounding.jsonl")


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------

def main() -> None:
    parser = argparse.ArgumentParser(
        description="Capture static grounding data from qontinui-web via UI Bridge",
    )
    parser.add_argument(
        "--ui-bridge-url",
        default=os.getenv("QONTINUI_UI_BRIDGE_URL", DEFAULT_UI_BRIDGE_URL),
        help=f"UI Bridge base URL (default: {DEFAULT_UI_BRIDGE_URL})",
    )
    parser.add_argument(
        "--output-dir",
        default=os.getenv("QONTINUI_EXPORT_DIR", DEFAULT_OUTPUT_DIR),
        help="Output directory for grounding.jsonl and images/",
    )
    parser.add_argument(
        "--routes",
        nargs="+",
        default=ROUTES,
        help="Routes to capture (default: /dev/grounding /visual)",
    )
    parser.add_argument(
        "--viewports",
        nargs="+",
        type=int,
        default=VIEWPORTS,
        help="Viewport widths in pixels (default: 1920 1440 768 375)",
    )
    args = parser.parse_args()

    run_capture(
        ui_bridge_url=args.ui_bridge_url,
        output_dir=Path(args.output_dir),
        routes=args.routes,
        viewports=args.viewports,
    )


if __name__ == "__main__":
    main()
