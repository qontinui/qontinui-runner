/**
 * The `pageHealth` IPC — the runner webview's door to the SDK's page-health
 * producer.
 *
 * `POST /ui-bridge/control/page-health` used to run a Rust port of the SDK's
 * analyzer over a `discover` reply (`screenshots.rs`). That port took no
 * runner-only input, so it was deleted: the Rust handler now sends this IPC
 * and parses the reply strictly as `Observation<PageHealthValue>`. What
 * answers is the SDK's own `diagnosePageHealth` (producer `sdk/page-health`),
 * over the same live global-registry snapshot the SDK's in-app `pageHealth`
 * command reads (`@qontinui/ui-bridge` >= 0.31.0, `react/commandHandlers.ts`).
 *
 * `readPageHealth` restates the SDK's `diagnoseSnapshotPageHealth` (and its
 * `pageHealthInputFromSnapshot` input step), which the package does not export
 * from its root: the input mapping is field-for-field, only the wording of a
 * snapshot-failure `detail` differs. The analysis is the SDK's own. A missing `elements` key
 * stays missing (never `?? []`): "could not see" must not become "saw
 * nothing".
 */

import {
  diagnosePageHealth,
  pageHealthUnknown,
  type PageHealthObservation,
} from "@qontinui/ui-bridge";

import { describeThrown } from "@/lib/utils";

/**
 * Take a registry snapshot and run the SDK's page-health producer over it.
 * Never throws: a snapshot that could not be taken is
 * `unknown{producer_failed}` — an answer, not a failed IPC.
 */
export function readPageHealth(takeSnapshot: () => unknown): PageHealthObservation {
  let snapshot: unknown;
  try {
    snapshot = takeSnapshot();
  } catch (err) {
    return pageHealthUnknown(
      "producer_failed",
      `could not take the registry snapshot: ${describeThrown(err, "createSnapshot threw")}`,
    );
  }
  const raw = (typeof snapshot === "object" && snapshot !== null ? snapshot : {}) as {
    elements?: unknown;
    components?: unknown;
    timestamp?: unknown;
    snapshotId?: unknown;
  };
  return diagnosePageHealth({
    elements: raw.elements,
    registeredComponents: Array.isArray(raw.components) ? raw.components.length : null,
    observedAt: typeof raw.timestamp === "number" ? raw.timestamp : null,
    source: typeof raw.snapshotId === "string" ? { snapshotId: raw.snapshotId } : null,
  });
}
