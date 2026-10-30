/**
 * UI Bridge read surface for the terminal page ROSTER.
 *
 * `list-pages` answers every persisted terminal page's name — the same names
 * the tab bar shows — so an agent (e.g. `/triage-sessions`) can address pages
 * by name without reading the WebView DOM. It is fed `allPages`, not
 * `pages` / `visiblePages`: `pages` drops pages detached into a pop-out, and
 * `visiblePages` also hides the internal `default` page while it is empty. A
 * roster missing either would make a caller report a real page as absent.
 *
 * Mounted by `App` in the MAIN window only (never in a page-pinned pop-out),
 * so a pop-out — which knows only its own page — never serves a one-page
 * roster under the same component id.
 */

import { useUIComponent } from "@qontinui/ui-bridge";
import { guardedHandler } from "@/lib/ui-bridge/guardedHandler";
import { MAIN_WINDOW_LABEL, readWindowLabel } from "./contexts/WindowAssignmentsContext";
import type { TerminalPageConfig } from "./useTerminalPages";

/** `list-pages` takes no argument; an undeclared key is refused by the guard. */
const LIST_PAGES_SCHEMA = {} as const;

/** One row of the `list-pages` answer. */
export interface PageRosterEntry {
  /** Page id; the internal default page is `"default"`, as in `GET /terminal-pages`. */
  pageId: string;
  /** The tab name the operator sees. */
  name: string;
  /** True for the main window's active page. */
  active: boolean;
  /** `"main"`, or the label of the pop-out window the page is detached into. */
  window: string;
}

/** Build the roster in tab order (the persisted order of `allPages`). */
export function buildPageRoster(
  allPages: readonly TerminalPageConfig[],
  boundPages: Readonly<Record<string, string>>,
  activePageId: string,
): PageRosterEntry[] {
  return allPages.map((p) => ({
    pageId: p.id,
    name: p.name,
    active: p.id === activePageId,
    window: boundPages[p.id] ?? MAIN_WINDOW_LABEL,
  }));
}

/**
 * True in the runner's main window. A pop-out (page-pinned or not) has its own
 * label; outside Tauri (tests) there is only the one window, so it reads main.
 * `readWindowLabel` is the one place that decides what "main" means.
 */
export const isMainWindow = (): boolean => readWindowLabel() === MAIN_WINDOW_LABEL;

export interface TerminalPagesBridgeProps {
  allPages: readonly TerminalPageConfig[];
  boundPages: Readonly<Record<string, string>>;
  activePageId: string;
}

export function TerminalPagesBridge({
  allPages,
  boundPages,
  activePageId,
}: TerminalPagesBridgeProps): null {
  useUIComponent({
    id: "terminal-pages",
    name: "Terminal Pages",
    description:
      "The terminal page roster. 'list-pages' returns every persisted page, including " +
      "pages detached into a pop-out window and the internal `default` page when it " +
      "exists (even while empty).",
    actions: [
      {
        id: "list-pages",
        label: "List Pages",
        description:
          "Return [{pageId, name, active, window}] for every terminal page in tab order. " +
          '`window` is "main" or the pop-out window label; the default page\'s pageId is "default".',
        paramSchema: LIST_PAGES_SCHEMA,
        // `read` — reports the page roster; changes nothing.
        effect: "read",
        handler: guardedHandler("list-pages", LIST_PAGES_SCHEMA, () =>
          buildPageRoster(allPages, boundPages, activePageId),
        ),
      },
    ],
  });
  return null;
}
