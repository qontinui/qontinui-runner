/**
 * "Close empty tabs": the fail-closed occupancy read behind the button, and the
 * button's visibility contract. The vitest environment is `node` with no React
 * Testing Library, so the markup is checked with `renderToStaticMarkup`.
 */

import { describe, it, expect, vi, beforeEach } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";

const invokeMock = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({ invoke: (...a: unknown[]) => invokeMock(...a) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));

import {
  readOccupiedPageIds,
  closeEmptyPagesWith,
  pageRemoval,
  type TerminalPageConfig,
} from "./useTerminalPages";
import { TerminalPageTabBar } from "./TerminalPageTabBar";

function answer(terminals: unknown, sessions: unknown) {
  invokeMock.mockImplementation(async (cmd: string) => {
    if (cmd === "terminal_list") return terminals;
    if (cmd === "terminal_session_list_open") return sessions;
    throw new Error(`unexpected ${cmd}`);
  });
}

const okTerminals = (...pageIds: string[]) => ({
  success: true,
  data: { terminals: pageIds.map((pageId, i) => ({ id: `t${i}`, pageId })) },
});
const okSessions = (...pageIds: string[]) => ({
  data: { sessions: pageIds.map((pageId) => ({ pageId })) },
});

describe("readOccupiedPageIds (fail-closed occupancy)", () => {
  beforeEach(() => {
    invokeMock.mockReset();
  });

  it("unions live terminals and restorable sessions", async () => {
    answer(okTerminals("a"), okSessions("b"));
    expect(await readOccupiedPageIds()).toEqual(new Set(["a", "b"]));
  });

  it("returns null when terminal_list throws (occupancy UNKNOWN, not empty)", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "terminal_list") throw new Error("boom");
      return okSessions("b");
    });
    expect(await readOccupiedPageIds()).toBeNull();
  });

  it("returns null when the session list is malformed", async () => {
    answer(okTerminals("a"), { data: {} });
    expect(await readOccupiedPageIds()).toBeNull();
  });

  it("returns null when terminal_list reports success: false", async () => {
    answer({ success: false }, okSessions());
    expect(await readOccupiedPageIds()).toBeNull();
  });

  it("bestEffort keeps what was read from the surviving source", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "terminal_list") throw new Error("boom");
      return okSessions("b");
    });
    expect(await readOccupiedPageIds(true)).toEqual(new Set(["b"]));
  });

  it("an empty but well-formed read is an empty set, not null", async () => {
    answer(okTerminals(), okSessions());
    expect(await readOccupiedPageIds()).toEqual(new Set());
  });
});

describe("TerminalPageTabBar close-empty button", () => {
  const page = (id: string): TerminalPageConfig => ({ id, name: id, createdAt: 0 });
  const render = (pages: TerminalPageConfig[], withHandler = true, emptyPageCount?: number) =>
    renderToStaticMarkup(
      <TerminalPageTabBar
        pages={pages}
        activePageId={pages[0].id}
        onSelectPage={() => {}}
        onAddPage={() => {}}
        onRemovePage={() => {}}
        onRenamePage={() => {}}
        onCloseEmptyPages={withHandler ? async () => 0 : undefined}
        emptyPageCount={emptyPageCount}
      />,
    );

  it("shows with two or more tabs and a handler", () => {
    expect(render([page("a"), page("b")])).toContain('aria-label="Close empty tabs"');
  });

  it("is hidden with a single tab", () => {
    expect(render([page("a")])).not.toContain("Close empty tabs");
  });

  it("is hidden when no handler is wired", () => {
    expect(render([page("a"), page("b")], false)).not.toContain("Close empty tabs");
  });

  it("is disabled with a hint when no tab is empty", () => {
    const html = render([page("a"), page("b")], true, 0);
    expect(html).toContain('disabled=""');
    expect(html).toContain("No empty tabs to close");
  });

  it("states how many tabs it will close", () => {
    const html = render([page("a"), page("b")], true, 2);
    expect(html).toContain("Close 2 empty tabs");
    expect(html).not.toContain('disabled=""');
  });
});

describe("closeEmptyPagesWith (the close loop)", () => {
  const pg = (id: string): TerminalPageConfig => ({ id, name: id, createdAt: 0 });
  const pages = [pg("a"), pg("b"), pg("c")];

  /** The record-only removal over `read`, recording every page it drops. */
  function removal(read: () => Promise<Set<string> | null>) {
    const dropped: string[] = [];
    const ops = pageRemoval(
      {
        markClosed: () => {},
        dropRecord: (id) => {
          dropped.push(id);
          return true;
        },
      },
      read,
    );
    return { dropped, removeEmpty: ops.removeEmptyPage };
  }

  it("closes only the pages that are empty on both reads", async () => {
    const read = async () => new Set(["a"]);
    const { dropped, removeEmpty } = removal(read);
    const n = await closeEmptyPagesWith(pages, read, removeEmpty);
    expect(dropped).toEqual(["b", "c"]);
    expect(n).toBe(2);
  });

  it("closes nothing when occupancy is unreadable", async () => {
    const remove = vi.fn(async () => true);
    expect(await closeEmptyPagesWith(pages, async () => null, remove)).toBe(0);
    expect(remove).not.toHaveBeenCalled();
  });

  it("skips a page that became occupied between the first read and its turn", async () => {
    // read #1 picks candidates b and c; by the time b's removal re-reads, a
    // terminal has landed on b.
    const reads = [new Set(["a"]), new Set(["a", "b"]), new Set(["a", "b"])];
    const read = async () => reads.shift() ?? null;
    const { dropped, removeEmpty } = removal(read);
    const n = await closeEmptyPagesWith(pages, read, removeEmpty);
    expect(dropped).toEqual(["c"]);
    expect(n).toBe(1);
  });

  it("skips a page whose re-check cannot be read", async () => {
    const reads: Array<Set<string> | null> = [new Set(["a"]), null, new Set(["a"])];
    const read = async () => reads.shift() ?? null;
    const { dropped, removeEmpty } = removal(read);
    const n = await closeEmptyPagesWith(pages, read, removeEmpty);
    expect(dropped).toEqual(["c"]);
    expect(n).toBe(1);
  });

  it("keeps one page when every visible page is empty", async () => {
    const read = async () => new Set<string>();
    const { dropped, removeEmpty } = removal(read);
    await closeEmptyPagesWith(pages, read, removeEmpty);
    expect(dropped).toEqual(["b", "c"]);
  });
});

/**
 * The removals as the hook wires them (`pageRemoval`), over a fake runner whose
 * terminal list moves between reads. "Close empty tabs" removes only a page's
 * RECORD: a terminal that lands on a page after the candidates were picked keeps
 * its page and is never closed — `removePage`, the explicit per-page close, would
 * have killed it.
 */
describe("pageRemoval (record-only removal of empty pages)", () => {
  const pg = (id: string): TerminalPageConfig => ({ id, name: id, createdAt: 0 });

  beforeEach(() => {
    invokeMock.mockReset();
  });

  /** A runner whose `terminal_list` answers `lists` in turn (the last repeats). */
  function runner(lists: Array<Array<{ id: string; pageId: string }>>) {
    const closed: string[] = [];
    invokeMock.mockImplementation(async (cmd: string, args?: { terminalId?: string }) => {
      if (cmd === "terminal_list") {
        const terminals = lists.length > 1 ? lists.shift()! : lists[0];
        return { success: true, data: { terminals } };
      }
      if (cmd === "terminal_session_list_open") return { data: { sessions: [] } };
      if (cmd === "terminal_close") {
        closed.push(String(args?.terminalId));
        return { success: true };
      }
      throw new Error(`unexpected ${cmd}`);
    });
    return closed;
  }

  function store() {
    const dropped: string[] = [];
    return {
      dropped,
      store: {
        markClosed: vi.fn(),
        dropRecord: (id: string) => {
          dropped.push(id);
          return true;
        },
      },
    };
  }

  it("a terminal that lands on a page between the fresh read and the removal survives and keeps the page", async () => {
    // Read #1 (the candidates) sees "b" empty; by "b"'s removal a terminal has
    // landed there.
    const closed = runner([
      [{ id: "t-a", pageId: "a" }],
      [
        { id: "t-a", pageId: "a" },
        { id: "landed", pageId: "b" },
      ],
    ]);
    const { dropped, store: s } = store();
    const n = await pageRemoval(s).closeEmptyPages([pg("a"), pg("b")]);
    expect(n).toBe(0);
    expect(dropped).toEqual([]);
    expect(closed).toEqual([]);
  });

  it("removes an empty page's record and closes no terminal", async () => {
    const closed = runner([[{ id: "t-a", pageId: "a" }]]);
    const { dropped, store: s } = store();
    expect(await pageRemoval(s).removeEmptyPage("b")).toBe(true);
    expect(dropped).toEqual(["b"]);
    // Nothing is being killed, so nothing needs guarding from the reconcile: a
    // terminal that lands on "b" later gets its tab back.
    expect(s.markClosed).not.toHaveBeenCalled();
    expect(closed).toEqual([]);
    expect(invokeMock.mock.calls.map((c) => c[0])).not.toContain("terminal_close");
  });

  it("keeps a page whose occupancy cannot be read", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "terminal_list") return { success: true, data: { terminals: [] } };
      throw new Error("session list unreadable");
    });
    const { dropped, store: s } = store();
    expect(await pageRemoval(s).removeEmptyPage("b")).toBe(false);
    expect(dropped).toEqual([]);
  });

  it("removePage stays the explicit close: it kills the page's terminals", async () => {
    const closed = runner([[{ id: "t-b", pageId: "b" }]]);
    const { dropped, store: s } = store();
    await pageRemoval(s).removePage("b");
    expect(closed).toEqual(["t-b"]);
    expect(dropped).toEqual(["b"]);
  });
});
