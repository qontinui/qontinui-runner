/**
 * The page reorganize MOVES panes — it never kills one.
 *
 * Every test drives `applyReorganizePlan` against a fake runner that holds a
 * terminal table and answers `terminal_list` / `terminal_set_page` /
 * `terminal_session_list_open` from it, and records every other command. The
 * page removal is the REAL one the hook wires (`pageRemoval`), reading the same
 * fake runner, so what a removal closes is observed, not assumed.
 */

import { describe, it, expect, vi, beforeEach } from "vitest";

const runnerInvoke = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({ invoke: (...a: unknown[]) => runnerInvoke(...a) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));

import { applyReorganizePlan, type ReorganizePlan } from "./applyReorganizePlan";
import { pageRemoval, type TerminalPageConfig } from "./useTerminalPages";

interface FakeTerminal {
  id: string;
  title: string;
  pageId: string;
}

interface FakeRunnerOptions {
  /** Terminal ids whose move the runner refuses. */
  refuseMove?: string[];
  /** Make `terminal_session_list_open` fail (occupancy UNKNOWN). */
  sessionsUnreadable?: boolean;
  /** Runs after each successful move — e.g. a terminal landing somewhere. */
  afterMoves?: (table: FakeTerminal[]) => void;
}

function fakeRunner(terminals: FakeTerminal[], opts: FakeRunnerOptions = {}) {
  const table = terminals.map((t) => ({ ...t }));
  const calls: Array<{ cmd: string; args?: Record<string, unknown> }> = [];
  let movesDone = false;
  const answer = async (cmd: string, args?: Record<string, unknown>) => {
    calls.push({ cmd, args });
    switch (cmd) {
      case "terminal_list":
        // The wire shape: `TerminalInfo` is camelCase — `pageId`, never `page_id`.
        return {
          success: true,
          data: { terminals: table.map((t) => ({ id: t.id, title: t.title, pageId: t.pageId })) },
        };
      case "terminal_session_list_open":
        if (opts.sessionsUnreadable) throw new Error("store unreadable");
        return { data: { sessions: [] } };
      case "terminal_set_page": {
        const id = String(args?.terminalId);
        if (opts.refuseMove?.includes(id)) throw new Error(`Terminal session not found: ${id}`);
        const t = table.find((x) => x.id === id);
        if (!t) throw new Error(`Terminal session not found: ${id}`);
        t.pageId = String(args?.pageId);
        return { success: true, message: null, data: null };
      }
      case "terminal_close": {
        const id = String(args?.terminalId);
        const i = table.findIndex((x) => x.id === id);
        if (i >= 0) table.splice(i, 1);
        return { success: true };
      }
      default:
        return { success: true };
    }
  };
  runnerInvoke.mockImplementation(async (cmd: string, args?: Record<string, unknown>) => {
    const r = await answer(cmd, args);
    if (cmd === "terminal_set_page" && opts.afterMoves && !movesDone) {
      movesDone = true;
      opts.afterMoves(table);
    }
    return r;
  });
  const invoke = (cmd: string, args?: Record<string, unknown>) => runnerInvoke(cmd, args);
  const commands = () => calls.map((c) => c.cmd);
  return { table, calls, commands, invoke };
}

const page = (id: string, name = id): TerminalPageConfig => ({ id, name, createdAt: 0 });

/** The page roster: `pageRemoval` over it, plus an `addPage` that mints ids. */
function roster(pages: TerminalPageConfig[]) {
  const dropped: string[] = [];
  const created: string[] = [];
  const ops = pageRemoval({
    markClosed: () => {},
    dropRecord: (id) => {
      dropped.push(id);
      return true;
    },
  });
  const addPage = (name: string) => {
    const id = `new-${created.length + 1}`;
    created.push(name);
    return id;
  };
  return { pages, dropped, created, ops, addPage };
}

function deps(
  runner: ReturnType<typeof fakeRunner>,
  r: ReturnType<typeof roster>,
  waitForPage: (id: string) => Promise<boolean> = async () => true,
) {
  return {
    invoke: runner.invoke,
    pages: r.pages,
    addPage: r.addPage,
    // The full removal object, as `useTerminalPages()` returns it.
    ...r.ops,
    waitForPage,
  };
}

const KILLING = ["terminal_close", "terminal_create", "terminal_save_scrollback"];

describe("applyReorganizePlan", () => {
  beforeEach(() => {
    runnerInvoke.mockReset();
  });

  it("moves each live pane with terminal_set_page and never closes or re-creates one", async () => {
    const runner = fakeRunner([
      { id: "s1", title: "web api", pageId: "p1" },
      { id: "s2", title: "runner ui", pageId: "p1" },
      { id: "s3", title: "web db", pageId: "p2" },
    ]);
    const r = roster([page("p1", "Mixed"), page("p2", "Other")]);
    const plan: ReorganizePlan = {
      pages: [
        { name: "Web", sessionIds: ["s1", "s3"] },
        { name: "Runner", sessionIds: ["s2"] },
      ],
    };

    const result = await applyReorganizePlan(plan, deps(runner, r));

    const moves = runner.calls.filter((c) => c.cmd === "terminal_set_page").map((c) => c.args);
    expect(moves).toEqual([
      { terminalId: "s1", pageId: "new-1" },
      { terminalId: "s3", pageId: "new-1" },
      { terminalId: "s2", pageId: "new-2" },
    ]);
    for (const cmd of KILLING) expect(runner.commands()).not.toContain(cmd);
    expect(result.moved.map((m) => m.id)).toEqual(["s1", "s3", "s2"]);
    expect(result.failed).toEqual([]);
    expect(r.created).toEqual(["Web", "Runner"]);
    // Every pane is still running, now on its planned page.
    expect(runner.table.map((t) => [t.id, t.pageId])).toEqual([
      ["s1", "new-1"],
      ["s2", "new-2"],
      ["s3", "new-1"],
    ]);
    // Both vacated pages are now empty: their records go, no PTY with them.
    expect(r.dropped).toEqual(["p1", "p2"]);
    expect(result.pagesRemoved).toEqual(["Mixed", "Other"]);
  });

  it("invokes nothing for sessions already on their target pages", async () => {
    const runner = fakeRunner([
      { id: "s1", title: "web api", pageId: "p1" },
      { id: "s2", title: "runner ui", pageId: "p2" },
    ]);
    const r = roster([page("p1", "Web"), page("p2", "Runner")]);
    const plan: ReorganizePlan = {
      pages: [
        { name: "Web", sessionIds: ["s1"] },
        { name: "Runner", sessionIds: ["s2"] },
      ],
    };

    const result = await applyReorganizePlan(plan, deps(runner, r));

    expect(runner.commands()).toEqual(["terminal_list"]);
    expect(result.moved).toEqual([]);
    expect(result.unchanged).toBe(2);
    expect(r.created).toEqual([]);
    expect(r.dropped).toEqual([]);
  });

  it("does not remove a page that still hosts a terminal the plan did not name", async () => {
    const runner = fakeRunner([
      { id: "s1", title: "web api", pageId: "p1" },
      { id: "u1", title: "unplanned", pageId: "p1" },
      { id: "s2", title: "web db", pageId: "p2" },
    ]);
    const r = roster([page("p1", "Mixed"), page("p2", "Web")]);
    const plan: ReorganizePlan = { pages: [{ name: "Web", sessionIds: ["s1", "s2"] }] };

    const result = await applyReorganizePlan(plan, deps(runner, r));

    expect(r.dropped).toEqual([]);
    expect(result.pagesKept).toEqual(["Mixed"]);
    expect(runner.commands()).not.toContain("terminal_close");
    expect(runner.table.find((t) => t.id === "u1")?.pageId).toBe("p1");
  });

  it("removes nothing when occupancy cannot be read", async () => {
    const runner = fakeRunner(
      [
        { id: "s1", title: "web api", pageId: "p1" },
        { id: "s2", title: "web db", pageId: "p2" },
      ],
      { sessionsUnreadable: true },
    );
    const r = roster([page("p1", "Old"), page("p2", "Web")]);
    const plan: ReorganizePlan = { pages: [{ name: "Web", sessionIds: ["s1", "s2"] }] };

    const result = await applyReorganizePlan(plan, deps(runner, r));

    expect(result.moved.map((m) => m.id)).toEqual(["s1"]);
    expect(r.dropped).toEqual([]);
    expect(result.pagesKept).toEqual(["Old"]);
  });

  it("keeps a vacated page a terminal lands on after the moves, and never closes that terminal", async () => {
    const runner = fakeRunner(
      [
        { id: "s1", title: "web api", pageId: "p1" },
        { id: "s2", title: "web db", pageId: "p2" },
      ],
      {
        // An API-created terminal lands on p1 once the move has emptied it.
        afterMoves: (table) => table.push({ id: "landed", title: "Terminal", pageId: "p1" }),
      },
    );
    const r = roster([page("p1", "Old"), page("p2", "Web")]);
    const plan: ReorganizePlan = { pages: [{ name: "Web", sessionIds: ["s1", "s2"] }] };

    const result = await applyReorganizePlan(plan, deps(runner, r));

    expect(runner.table.some((t) => t.id === "landed")).toBe(true);
    expect(runner.commands()).not.toContain("terminal_close");
    expect(r.dropped).toEqual([]);
    expect(result.pagesKept).toEqual(["Old"]);
  });

  it("leaves a pane whose move fails where it is, named in the result, and keeps its page", async () => {
    const runner = fakeRunner(
      [
        { id: "s1", title: "web api", pageId: "p1" },
        { id: "s2", title: "web db", pageId: "p2" },
      ],
      { refuseMove: ["s1"] },
    );
    const r = roster([page("p1", "Old"), page("p2", "Web")]);
    const plan: ReorganizePlan = { pages: [{ name: "Web", sessionIds: ["s1", "s2"] }] };

    const result = await applyReorganizePlan(plan, deps(runner, r));

    expect(result.failed).toEqual([
      {
        id: "s1",
        title: "web api",
        toPage: "Web",
        reason: "Error: Terminal session not found: s1",
      },
    ]);
    expect(runner.table.find((t) => t.id === "s1")?.pageId).toBe("p1");
    expect(r.dropped).toEqual([]);
    for (const cmd of KILLING) expect(runner.commands()).not.toContain(cmd);
  });

  it("moves nothing onto a page that never starts listening for the move", async () => {
    const runner = fakeRunner([
      { id: "s1", title: "web api", pageId: "p1" },
      { id: "s2", title: "runner ui", pageId: "p1" },
    ]);
    const r = roster([page("p1", "Mixed")]);
    const plan: ReorganizePlan = {
      pages: [
        { name: "Web", sessionIds: ["s1"] },
        { name: "Mixed", sessionIds: ["s2"] },
      ],
    };
    const waited: string[] = [];
    const result = await applyReorganizePlan(
      plan,
      deps(runner, r, async (id) => {
        waited.push(id);
        return false;
      }),
    );

    expect(waited).toEqual(["new-1"]);
    expect(runner.commands()).not.toContain("terminal_set_page");
    expect(result.failed.map((f) => [f.id, f.reason])).toEqual([
      ["s1", 'page "Web" did not come up'],
    ]);
    // The page made for the plan received nothing and is empty: it goes.
    expect(r.dropped).toEqual(["new-1"]);
  });

  it("refuses to move anything when the terminal list cannot be read", async () => {
    runnerInvoke.mockImplementation(async () => {
      throw new Error("ipc down");
    });
    const r = roster([page("p1")]);
    await expect(
      applyReorganizePlan(
        { pages: [{ name: "Web", sessionIds: ["s1"] }] },
        {
          invoke: (cmd, args) => runnerInvoke(cmd, args),
          pages: r.pages,
          addPage: r.addPage,
          removeEmptyPage: r.ops.removeEmptyPage,
          waitForPage: async () => true,
        },
      ),
    ).rejects.toThrow(/nothing was moved/);
    expect(r.created).toEqual([]);
  });
});
