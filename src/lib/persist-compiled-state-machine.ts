/**
 * persistCompiledStateMachine — Push a compiled state machine to the backend.
 *
 * After `compileStateMachineFromSpecs` produces a runtime state machine for
 * in-memory loading via `__UI_BRIDGE__.loadStateMachine`, call this helper to
 * also persist it to Postgres via `POST /state-machine/save-compiled`.
 *
 * This is best-effort: failures are logged (warn) but never thrown — callers
 * typically invoke it as `void persistCompiledStateMachine(...)` so the UI
 * flow continues even if the backend is unreachable.
 */

import { invoke } from "@tauri-apps/api/core";
import { tracedFetch } from "@/lib/runner-api";
import type { PersistedStateMachine } from "@qontinui/ui-bridge-auto";

export async function persistCompiledStateMachine(
  stateMachine: PersistedStateMachine,
  name = "spec-compiled",
): Promise<void> {
  // Resolve the runner's own port via Tauri IPC rather than the cached
  // `_apiBase`. Mount-time callers (useStateMachineRegistration's on-mount
  // auto-compile) run before `setApiPort(...)` fires from useApiReady —
  // without this, the helper POSTs to the default `localhost:9876` (the
  // primary runner), not the current runner's own port.
  let port: number;
  try {
    port = await invoke<number>("get_api_port");
    if (!port || port <= 0) throw new Error(`invalid port ${port}`);
  } catch (err) {
    console.warn(`[state-machine] save-compiled skipped (no port yet):`, err);
    return;
  }

  try {
    const res = await tracedFetch(`http://localhost:${port}/state-machine/save-compiled`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ name, compiled: stateMachine }),
    });
    if (!res.ok) {
      console.warn(`[state-machine] save-compiled returned ${res.status}`);
    }
  } catch (err) {
    console.warn(`[state-machine] save-compiled failed:`, err);
  }
}
