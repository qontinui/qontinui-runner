/**
 * useSavedProjects
 *
 * React hook for reading/writing the persisted list of user-selected projects.
 *
 * Projects are persisted by the Rust backend. The Tauri commands consumed here
 * (`list_saved_projects`, `save_saved_projects`, `add_saved_project`,
 * `remove_saved_project`) are defined in
 * `src-tauri/src/commands/saved_projects.rs` (Phase 1 of the three-phase plan).
 *
 * The wire shape is camelCase JSON (see `SavedProject` below). This hook is the
 * canonical TypeScript location for the type — import it from here rather than
 * redeclaring it elsewhere.
 */

import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { createLogger } from "@/lib/logger";

const log = createLogger("useSavedProjects");

/**
 * Wire shape for a persisted project. Mirrors
 * `qontinui_types::projects::SavedProject` (qontinui-schemas
 * `rust/src/projects.rs`) serialized as camelCase.
 *
 * TODO(shared-types): this should be
 * `import type { SavedProject } from "@qontinui/shared-types"` — the Rust
 * struct is the source of truth and the TS binding is generated from it.
 * It cannot be today: this app consumes `@qontinui/shared-types` from **npm**
 * (`package.json` `^0.6.0`; `vite.config.ts:242` notes the deliberate move off
 * path aliases), and the newest published version is 0.8.1. The schemas repo's
 * `ts/src/generated/` tree — which `ts/src/index.ts` already imports from —
 * was deleted in `a95d51da` (2026-06-29), so `npm run build` fails there and
 * the `ts-v0.9.0` publish never landed. Restoring that tree and publishing the
 * next version is the unblock; swap this declaration for the import and bump
 * the dependency range in the same change.
 */
export interface SavedProject {
  /** Stable UUID key. Survives a path move. */
  id: string;
  path: string;
  name: string;
  projectType: string;
  manifest: string;
  description?: string;
  emoji?: string;
  color?: string;
  frontPageUrl?: string;
  processIds?: string[];
  /** A hint, not a handle — the page may not exist in this window. */
  terminalPageId?: string;
  zoneProfile?: string;
  repoSlug?: string;
  notes?: string;
  pinned?: boolean;
  lastOpenedMs?: number;
}

export interface UseSavedProjectsResult {
  projects: SavedProject[];
  loading: boolean;
  error: string | null;
  refresh: () => Promise<void>;
  addProject: (project: SavedProject) => Promise<void>;
  /** Keyed by `SavedProject.id` — a project's path can move, its id cannot. */
  removeProject: (id: string) => Promise<void>;
  saveAll: (projects: SavedProject[]) => Promise<void>;
}

/**
 * Guard that returns true when the Tauri invoke bridge is available at runtime.
 * During SSR / unit tests / plain-browser dev, `window.__TAURI_INTERNALS__` is
 * missing and `invoke` will reject. We detect this up front so the hook can
 * degrade to an empty list without spamming errors.
 */
function isTauriAvailable(): boolean {
  if (typeof window === "undefined") return false;
  // Tauri v2 exposes this internal object on the window when the runtime is present.
  return (
    typeof (window as unknown as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__ !==
      "undefined" || typeof (window as unknown as { __TAURI__?: unknown }).__TAURI__ !== "undefined"
  );
}

/**
 * Hook that wraps the saved-projects persistence store.
 *
 * - Calls `list_saved_projects` on mount and on every `refresh()`.
 * - `addProject` / `removeProject` / `saveAll` invoke the corresponding
 *   Rust commands, then call `refresh()` so the returned list stays in sync.
 * - If the Tauri runtime is unavailable, returns an empty list and logs a
 *   warning rather than throwing.
 */
export function useSavedProjects(): UseSavedProjectsResult {
  const [projects, setProjects] = useState<SavedProject[]>([]);
  // Start as `loading: true` so mount-time consumers don't misinterpret the
  // initial `projects = []` as "definitively empty" before the async
  // `list_saved_projects` call has had a chance to fire. The mount-effect
  // below calls `refresh()` which sets loading = false once the invoke
  // resolves (or earlier when Tauri isn't available).
  const [loading, setLoading] = useState<boolean>(true);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    if (!isTauriAvailable()) {
      log.warn("Tauri runtime not available; returning empty saved projects list");
      setProjects([]);
      setError(null);
      // Flip the mount-time `loading: true` default off so consumers aren't
      // stuck waiting forever in non-Tauri environments (SSR, unit tests,
      // plain-browser dev).
      setLoading(false);
      return;
    }
    setLoading(true);
    setError(null);
    try {
      const result = await invoke<SavedProject[]>("list_saved_projects");
      setProjects(Array.isArray(result) ? result : []);
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      log.error("Failed to list saved projects", msg);
      setError(msg);
      setProjects([]);
    } finally {
      setLoading(false);
    }
  }, []);

  const addProject = useCallback(
    async (project: SavedProject) => {
      if (!isTauriAvailable()) {
        log.warn("Tauri runtime not available; cannot add saved project");
        return;
      }
      try {
        await invoke<void>("add_saved_project", { project });
      } catch (err) {
        const msg = err instanceof Error ? err.message : String(err);
        log.error("Failed to add saved project", msg);
        setError(msg);
        throw err;
      }
      await refresh();
    },
    [refresh],
  );

  const removeProject = useCallback(
    async (id: string) => {
      if (!isTauriAvailable()) {
        log.warn("Tauri runtime not available; cannot remove saved project");
        return;
      }
      try {
        await invoke<void>("remove_saved_project", { id });
      } catch (err) {
        const msg = err instanceof Error ? err.message : String(err);
        log.error("Failed to remove saved project", msg);
        setError(msg);
        throw err;
      }
      await refresh();
    },
    [refresh],
  );

  const saveAll = useCallback(
    async (next: SavedProject[]) => {
      if (!isTauriAvailable()) {
        log.warn("Tauri runtime not available; cannot save projects");
        return;
      }
      try {
        await invoke<void>("save_saved_projects", { projects: next });
      } catch (err) {
        const msg = err instanceof Error ? err.message : String(err);
        log.error("Failed to save saved projects", msg);
        setError(msg);
        throw err;
      }
      await refresh();
    },
    [refresh],
  );

  useEffect(() => {
    // eslint-disable-next-line react-hooks/set-state-in-effect -- initial load of persisted projects on mount
    void refresh();
  }, [refresh]);

  return {
    projects,
    loading,
    error,
    refresh,
    addProject,
    removeProject,
    saveAll,
  };
}
