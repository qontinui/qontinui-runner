"""FastAPI HTTP surface for the ios_bridge sidecar.

Plan 2: the Rust runner spawns this service on a random localhost port and
calls these endpoints. The port is printed to stdout as JSON on a single
line at startup — the Rust side reads stdout to discover it.
"""

from __future__ import annotations

import argparse
import json
import logging
import socket
import sys
from contextlib import asynccontextmanager

import uvicorn
from fastapi import FastAPI, HTTPException
from fastapi.responses import Response

from . import device_manager as dm

logger = logging.getLogger("ios_bridge.server")


# ---------------------------------------------------------------------------
# FastAPI app
# ---------------------------------------------------------------------------


@asynccontextmanager
async def lifespan(_: FastAPI):
    logger.info("ios_bridge starting")
    yield
    logger.info("ios_bridge shutting down")


app = FastAPI(title="ios_bridge", version="0.1.0", lifespan=lifespan)


@app.get("/health")
def health() -> dict:
    return {"ok": True, "service": "ios_bridge"}


@app.get("/devices")
def list_devices() -> dict:
    devices = dm.list_devices()
    return {
        "devices": [
            {
                "udid": d.udid,
                "name": d.name,
                "productType": d.product_type,
                "productVersion": d.product_version,
                "paired": d.paired,
                "connection": d.connection,
            }
            for d in devices
        ]
    }


@app.post("/pair/{udid}")
def pair(udid: str) -> dict:
    return dm.initiate_pair(udid)


@app.post("/forward")
def forward(payload: dict) -> dict:
    udid = payload.get("udid")
    device_port = payload.get("devicePort") or payload.get("device_port")
    if not udid or not device_port:
        raise HTTPException(status_code=400, detail="udid and devicePort required")
    try:
        local_port = dm.start_forward(udid, int(device_port))
    except Exception as e:
        raise HTTPException(status_code=500, detail=str(e)) from e
    return {"localPort": local_port, "udid": udid, "devicePort": int(device_port)}


@app.get("/screenshot/{udid}")
def screenshot(udid: str) -> Response:
    try:
        png = dm.screenshot_png(udid)
    except Exception as e:
        raise HTTPException(status_code=500, detail=str(e)) from e
    return Response(content=png, media_type="image/png")


@app.get("/syslog/{udid}")
def syslog(udid: str, limit: int = 200) -> dict:
    return {"lines": dm.read_syslog(udid, limit=limit)}


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------


def _pick_free_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--port", type=int, default=0, help="0 = auto")
    parser.add_argument("--log-level", default="info")
    args = parser.parse_args()

    port = args.port or _pick_free_port()

    # Signal the port to the Rust parent. ONE JSON line, flushed immediately.
    print(json.dumps({"port": port, "service": "ios_bridge"}), flush=True)

    uvicorn.run(
        app,
        host="127.0.0.1",
        port=port,
        log_level=args.log_level,
        access_log=False,
    )


if __name__ == "__main__":
    main()
