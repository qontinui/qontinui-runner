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
import json
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
# Path setup — add qontinui-train to sys.path for grounding_record imports.
# This script uses its own lightweight requests-based UI Bridge client to
# avoid a heavy httpx dependency; the equivalent pattern is also available
# via `ui_bridge.CaptureHostDriver` for users who prefer the official SDK.
# ---------------------------------------------------------------------------
_SCRIPT_DIR = Path(__file__).resolve().parent
_BRIDGE_DIR = _SCRIPT_DIR.parent  # python-bridge/
_ROOT = _BRIDGE_DIR.parent.parent  # qontinui-root/
_TRAIN_ROOT = _ROOT / "qontinui-train"

if str(_TRAIN_ROOT) not in sys.path:
    sys.path.insert(0, str(_TRAIN_ROOT))
if str(_BRIDGE_DIR) not in sys.path:
    sys.path.insert(0, str(_BRIDGE_DIR))

# Import grounding_record directly to avoid __init__.py pulling in unrelated
# modules (training_data_exporter, training_export_service) that depend on
# packages only available inside the runner's full Poetry environment.
import importlib.util as _ilu  # noqa: E402

_gr_spec = _ilu.spec_from_file_location(
    "qontinui_train.export.grounding_record",
    _TRAIN_ROOT / "qontinui_train" / "export" / "grounding_record.py",
)
_gr_mod = _ilu.module_from_spec(_gr_spec)
_gr_mod.__package__ = "qontinui_train.export"
sys.modules[_gr_spec.name] = _gr_mod
_gr_spec.loader.exec_module(_gr_mod)

GroundingElement = _gr_mod.GroundingElement
GroundingJSONLWriter = _gr_mod.GroundingJSONLWriter
GroundingRecord = _gr_mod.GroundingRecord

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

# Isolated component page base route — the App Router page renders via the
# real design-system components (not hand-written HTML).  Capture uses
# hard navigation (full reload) so the UI Bridge SDK re-initialises per sample.
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

    def navigate(self, url: str, hard: bool = False) -> dict:
        """Navigate the connected browser to a URL path.

        When *hard* is True the SDK bypasses client-side (router.push)
        navigation and does a full page reload via ``window.location.href``.
        This re-initialises the UI Bridge provider tree, which is necessary
        on pages that unmount the SDK (e.g. the isolated component renderer).
        """
        return self._post("/control/page/navigate", {"url": url, "hard": hard})

    def wait_for_tab(self, timeout_s: float = 10.0) -> bool:
        """Poll /health until a connected tab appears or *timeout_s* elapses."""
        deadline = time.time() + timeout_s
        while time.time() < deadline:
            try:
                h = self._get("/health").get("data", {})
                if h.get("connectedTabs"):
                    return True
            except Exception:
                pass
            time.sleep(0.3)
        return False

    def set_viewport_constraints(self, width: int) -> dict:
        """Apply CSS viewport width constraints."""
        return self._post("/control/viewport-constraints", {"width": width})

    def restore_viewport(self) -> dict:
        """Remove CSS viewport constraints."""
        return self._post("/control/viewport-constraints", {"restore": True})

    def element_action(
        self, element_id: str, action: str, params: dict | None = None,
    ) -> dict:
        """Invoke an action on a registered element (click, setValue, etc.)."""
        body: dict = {"action": action}
        if params:
            body["params"] = params
        return self._post(f"/control/element/{element_id}/action", body)

    def get_body_attributes(self) -> dict:
        """Return the `<body>` element attributes from the latest snapshot.

        Used by the capture-host loop to read the current sample's bbox
        (rendered by the iframe and relayed via postMessage → body data-attrs).
        """
        snap = self.get_control_snapshot()
        # Search for the body-level attributes if the snapshot exposes them,
        # otherwise scan element list for a root element.
        attrs = snap.get("bodyAttributes") or snap.get("body")
        if isinstance(attrs, dict):
            return attrs
        for el in snap.get("elements", []):
            if el.get("type") == "body" or el.get("tag", "").lower() == "body":
                return el.get("attributes") or {}
        return {}

    def get_control_snapshot(self) -> dict:
        """Get full control snapshot with elements and viewport."""
        body = self._get("/control/snapshot")
        return body.get("data", body)


# ---------------------------------------------------------------------------
# Screenshot capture (mss — same pattern as trajectory_logger.py)
# ---------------------------------------------------------------------------

def capture_screen(monitor_index: int = 1) -> tuple[bytes, int, int]:
    """Capture a monitor as PNG bytes. Returns (png_bytes, width, height).

    ``monitor_index`` is the mss monitor index:
      * 0 = all monitors combined (virtual bounding box)
      * 1 = primary monitor (default)
      * 2+ = additional monitors, in order reported by the OS
    """
    import mss

    with mss.mss() as sct:
        if monitor_index < 0 or monitor_index >= len(sct.monitors):
            raise ValueError(
                f"monitor_index={monitor_index} out of range; "
                f"available: 0..{len(sct.monitors) - 1}"
            )
        mon = sct.monitors[monitor_index]
        shot = sct.grab(mon)
        arr = np.array(shot)[:, :, :3]  # BGRA → BGR

    h, w = arr.shape[:2]
    img = Image.fromarray(arr[..., ::-1])  # BGR → RGB for PIL
    buf = io.BytesIO()
    img.save(buf, format="PNG")
    return buf.getvalue(), w, h


# ---------------------------------------------------------------------------
# Estimated component sizes (pixels) — used when UI Bridge snapshot is
# unavailable (the API route page runs without the UI Bridge SDK).
# Values are approximate defaults at 1920×1080 with default browser zoom.
# ---------------------------------------------------------------------------

COMPONENT_SIZES: dict[str, tuple[int, int]] = {
    "Button": (120, 40),
    "Badge": (80, 24),
    "Input": (256, 40),
    "Textarea": (256, 80),
    "Checkbox": (120, 24),
    "Switch": (120, 24),
    "Toggle": (72, 40),
    "Select": (192, 40),
    "Slider": (256, 20),
    "Progress": (256, 16),
    "Tabs": (288, 80),
    "Card": (288, 200),
    "Separator": (256, 50),
    "Label": (256, 56),
}


def estimate_target_bbox(
    params: dict, screen_w: int, screen_h: int,
) -> GroundingElement:
    """Estimate the bounding box of the rendered component.

    The isolated page positions the component at ``(left%, top%)`` of the
    viewport with ``transform: translate(-50%, -50%)``.  We combine the
    known position with an approximate component size to produce a bbox.
    """
    component = params["component"]
    est_w, est_h = COMPONENT_SIZES.get(component, (120, 40))

    # Center of the component in pixels
    cx = screen_w * params["left"] / 100.0
    cy = screen_h * params["top"] / 100.0

    # Bbox top-left
    x = max(0, int(cx - est_w / 2))
    y = max(0, int(cy - est_h / 2))
    w = min(est_w, screen_w - x)
    h = min(est_h, screen_h - y)

    variant = params.get("variant", "default")
    state_str = params.get("state", "enabled")
    label = f"{variant.title()} {component}"
    if state_str == "disabled":
        label = f"Disabled {label}"

    return GroundingElement(
        role=component.lower(),
        text=label,
        bbox=(x, y, w, h),
        interactable=state_str != "disabled",
    )


def find_target_element(snapshot: dict) -> GroundingElement | None:
    """Find the grounding-target element in the UI Bridge snapshot.

    Returns ``None`` if no target element is found (e.g. the page is
    served without the UI Bridge SDK).
    """
    for el in snapshot.get("elements", []):
        attrs: dict = el.get("attributes", {}) or {}
        val = attrs.get("data-grounding-target")
        if val is None:
            val = el.get("data-grounding-target")
        if val is not None and str(val).lower() not in ("false", "0", ""):
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
    return None


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


def build_isolated_url(params: dict, sample_index: int | None = None) -> str:
    """Construct the isolated component page URL from sample params."""
    qs_params = {
        "component": params["component"],
        "variant": params["variant"],
        "size": params["size"],
        "state": params["state"],
        "theme": params["theme"],
        "bg": params["bg"],
        "left": params["left"],
        "top": params["top"],
    }
    if sample_index is not None:
        qs_params["sampleIndex"] = sample_index
    return f"{ISOLATED_ROUTE}?{urlencode(qs_params)}"


def build_api_isolated_url(params: dict, sample_index: int | None = None) -> str:
    """Construct the /api/grounding-isolated URL (SDK-less standalone HTML)."""
    qs_params = {
        "component": params["component"],
        "variant": params["variant"],
        "size": params["size"],
        "state": params["state"],
        "theme": params["theme"],
        "bg": params["bg"],
        "left": params["left"],
        "top": params["top"],
    }
    if sample_index is not None:
        qs_params["sampleIndex"] = sample_index
    return f"/api/grounding-isolated?{urlencode(qs_params)}"


# ---------------------------------------------------------------------------
# Main capture loop
# ---------------------------------------------------------------------------

def _derive_host_url(ui_bridge_url: str, path: str = "/dev/grounding/capture-host") -> str:
    """Convert a UI Bridge URL like http://host:port/api/ui-bridge to a
    full page URL at ``path`` on the same origin."""
    from urllib.parse import urlparse, urlunparse

    parsed = urlparse(ui_bridge_url)
    # Drop the /api/ui-bridge suffix from the path
    return urlunparse(parsed._replace(path=path, query="", fragment=""))


def _has_capture_host_elements(client: UIBridgeClient) -> bool:
    """Return True if the connected tab has registered capture-host elements."""
    try:
        snap = client.get_control_snapshot()
    except Exception:
        return False
    seen_ids = {el.get("id") for el in snap.get("elements", [])}
    return "capture-next-url" in seen_ids and "capture-advance" in seen_ids


def _wait_for_capture_host(
    client: UIBridgeClient, timeout_s: float = 15.0,
) -> bool:
    """Wait until a tab is connected AND it's the capture-host page."""
    deadline = time.time() + timeout_s
    while time.time() < deadline:
        try:
            if client.health_check():
                h = client._get("/health").get("data", {})
                if h.get("connectedTabs") and _has_capture_host_elements(client):
                    return True
        except Exception:
            pass
        time.sleep(0.3)
    return False


def _ensure_capture_host_connected(
    client: UIBridgeClient,
    ui_bridge_url: str,
    max_attempts: int = 3,
) -> None:
    """Guarantee the browser has the capture-host page open with a healthy SDK.

    Strategy:
      1. If a tab is already on the capture-host page, we're done.
      2. Else if any tab is connected, try navigating it to capture-host via
         UI Bridge (hard reload).
      3. Else open the capture-host URL in the user's default browser via
         ``webbrowser.open``, creating a fresh tab.
      4. Verify the connected tab is actually on capture-host by looking for
         the registered ``capture-next-url`` and ``capture-advance`` elements.
    """
    import webbrowser

    host_url = _derive_host_url(ui_bridge_url)

    # Fast path — already on the capture host
    if _has_capture_host_elements(client):
        logger.info("Capture host already connected")
        return

    for attempt in range(1, max_attempts + 1):
        try:
            health = client._get("/health").get("data", {})
        except Exception:
            health = {}
        tab_count = len(health.get("connectedTabs", []) or [])

        if tab_count > 0:
            try:
                logger.info(
                    "Redirecting existing tab to capture-host (attempt %d)", attempt,
                )
                client.navigate("/dev/grounding/capture-host", hard=True)
                if _wait_for_capture_host(client, timeout_s=15.0):
                    logger.info("Capture host connected")
                    return
            except Exception as exc:
                logger.warning("Navigate via UI Bridge failed: %s", exc)

        logger.info(
            "Opening capture-host in default browser: %s (attempt %d)",
            host_url, attempt,
        )
        try:
            webbrowser.open(host_url, new=2)  # new=2 → new tab
        except Exception as exc:
            logger.warning("webbrowser.open failed: %s", exc)

        if _wait_for_capture_host(client, timeout_s=20.0):
            logger.info("Capture host connected")
            return

    logger.error(
        "Failed to bootstrap capture-host after %d attempts. "
        "Manually open %s in a browser and rerun.",
        max_attempts, host_url,
    )
    sys.exit(1)


def run_capture_host(
    ui_bridge_url: str,
    output_dir: Path,
    num_samples: int,
    seed: int,
    monitor_index: int = 1,
) -> None:
    """Capture via the `/dev/grounding/capture-host` outer page.

    The outer page keeps a stable UI Bridge SDK connection while cycling
    an inner iframe through isolated-sample URLs.  This avoids the
    SDK-unmount issue observed when navigating directly to the isolated
    page with position:fixed backdrop.
    """
    client = UIBridgeClient(ui_bridge_url)
    writer = GroundingJSONLWriter(output_dir)

    logger.info("UI Bridge: %s", ui_bridge_url)
    logger.info("Output:    %s", output_dir)
    logger.info("Samples:   %d  (seed=%d, capture-host mode)", num_samples, seed)

    if not client.health_check():
        logger.error("UI Bridge not reachable at %s", ui_bridge_url)
        sys.exit(1)

    # Ensure the browser is on the capture-host page with a healthy SDK.
    _ensure_capture_host_connected(client, ui_bridge_url)
    time.sleep(1.0)  # let React hydrate, register ui-bridge elements

    samples = draw_samples(num_samples, seed)
    total_written = 0
    total_skipped = 0
    total_errors = 0

    for idx, params in enumerate(samples):
        sample_url = build_api_isolated_url(params, sample_index=idx)

        try:
            # Drive the outer page: setValue on the URL input + click advance.
            # The delay between the two commands lets React's controlled-input
            # onChange propagate before the button handler reads the state.
            client.element_action(
                "capture-next-url", "setValue", {"value": sample_url},
            )
            time.sleep(0.25)
            client.element_action("capture-advance", "click")

            # Wait for the iframe's measurement to arrive. We try two
            # channels in order of reliability:
            #   1) Server-side signal channel at /api/grounding-isolated/bbox
            #      — the iframe POSTs its bbox here on load. Works regardless
            #      of browser tab focus (fetch() runs even in backgrounded
            #      tabs; postMessage doesn't always).
            #   2) Echo-input via UI Bridge snapshot (capture-last-bbox /
            #      capture-last-echo). Works when the host tab is focused.
            bbox: dict | None = None
            base_origin = ui_bridge_url.split("/api/ui-bridge")[0]
            bbox_endpoint = f"{base_origin}/api/grounding-isolated/bbox?sampleIndex={idx}"
            deadline = time.time() + 6.0
            while time.time() < deadline:
                # Channel 1: server-side signal (works regardless of focus).
                try:
                    resp = requests.get(bbox_endpoint, timeout=3)
                    payload = resp.json() if resp.ok else {}
                    if (
                        payload.get("found")
                        and (b := payload.get("bbox"))
                        and int(b.get("width", 0)) > 0
                        and int(b.get("height", 0)) > 0
                    ):
                        bbox = b
                        break
                except Exception:
                    pass
                # Channel 2: echo input via UI Bridge snapshot.
                try:
                    snap = client.get_control_snapshot()
                    for el in snap.get("elements", []):
                        if el.get("id") not in (
                            "capture-last-echo", "capture-last-bbox",
                        ):
                            continue
                        raw = (
                            (el.get("state") or {}).get("value")
                            or el.get("value")
                            or ""
                        )
                        if raw:
                            try:
                                candidate = json.loads(raw)
                            except Exception:
                                candidate = None
                            if (
                                candidate
                                and int(candidate.get("sampleIndex", -1)) == idx
                            ):
                                bbox = candidate
                                break
                    if bbox:
                        break
                except Exception:
                    pass
                time.sleep(0.1)

            # Settle before screenshot
            time.sleep(SETTLE_DELAY)

            # Prefer the iframe's html2canvas render (tab-focus-independent)
            # over mss monitor capture, which only shows whichever tab is
            # actually visible on the captured monitor. Fall back to mss
            # when the server doesn't have a rendered PNG ready.
            png_bytes = None
            screen_w = screen_h = 0
            screenshot_endpoint = (
                f"{base_origin}/api/grounding-isolated/screenshot"
                f"?sampleIndex={idx}"
            )
            shot_deadline = time.time() + 4.0
            while time.time() < shot_deadline:
                try:
                    resp = requests.get(screenshot_endpoint, timeout=3)
                    if resp.status_code == 200 and resp.content:
                        png_bytes = resp.content
                        try:
                            screen_w = int(resp.headers.get("X-Sample-Width", 0))
                            screen_h = int(resp.headers.get("X-Sample-Height", 0))
                        except (TypeError, ValueError):
                            pass
                        break
                except Exception:
                    pass
                time.sleep(0.2)

            screenshot_source = "iframe"
            if png_bytes is None:
                png_bytes, screen_w, screen_h = capture_screen(monitor_index)
                screenshot_source = "mss"
            elif not screen_w or not screen_h:
                # Fallback to mss just for viewport dimensions
                _, screen_w, screen_h = capture_screen(monitor_index)

            # Build GroundingElement from reported bbox, falling back to estimate
            if bbox and int(bbox.get("width", 0)) > 0 and int(bbox.get("height", 0)) > 0:
                target_el = GroundingElement(
                    role=params["component"].lower(),
                    text=f"{params['variant'].title()} {params['component']}",
                    bbox=(
                        int(bbox["x"]),
                        int(bbox["y"]),
                        int(bbox["width"]),
                        int(bbox["height"]),
                    ),
                    interactable=params["state"] != "disabled",
                )
            else:
                target_el = estimate_target_bbox(params, screen_w, screen_h)

            # Sanity filter: skip any sample whose bbox or screenshot is
            # too small to be useful training data.  We've seen tiny
            # screenshots (from slow html2canvas returns) and [0,0,1,1]
            # bboxes (from estimate fallback running against a 0×0
            # viewport) create garbage records that pollute the dataset.
            bx, by, bw, bh = target_el.bbox
            if bw < 10 or bh < 10:
                logger.debug(
                    "Sample %d: bbox too small (%dx%d) — skipping", idx, bw, bh,
                )
                total_skipped += 1
                continue
            if screen_w < 200 or screen_h < 200:
                logger.debug(
                    "Sample %d: screenshot too small (%dx%d) — skipping",
                    idx, screen_w, screen_h,
                )
                total_skipped += 1
                continue

            record = GroundingRecord(
                image_hash="",
                image_path="",
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
                    "capture_mode": "host",
                    "bbox_source": "iframe" if bbox else "estimate",
                    "screenshot_source": screenshot_source,
                },
            )

            if writer.write(record, png_bytes):
                total_written += 1
            else:
                total_skipped += 1

        except Exception as exc:
            logger.warning("Sample %d error: %s", idx, exc)
            total_errors += 1
            continue

        if (idx + 1) % 50 == 0:
            logger.info(
                "Progress: %d/%d — written=%d skipped=%d errors=%d",
                idx + 1, num_samples, total_written, total_skipped, total_errors,
            )

    logger.info(
        "Done. written=%d skipped=%d errors=%d → %s",
        total_written, total_skipped, total_errors,
        output_dir / "grounding.jsonl",
    )


def run_capture(
    ui_bridge_url: str,
    output_dir: Path,
    num_samples: int,
    seed: int,
    monitor_index: int = 1,
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
            # Hard navigation: full page reload so the UI Bridge SDK
            # re-initialises cleanly on every sample. The isolated page's
            # component tree has been observed to drop the SSE listener
            # during React's client-side transitions; a hard reload avoids
            # the problem entirely.
            client.navigate(nav_url, hard=True)
            # Wait for the browser to reconnect to UI Bridge before snapshotting
            if not client.wait_for_tab(timeout_s=8.0):
                logger.debug("Sample %d: UI Bridge tab did not reconnect", idx)
            time.sleep(SETTLE_DELAY)

            png_bytes, screen_w, screen_h = capture_screen(monitor_index)

            # Try UI Bridge snapshot first; fall back to estimated bbox.
            try:
                snapshot = client.get_control_snapshot()
                target_el = find_target_element(snapshot)
            except Exception:
                target_el = None

            if target_el is None:
                target_el = estimate_target_bbox(params, screen_w, screen_h)

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
    parser.add_argument(
        "--monitor",
        type=int,
        default=int(os.getenv("QONTINUI_CAPTURE_MONITOR", "1")),
        help=(
            "Monitor index to capture: 0=all, 1=primary, 2+=secondary "
            "(default: 1; env QONTINUI_CAPTURE_MONITOR)"
        ),
    )
    parser.add_argument(
        "--mode",
        choices=("host", "direct"),
        default="host",
        help=(
            "Capture strategy: 'host' drives an outer /dev/grounding/capture-host "
            "page with an inner iframe (stable SDK), 'direct' navigates the tab "
            "to each isolated URL with hard reloads. (default: host)"
        ),
    )
    args = parser.parse_args()

    runner = run_capture_host if args.mode == "host" else run_capture
    runner(
        ui_bridge_url=args.ui_bridge_url,
        output_dir=Path(args.output_dir),
        num_samples=args.num_samples,
        seed=args.seed,
        monitor_index=args.monitor,
    )


if __name__ == "__main__":
    main()
