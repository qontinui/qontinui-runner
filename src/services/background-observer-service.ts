/**
 * Background Observer Service — manages the screenpipe-inspired continuous
 * capture for UI Bridge-connected apps.
 *
 * Starts a BackgroundObserver that periodically captures semantic snapshots
 * and sends them to the activity timeline via Tauri IPC.
 */

import { invoke } from "@tauri-apps/api/core";
import { getGlobalRegistry, type UIBridgeRegistry } from "@qontinui/ui-bridge";
import {
  BackgroundObserver,
  type BackgroundObserverDeps,
  type TimelineCapturePayload,
} from "@qontinui/ui-bridge/ai";
import type { ControlSnapshot } from "@qontinui/ui-bridge/control";
import { SemanticSnapshotManager } from "@qontinui/ui-bridge/ai";

let observer: BackgroundObserver | null = null;

/**
 * Send a timeline capture to the Rust backend via Tauri IPC.
 */
async function persistCapture(payload: TimelineCapturePayload): Promise<void> {
  try {
    await invoke("insert_activity_entry", {
      input: {
        textContent: payload.textContent,
        sourceType: payload.sourceType,
        captureMode: payload.captureMode,
        appName: payload.appName,
        windowTitle: payload.windowTitle,
        url: payload.url,
        elementCount: payload.elementCount,
        metadataJson: payload.metadataJson,
      },
    });
  } catch (err) {
    // Non-fatal: PG may not be available
    console.warn("[BackgroundObserverService] Failed to persist capture:", err);
  }
}

/**
 * Start the background observer if not already running.
 * Should be called once when the app mounts and UI Bridge is initialized.
 */
export function startBackgroundObserver(): void {
  if (observer?.isRunning) return;

  const registry = getGlobalRegistry() as UIBridgeRegistry | null;
  if (!registry) {
    console.warn("[BackgroundObserverService] Registry not available yet");
    return;
  }

  const snapshotManager = new SemanticSnapshotManager();

  const deps: BackgroundObserverDeps = {
    snapshotManager,
    createControlSnapshot: (): ControlSnapshot => {
      const elements = registry.getAllElements();
      const components = registry.getAllComponents();
      const workflows = registry.getAllWorkflows();
      return {
        timestamp: Date.now(),
        // eslint-disable-next-line @typescript-eslint/no-explicit-any
        elements: elements.map((el: any) => ({
          id: el.id,
          type: el.type,
          label: el.label,
          // ── THE NINTH EMITTER ────────────────────────────────────────
          // Plan `2026-09-04-effect-calculus-joins-the-component-action-registry`,
          // Design decision 4. This builder flattens an element's custom-action
          // NAMES into `actions`, which both misrepresents them as standard
          // verbs and discards everything else the registration carries —
          // including, since 2026-09-11, the author's `effect` safety class.
          // Every other producer keeps them separate; `ControlSnapshot`'s own
          // doc says custom actions belong in `customActions` "rather than
          // merged into `actions`". This one was on nobody's projection list.
          //
          // WHAT IS STILL DROPPED HERE, AND WHY IT CANNOT BE CARRIED YET.
          // `effect` does not survive this hop. Not by choice: this repo pins
          // `@qontinui/ui-bridge ^0.24.0`, and in the PUBLISHED 0.24.0 types
          // `ControlSnapshot.elements[].customActions` is still `string[]`, so
          // emitting objects here is a compile error (TS2322, measured
          // 2026-09-11: "Type '{ effect?: any; id: string; }' is not assignable
          // to type 'string'"). The SDK side of the widening landed in the same
          // plan step but is not published, and DD4 step 4 is where this repo
          // bumps the pin past it. Stated rather than left silent: a silent
          // drop is the exact defect that step exists to end.
          //
          // AT STEP 4, this becomes two lines — drop the flatten below, and
          //   customActions: serializeElementCustomActions(el.customActions)
          // (exported from `@qontinui/ui-bridge`). The flatten is kept until
          // then only so the names do not vanish from the activity timeline in
          // the interim: `AIDiscoveredElement.customActions` is the channel that
          // carries them properly, and it arrives with the same SDK bump.
          actions: [...el.actions, ...(el.customActions ? Object.keys(el.customActions) : [])],
          // Populated now so the canonical field is no longer empty; still
          // names-only for the reason above.
          customActions: el.customActions ? Object.keys(el.customActions) : undefined,
          state: el.getState(),
          registeredAt: el.registeredAt,
          mounted: el.mounted,
          category: el.category,
          contentMetadata: el.contentMetadata,
          mediaMetadata: el.mediaMetadata,
        })),
        // eslint-disable-next-line @typescript-eslint/no-explicit-any
        components: components.map((comp: any) => ({
          id: comp.id,
          name: comp.name,
          // eslint-disable-next-line @typescript-eslint/no-explicit-any
          actions: comp.actions.map((a: any) => a.id),
          registeredAt: comp.registeredAt,
          mounted: comp.mounted,
        })),
        // eslint-disable-next-line @typescript-eslint/no-explicit-any
        workflows: workflows.map((wf: any) => ({
          id: wf.id,
          name: wf.name,
          stepCount: wf.steps.length,
        })),
        activeRuns: [],
      };
    },
    onCapture: persistCapture,
  };

  observer = new BackgroundObserver(deps, {
    minCaptureIntervalMs: 10000, // Every 10 seconds
    maxCaptureIntervalMs: 120000, // Force capture every 2 minutes
    maxConsecutiveErrors: 5,
  });

  observer.start();
  console.info("[BackgroundObserverService] Started");
}

/**
 * Stop the background observer.
 */
export function stopBackgroundObserver(): void {
  if (observer) {
    observer.stop();
    observer = null;
    console.info("[BackgroundObserverService] Stopped");
  }
}

/**
 * Check if the background observer is running.
 */
export function isBackgroundObserverRunning(): boolean {
  return observer?.isRunning ?? false;
}
