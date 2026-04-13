#!/usr/bin/env python3
"""Isolated component grounding-data capture via UI Bridge + mss.

Connects to a running qontinui-web dev server's UI Bridge API, navigates to
isolated component pages, captures screenshots of individual component
configurations, and writes GroundingRecord JSONL for grounding-model
fine-tuning.

Each sample is a randomly-drawn combination of:
    component × variant × size × state × theme × background × (left, top)

Usage::

    # Start qontinui-web dev server first:
    cd qontinui-web/frontend && npm run dev

    # Open the browser so the UI Bridge SDK connects, then run:
    python scripts/capture_grounding_isolated.py

    # Custom options:
    python scripts/capture_grounding_isolated.py \\
        --num-samples 2000 --seed 7 --output-dir ~/datasets/grounding-iso

Requires: requests, mss, Pillow, numpy
"""

from __future__ import annotations

import argparse
import io
import logging
import os
import random
import sys
import time
from datetime import UTC, datetime
from pathlib import Path
from urllib.parse import urlencode

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
DEFAULT_OUTPUT_DIR = "dataset-isolated"
DEFAULT_NUM_SAMPLES = 5000
DEFAULT_SEED = 42

# HTTP timeout for UI Bridge calls (seconds)
REQUEST_TIMEOUT = 20

# Settle time after navigation (seconds)
SETTLE_DELAY = 0.5

# Isolated component page base route
ISOLATED_ROUTE = "/dev/grounding/isolated"

# ---------------------------------------------------------------------------
# Component combinatorial matrix
# ---------------------------------------------------------------------------

COMPONENTS: dict[str, dict[str, list[str]]] = {
    "Button": {
        "variants": ["default", "secondary", "destructive", "outline", "ghost", "link"],
        "sizes": ["sm", "default", "lg"],
        "states": ["enabled", "disabled"],
    },
    "Badge": {
        "variants": ["default", "secondary", "destructive", "outline", "success", "warning", "info"],
        "sizes": ["default"],
        "states": ["enabled"],
    },
    "Input": {
        "variants": ["default", "password", "invalid"],
        "sizes": ["default"],
        "states": ["enabled", "disabled"],
    },
    "Textarea": {
        "variants": ["default"],
        "sizes": ["default"],
        "states": ["enabled", "disabled"],
    },
    "Checkbox": {
        "variants": ["default"],
        "sizes": ["default"],
        "states": ["unchecked", "checked", "disabled"],
    },
    "Switch": {
        "variants": ["default"],
        "sizes": ["default"],
        "states": ["off", "on", "disabled"],
    },
    "Toggle": {
        "variants": ["default"],
        "sizes": ["default"],
        "states": ["unpressed", "pressed", "disabled"],
    },
    "Select": {
        "variants": ["default"],
        "sizes": ["default"],
        "states": ["enabled"],
    },
    "Slider": {
        "variants": ["default"],
        "sizes": ["default"],
        "states": ["enabled"],
    },
    "Progress": {
        "variants": ["default"],
        "sizes": ["default"],
        "states": ["enabled"],
    },
}

BACKGROUNDS: list[str] = [
    "solid-blue", "solid-red", "solid-green", "solid-purple", "solid-orange",
    "solid-yellow", "solid-pink", "solid-teal", "solid-gray", "solid-white",
    "solid-black", "solid-slate", "solid-zinc", "solid-stone", "solid-neutral",
    "solid-indigo", "solid-violet", "solid-fuchsia", "solid-rose", "solid-cyan",
    "solid-emerald", "solid-lime", "solid-amber", "solid-sky",
    "gradient-purple-blue", "gradient-red-orange", "gradient-green-teal",
    "gradient-pink-purple", "gradient-blue-cyan", "gradient-dark",
]

THEMES: list[str] = ["light", "dark"]


# ---------------------------------------------------------------------------
# UI Bridge HTTP client (mirrored from capture_grounding_static.py)
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
# Target element extraction
# ---------------------------------------------------------------------------

def find_target_element(snapshot: dict) -> GroundingElement | None:
    """Find the grounding-target element in the UI Bridge snapshot.

    Looks first for an element whose attributes include
    ``data-grounding-target="true"``, then falls back to any element that
    carries the ``data-grounding-target`` key regardless of value.

    Returns ``None`` if no target element is found.
    """
    for el in snapshot.get("elements", []):
        attrs: dict = el.get("attributes", {}) or {}
        val = attrs.get("data-grounding-target")
        if val is None:
            # Some snapshots store custom data-* keys at the top level
            val = el.get("data-grounding-target")
        if val is not None and str(val).lower() not in ("false", "0", ""):
            return _snapshot_el_to_grounding(el)

    return None


def _snapshot_el_to_grounding(el: dict) -> GroundingElement | None:
    """Convert a single snapshot element dict to a GroundingElement."""
    state = el.get("state", {})
    rect = state.get("rect", {})

    x = int(rect.get("x", 0))
    y = int(rect.get("y", 0))
    w = int(rect.get("width", 0))
    h = int(rect.get("height", 0))

    if w <= 0 or h <= 0:
        return None

    category = el.get("category")
    interactable = category == "interactive" or bool(el.get("actions"))

    return GroundingElement(
        role=el.get("type", "unknown"),
        text=el.get("label"),
        bbox=(x, y, w, h),
        interactable=interactable,
    )


# ---------------------------------------------------------------------------
# Sample generation
# ---------------------------------------------------------------------------

def build_sample_matrix() -> list[dict]:
    """Enumerate every (component, variant, size, state) combination."""
    combos: list[dict] = []
    for component, cfg in COMPONENTS.items():
        for variant in cfg["variants"]:
            for size in cfg["sizes"]:
                for state in cfg["states"]:
                    combos.append(
                        {
                            "component": component,
                            "variant": variant,
                            "size": size,
                            "state": state,
                        }
                    )
    return combos


def draw_samples(num_samples: int, seed: int) -> list[dict]:
    """Draw *num_samples* random samples from the full combinatorial space."""
    rng = random.Random(seed)
    matrix = build_sample_matrix()
    samples: list[dict] = []

    for _ in range(num_samples):
        base = rng.choice(matrix).copy()
        base["theme"] = rng.choice(THEMES)
        base["bg"] = rng.choice(BACKGROUNDS)
        base["left"] = rng.randint(5, 95)
        base["top"] = rng.randint(5, 95)
        samples.append(base)

    return samples


def build_isolated_url(params: dict) -> str:
    """Construct the isolated component page URL from sample params."""
    qs = urlencode(
        {
            "component": params["component"],
            "variant": params["variant"],
            "size": params["size"],
            "state": params["state"],
            "theme": params["theme"],
            "bg": params["bg"],
            "left": params["left"],
            "top": params["top"],
        }
    )
    return f"{ISOLATED_ROUTE}?{qs}"


# ---------------------------------------------------------------------------
# Main capture loop
# ---------------------------------------------------------------------------

def run_capture(
    ui_bridge_url: str,
    output_dir: Path,
    num_samples: int,
    seed: int,
) -> None:
    client = UIBridgeClient(ui_bridge_url)
    writer = GroundingJSONLWriter(output_dir)

    logger.info("UI Bridge: %s", ui_bridge_url)
    logger.info("Output:    %s", output_dir)
    logger.info("Samples:   %d  (seed=%d)", num_samples, seed)

    # Health check
    if not client.health_check():
        logger.error(
            "UI Bridge not reachable at %s. "
            "Is qontinui-web running and is the page open in a browser?",
            ui_bridge_url,
        )
        sys.exit(1)

    logger.info("UI Bridge connected")

    samples = draw_samples(num_samples, seed)

    total_written = 0
    total_skipped = 0
    total_errors = 0

    for idx, params in enumerate(samples):
        nav_url = build_isolated_url(params)

        try:
            client.navigate(nav_url)
            time.sleep(SETTLE_DELAY)

            snapshot = client.get_control_snapshot()
            target_el = find_target_element(snapshot)

            if target_el is None:
                logger.debug(
                    "Sample %d: no grounding target found in snapshot — skipping (%s)",
                    idx,
                    nav_url,
                )
                total_skipped += 1
                continue

            png_bytes, screen_w, screen_h = capture_screen()

            record = GroundingRecord(
                image_hash="",  # computed by writer
                image_path="",  # computed by writer
                viewport_width=screen_w,
                viewport_height=screen_h,
                elements=[target_el],
                action=None,
                source="static",
                timestamp=datetime.now(UTC).isoformat(),
                metadata={
                    "component": params["component"],
                    "variant": params["variant"],
                    "size": params["size"],
                    "state": params["state"],
                    "theme": params["theme"],
                    "bg": params["bg"],
                    "left": params["left"],
                    "top": params["top"],
                    "seed": seed,
                    "sample_index": idx,
                },
            )

            written = writer.write(record, png_bytes)
            if written:
                total_written += 1
            else:
                total_skipped += 1

        except Exception as exc:
            logger.warning("Sample %d error: %s — skipping", idx, exc)
            total_errors += 1
            continue

        if (idx + 1) % 100 == 0:
            logger.info(
                "Progress: %d/%d — written=%d skipped=%d errors=%d",
                idx + 1,
                num_samples,
                total_written,
                total_skipped,
                total_errors,
            )

    logger.info(
        "Done. written=%d skipped=%d errors=%d → %s",
        total_written,
        total_skipped,
        total_errors,
        output_dir / "grounding.jsonl",
    )


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------

def main() -> None:
    parser = argparse.ArgumentParser(
        description=(
            "Capture isolated component grounding data from qontinui-web via UI Bridge"
        ),
    )
    parser.add_argument(
        "--ui-bridge-url",
        default=os.getenv("QONTINUI_UI_BRIDGE_URL", DEFAULT_UI_BRIDGE_URL),
        help=f"UI Bridge base URL (default: {DEFAULT_UI_BRIDGE_URL})",
    )
    parser.add_argument(
        "--output-dir",
        default=os.getenv("QONTINUI_EXPORT_DIR", DEFAULT_OUTPUT_DIR),
        help="Output directory for grounding.jsonl and images/ (default: dataset-isolated)",
    )
    parser.add_argument(
        "--num-samples",
        type=int,
        default=DEFAULT_NUM_SAMPLES,
        help=f"Number of random samples to capture (default: {DEFAULT_NUM_SAMPLES})",
    )
    parser.add_argument(
        "--seed",
        type=int,
        default=DEFAULT_SEED,
        help=f"Random seed for reproducibility (default: {DEFAULT_SEED})",
    )
    args = parser.parse_args()

    run_capture(
        ui_bridge_url=args.ui_bridge_url,
        output_dir=Path(args.output_dir),
        num_samples=args.num_samples,
        seed=args.seed,
    )


if __name__ == "__main__":
    main()
