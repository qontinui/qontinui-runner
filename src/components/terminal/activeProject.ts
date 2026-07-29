/**
 * The terminal surface's read-only view of "which project is active".
 *
 * The Projects dashboard owns project state (Rust `settings.json` +
 * `ProjectContext`, projects-dashboard plan §4/§10). The terminal only needs
 * one fact from it — the active project's ROOT PATH — to answer "are any of
 * these terminals in a different folder?" (§7.2 step 4). Rather than couple
 * the terminal tree to the projects tree, activation is published as a
 * `project-activated` window `CustomEvent` (the event name the plan's §10
 * already assigns to `ProjectContext`), exactly like the existing
 * `terminal-open-page` / `navigate-to-active` idioms.
 *
 * The last hint is mirrored into `instanceStorage` so the answer survives a
 * webview reload and is available on the FIRST render (no flash of a stale
 * chip). Storage is a cache of the event, never the source of truth — the Rust
 * `active_project_id` remains authoritative.
 */

import { useEffect, useState } from "react";

import { instanceStorage } from "@/lib/instance-storage";

/** Window event dispatched when a project is activated (or deactivated). */
export const PROJECT_ACTIVATED_EVENT = "project-activated";

const STORAGE_KEY = "qontinui-active-project";

export interface ActiveProjectHint {
  /** `SavedProject.id`, when known. */
  id?: string;
  /** Display name, used to label a page / chip. */
  name?: string;
  /** The project root on disk — the only field the terminal surface needs. */
  path?: string;
}

/** The cached hint, or `null` when no project has been activated. */
export function getActiveProjectHint(): ActiveProjectHint | null {
  const hint = instanceStorage.getJSON<ActiveProjectHint | null>(STORAGE_KEY, null);
  return hint && typeof hint === "object" ? hint : null;
}

/**
 * Publish an activation. Called by the Projects surface; also usable from the
 * UI Bridge / automation to point the terminal at a folder.
 */
export function setActiveProjectHint(hint: ActiveProjectHint | null): void {
  instanceStorage.setJSON(STORAGE_KEY, hint);
  window.dispatchEvent(new CustomEvent(PROJECT_ACTIVATED_EVENT, { detail: hint }));
}

/**
 * Subscribe to activations. Handles both the in-window `CustomEvent` and the
 * cross-window `storage` event (a pop-out terminal window should follow the
 * main window's project).
 */
export function subscribeActiveProject(cb: (hint: ActiveProjectHint | null) => void): () => void {
  const onActivated = (e: Event) => {
    const detail = (e as CustomEvent<ActiveProjectHint | null>).detail ?? null;
    // Cache whatever the publisher sent so a reload starts from it even when
    // the publisher used a raw `dispatchEvent` instead of `setActiveProjectHint`.
    instanceStorage.setJSON(STORAGE_KEY, detail);
    cb(detail);
  };
  const onStorage = () => cb(getActiveProjectHint());
  window.addEventListener(PROJECT_ACTIVATED_EVENT, onActivated as EventListener);
  window.addEventListener("storage", onStorage);
  return () => {
    window.removeEventListener(PROJECT_ACTIVATED_EVENT, onActivated as EventListener);
    window.removeEventListener("storage", onStorage);
  };
}

/** React binding over {@link subscribeActiveProject}. */
export function useActiveProjectHint(): ActiveProjectHint | null {
  const [hint, setHint] = useState<ActiveProjectHint | null>(getActiveProjectHint);
  useEffect(() => subscribeActiveProject(setHint), []);
  return hint;
}
