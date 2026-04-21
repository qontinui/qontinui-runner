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

import { getApiBase, tracedFetch } from "@/lib/runner-api";
import type { PersistedStateMachine } from "@qontinui/ui-bridge-auto";

export async function persistCompiledStateMachine(
  stateMachine: PersistedStateMachine,
  name = "spec-compiled",
): Promise<void> {
  try {
    const res = await tracedFetch(`${getApiBase()}/state-machine/save-compiled`, {
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
