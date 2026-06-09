import { useState, useCallback, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { instanceStorage } from "@/lib/instance-storage";

export interface TerminalPageConfig {
  id: string;
  name: string;
  createdAt: number;
}

const STORAGE_KEY = "qontinui-terminal-pages";
const ACTIVE_PAGE_KEY = "qontinui-terminal-active-page";

function loadPages(): TerminalPageConfig[] {
  const pages = instanceStorage.getJSON<TerminalPageConfig[]>(STORAGE_KEY, []);
  if (pages.length === 0) {
    return [{ id: "default", name: "Terminal", createdAt: 0 }];
  }
  return pages;
}

function savePages(pages: TerminalPageConfig[]) {
  instanceStorage.setJSON(STORAGE_KEY, pages);
}

/**
 * Pure reconciliation: union the persisted page tabs with the distinct backend
 * page_ids, synthesizing a tab for any backend id NOT already persisted so a
 * minted page (and continuations restored onto it) get a visible, selectable
 * tab. "default" is always present already (loadPages) — never synthesized.
 * Operator-created pages and their names are left intact (only ADD missing ids;
 * never rename/remove existing). Synthesized names are sequential ("Page N")
 * based on the count of existing non-default pages, so they're deterministic
 * and survive a reload once persisted.
 *
 * Returns the SAME array reference when nothing was added, so callers can skip
 * a redundant state update / persist (keeps the effect idempotent under React
 * strict-mode double-invoke).
 *
 * Exported so the union/synthesis logic is unit-testable without booting React
 * or Tauri (same precedent as `fetchOpenRecords` / `shouldIngestCreatedTerminal`).
 */
export function reconcilePages(
  persisted: TerminalPageConfig[],
  backendPageIds: Iterable<string>,
): TerminalPageConfig[] {
  const known = new Set(persisted.map((p) => p.id));
  const missing: string[] = [];
  for (const rawId of backendPageIds) {
    const id = rawId || "default";
    if (id === "default") continue;
    if (known.has(id)) continue;
    known.add(id); // dedupe across both backend sources
    missing.push(id);
  }
  if (missing.length === 0) return persisted;

  let nextN = persisted.filter((p) => p.id !== "default").length + 1;
  const now = Date.now();
  const synthesized = missing.map((id) => ({
    id,
    name: `Page ${nextN++}`,
    createdAt: now,
  }));
  return [...persisted, ...synthesized];
}

export function useTerminalPages() {
  const [pages, setPages] = useState<TerminalPageConfig[]>(loadPages);
  const [activePageId, setActivePageIdState] = useState<string>(
    () => instanceStorage.getItem(ACTIVE_PAGE_KEY) || "default",
  );

  const setActivePageId = useCallback((id: string) => {
    setActivePageIdState(id);
    instanceStorage.setItem(ACTIVE_PAGE_KEY, id);
  }, []);

  const addPage = useCallback(
    (name: string) => {
      const id = crypto.randomUUID();
      const newPage: TerminalPageConfig = { id, name, createdAt: Date.now() };
      setPages((prev) => {
        const updated = [...prev, newPage];
        savePages(updated);
        return updated;
      });
      setActivePageId(id);
      return id;
    },
    [setActivePageId],
  );

  const removePage = useCallback(async (id: string) => {
    // Close all PTYs belonging to this page
    try {
      const result = await invoke<{
        success: boolean;
        data?: { terminals: Array<{ id: string; page_id: string }> };
      }>("terminal_list");
      if (result.success && result.data) {
        const pageTerminals = result.data.terminals.filter((t) => (t.page_id || "default") === id);
        for (const t of pageTerminals) {
          invoke("terminal_close", { terminalId: t.id }).catch(() => {});
        }
      }
    } catch {
      // Best effort
    }

    // Clean up namespaced instanceStorage keys
    if (id !== "default") {
      const prefix = `page:${id}:`;
      instanceStorage.removeByPrefix(prefix);
    }

    setPages((prev) => {
      if (prev.length <= 1) return prev; // Can't remove last page
      const updated = prev.filter((p) => p.id !== id);
      savePages(updated);
      return updated;
    });

    // Switch to another page if active was deleted
    setActivePageIdState((currentActive) => {
      if (currentActive === id) {
        // We need to find the first page that isn't the removed one
        // Use loadPages() to get the latest from storage after setPages runs
        const fallback = "default";
        instanceStorage.setItem(ACTIVE_PAGE_KEY, fallback);
        return fallback;
      }
      return currentActive;
    });
  }, []);

  const renamePage = useCallback((id: string, name: string) => {
    setPages((prev) => {
      const updated = prev.map((p) => (p.id === id ? { ...p, name } : p));
      savePages(updated);
      return updated;
    });
  }, []);

  // Reconcile backend-known page_ids into the tab list. A backend-spawned gate
  // continuation can land on a freshly-minted page_id that isn't in the
  // persisted tab list, so without this it would render nowhere. We union the
  // distinct ids from BOTH backend sources and synthesize a tab for any id not
  // already persisted (see `reconcilePages`). Best-effort: a failed invoke must
  // not throw out of the effect.
  const reconcile = useCallback(async () => {
    const ids = new Set<string>();

    // Source 1: live terminals (terminal_list). Empty on a cold restart.
    try {
      const result = await invoke<{
        success: boolean;
        data?: { terminals: Array<{ id: string; page_id: string }> };
      }>("terminal_list");
      if (result.success && result.data) {
        for (const t of result.data.terminals) {
          ids.add(t.page_id || "default");
        }
      }
    } catch {
      // Best effort
    }

    // Source 2: durable restore records (terminal_session_list_open). This is
    // the cold-restart source — a minted page holding only not-yet-restored
    // continuations gets no tab from terminal_list alone, and fetchOpenRecords
    // would then drop those records. Unioning the durable pageIds rebuilds the
    // tab from durable state.
    try {
      const resp = await invoke<{
        data?: { sessions?: Array<{ pageId?: string }> };
      }>("terminal_session_list_open");
      const sessions = resp?.data?.sessions;
      if (Array.isArray(sessions)) {
        for (const rec of sessions) {
          ids.add(rec.pageId ?? "default");
        }
      }
    } catch {
      // Best effort
    }

    setPages((prev) => {
      const next = reconcilePages(prev, ids);
      if (next === prev) return prev; // nothing missing — no persist / no re-render
      savePages(next);
      return next;
    });
  }, []);

  // Run reconciliation once on mount and on a debounced `terminal-created`
  // event (a burst of continuations collapses to a single reconcile).
  useEffect(() => {
    let unlisten: (() => void) | null = null;
    let timer: ReturnType<typeof setTimeout> | null = null;
    let disposed = false;

    // `reconcile` is async: its `setPages` only runs AFTER both awaited invokes
    // resolve (a future microtask), never synchronously during this effect's
    // render pass — so the cascading-render concern the rule guards against does
    // not apply here.
    // eslint-disable-next-line react-hooks/set-state-in-effect
    void reconcile();

    listen("terminal-created", () => {
      if (timer) clearTimeout(timer);
      timer = setTimeout(() => {
        timer = null;
        void reconcile();
      }, 400);
    }).then((fn) => {
      if (disposed) {
        fn();
      } else {
        unlisten = fn;
      }
    });

    return () => {
      disposed = true;
      if (timer) clearTimeout(timer);
      unlisten?.();
    };
  }, [reconcile]);

  return {
    pages,
    activePageId,
    setActivePageId,
    addPage,
    removePage,
    renamePage,
  };
}
