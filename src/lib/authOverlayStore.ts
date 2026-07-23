import { useSyncExternalStore } from "react";
import type { AuthGateOverlay } from "./authGatePresentation";

/**
 * Module-level broadcast of the current auth-overlay surface (P2 auth-gate
 * overlay). `AppContent` computes the gate (it owns `setupCompleted` and the
 * auth context) and publishes here; sibling trees that render OUTSIDE
 * `AppContent` — the tutorial/demo/prompt overlays, `BackgroundTaskPill`,
 * `BuildRefreshBanner` — subscribe so they can go `inert` in lockstep with
 * the main tree. Without this, those fixed-position siblings stay
 * keyboard-reachable (Tab from the LoginScreen walks into their invisible
 * controls) even though the opaque z-20000 overlay wins hit-testing.
 *
 * A store rather than context because the consumers sit ABOVE / beside the
 * component that computes the value, and threading the gate's inputs up
 * would duplicate `setupCompleted` state.
 */
let currentOverlay: AuthGateOverlay | null = null;
const listeners = new Set<() => void>();

export function setAuthOverlay(surface: AuthGateOverlay | null): void {
  if (currentOverlay === surface) return;
  currentOverlay = surface;
  for (const l of [...listeners]) l();
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

function getSnapshot(): AuthGateOverlay | null {
  return currentOverlay;
}

/** The auth-overlay surface currently covering the app, or null. */
export function useAuthOverlay(): AuthGateOverlay | null {
  return useSyncExternalStore(subscribe, getSnapshot);
}
