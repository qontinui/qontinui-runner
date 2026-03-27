/**
 * Background Observer Service — manages the screenpipe-inspired continuous
 * capture for UI Bridge-connected apps.
 *
 * Starts a BackgroundObserver that periodically captures semantic snapshots
 * and sends them to the activity timeline via Tauri IPC.
 */

import { invoke } from "@tauri-apps/api/core";
import { getGlobalRegistry } from "@qontinui/ui-bridge/core";
import {
  BackgroundObserver,
  type BackgroundObserverDeps,
  type TimelineCapturePayload,
} from "@qontinui/ui-bridge/ai";
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
    console.debug("[BackgroundObserverService] Failed to persist capture:", err);
  }
}

/**
 * Start the background observer if not already running.
 * Should be called once when the app mounts and UI Bridge is initialized.
 */
export function startBackgroundObserver(): void {
  if (observer?.isRunning) return;

  const registry = getGlobalRegistry();
  if (!registry) {
    console.debug("[BackgroundObserverService] Registry not available yet");
    return;
  }

  const snapshotManager = new SemanticSnapshotManager();

  const deps: BackgroundObserverDeps = {
    snapshotManager,
    createControlSnapshot: () => registry.captureSnapshot(),
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
