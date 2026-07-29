import { useState, useCallback, useEffect, useMemo, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { instanceStorage } from "@/lib/instance-storage";
import { getPageBindings, subscribePageBindings } from "@/lib/pageBindings";

export interface TerminalPageConfig {
  id: string;
  name: string;
  createdAt: number;
  /**
   * Default cwd for terminals spawned on this page (projects-dashboard plan
   * §7.2 step 2). When a project is activated its root is written here, and
   * `useTerminalManager.createTerminal` falls back to it whenever the caller
   * passes no explicit `workingDir` — so EVERY future terminal on the page
   * opens at the project root, not just the ones spawned at activation time.
   *
   * The fallback is applied frontend-side, BEFORE the value reaches the Rust
   * `terminal_create` command: that command derives `intent_repo` from
   * `working_dir` (`commands/terminal.rs:97-111`) and may then reassign
   * `working_dir` to a freshly-allocated isolated worktree
   * (`:122-128`). Passing `undefined` and letting Rust guess is therefore NOT
   * equivalent — the page default has to arrive AS the `working_dir` argument.
   *
   * Optional: pages persisted before this field existed load unchanged (it is
   * simply absent, and `resolveSpawnWorkingDir` then behaves exactly as
   * before).
   */
  defaultWorkingDir?: string;
}

/**
 * `instanceStorage` key holding the persisted `TerminalPageConfig[]`.
 * Exported so the persistence round-trip is testable against the real key
 * rather than a copy of it.
 */
export const PAGES_STORAGE_KEY = "qontinui-terminal-pages";
const STORAGE_KEY = PAGES_STORAGE_KEY;
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

/**
 * The persisted page list, with the implicit "default" page injected when
 * nothing has been persisted yet.
 *
 * Exported so callers that need the page roster OUTSIDE React (and the
 * persistence round-trip tests) can read the same source the hook does. The
 * value round-trips through `JSON.stringify`/`parse` in `instanceStorage`, so
 * every own enumerable field on `TerminalPageConfig` — including the optional
 * `defaultWorkingDir` — survives save/load unchanged, and a page persisted
 * WITHOUT the field loads as a plain `TerminalPageConfig` with it absent.
 */
export function loadPages(): TerminalPageConfig[] {
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
 * tab. This INCLUDES "default": `loadPages` only injects the default page when
 * NO pages are persisted, so once the operator has created their own pages the
 * default page is absent from the persisted list — a terminal parked on
 * "default" (e.g. any created via `POST /terminals` with no `pageId`) would
 * then render nowhere. Synthesizing it here (named "Terminal", its canonical
 * name) is what keeps such a terminal reachable instead of orphaned.
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

/**
 * The pages to RENDER as tabs in the tab strip. Identical to `pages` except the
 * INTERNAL "default" page is hidden while it hosts zero live/restorable sessions.
 *
 * Commit 681e1112f renders "default" as a tab so API-created terminals (a
 * `POST /terminals` with no `pageId` lands on "default") have a reachable home;
 * but on a normal boot with nothing parked there that surfaced an empty,
 * confusing "Terminal" tab (the 2026-07-19 mass-strand boot). We keep
 * 681e1112f's benefit WITHOUT the empty-boot tab: `occupiedPageIds` is the set
 * of page ids that currently host at least one live terminal OR restorable
 * session (built by `reconcile` from the SAME `terminal_list` +
 * `terminal_session_list_open` sources that synthesize the tab), so the moment a
 * terminal or restorable session lands on "default" it re-enters that set and
 * the tab reappears instantly.
 *
 * Only "default" is special-cased. A USER-created page always shows its tab even
 * when empty — the operator made it on purpose, so predictability wins over
 * auto-hiding.
 *
 * Never returns an empty list: if hiding the empty default would leave no tabs
 * at all (default is the only page and nothing is parked on it — e.g. the very
 * first boot), we show it anyway. A single empty default tab is strictly better
 * than a blank tab strip with no active/visible tab.
 *
 * Pure + exported so the derivation is unit-testable without React/Tauri (same
 * precedent as `reconcilePages`).
 */
export function computeVisiblePages(
  pages: TerminalPageConfig[],
  occupiedPageIds: ReadonlySet<string>,
): TerminalPageConfig[] {
  const visible = pages.filter((p) => p.id !== "default" || occupiedPageIds.has("default"));
  return visible.length > 0 ? visible : pages;
}

/** Trim + treat blank as absent. Shared by the page-default helpers. */
function cleanOptional(v: string | null | undefined): string | undefined {
  const t = v?.trim();
  return t ? t : undefined;
}

/**
 * Pure: set (or, with `undefined`/blank, clear) a page's `defaultWorkingDir`.
 *
 * Returns the SAME array reference when nothing changed, so the caller can
 * skip a redundant persist / re-render — same contract as `reconcilePages`.
 * Clearing DELETES the key rather than storing `undefined`, so a cleared page
 * serializes byte-identically to one that never had the field.
 *
 * Exported so the mutation is unit-testable without React or localStorage.
 */
export function applyPageDefaultWorkingDir(
  pages: TerminalPageConfig[],
  pageId: string,
  dir: string | null | undefined,
): TerminalPageConfig[] {
  const next = cleanOptional(dir);
  const idx = pages.findIndex((p) => p.id === pageId);
  if (idx < 0) return pages;
  if (pages[idx].defaultWorkingDir === next) return pages;
  const updated = pages.slice();
  if (next === undefined) {
    const cleared = { ...pages[idx] };
    delete cleared.defaultWorkingDir;
    updated[idx] = cleared;
  } else {
    updated[idx] = { ...pages[idx], defaultWorkingDir: next };
  }
  return updated;
}

/** Outcome of {@link resolveProjectPage}. */
export interface ProjectPageResolution {
  /** The page id the project is bound to AFTER resolution. */
  pageId: string;
  /** The page list to persist (SAME reference as the input when unchanged). */
  pages: TerminalPageConfig[];
  /**
   * True when the page did not exist and was created. The caller must persist
   * `pageId` back onto the project record — a `created` resolution means the
   * stored binding was absent or DANGLING.
   */
  created: boolean;
}

/**
 * Pure: resolve a project's bound terminal page, tolerating a DANGLING id.
 *
 * Project → page binding is stored as `terminal_page_id` in Rust
 * `settings.json`, while the pages themselves live in `instanceStorage`
 * (localStorage, port-namespaced). Rust cannot read localStorage, so a stored
 * id can perfectly legitimately name a page that does not exist in THIS
 * window — a cleared WebView2 profile, a different runner instance, a page the
 * operator deleted. Per plan §4 and §12 that is NORMAL, not exceptional:
 *
 *   - a bound id that EXISTS resolves to it (`created: false`);
 *   - a bound id that does NOT exist is re-created UNDER THE SAME ID, named
 *     after the project (`created: true`). Reusing the id rather than minting a
 *     fresh one keeps the binding stable across windows: two windows that both
 *     start dangling converge on one page id instead of forking two;
 *   - no bound id at all mints a fresh uuid (`created: true`).
 *
 * It never throws and never returns "not found" — an activation must never
 * fail on a dangling page id.
 *
 * A blank/absent `defaultWorkingDir` means "leave the page's default alone",
 * NOT "clear it" — resolving a project page must never silently unpin an
 * existing page's cwd. Clearing is the explicit job of
 * {@link applyPageDefaultWorkingDir} (via the hook's `setPageDefaultWorkingDir`).
 *
 * `mintId` is injectable so the mint path is deterministic under test.
 */
export function resolveProjectPage(
  pages: TerminalPageConfig[],
  boundPageId: string | null | undefined,
  projectName: string,
  defaultWorkingDir?: string | null,
  mintId: () => string = () => crypto.randomUUID(),
): ProjectPageResolution {
  const bound = cleanOptional(boundPageId);
  const dir = cleanOptional(defaultWorkingDir);
  const existing = bound ? pages.find((p) => p.id === bound) : undefined;

  if (existing) {
    return {
      pageId: existing.id,
      pages: dir ? applyPageDefaultWorkingDir(pages, existing.id, dir) : pages,
      created: false,
    };
  }

  const id = bound ?? mintId();
  const page: TerminalPageConfig = {
    id,
    name: cleanOptional(projectName) ?? "Project",
    createdAt: Date.now(),
  };
  if (dir) page.defaultWorkingDir = dir;
  return { pageId: id, pages: [...pages, page], created: true };
}

export function reconcilePages(
  persisted: TerminalPageConfig[],
  backendPageIds: Iterable<string>,
): TerminalPageConfig[] {
  const known = new Set(persisted.map((p) => p.id));
  const missing: string[] = [];
  for (const rawId of backendPageIds) {
    const id = rawId || "default";
    if (known.has(id)) continue;
    known.add(id); // dedupe across both backend sources
    missing.push(id);
  }
  if (missing.length === 0) return persisted;

  let nextN = persisted.filter((p) => p.id !== "default").length + 1;
  const now = Date.now();
  const synthesized = missing.map((id) => ({
    id,
    // "default" keeps its canonical name (matches `loadPages`); any other
    // minted id gets a sequential "Page N".
    name: id === "default" ? "Terminal" : `Page ${nextN++}`,
    createdAt: now,
  }));
  return [...persisted, ...synthesized];
}

export function useTerminalPages() {
  // A page-pinned pop-out window is fixed to one page for its whole lifetime.
  const [pinnedPageId] = useState<string | null>(readPinnedPageId);
  const isPinned = pinnedPageId !== null;

  const [allPages, setPages] = useState<TerminalPageConfig[]>(loadPages);
  // Latest page roster, readable SYNCHRONOUSLY by `ensureProjectPage` (which
  // must return the resolved page id to its caller immediately, before the
  // `setPages` updater has run). Kept in sync from an effect and, eagerly,
  // from the updaters that write it.
  const allPagesRef = useRef<TerminalPageConfig[]>(allPages);
  useEffect(() => {
    allPagesRef.current = allPages;
  }, [allPages]);
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

  // Page ids that currently host at least one live terminal OR restorable
  // session — the "occupied" signal that decides whether the internal "default"
  // page earns a visible tab (see `computeVisiblePages`). Populated by
  // `reconcile` from the exact same backend sources that synthesize tabs, so a
  // terminal landing on "default" both mints its tab AND lights it as occupied
  // in one pass. Empty until the first reconcile resolves; a truly-empty default
  // is hidden (or shown solo via the never-empty fallback), which is the desired
  // boot behavior.
  const [occupiedPageIds, setOccupiedPageIds] = useState<ReadonlySet<string>>(
    () => new Set<string>(),
  );

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

  // The subset of `pages` rendered as TABS: same as `pages` but the internal
  // "default" page is hidden while it hosts zero live/restorable sessions (keeps
  // 681e1112f's API-terminal home without the empty-boot "Terminal" tab). Drives
  // the tab strip AND the active-page fallback below, so the active page is
  // always one with a visible tab. See `computeVisiblePages`.
  const visiblePages = useMemo<TerminalPageConfig[]>(
    () => computeVisiblePages(pages, occupiedPageIds),
    [pages, occupiedPageIds],
  );

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
   * Set (or clear, with `undefined`) the cwd every terminal spawned on `id`
   * defaults to. See `TerminalPageConfig.defaultWorkingDir`.
   */
  const setPageDefaultWorkingDir = useCallback((id: string, dir: string | null | undefined) => {
    setPages((prev) => {
      const updated = applyPageDefaultWorkingDir(prev, id, dir);
      if (updated === prev) return prev; // no change — no persist, no re-render
      savePages(updated);
      allPagesRef.current = updated;
      return updated;
    });
  }, []);

  /**
   * Resolve (creating if needed) the terminal page a project is bound to, set
   * it as the page default cwd target, and activate it.
   *
   * The returned `pageId` is authoritative: when `created` is true the caller
   * MUST persist it back onto the project's `terminal_page_id`, because the
   * stored binding was absent or dangling (see {@link resolveProjectPage} —
   * pages live in `instanceStorage`, the binding lives in Rust
   * `settings.json`, so dangling ids are normal). Never throws; a dangling id
   * can never fail an activation.
   */
  const ensureProjectPage = useCallback(
    (opts: {
      /** The project's stored `terminal_page_id`, if any. */
      boundPageId?: string | null;
      /** Used as the page name when a page has to be created. */
      projectName: string;
      /** The project root; becomes the page's `defaultWorkingDir`. */
      defaultWorkingDir?: string | null;
    }): { pageId: string; created: boolean } => {
      const seed = resolveProjectPage(
        allPagesRef.current,
        opts.boundPageId,
        opts.projectName,
        opts.defaultWorkingDir,
      );
      setPages((prev) => {
        // Re-resolve against the authoritative `prev` (the ref can lag by a
        // tick), pinning the id already reported to the caller so the two
        // resolutions cannot diverge: with `seed.pageId` as the bound id this
        // is idempotent — present → no-op, absent → created under that id.
        const { pages: updated } = resolveProjectPage(
          prev,
          seed.pageId,
          opts.projectName,
          opts.defaultWorkingDir,
        );
        if (updated === prev) return prev;
        savePages(updated);
        allPagesRef.current = updated;
        return updated;
      });
      setActivePageId(seed.pageId);
      return { pageId: seed.pageId, created: seed.created };
    },
    [setActivePageId],
  );

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

    // Record occupancy from the SAME unioned id set: `ids` is exactly the page
    // ids that currently host ≥1 live terminal (terminal_list) or restorable
    // session (terminal_session_list_open). This is what `computeVisiblePages`
    // reads to decide whether the "default" tab is earned. A fresh Set so the
    // memo sees a new reference and recomputes.
    setOccupiedPageIds(new Set(ids));

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
  // detached into a pop-out (or is the empty "default" page we now hide), fall
  // back to the first visible page so the operator is never stranded on a page
  // with no tab. If EVERY page is detached, mint a fresh docked page so the main
  // window always has something to show. Derived from `visiblePages` (not
  // `pages`) so the hidden-empty-default case triggers the fallback too.
  useEffect(() => {
    if (isPinned) return;
    const visibleIds = visiblePages.map((p) => p.id);
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
  }, [isPinned, visiblePages, activePageId, addPage, setActivePageId]);

  return {
    pages,
    /**
     * The pages to render as TABS — `pages` minus the internal "default" page
     * when it hosts no live/restorable session (see `computeVisiblePages`).
     * Consumers that HOST sessions (session provider, reorganize) use `pages`
     * (the full set, so a hidden-but-live default still hosts its terminals);
     * only the tab strip renders `visiblePages`.
     */
    visiblePages,
    activePageId,
    setActivePageId,
    addPage,
    openPage,
    removePage,
    renamePage,
    setPageDefaultWorkingDir,
    ensureProjectPage,
    /** True in a page-pinned pop-out window (shows one fixed page, minimal chrome). */
    isPinned,
  };
}
