/**
 * persistQuarantinedCompilation — Push a quarantined state-machine
 * compilation to the backend, which writes it to the on-disk quarantine
 * directory (`~/.qontinui/state-machine-quarantine/<id>.json`).
 *
 * After `compileStateMachineFromSpecs` rejects a batch due to duplicate
 * elements (`{ compiled: false, reason: "quarantined", quarantine }`), call
 * this helper to persist the record so operators can inspect the conflicting
 * specs offline. Mirrors the shape of `persistCompiledStateMachine` — it's
 * best-effort, failures are logged (warn) but never thrown. Callers
 * typically invoke it as `void persistQuarantinedCompilation(...)`.
 */

import { invoke } from "@tauri-apps/api/core";
import { tracedFetch } from "@/lib/runner-api";
import type { QuarantineRecord } from "@/lib/compile-state-machine";

export async function persistQuarantinedCompilation(record: QuarantineRecord): Promise<void> {
  // Resolve the runner's own port via Tauri IPC rather than the cached
  // `_apiBase` — see the same rationale in persistCompiledStateMachine.
  let port: number;
  try {
    port = await invoke<number>("get_api_port");
    if (!port || port <= 0) throw new Error(`invalid port ${port}`);
  } catch (err) {
    console.warn(`[state-machine] quarantine persist skipped (no port yet):`, err);
    return;
  }

  try {
    const res = await tracedFetch(`http://localhost:${port}/state-machine/quarantine`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(record),
    });
    if (!res.ok) {
      console.warn(`[state-machine] quarantine persist returned ${res.status}`);
    }
  } catch (err) {
    console.warn(`[state-machine] quarantine persist failed:`, err);
  }
}
