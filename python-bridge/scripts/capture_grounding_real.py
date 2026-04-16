#!/usr/bin/env python3
"""Real-world grounding capture for eval.

Drives the capture-host page through a list of real qontinui-web routes,
triggering the host's DOM-walk + html2canvas extractor (same-origin iframe
access). Produces GroundingRecord JSONL + images/ keyed by route — suitable
for building an eval set that tests whether a fine-tuned model transfers
from synthetic isolated renders to multi-element product UI.

Pipeline:
    1. For each route, setValue on `capture-next-url` with the full URL,
       click `capture-advance` to load the iframe.
    2. Wait for the iframe to render (fixed settle delay — real pages vary).
    3. Click `capture-real-extract` to trigger the host's DOM walk +
       html2canvas + POST to /api/grounding-isolated/page-capture.
    4. Poll GET /api/grounding-isolated/page-capture?key=<url> until the
       entry appears.
    5. Download the PNG and merge the element list into a GroundingRecord.

Usage::

    python scripts/capture_grounding_real.py \\
        --output-dir dataset-real \\
        --routes /dev/grounding /visual \\
        --settle 4.0
"""

from __future__ import annotations

import argparse
import importlib.util as _ilu
import io
import json
import logging
import sys
import time
from datetime import UTC, datetime
from pathlib import Path

import requests
from PIL import Image

# ---------------------------------------------------------------------------
# Path setup — reuse the GroundingRecord/Writer from qontinui-train
# ---------------------------------------------------------------------------
_SCRIPT_DIR = Path(__file__).resolve().parent
_BRIDGE_DIR = _SCRIPT_DIR.parent
_ROOT = _BRIDGE_DIR.parent.parent
_TRAIN_ROOT = _ROOT / "qontinui-train"
if str(_TRAIN_ROOT) not in sys.path:
    sys.path.insert(0, str(_TRAIN_ROOT))
if str(_BRIDGE_DIR) not in sys.path:
    sys.path.insert(0, str(_BRIDGE_DIR))

_gr_spec = _ilu.spec_from_file_location(
    "qontinui_train.export.grounding_record",
    _TRAIN_ROOT / "qontinui_train" / "export" / "grounding_record.py",
)
_gr_mod = _ilu.module_from_spec(_gr_spec)
_gr_mod.__package__ = "qontinui_train.export"
sys.modules[_gr_spec.name] = _gr_mod
_gr_spec.loader.exec_module(_gr_mod)

GroundingElement = _gr_mod.GroundingElement  # noqa: E402
GroundingJSONLWriter = _gr_mod.GroundingJSONLWriter  # noqa: E402
GroundingRecord = _gr_mod.GroundingRecord  # noqa: E402


logging.basicConfig(
    level=logging.INFO,
    format="%(asctime)s [%(levelname)s] %(message)s",
)
logger = logging.getLogger(__name__)

DEFAULT_UI_BRIDGE_URL = "http://localhost:3001/api/ui-bridge"
DEFAULT_BASE_URL = "http://localhost:3001"
DEFAULT_SETTLE = 4.0
DEFAULT_ROUTES = ["/dev/grounding", "/visual"]

# Map DOM tag/role to the component-ish role used in VLM training prompts.
TAG_TO_ROLE = {
    "button": "Button",
    "input": "Input",
    "textarea": "Textarea",
    "a": "Link",
    "switch": "Switch",
    "checkbox": "Checkbox",
    "tab": "Tab",
    "slider": "Slider",
    "radio": "Radio",
    "combobox": "Select",
    "menuitem": "MenuItem",
}


def normalise_role(tag: str, role: str) -> str:
    if role and role != tag:
        return TAG_TO_ROLE.get(role, role.title())
    return TAG_TO_ROLE.get(tag, tag.title())


def _pick_live_tab(ui_bridge: str) -> str | None:
    """Find a tab that actually has the capture-host elements registered.

    Multiple tabs can be connected (e.g. a stale tab still heartbeating);
    the primary one may not be the one running our latest code. Iterate
    connected tabs and pick the first whose snapshot includes
    `capture-next-url`.
    """
    h = requests.get(f"{ui_bridge}/health", timeout=5).json().get("data", {})
    tabs = h.get("connectedTabs") or []
    for tid in tabs:
        try:
            r = requests.get(
                f"{ui_bridge}/control/snapshot",
                params={"tabId": tid},
                timeout=10,
            )
            ids = {
                el.get("id")
                for el in r.json().get("data", {}).get("elements", [])
            }
            if "capture-next-url" in ids and "capture-advance" in ids:
                return tid
        except Exception:
            continue
    # fall back to primary if no explicit match
    return h.get("primaryTabId")


def drive_one_page(
    ui_bridge: str,
    base_url: str,
    route: str,
    settle_s: float,
    poll_s: float = 10.0,
    target_tab_id: str | None = None,
) -> dict | None:
    """Navigate the capture-host iframe to *route*, trigger extract, wait.

    Returns the stored page-capture entry, or None on timeout.
    """
    sess = requests.Session()
    full_url = base_url + route
    params = {"targetTabId": target_tab_id} if target_tab_id else None

    logger.info("→ %s", route)

    # 1. setValue on capture-next-url
    sess.post(
        f"{ui_bridge}/control/element/capture-next-url/action",
        json={"action": "setValue", "params": {"value": full_url}},
        params=params,
        timeout=15,
    )
    time.sleep(0.25)

    # 2. Click advance to load the iframe
    sess.post(
        f"{ui_bridge}/control/element/capture-advance/action",
        json={"action": "click"},
        params=params,
        timeout=15,
    )

    # 3. Wait for the iframe to render. The host has an onLoad handler that
    #    auto-triggers captureReal() for non-isolated URLs, so we don't need
    #    to click the extract button (which wouldn't work anyway — the
    #    iframe's UI Bridge SDK takes over tab registration and unregisters
    #    the host's buttons as soon as its React tree mounts).
    time.sleep(settle_s)

    # 4. Poll the server-side store for the entry keyed by the full URL.
    deadline = time.time() + poll_s
    endpoint = f"{base_url}/api/grounding-isolated/page-capture"
    while time.time() < deadline:
        try:
            resp = sess.get(endpoint, params={"key": full_url}, timeout=5)
            if resp.status_code == 200:
                data = resp.json()
                if data.get("found"):
                    return data
        except Exception:
            pass
        time.sleep(0.3)

    logger.warning("  no page-capture entry within %.1fs", poll_s)
    return None


def fetch_png(base_url: str, key: str) -> bytes | None:
    try:
        r = requests.get(
            f"{base_url}/api/grounding-isolated/page-capture",
            params={"key": key, "format": "png"},
            timeout=15,
        )
        if r.status_code == 200 and r.content:
            return r.content
    except Exception:
        pass
    return None


def run_capture(
    ui_bridge_url: str,
    base_url: str,
    output_dir: Path,
    routes: list[str],
    settle_s: float,
) -> None:
    # Normalise routes — Git Bash may pass //dev/grounding with double slash
    routes = [("/" + r.lstrip("/")) for r in routes]

    writer = GroundingJSONLWriter(output_dir)
    logger.info("UI Bridge: %s", ui_bridge_url)
    logger.info("Base URL:  %s", base_url)
    logger.info("Output:    %s", output_dir)
    logger.info("Routes:    %s", routes)

    # Self-drive mode: navigate ONCE to capture-host with a ?real=...
    # query that encodes the full route list + interval. The page cycles
    # itself, auto-capturing on each iframe onLoad. This avoids per-step
    # UI Bridge command routing (which gets flaky when multiple tabs are
    # registered).
    interval_ms = int(settle_s * 1000)
    real_param = ",".join(routes)
    import urllib.parse
    host_url = (
        f"{base_url}/dev/grounding/capture-host"
        f"?real={urllib.parse.quote(real_param, safe=',/')}"
        f"&interval={interval_ms}"
        f"&_t={int(time.time())}"
    )
    logger.info("driving host: %s", host_url)

    # Use UI Bridge hard navigation — forces full reload of the current
    # tab to pick up the ?real= query param that triggers self-drive.
    # webbrowser.open() tends to focus an existing tab without reloading,
    # which skips the useEffect that reads search params at mount.
    host_path = host_url.split(base_url, 1)[1]
    try:
        requests.post(
            f"{ui_bridge_url}/control/page/navigate",
            json={"url": host_path, "hard": True},
            timeout=10,
        )
    except Exception as exc:
        logger.warning("UI Bridge navigate failed (%s); trying webbrowser", exc)
        try:
            import webbrowser
            webbrowser.open(host_url, new=2)
        except Exception:
            pass

    # Wait for each route to be captured — allow settle_s per route + buffer
    total_wait = (len(routes) + 1) * settle_s + 10.0
    logger.info("waiting up to %.1fs for captures to complete...", total_wait)

    total_written = 0
    total_skipped = 0
    total_elements = 0

    # Poll for each expected route to appear in the page-capture store
    deadline = time.time() + total_wait
    captured = {}
    pending = set(base_url + r for r in routes)
    while pending and time.time() < deadline:
        try:
            r = requests.get(
                f"{base_url}/api/grounding-isolated/page-capture", timeout=5,
            )
            for e in r.json().get("entries", []):
                key = e.get("key")
                if key in pending and key not in captured:
                    logger.info("  captured %s (%d elements)", key, e["elements"])
                    pending.discard(key)
                    # Fetch full entry
                    full = requests.get(
                        f"{base_url}/api/grounding-isolated/page-capture",
                        params={"key": key},
                        timeout=10,
                    ).json()
                    if full.get("found"):
                        captured[key] = full
        except Exception:
            pass
        if pending:
            time.sleep(1.0)

    if pending:
        logger.warning("never captured: %s", pending)

    for route in routes:
        full_url = base_url + route
        entry = captured.get(full_url)
        if not entry:
            total_skipped += 1
            continue

        png_bytes = fetch_png(base_url, entry["key"])
        if not png_bytes:
            logger.warning("  no PNG fetched for %s", route)
            total_skipped += 1
            continue

        # Resolve screen dimensions from the PNG header
        img = Image.open(io.BytesIO(png_bytes))
        screen_w, screen_h = img.size

        els = []
        for raw in entry.get("elements", []):
            bbox = raw.get("bbox", [0, 0, 0, 0])
            if len(bbox) < 4:
                continue
            x, y, w, h = int(bbox[0]), int(bbox[1]), int(bbox[2]), int(bbox[3])
            if w < 10 or h < 10:
                continue
            tag = str(raw.get("tag", ""))
            role_attr = str(raw.get("role", ""))
            role_norm = normalise_role(tag, role_attr)
            els.append(
                GroundingElement(
                    role=role_norm.lower(),
                    text=str(raw.get("text", "")).strip() or None,
                    bbox=(x, y, w, h),
                    interactable=True,
                )
            )

        if not els:
            logger.info("  %s: 0 valid elements — skipped", route)
            total_skipped += 1
            continue

        record = GroundingRecord(
            image_hash="",
            image_path="",
            viewport_width=screen_w,
            viewport_height=screen_h,
            elements=els,
            action=None,
            source="static",
            timestamp=datetime.now(UTC).isoformat(),
            metadata={
                "route": route,
                "url": entry.get("url"),
                "capture_mode": "real",
                "screenshot_source": "iframe-html2canvas",
                "element_count": len(els),
            },
        )

        if writer.write(record, png_bytes):
            total_written += 1
            total_elements += len(els)
            logger.info(
                "  %s: %d elements, %d×%d",
                route, len(els), screen_w, screen_h,
            )
        else:
            total_skipped += 1

    logger.info(
        "Done. written=%d skipped=%d total_elements=%d → %s",
        total_written,
        total_skipped,
        total_elements,
        output_dir / "grounding.jsonl",
    )


def main() -> None:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--ui-bridge-url", default=DEFAULT_UI_BRIDGE_URL)
    p.add_argument("--base-url", default=DEFAULT_BASE_URL)
    p.add_argument("--output-dir", default="dataset-real")
    p.add_argument("--routes", nargs="+", default=DEFAULT_ROUTES)
    p.add_argument("--settle", type=float, default=DEFAULT_SETTLE)
    args = p.parse_args()

    run_capture(
        ui_bridge_url=args.ui_bridge_url,
        base_url=args.base_url,
        output_dir=Path(args.output_dir),
        routes=args.routes,
        settle_s=args.settle,
    )


if __name__ == "__main__":
    main()
