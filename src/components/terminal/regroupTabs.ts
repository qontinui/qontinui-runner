/**
 * `/regroup <per-tab>` — the pure core.
 *
 * Takes every window on every terminal tab (page) and repacks them into tabs
 * of at most `perTab` windows each:
 *
 *  - plain terminals with nothing on screen are closed;
 *  - every other window (AI sessions, terminals with content) is kept, in
 *    order — page by page, and within a page in grid order;
 *  - kept windows fill the existing tabs in their existing order, `perTab` at
 *    a time, and new tabs are added when they run out;
 *  - tabs left with nothing are removed.
 *
 * Example: tab1 has 12 AI sessions, tab2 is empty, tab3 has a terminal with
 * content, an empty terminal and an AI session. `/regroup 6` closes the empty
 * terminal and yields tab1 = 6 AI, tab2 = 6 AI, tab3 = the terminal with
 * content + the AI session.
 *
 * Some windows cannot change tab: a Conductor worker view and a remote mirror
 * have no local PTY for `terminal_set_page` to move, and a plan viewer is a
 * frontend-only tab. Those are PINNED: they stay on their own tab and use up
 * that tab's slots, and a tab holding one is never removed.
 *
 * Pure so the packing is unit-testable without React or Tauri; the classify
 * step (which needs each terminal's screen) and the execution live in
 * `useRegroupTabs.ts`.
 */

/** What a window is, for regrouping. */
export type RegroupKind = "ai" | "content" | "empty";

export interface RegroupItem {
  id: string;
  title: string;
  kind: RegroupKind;
  /** False for windows that cannot change tab (see the module comment). */
  movable: boolean;
}

export interface RegroupSourcePage {
  pageId: string;
  /** The page's windows in grid order. */
  items: RegroupItem[];
}

export interface RegroupTarget {
  /** The existing page this group reuses, or `null` for a page to create. */
  pageId: string | null;
  /** Window ids in their target grid order. */
  tabIds: string[];
}

export interface RegroupPlan {
  /** Plain terminals with an empty screen, with the page that holds each. */
  close: { pageId: string; id: string; title: string }[];
  /** Resulting tabs, in order. Never empty. */
  targets: RegroupTarget[];
  /** Existing pages left with nothing; removed. */
  removePageIds: string[];
}

/**
 * Plan a regroup. `pages` is the tab strip in display order; `perTab` must be
 * a positive integer (the command validates it before calling).
 */
export function planRegroup(pages: readonly RegroupSourcePage[], perTab: number): RegroupPlan {
  const close: RegroupPlan["close"] = [];
  // Every kept window, with its global position so each target keeps the
  // original order even where pinned and moved windows interleave.
  const movable: { id: string; seq: number }[] = [];
  const pinned = new Map<string, { id: string; seq: number }[]>();
  let seq = 0;
  for (const page of pages) {
    for (const item of page.items) {
      if (item.kind === "empty" && item.movable) {
        close.push({ pageId: page.pageId, id: item.id, title: item.title });
        continue;
      }
      const entry = { id: item.id, seq: seq++ };
      if (item.movable) {
        movable.push(entry);
      } else {
        const list = pinned.get(page.pageId) ?? [];
        list.push(entry);
        pinned.set(page.pageId, list);
      }
    }
  }

  const slots: { pageId: string | null; entries: { id: string; seq: number }[] }[] = pages.map(
    (p) => ({ pageId: p.pageId, entries: [...(pinned.get(p.pageId) ?? [])] }),
  );
  let next = 0;
  for (const slot of slots) {
    while (next < movable.length && slot.entries.length < perTab) {
      slot.entries.push(movable[next++]);
    }
  }
  while (next < movable.length) {
    const slot = { pageId: null, entries: [] as { id: string; seq: number }[] };
    while (next < movable.length && slot.entries.length < perTab) {
      slot.entries.push(movable[next++]);
    }
    slots.push(slot);
  }

  const toTarget = (slot: (typeof slots)[number]): RegroupTarget => ({
    pageId: slot.pageId,
    tabIds: [...slot.entries].sort((a, b) => a.seq - b.seq).map((e) => e.id),
  });
  const filled = slots.filter((s) => s.entries.length > 0);
  const empty = slots.filter((s) => s.entries.length === 0 && s.pageId !== null);
  // Nothing kept anywhere: keep the first tab, empty. The tab strip always
  // holds at least one page (`useTerminalPages.removePage` refuses the last).
  if (filled.length === 0) {
    const first = empty.shift();
    return {
      close,
      targets: [{ pageId: first?.pageId ?? null, tabIds: [] }],
      removePageIds: empty.map((s) => s.pageId as string),
    };
  }
  return {
    close,
    targets: filled.map(toTarget),
    removePageIds: empty.map((s) => s.pageId as string),
  };
}

/** Characters a shell prompt ends with: PowerShell/cmd `>`, sh `$`, root `#`, zsh `%`, starship/p10k. */
const PROMPT_END = /[>$#%\u276f\u279c]$/;

/**
 * Does a terminal screen hold any text? `lines` is the server-side grid
 * (`terminal_grid_text`), which renders exactly what the operator sees.
 *
 * A terminal nobody has used shows only its prompt — the runner starts
 * PowerShell with `-NoLogo`, so that is the one line on screen. So a screen is
 * EMPTY only when it is blank, or its single non-blank line ends like a bare
 * prompt. Anything else is content: a command and its output, a second
 * prompt, and — the case that matters — `PS C:\> python long_job.py`, one
 * line holding a command that is still running silently. Closing that
 * terminal would kill the job, so it is never read as empty.
 *
 * Errs toward content: a prompt this does not recognise counts as text.
 */
export function screenHasContent(lines: readonly string[]): boolean {
  const nonBlank = lines.map((l) => l.trimEnd()).filter((l) => l.trim() !== "");
  if (nonBlank.length === 0) return false;
  if (nonBlank.length > 1) return true;
  return !PROMPT_END.test(nonBlank[0]);
}
