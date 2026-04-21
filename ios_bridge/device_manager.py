"""Thin wrapper around pymobiledevice3.

Plan 2: the Rust side of the runner calls this over HTTP; this module
translates each HTTP request into the relevant pymobiledevice3 call. All
functions are synchronous — FastAPI runs them on a thread pool.

We intentionally keep this tiny: one function per device operation, no state
beyond what pymobiledevice3 itself holds. Lifecycle (spawn / stop) is the
Rust caller's concern.
"""

from __future__ import annotations

import io
import logging
from dataclasses import dataclass
from typing import Any

logger = logging.getLogger("ios_bridge.device_manager")


# ---------------------------------------------------------------------------
# Types
# ---------------------------------------------------------------------------


@dataclass
class IosDevice:
    """Subset of LockdownClient metadata we expose over HTTP."""

    udid: str
    name: str | None
    product_type: str | None  # e.g. "iPhone14,2"
    product_version: str | None  # e.g. "17.4"
    paired: bool
    connection: str  # "usb" | "network"


# ---------------------------------------------------------------------------
# Device listing
# ---------------------------------------------------------------------------


def list_devices() -> list[IosDevice]:
    """Enumerate USB and Bonjour-discovered iOS devices.

    USB devices come from the usbmux daemon (started automatically by
    pymobiledevice3 on first use). Bonjour-discovered devices come from the
    network discovery service (iOS 14+).
    """
    try:
        from pymobiledevice3.usbmux import list_devices as usbmux_list
    except Exception as e:
        logger.warning("pymobiledevice3 not importable: %s", e)
        return []

    out: list[IosDevice] = []

    # USB-attached devices
    try:
        for muxd in usbmux_list():
            udid = getattr(muxd, "serial", None) or getattr(muxd, "udid", None)
            if not udid:
                continue
            info = _lockdown_info_safe(udid)
            out.append(
                IosDevice(
                    udid=udid,
                    name=info.get("DeviceName"),
                    product_type=info.get("ProductType"),
                    product_version=info.get("ProductVersion"),
                    paired=bool(info.get("paired", False)),
                    connection="usb",
                )
            )
    except Exception as e:
        logger.warning("usbmux enumeration failed: %s", e)

    # Network (Bonjour) devices — best-effort, iOS 14+
    try:
        from pymobiledevice3.bonjour import browse_remoted  # type: ignore

        for svc in browse_remoted(timeout=1.0):
            udid = getattr(svc, "udid", None)
            if not udid:
                continue
            # Skip duplicates already seen via USB.
            if any(d.udid == udid for d in out):
                continue
            out.append(
                IosDevice(
                    udid=udid,
                    name=getattr(svc, "name", None),
                    product_type=None,
                    product_version=None,
                    paired=False,
                    connection="network",
                )
            )
    except Exception as e:
        logger.debug("Bonjour browse skipped: %s", e)

    return out


def _lockdown_info_safe(udid: str) -> dict[str, Any]:
    """Return a dict of LockdownClient values or {} on any error."""
    try:
        from pymobiledevice3.lockdown import create_using_usbmux

        client = create_using_usbmux(serial=udid)
        info: dict[str, Any] = {
            "DeviceName": client.get_value(domain=None, key="DeviceName"),
            "ProductType": client.get_value(domain=None, key="ProductType"),
            "ProductVersion": client.get_value(domain=None, key="ProductVersion"),
            "paired": True,  # create_using_usbmux succeeds only when paired
        }
        return info
    except Exception as e:
        logger.debug("lockdown info for %s failed: %s", udid, e)
        return {"paired": False}


# ---------------------------------------------------------------------------
# Pairing
# ---------------------------------------------------------------------------


def initiate_pair(udid: str) -> dict[str, Any]:
    """Trigger the 'Trust this computer' dialog on the device.

    The user must tap 'Trust' on the iPhone for the pairing to complete.
    This function returns immediately — callers should poll `list_devices`
    and check `paired=True`.
    """
    try:
        from pymobiledevice3.lockdown import create_using_usbmux

        # create_using_usbmux triggers the trust prompt if not paired.
        _ = create_using_usbmux(serial=udid, autopair=True)
        return {"ok": True, "message": "pairing initiated"}
    except Exception as e:
        return {"ok": False, "error": str(e)}


# ---------------------------------------------------------------------------
# Port forwarding
# ---------------------------------------------------------------------------


def start_forward(udid: str, device_port: int) -> int:
    """Open a local TCP listener that proxies to `device_port` on the device.

    Returns the local port. Runs a background thread for the accept loop;
    callers are expected to tear it down when the service stops.

    Implementation note: pymobiledevice3's TcpForwarder binds to a specific
    port. We bind :0 via a pre-check socket, then pass that port. Not
    race-free but close enough for single-user dev use.
    """
    import socket
    import threading

    # Reserve an ephemeral port by creating and closing a temporary listener
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind(("127.0.0.1", 0))
        local_port = s.getsockname()[1]

    try:
        from pymobiledevice3.lockdown import create_using_usbmux
        from pymobiledevice3.tcp_forwarder import LockdownTcpForwarder

        lockdown = create_using_usbmux(serial=udid)
        forwarder = LockdownTcpForwarder(lockdown, local_port, device_port)
        t = threading.Thread(
            target=forwarder.start,
            name=f"ios-fwd-{udid[:8]}-{device_port}",
            daemon=True,
        )
        t.start()
        logger.info(
            "started iOS forward udid=%s device_port=%s local_port=%s",
            udid,
            device_port,
            local_port,
        )
        return local_port
    except Exception as e:
        raise RuntimeError(f"forward failed: {e}") from e


# ---------------------------------------------------------------------------
# Screenshot / syslog
# ---------------------------------------------------------------------------


def screenshot_png(udid: str) -> bytes:
    """Capture the device screen as PNG bytes."""
    from pymobiledevice3.lockdown import create_using_usbmux
    from pymobiledevice3.services.screenshot import ScreenshotService

    lockdown = create_using_usbmux(serial=udid)
    with ScreenshotService(lockdown=lockdown) as svc:
        png = svc.take_screenshot()
    return bytes(png)


def read_syslog(udid: str, limit: int = 200) -> list[str]:
    """Read up to `limit` lines from the device syslog, then return.

    Not a stream — callers that want live tail should poll.
    """
    from pymobiledevice3.lockdown import create_using_usbmux
    from pymobiledevice3.services.syslog import SyslogService

    lockdown = create_using_usbmux(serial=udid)
    lines: list[str] = []
    try:
        with SyslogService(lockdown=lockdown) as svc:
            for entry in svc.watch(num_of_lines=limit):
                lines.append(str(entry))
                if len(lines) >= limit:
                    break
    except Exception as e:
        logger.warning("syslog read failed for %s: %s", udid, e)
    return lines
