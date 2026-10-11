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

import { readOccupiedPageIds, type TerminalPageConfig } from "./useTerminalPages";
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
  const render = (pages: TerminalPageConfig[], withHandler = true) =>
    renderToStaticMarkup(
      <TerminalPageTabBar
        pages={pages}
        activePageId={pages[0].id}
        onSelectPage={() => {}}
        onAddPage={() => {}}
        onRemovePage={() => {}}
        onRenamePage={() => {}}
        onCloseEmptyPages={withHandler ? async () => 0 : undefined}
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
});
