/**
 * The terminal surface's read-only view of "which project is active".
 *
 * The Projects dashboard owns project state (Rust `settings.json` +
 * `ProjectContext`, projects-dashboard plan §4/§10). Rather than couple the
 * terminal tree to the projects tree, activation is published as a
 * `project-activated` window `CustomEvent` (the event name the plan's §10
 * already assigns to `ProjectContext`), exactly like the existing
 * `terminal-open-page` / `navigate-to-active` idioms.
 *
 * The hint carries the four fields §7.2 needs and nothing else: the terminal
 * side cannot read the project registry — it has no Tauri call for it and no
 * business owning one — so this event IS the transport. `path` answers "are
 * any of these terminals in a different folder?" (step 4, `ProjectFolderChip`);
 * `id` + `terminalPageId` + `zoneProfile` drive the page binding, its
 * `defaultWorkingDir`, and the profile restore (steps 1–3,
 * `useProjectPageActivation`).
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
  /** The project root on disk — the only field the CHIP needs. */
  path?: string;
  /**
   * `SavedProject.terminalPageId` — the page this project last resolved onto.
   *
   * A hint about a hint: the binding is persisted in Rust `settings.json`
   * while the pages live in `instanceStorage`, so this id can name a page that
   * does not exist in this window. `resolveProjectPage` tolerates that by
   * re-creating the page under the same id; it is never an error. Absent means
   * "no page bound yet".
   */
  terminalPageId?: string;
  /**
   * `SavedProject.zoneProfile` — the zone profile to reinstate on activation
   * (§7.2 step 3). A NAME, resolved against the target page's saved profiles;
   * a profile the operator has since deleted degrades to "nothing to restore".
   */
  zoneProfile?: string;
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
