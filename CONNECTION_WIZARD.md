# Connection Wizard — User Guide

A 5-step modal that walks you through connecting a physical phone (Android
or iOS) to the runner so UI Bridge automation can reach it.

Open it from **Settings → Mobile → Physical Devices → "Connection Wizard"**.

## Step 1 — Connection Type

| Option | What it means | Works for |
|---|---|---|
| **USB** | Phone plugged in with USB debugging on | Android (today) / iOS (Plan 2) |
| **Wi-Fi** | Phone on the same local network | Android / iOS |
| **Remote** | Phone on a different network (airport, coffee shop, etc.) | Android / iOS |

**Managed relay is coming soon.** Until then, Remote uses either P2P or a
rathole tunnel you host yourself (see the Remote step below).

## Step 2 — Device Discovery

### USB branch

Shows every device reported by `adb devices -l` (Android, via pure-Rust
`adb_client`) and every device reported by `pymobiledevice3` (iOS). If you
see no devices:

- Android: enable **Developer Options → USB Debugging**; accept the RSA
  fingerprint prompt when the phone shows it; trust the computer
- iOS: accept "Trust this computer" on the iPhone; install `pymobiledevice3`
  (`pip install pymobiledevice3`) — required for iOS detection
- Both: confirm via `adb devices` (Android) or `python -m ios_bridge` + curl
  `/devices` (iOS)

### Wi-Fi branch

Listens for mDNS advertisements under `_uibridge._tcp.local.`. Your mobile
app must announce itself (set `enableMdnsAnnounce={true}` in
`UIBridgeNativeProvider`). A manual IP fallback is available if your router
blocks mDNS.

### Remote branch

Enter your rathole server address and a shared token. Click **Generate
config** — the wizard produces two TOML files:

- **client.toml** — drop in alongside the runner; its tunnel client connects here
- **server.toml** — deploy to your VPS (Oracle Cloud Free Tier recommended —
  see `scripts/RATHOLE-ORACLE.md` for a one-shot install script)

## Step 3 — Mobile App Setup

For LAN and Remote connections, the wizard shows the code snippet to add to
your React Native app's root:

```tsx
<UIBridgeNativeProvider
  appId="my-app"
  appType="mobile"
  enableMdnsAnnounce={true}
  serverPort={8087}
>
  …
</UIBridgeNativeProvider>
```

USB connections skip this step — ADB forwarding works transparently without
SDK changes.

**Release-build gotcha**: qontinui-mobile APKs with `versionCode ≥ 8` require
you to flip **Settings → Developer → Local UI Bridge server** ON in the app
(one-time, per-install). The local HTTP server is off by default in release
builds for security, since the toggled-on port is reachable from anyone on
the LAN.

## Step 4 — Verification

Three live checks, every one made by the runner. The wizard never calls the
phone from the webview, because a phone on `@qontinui/ui-bridge-native` 0.11.0
or later refuses any browser caller, and off a cable it admits only a runner that
signs each request with the token the phone issued it.

1. **Transport established** — USB: `POST /ui-bridge/apps/forward-device`
   (iOS: `/ui-bridge/ios/forward`) allocates the forward and fetches the phone's
   pairing token over the cable. A phone that admits the cable as on the phone
   itself needs no code there. LAN: the device's row in `GET /ui-bridge/devices`,
   which is the phone's own id for an mDNS pick and `lan-<ip>` for a typed
   address. The address is registered only when it has no row yet, so a retry
   reuses the row and its proxy. Remote: rathole tunnel up.
2. **Runner connected** — `POST /ui-bridge/devices/{id}/connect` for a LAN row,
   `POST /ui-bridge/sdk/connect` otherwise. Connecting makes the device the
   runner's one active app, in place of whatever it was connected to. A phone
   that requires pairing makes the wizard ask for the 6-digit code the app's
   Settings shows after **Pair** is tapped. That includes an Android phone over
   USB that did not admit the cable as local, once the runner's USB scanner has
   listed it; an iPhone has no USB row to pair through, so pair it over Wi-Fi.
   The wizard sends the code to `POST /ui-bridge/devices/{id}/pair` and runs the
   check again. The runner keeps the token in its encrypted store and signs every
   later request to that phone. If the runner already holds a different token for
   the phone (`PAIRING_TOKEN_HELD`), the wizard offers to replace it, which is
   right only when the pairing was revoked in the app's Settings. That pairing
   spent the code, so replacing asks for a new one.
3. **Elements readable** — `GET /ui-bridge/sdk/snapshot` on the runner returns
   non-empty elements.

On failure, the wizard shows the specific error + a retry button. Common
fixes:

- **Stage 1 USB fails**: confirm phone shows in `adb devices` as `device`
  (not `unauthorized`); on first connect you need to tap "Trust".
- **Stage 2 empty**: phone's UI Bridge server isn't bound. For release
  builds, check the Developer toggle (above). Check `adb shell netstat -ltn
  | grep 8087` — you should see `LISTEN`.
- **Stage 3 empty**: the app is running but no components registered
  elements. Open a screen that uses `useUIElement` and retry.

## Step 5 — Complete

Shows a summary card with device id, platform, transport, proxy URL, and
first few element labels. Closing the wizard refreshes the Physical Devices
list so the connected phone shows up there for future reference.

---

## Troubleshooting

| Symptom | Cause | Fix |
|---|---|---|
| "Network request failed" on mobile sign-in | Android default blocks cleartext HTTP for API 28+ | Ensure `android:usesCleartextTraffic="true"` in the merged manifest (baked into APK v9+) |
| "Network request failed" after manifest fix | Keyboard auto-upgraded `http://` to `https://` | Dump the URL field via `uiautomator dump` — if it shows `https`, clear + retype |
| "Update cannot be installed" from Play Store | Sideloaded APK signed with upload key; Play delivers with app-signing key | `adb uninstall io.qontinui.mobile` → install from Play Store |
| Port 8087 not LISTEN on phone | Local UI Bridge server toggle is off (release build) | In-app Settings → Developer → flip toggle, force-stop, reopen |
| `/connect-runner` 404 in browser | Primary runner's `web_base_url` points at FastAPI (8000) not Next.js (3001) | Update `web_base_url` in `%APPDATA%\com.qontinui.runner\settings.json` |

## Quick reference: UI Bridge discovery from anywhere

Both runner and mobile expose these after the transport is up:

- `GET /ui-bridge/_help` — full route index + action reference + tips
- `GET /ui-bridge/_routes` — minimal `[{method, path}]` list
- `GET /ui-bridge/control/snapshot?visibleOnly=true` — canonical element source
- `GET /ui-bridge/health` — liveness + app metadata
