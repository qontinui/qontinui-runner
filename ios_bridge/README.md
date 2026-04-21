# ios_bridge

A tiny Python sidecar the runner spawns to talk to iOS devices. Implements
Apple's USB (usbmux) + LAN (Bonjour/RemoteXPC) protocols via
[pymobiledevice3](https://github.com/doronz88/pymobiledevice3), exposed over
localhost HTTP so the Rust runner can call it like any other service.

## When does this run?

The runner spawns it **on first iOS request** (e.g. when the connection
wizard's USB branch scans for devices). It binds to a random localhost port
and prints a one-line JSON handshake to stdout so the runner can find it.

If you never connect an iOS device, this sidecar never starts.

## Install

Requires Python 3.10+ on `PATH` (or set `IOS_BRIDGE_PYTHON=/path/to/python`).

```bash
cd qontinui-runner/ios_bridge
pip install -r requirements.txt
```

On Windows, pymobiledevice3 also needs libusb. The `pip install` pulls in
`libusb1` which bundles the DLL — no separate install step needed.

## Manually run for testing

```bash
cd qontinui-runner
python -m ios_bridge --port 8765 --log-level debug
# In another shell:
curl http://127.0.0.1:8765/devices
```

## Endpoints

| Method | Path                   | Notes                                     |
|--------|------------------------|-------------------------------------------|
| GET    | `/health`              | Liveness probe                            |
| GET    | `/devices`             | USB + Bonjour-discovered iOS devices      |
| POST   | `/pair/{udid}`         | Triggers "Trust this computer" on device  |
| POST   | `/forward`             | Body: `{udid, devicePort}` → `{localPort}` |
| GET    | `/screenshot/{udid}`   | Returns `image/png`                       |
| GET    | `/syslog/{udid}?limit` | Returns recent syslog lines               |

## Env vars

- `IOS_BRIDGE_DIR` — override path to this directory
- `IOS_BRIDGE_PYTHON` — override Python interpreter

## Why a separate process?

`pymobiledevice3` is Python-only; embedding a Python interpreter in the Rust
binary (PyO3 + bundled interpreter) adds 40-60MB to runner size and couples
its async runtime to Python's GIL. A subprocess is simpler: the Rust side
manages the lifecycle (kill-on-drop) and the Python side can be iterated
on independently.
