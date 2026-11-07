/**
 * The page reorganize ("Reorganize pages by topic") carried out — MOVES, never
 * kills.
 *
 * The dialog proposes which page each session belongs on; this applies it:
 *
 *  1. read every terminal's CURRENT page from `terminal_list`'s `pageId` (the
 *     wire field; `page_id` is absent there, so reading it saw every pane as
 *     off-page and closed and re-created all of them — m7-4);
 *  2. resolve each planned page by name, creating the ones that do not exist;
 *  3. wait until each target page listens for `terminal-page-changed` (a page
 *     created a moment ago has no listener yet, and a move it misses is not
 *     adopted until that page's next re-sync — its tab is gone meanwhile);
 *  4. move each pane the plan places on ANOTHER page with `terminal_set_page`
 *     — the PTY and whatever runs in it (an AI session) keep running; a pane
 *     already on its target page is not touched. A move that fails leaves the
 *     pane where it is, named in the result;
 *  5. remove the pages the moves vacated through `removeEmptyPage`, which
 *     re-reads occupancy at removal time and drops only the page's record: a
 *     page still hosting a terminal the plan did not name (or a failed move,
 *     or one that landed after the moves) is kept, and an unreadable occupancy
 *     removes nothing.
 *
 * Nothing here saves scrollback, closes a terminal or spawns one. Every effect
 * is injected so the whole sequence is testable without React or Tauri.
 */

import { terminalPageId, type TerminalPageConfig } from "./useTerminalPages";

export interface ReorganizePlan {
  pages: Array<{
    name: string;
    sessionIds: string[];
  }>;
}

/** The Tauri `invoke` shape the reorganize uses. */
export type ReorganizeInvoke = (cmd: string, args?: Record<string, unknown>) => Promise<unknown>;

export interface ReorganizeDeps {
  invoke: ReorganizeInvoke;
  /** This window's page roster when the plan is applied. */
  pages: ReadonlyArray<Pick<TerminalPageConfig, "id" | "name">>;
  /** Create a page named `name`; returns its id. */
  addPage: (name: string) => string;
  /**
   * Record-only removal of a page that is empty at removal time
   * (`useTerminalPages().removeEmptyPage`); returns whether it was removed.
   */
  removeEmptyPage: (id: string) => Promise<boolean>;
  /**
   * Resolves `true` once `pageId` listens for `terminal-page-changed` in this
   * window, `false` when it never came up (`waitForPageMoveListener`).
   */
  waitForPage: (pageId: string) => Promise<boolean>;
}

/** One pane the reorganize moved, or tried to. */
export interface ReorganizeMove {
  id: string;
  title: string;
  /** The page the plan put it on (by name). */
  toPage: string;
}

export interface ReorganizeFailure extends ReorganizeMove {
  /** Why it did not move; the pane is still on its own page. */
  reason: string;
}

export interface ReorganizeResult {
  /** Panes moved to their planned page — still running. */
  moved: ReorganizeMove[];
  /** Panes the plan placed where they already were — left untouched. */
  unchanged: number;
  /** Panes whose move did not happen. Each stays where it was. */
  failed: ReorganizeFailure[];
  /** Names of the pages created for the plan. */
  pagesCreated: string[];
  /** Names of the vacated pages removed (record only — no terminal closed). */
  pagesRemoved: string[];
  /** Names of vacated pages kept: still hosting a terminal, or unreadable. */
  pagesKept: string[];
}

/** `terminal_list` could not be read, so nothing was moved. */
export class ReorganizeListUnreadable extends Error {
  constructor(detail: string) {
    super(`Could not read the terminal list, so nothing was moved (${detail}).`);
    this.name = "ReorganizeListUnreadable";
  }
}

interface ListedTerminal {
  id: string;
  title?: string;
  pageId?: string;
  page_id?: string;
}

async function listTerminals(invoke: ReorganizeInvoke): Promise<ListedTerminal[]> {
  let resp: unknown;
  try {
    resp = await invoke("terminal_list");
  } catch (err) {
    throw new ReorganizeListUnreadable(String(err));
  }
  const r = resp as { success?: boolean; data?: { terminals?: unknown } } | null;
  const terminals = r?.data?.terminals;
  if (!r?.success || !Array.isArray(terminals)) {
    throw new ReorganizeListUnreadable("the runner answered without a terminal list");
  }
  return terminals as ListedTerminal[];
}

/** A rejected invoke, or a `CommandResponse` that says `success: false`. */
async function moveFailure(
  invoke: ReorganizeInvoke,
  terminalId: string,
  pageId: string,
): Promise<string | null> {
  try {
    const resp = (await invoke("terminal_set_page", { terminalId, pageId })) as {
      success?: boolean;
      message?: string | null;
    } | null;
    if (resp?.success === false) return resp.message || "the runner refused the move";
    return null;
  } catch (err) {
    return String(err);
  }
}

export async function applyReorganizePlan(
  plan: ReorganizePlan,
  deps: ReorganizeDeps,
): Promise<ReorganizeResult> {
  // ── 1. where every pane is now ───────────────────────────────────────────
  const listed = await listTerminals(deps.invoke);
  const byId = new Map(listed.map((t) => [t.id, t]));
  const titleOf = (id: string) => byId.get(id)?.title || id.slice(0, 8);

  // ── 2. resolve (or create) each planned page ─────────────────────────────
  // A session the plan names twice goes to its FIRST group; a group left with
  // no session creates no page.
  const pageIdByName = new Map<string, string>();
  for (const p of deps.pages) if (!pageIdByName.has(p.name)) pageIdByName.set(p.name, p.id);
  const known = new Set(deps.pages.map((p) => p.id));
  const placed = new Set<string>();
  const groups: Array<{ name: string; pageId: string; sessionIds: string[] }> = [];
  const pagesCreated: string[] = [];
  const createdIds = new Set<string>();
  for (const group of plan.pages) {
    const sessionIds = group.sessionIds.filter((id) => !placed.has(id));
    if (sessionIds.length === 0) continue;
    for (const id of sessionIds) placed.add(id);
    let pageId = pageIdByName.get(group.name);
    if (!pageId) {
      pageId = deps.addPage(group.name);
      pageIdByName.set(group.name, pageId);
      pagesCreated.push(group.name);
      createdIds.add(pageId);
    }
    groups.push({ name: group.name, pageId, sessionIds });
  }
  const targetIds = new Set(groups.map((g) => g.pageId));
  const nameOf = (pageId: string) =>
    deps.pages.find((p) => p.id === pageId)?.name ??
    groups.find((g) => g.pageId === pageId)?.name ??
    pageId;

  // ── 3. every target page that receives a move must be listening ─────────
  const offPage = (sessionId: string, pageId: string) => {
    const t = byId.get(sessionId);
    return t !== undefined && terminalPageId(t) !== pageId;
  };
  const receiving = new Set(
    groups.filter((g) => g.sessionIds.some((id) => offPage(id, g.pageId))).map((g) => g.pageId),
  );
  const ready = new Map(
    await Promise.all([...receiving].map(async (id) => [id, await deps.waitForPage(id)] as const)),
  );

  // ── 4. move ───────────────────────────────────────────────────────────────
  const moved: ReorganizeMove[] = [];
  const failed: ReorganizeFailure[] = [];
  let unchanged = 0;
  const vacated = new Set<string>();
  const received = new Set<string>();
  for (const group of groups) {
    for (const id of group.sessionIds) {
      const entry = { id, title: titleOf(id), toPage: group.name };
      const t = byId.get(id);
      if (!t) {
        failed.push({ ...entry, reason: "it is no longer running" });
        continue;
      }
      const from = terminalPageId(t);
      if (from === group.pageId) {
        unchanged++;
        continue;
      }
      if (!ready.get(group.pageId)) {
        failed.push({ ...entry, reason: `page "${group.name}" did not come up` });
        continue;
      }
      const reason = await moveFailure(deps.invoke, id, group.pageId);
      if (reason) {
        failed.push({ ...entry, reason });
        continue;
      }
      moved.push(entry);
      vacated.add(from);
      received.add(group.pageId);
    }
  }

  // ── 5. remove what the moves emptied — record only, re-read at removal ───
  // Candidates: this window's pages a moved pane left that the plan does not
  // target, plus a page created for the plan that received nothing. Never the
  // internal "default" page.
  const candidates = [
    ...[...vacated].filter((id) => known.has(id) && !targetIds.has(id)),
    ...[...createdIds].filter((id) => !received.has(id)),
  ].filter((id) => id !== "default");
  const pagesRemoved: string[] = [];
  const pagesKept: string[] = [];
  for (const id of candidates) {
    const name = nameOf(id);
    if (await deps.removeEmptyPage(id)) pagesRemoved.push(name);
    else pagesKept.push(name);
  }

  return { moved, unchanged, failed, pagesCreated, pagesRemoved, pagesKept };
}
