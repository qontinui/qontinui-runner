/**
 * `/regroup <per-tab>` — the executor. The packing itself is pure
 * (`regroupTabs.ts`); this hook classifies every window, then carries the plan
 * out across pages:
 *
 *  1. close the plain terminals whose screen is empty;
 *  2. create the pages the plan needs beyond the existing ones;
 *  3. MOVE every window that changes page with `terminal_set_page` — a live
 *     move that keeps the PTY (and the AI session in it) running, never the
 *     close-and-recreate the AI "Reorganize pages" dialog does;
 *  4. wait until every page holds exactly what the plan gave it, then compact
 *     each page's grid in plan order (`useZoneLayout.requestCompaction`);
 *  5. remove the pages left empty — but only after the runner itself lists no
 *     terminal on them, because `removePage` closes every PTY it still finds
 *     there, and a move that failed must not turn into a killed session.
 */

import { invoke } from "@tauri-apps/api/core";

import { useAllTerminalSessions, type TerminalSessionContextValue } from "./contexts";
import { hasAiSession, orderTabsByZone } from "./pruneTerminals";
import { planRegroup, screenHasContent, type RegroupItem, type RegroupKind } from "./regroupTabs";
import type { CommandResponse } from "./types";
import type { TerminalTab } from "./useTerminalManager";
import { pageIdsFromTerminals } from "./useTerminalPages";

/** How long step 4 waits for moved tabs to land before compacting anyway. */
const SETTLE_TIMEOUT_MS = 8_000;
const SETTLE_POLL_MS = 100;
/** Extra wait after a new page mounts, for its move listener to register. */
const LISTENER_GRACE_MS = 250;

export interface RegroupReport {
  /** Empty plain terminals closed. */
  closed: number;
  /** Windows moved to another page. */
  moved: number;
  /** Moves the runner refused, by title. Those windows stay where they were. */
  failedMoves: string[];
  /** Pages created / removed. */
  pagesCreated: number;
  pagesRemoved: number;
  /** Pages that should have been removed but still hold a terminal. */
  pagesKept: number;
  /** Resulting tab count. */
  tabs: number;
  /** False when step 4 timed out before every moved tab landed. */
  settled: boolean;
}

function isMovable(tab: TerminalTab): boolean {
  // No local PTY to move (worker view, remote mirror), not a PTY at all (plan
  // viewer), or an exited tombstone the runner may no longer hold. See
  // `regroupTabs.ts`.
  return (
    tab.isAlive && !tab.sessionBacked && !tab.taskRunId && tab.remote == null && tab.type !== "plan"
  );
}

/**
 * Classify one window. A terminal whose screen cannot be read is reported as
 * having content: the only thing an unreadable screen can never justify is
 * closing the terminal.
 */
async function classify(
  tab: TerminalTab,
  value: TerminalSessionContextValue,
  now: number,
): Promise<RegroupKind> {
  if (hasAiSession(tab, { sessionStates: value.sessionStates, now })) return "ai";
  if (!isMovable(tab)) return "content";
  try {
    const res = await invoke<CommandResponse>("terminal_grid_text", { terminalId: tab.id });
    const lines = (res?.data as { lines?: unknown } | null)?.lines;
    if (!res?.success || !Array.isArray(lines)) return "content";
    return screenHasContent(lines.map(String)) ? "content" : "empty";
  } catch {
    return "content";
  }
}

const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

/**
 * Returns the regroup executor, or `null` when this window was given no page
 * operations. Deliberately not memoized: its one caller is a
 * `useCommandAction` handler, which already keeps the latest closure in a ref,
 * and the command test kits call `useTerminalCommands` outside a React render.
 */
export function useRegroupTabs(): ((perTab: number) => Promise<RegroupReport>) | null {
  const { getPages, pageOps } = useAllTerminalSessions();
  // No roster to act on (a page-pinned pop-out, where App passes no page ops;
  // a test kit): no executor, so the command reports it cannot run instead of
  // throwing.
  if (!pageOps) return null;

  return async (perTab: number): Promise<RegroupReport> => {
    const values = getPages();
    const now = Date.now();

    // ── classify, page by page in tab-strip order ────────────────────────
    const sources = [];
    const titleOf = new Map<string, string>();
    const pageOf = new Map<string, string>();
    for (const page of pageOps.visiblePages) {
      const value = values[page.id];
      const tabs = value ? orderTabsByZone(value.tabs, value.zoneLayout.assignments) : [];
      const items: RegroupItem[] = [];
      for (const tab of tabs) {
        titleOf.set(tab.id, tab.title);
        pageOf.set(tab.id, page.id);
        items.push({
          id: tab.id,
          title: tab.title,
          kind: value ? await classify(tab, value, now) : "content",
          movable: isMovable(tab),
        });
      }
      sources.push({ pageId: page.id, items });
    }
    const plan = planRegroup(sources, perTab);

    // ── 1. close empty terminals ─────────────────────────────────────────
    for (const { pageId, id } of plan.close) values[pageId]?.closeTerminal(id);

    // ── 2. create pages ──────────────────────────────────────────────────
    let pagesCreated = 0;
    const targetPageIds = plan.targets.map((target, i) => {
      if (target.pageId) return target.pageId;
      pagesCreated++;
      return pageOps.addPage(`Page ${i + 1}`);
    });
    // A new page adopts a moved terminal through its `terminal-page-changed`
    // listener, which only exists once the page's scope has mounted and
    // registered it. Moving before that drops the event: the source page
    // evicts the tab and nothing adopts it. Wait for every new scope, then one
    // more beat for its listener registration (itself an IPC call).
    if (pagesCreated > 0) {
      const mounted = () => targetPageIds.every((id) => getPages()[id]);
      const mountDeadline = Date.now() + SETTLE_TIMEOUT_MS;
      while (!mounted() && Date.now() < mountDeadline) await sleep(SETTLE_POLL_MS);
      await sleep(LISTENER_GRACE_MS);
    }

    // ── 3. move ──────────────────────────────────────────────────────────
    let moved = 0;
    const failedMoves: string[] = [];
    const failedIds = new Set<string>();
    for (const [i, target] of plan.targets.entries()) {
      for (const id of target.tabIds) {
        if (pageOf.get(id) === targetPageIds[i]) continue;
        try {
          await invoke<CommandResponse>("terminal_set_page", {
            terminalId: id,
            pageId: targetPageIds[i],
          });
          moved++;
        } catch {
          failedMoves.push(titleOf.get(id) ?? id);
          failedIds.add(id);
        }
      }
    }

    // ── 4. wait for the moves to land, then compact each page ────────────
    // A failed move stays on its source page, so it is expected THERE.
    const expected = plan.targets.map((target, i) => {
      const ids = target.tabIds.filter((id) => !failedIds.has(id));
      for (const id of failedIds) {
        if (pageOf.get(id) === targetPageIds[i]) ids.push(id);
      }
      return ids;
    });
    const landed = () => {
      const current = getPages();
      return targetPageIds.every((pageId, i) => {
        const tabs = current[pageId]?.tabs;
        if (!tabs) return false;
        const held = new Set(tabs.map((t) => t.id));
        return expected[i].every((id) => held.has(id));
      });
    };
    const deadline = Date.now() + SETTLE_TIMEOUT_MS;
    while (!landed() && Date.now() < deadline) await sleep(SETTLE_POLL_MS);
    const settled = landed();
    const closedIds = plan.close.map((c) => c.id);
    const after = getPages();
    targetPageIds.forEach((pageId, i) => {
      after[pageId]?.zoneLayout.requestCompaction(expected[i], closedIds);
    });

    // ── 5. remove empty pages, only once the runner agrees they are ──────
    let pagesRemoved = 0;
    let pagesKept = 0;
    if (plan.removePageIds.length > 0) {
      let occupied: Set<string> | null = null;
      try {
        const res = await invoke<CommandResponse>("terminal_list");
        const terminals = (res?.data as { terminals?: Array<{ pageId?: string }> } | null)
          ?.terminals;
        if (res?.success && Array.isArray(terminals)) {
          occupied = new Set(pageIdsFromTerminals(terminals));
        }
      } catch {
        // Unreadable roster → remove nothing (below).
      }
      for (const pageId of plan.removePageIds) {
        if (!occupied || occupied.has(pageId)) {
          pagesKept++;
          continue;
        }
        await pageOps.removePage(pageId);
        pagesRemoved++;
      }
    }

    pageOps.setActivePageId(targetPageIds[0]);
    return {
      closed: plan.close.length,
      moved,
      failedMoves,
      pagesCreated,
      pagesRemoved,
      pagesKept,
      tabs: targetPageIds.length,
      settled,
    };
  };
}
