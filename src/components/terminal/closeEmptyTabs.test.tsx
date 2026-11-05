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
    expect(await readOccupiedPageIds()).toEqual({ ids: new Set(["a", "b"]), complete: true });
  });

  it("is incomplete when terminal_list throws, and keeps what the other source read", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "terminal_list") throw new Error("boom");
      return okSessions("b");
    });
    expect(await readOccupiedPageIds()).toEqual({ ids: new Set(["b"]), complete: false });
  });

  it("is incomplete when the session list is malformed", async () => {
    answer(okTerminals("a"), { data: {} });
    expect(await readOccupiedPageIds()).toEqual({ ids: new Set(["a"]), complete: false });
  });

  it("is incomplete when terminal_list reports success: false", async () => {
    answer({ success: false }, okSessions());
    expect((await readOccupiedPageIds()).complete).toBe(false);
  });

  it("is incomplete when a drain withholds the restorable set (empty is UNKNOWN)", async () => {
    answer(okTerminals("a"), {
      success: true,
      data: { sessions: [], deferredByDrain: { state: "drained", reason: "coord drained" } },
    });
    expect(await readOccupiedPageIds()).toEqual({ ids: new Set(["a"]), complete: false });
  });

  it("an empty but well-formed read is a complete empty set", async () => {
    answer(okTerminals(), okSessions());
    expect(await readOccupiedPageIds()).toEqual({ ids: new Set(), complete: true });
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
  const read = (...ids: string[]) => ({ ids: new Set(ids), complete: true });
  const UNKNOWN = { ids: new Set<string>(), complete: false };
  const recorder = () => {
    const removed: string[] = [];
    return {
      removed,
      remove: async (id: string) => {
        removed.push(id);
        return id;
      },
    };
  };

  it("removes only the pages that are empty on both reads", async () => {
    const r = recorder();
    const n = await closeEmptyPagesWith(pages, async () => read("a"), r.remove);
    expect(r.removed).toEqual(["b", "c"]);
    expect(n).toBe(2);
  });

  it("removes nothing when occupancy is unreadable", async () => {
    const remove = vi.fn(async () => "x");
    expect(await closeEmptyPagesWith(pages, async () => UNKNOWN, remove)).toBe(0);
    expect(remove).not.toHaveBeenCalled();
  });

  it("removes nothing on a drain-deferred read, even though it lists no restorable rows", async () => {
    invokeMock.mockReset();
    answer(okTerminals("a"), {
      success: true,
      data: { sessions: [], deferredByDrain: { state: "drained", reason: "x" } },
    });
    const r = recorder();
    expect(await closeEmptyPagesWith(pages, readOccupiedPageIds, r.remove)).toBe(0);
    expect(r.removed).toEqual([]);
  });

  it("skips a page that became occupied between the first read and its turn", async () => {
    const r = recorder();
    // read #1 picks candidates b and c; by the time b's per-page re-check runs,
    // a terminal has landed on b.
    const reads = [read("a"), read("a", "b"), read("a", "b")];
    const n = await closeEmptyPagesWith(pages, async () => reads.shift() ?? UNKNOWN, r.remove);
    expect(r.removed).toEqual(["c"]);
    expect(n).toBe(1);
  });

  it("skips a page whose re-check cannot be read", async () => {
    const r = recorder();
    const reads = [read("a"), UNKNOWN, read("a")];
    const n = await closeEmptyPagesWith(pages, async () => reads.shift() ?? UNKNOWN, r.remove);
    expect(r.removed).toEqual(["c"]);
    expect(n).toBe(1);
  });

  it("does not count a page the record-only removal kept", async () => {
    const n = await closeEmptyPagesWith(
      pages,
      async () => read("a"),
      async (id) => (id === "b" ? null : id),
    );
    expect(n).toBe(1);
  });

  it("keeps one page when every visible page is empty", async () => {
    const r = recorder();
    await closeEmptyPagesWith(pages, async () => read(), r.remove);
    expect(r.removed).toEqual(["b", "c"]);
  });
});
