/**
 * The device's coord drain, read by the doctor BEFORE the first unit. Plan Phase 3:
 * "the doctor reads drain state itself and refuses to start a run on a drained or
 * unknown-drain device (a drain exists so the box can reach idle)". The runner
 * adapter's per-launch check (`checkDrain`) stays as a backstop.
 *
 * The rule mirrors the runner's own (`src-tauri/src/coord_drain_state.rs`):
 *   - no `~/.qontinui/machine.json`, or one that names no device: NOT ENROLLED.
 *     No device row exists that coord could drain, so a run is allowed (this is
 *     every CI runner).
 *   - a device: `GET {coord}/coord/devices/me/drain` on its device JWT, which
 *     answers `{state: "clear" | "drained" | "unreadable", until?, reason?}`.
 *   - anything that cannot be established — an unreadable machine.json, a device
 *     id that is not a UUID, no live device JWT, a failed or unrecognised answer —
 *     is UNKNOWN, and the doctor refuses on it exactly as on `drained`.
 *
 * Read-only: one GET, and the token only ever travels in its Authorization header.
 */

import { readFileSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";

const OPERATIVE = "OPERATIVE";
const INOPERATIVE = "INOPERATIVE-ON-THIS-MACHINE";
const UNKNOWN = "UNKNOWN";

export const DEFAULT_COORD_URL = "https://coord.qontinui.io";
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

/** A JWT's payload, or null (the payload is base64url, not encrypted). */
function jwtPayload(token) {
  try {
    return JSON.parse(Buffer.from(String(token).split(".")[1].replace(/-/g, "+").replace(/_/g, "/"), "base64").toString("utf8"));
  } catch {
    return null;
  }
}

/** The first device JWT that is still live for at least 30 s: `$COORD_DEVICE_JWT`, then the file. */
export function liveDeviceToken({ env = process.env, home = homedir(), read = readFileSync, now = Date.now } = {}) {
  const candidates = [];
  if (typeof env.COORD_DEVICE_JWT === "string" && env.COORD_DEVICE_JWT.trim()) candidates.push(env.COORD_DEVICE_JWT.trim());
  try {
    const t = String(read(join(home, ".qontinui", "coord-device-jwt"), "utf8")).trim();
    if (t) candidates.push(t);
  } catch {
    // no file token
  }
  for (const t of candidates) {
    const p = jwtPayload(t);
    if (p && (typeof p.exp !== "number" || p.exp * 1000 > now() + 30000)) return t;
  }
  return null;
}

/**
 * @returns {Promise<{state: "clear" | "drained" | "not_enrolled" | "unknown", detail: string}>}
 */
export async function readDeviceDrain({
  home = homedir(),
  env = process.env,
  fetch = globalThis.fetch,
  now = Date.now,
  read = readFileSync,
  timeoutMs = 15000,
} = {}) {
  let machine;
  try {
    machine = JSON.parse(String(read(join(home, ".qontinui", "machine.json"), "utf8")));
  } catch (e) {
    if (e?.code === "ENOENT") {
      return { state: "not_enrolled", detail: "~/.qontinui/machine.json is missing: this machine is not a coord device, so nothing can drain it" };
    }
    return { state: "unknown", detail: `~/.qontinui/machine.json could not be read (${e?.code ?? e?.name ?? "error"})` };
  }
  const deviceId = machine?.device_id;
  if (deviceId === undefined || deviceId === null || deviceId === "") {
    return { state: "not_enrolled", detail: "machine.json names no device_id: this machine is not a coord device" };
  }
  if (typeof deviceId !== "string" || !UUID.test(deviceId)) {
    return { state: "unknown", detail: "machine.json's device_id is stated but is not a UUID" };
  }
  const token = liveDeviceToken({ env, home, read, now });
  if (!token) {
    return { state: "unknown", detail: "no live device JWT ($COORD_DEVICE_JWT or ~/.qontinui/coord-device-jwt) to read this device's drain with" };
  }
  const base = String(env.COORD_HTTP_URL || DEFAULT_COORD_URL).replace(/\/+$/, "");
  try {
    const res = await fetch(`${base}/coord/devices/me/drain`, {
      headers: { Authorization: `Bearer ${token}` },
      signal: AbortSignal.timeout(timeoutMs),
    });
    if (!res.ok) return { state: "unknown", detail: `GET /coord/devices/me/drain answered ${res.status}` };
    const body = await res.json();
    if (body?.state === "clear") return { state: "clear", detail: "coord reports this device clear" };
    if (body?.state === "drained") {
      return { state: "drained", detail: `coord reports this device drained${body.reason ? ` (${body.reason})` : ""}${body.until ? ` until ${body.until}` : ""}` };
    }
    return { state: "unknown", detail: `coord answered the drain state ${JSON.stringify(body?.state ?? null)}` };
  } catch (e) {
    return { state: "unknown", detail: `GET /coord/devices/me/drain failed: ${e?.message ?? e}` };
  }
}

/** The doctor's verdict on a drain read: only clear or not-enrolled lets a run start. */
export function drainMechanism(d) {
  if (!d) return { state: UNKNOWN, reason: "the device drain was not read" };
  if (d.state === "clear" || d.state === "not_enrolled") return { state: OPERATIVE, reason: d.detail };
  if (d.state === "drained") return { state: INOPERATIVE, reason: `refused: ${d.detail}; a drain exists so the box can reach idle` };
  return { state: UNKNOWN, reason: `refused: the device drain is unknown (${d.detail})` };
}

