import { useState, useCallback, useEffect, useMemo } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { instanceStorage } from "@/lib/instance-storage";
import { getPageBindings, subscribePageBindings } from "@/lib/pageBindings";

export interface TerminalPageConfig {
  id: string;
  name: string;
  createdAt: number;
}

const STORAGE_KEY = "qontinui-terminal-pages";
const ACTIVE_PAGE_KEY = "qontinui-terminal-active-page";

/**
 * A page-pinned pop-out window carries `?page=<id>` (set by the pop-out-page
 * boot hint in `terminal_windows.rs`). Such a window shows ONLY that one page,
 * fixes it as active, and never mutates the shared page list / active-page key —
 * it is a read-only detached view of one page. Returns null in the main window.
 */
function readPinnedPageId(): string | null {
  try {
    const v = new URLSearchParams(window.location.search).get("page");
    return v && v.trim() ? v : null;
  } catch {
    return null;
  }
}

function loadPages(): TerminalPageConfig[] {
  const pages = instanceStorage.getJSON<TerminalPageConfig[]>(STORAGE_KEY, []);
  if (pages.length === 0) {
    return [{ id: "default", name: "Terminal", createdAt: 0 }];
  }
  return pages;
}

/**
 * The page ids that actually EXIST in this window's persisted layout (same
 * source + empty-list semantics as `loadPages`, so a fresh install reports
 * `["default"]`). Restore uses this to detect ORPHAN records — sessions whose
 * `pageId` references no live page (e.g. the hook-confirm path's hardcoded
 * "default" on a layout that replaced the default page) — which would
 * otherwise never restore and be closed `no-terminal` by the orphan sweep.
 */
export function loadKnownPageIds(): string[] {
  return loadPages().map((p) => p.id);
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
/**
 * Normalized page_ids from a `terminal_list` `terminals` array.
 *
 * `TerminalInfo` is `#[serde(rename_all = "camelCase")]` (qontinui-schemas
 * `terminal.rs`), so the wire field is `pageId` — reading `page_id` here
 * silently lands EVERY terminal on `"default"` and the reconcile never
 * synthesizes a tab (the bug live-verification on a temp runner caught). The
 * `page_id` fallback is defensive only (legacy/round-trip forms). Exported so
 * the wire-field contract is unit-testable without Tauri.
 */
export function pageIdsFromTerminals(
  terminals: Array<{ pageId?: string; page_id?: string }>,
): string[] {
  return terminals.map((t) => t.pageId || t.page_id || "default");
}

/**
 * Normalized page_ids from a `terminal_session_list_open` `sessions` array
 * (durable restore records). `TerminalSessionRecord` is also camelCase, so the
 * wire field is `pageId` (matches `fetchOpenRecords` in
 * `useTerminalInitialization.ts`). Exported for the same contract test.
 */
export function pageIdsFromSessions(sessions: Array<{ pageId?: string }>): string[] {
  return sessions.map((s) => s.pageId ?? "default");
}

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
  // A page-pinned pop-out window is fixed to one page for its whole lifetime.
  const [pinnedPageId] = useState<string | null>(readPinnedPageId);
  const isPinned = pinnedPageId !== null;

  const [allPages, setPages] = useState<TerminalPageConfig[]>(loadPages);
  const [activePageId, setActivePageIdState] = useState<string>(() =>
    isPinned ? pinnedPageId! : instanceStorage.getItem(ACTIVE_PAGE_KEY) || "default",
  );

  // `pageId → windowLabel` for pages currently detached into a pop-out. Read
  // synchronously from the localStorage mirror so the FIRST render already
  // hides detached pages (and never picks one as active) — avoiding the
  // boot race where main would briefly restore a page the pop-out owns.
  // `WindowAssignmentsContext` keeps the mirror in sync with the Rust truth.
  const [boundPages, setBoundPages] = useState<Record<string, string>>(getPageBindings);
  useEffect(() => subscribePageBindings(() => setBoundPages(getPageBindings())), []);

  const setActivePageId = useCallback(
    (id: string) => {
      if (isPinned) return; // pinned window: active page is fixed
      setActivePageIdState(id);
      instanceStorage.setItem(ACTIVE_PAGE_KEY, id);
    },
    [isPinned],
  );

  // The pages this window should surface: a pinned pop-out shows ONLY its page;
  // the main window shows every page NOT detached into a pop-out (those live in
  // their own window). Memoized so the normalize-active effect below is stable.
  const pages = useMemo<TerminalPageConfig[]>(() => {
    if (isPinned) {
      const found = allPages.find((p) => p.id === pinnedPageId);
      return [found ?? { id: pinnedPageId!, name: "Terminal", createdAt: 0 }];
    }
    return allPages.filter((p) => !boundPages[p.id]);
  }, [isPinned, pinnedPageId, allPages, boundPages]);

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
        data?: { terminals: Array<{ id: string; pageId?: string; page_id?: string }> };
      }>("terminal_list");
      if (result.success && result.data) {
        // TerminalInfo is `#[serde(rename_all = "camelCase")]`, so the wire
        // field is `pageId` (the `page_id` fallback is defensive only).
        const pageTerminals = result.data.terminals.filter(
          (t) => (t.pageId || t.page_id || "default") === id,
        );
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

  /**
   * Open + activate a page by an EXPLICIT id (unlike {@link addPage}, which
   * mints a random uuid). Used by `/orchestrate` to navigate to a run's page
   * where `pageId === run_id`: workers dispatched by the conductor land on
   * that page (durably recorded with `page_id = run_id`), so activating it
   * shows the org-chart zone grid fill in. Idempotent — re-opening an
   * existing run page just re-activates it (never duplicates the tab).
   */
  const openPage = useCallback(
    (id: string, name: string) => {
      if (!id) return;
      setPages((prev) => {
        if (prev.some((p) => p.id === id)) return prev; // already present — just activate below
        const updated = [...prev, { id, name, createdAt: Date.now() }];
        savePages(updated);
        return updated;
      });
      setActivePageId(id);
    },
    [setActivePageId],
  );

  // `/orchestrate` (and any future explicit-page navigator) dispatches a
  // `terminal-open-page` window CustomEvent rather than prop-drilling a page
  // setter through App → providers → command ctx. Mirrors the existing
  // event-based navigation idiom (`ui-bridge-navigate`, `navigate-to-active`).
  useEffect(() => {
    const handler = (e: Event) => {
      const detail = (e as CustomEvent<{ pageId?: string; name?: string }>).detail;
      if (!detail?.pageId) return;
      openPage(detail.pageId, detail.name ?? `Run ${detail.pageId.slice(0, 8)}`);
    };
    window.addEventListener("terminal-open-page", handler as EventListener);
    return () => window.removeEventListener("terminal-open-page", handler as EventListener);
  }, [openPage]);

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
        data?: { terminals: Array<{ id: string; pageId?: string; page_id?: string }> };
      }>("terminal_list");
      if (result.success && result.data) {
        for (const id of pageIdsFromTerminals(result.data.terminals)) ids.add(id);
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
        for (const id of pageIdsFromSessions(sessions)) ids.add(id);
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
    // A pinned pop-out renders one fixed page and must not touch the shared
    // page list — skip reconciliation entirely.
    if (isPinned) return;

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
  }, [reconcile, isPinned]);

  // Keep the active page valid for the MAIN window: when the active page gets
  // detached into a pop-out (or otherwise disappears from the visible set),
  // fall back to the first visible page. If EVERY page is detached, mint a
  // fresh docked page so the main window always has something to show.
  useEffect(() => {
    if (isPinned) return;
    const visibleIds = pages.map((p) => p.id);
    if (visibleIds.length === 0) {
      // Intentional sync from an EXTERNAL system (page bindings written by other
      // windows): the active page was detached out from under us with no docked
      // page left, so mint one. Rare, idempotent (a fresh page is never bound),
      // and converges in one extra render — same exception the reconcile effect
      // above relies on.
      // eslint-disable-next-line react-hooks/set-state-in-effect
      addPage("Terminal");
      return;
    }
    if (!visibleIds.includes(activePageId)) {
      setActivePageId(visibleIds[0]);
    }
  }, [isPinned, pages, activePageId, addPage, setActivePageId]);

  return {
    pages,
    activePageId,
    setActivePageId,
    addPage,
    openPage,
    removePage,
    renamePage,
    /** True in a page-pinned pop-out window (shows one fixed page, minimal chrome). */
    isPinned,
  };
}
