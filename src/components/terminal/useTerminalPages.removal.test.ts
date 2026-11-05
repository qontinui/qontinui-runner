/**
 * `useTerminalPages().removeEmptyPage`, entered where its consumers enter it:
 * the hook's own returned callback (the page reorganize and "Close empty tabs"
 * call it). It removes a page's RECORD only, and only a page that is empty when
 * it re-reads occupancy — it never closes a terminal.
 *
 * The hook runs inside the same one-render harness as `useTerminalPages.test.ts`
 * (`renderToStaticMarkup`, no React Testing Library under `environment:
 * "node"`); the runner is an `invoke` mock.
 */

import { beforeEach, describe, expect, it, vi } from "vitest";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

const invokeMock = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({ invoke: (...a: unknown[]) => invokeMock(...a) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => {}) }));

import { instanceStorage } from "@/lib/instance-storage";
import { PAGES_STORAGE_KEY, useTerminalPages } from "./useTerminalPages";

/** A runner whose terminal list is `terminals`; records every close it is asked for. */
function runner(terminals: Array<{ id: string; pageId: string }>) {
  const closed: string[] = [];
  invokeMock.mockImplementation(async (cmd: string, args?: { terminalId?: string }) => {
    if (cmd === "terminal_list") return { success: true, data: { terminals } };
    if (cmd === "terminal_session_list_open") return { data: { sessions: [] } };
    if (cmd === "terminal_close") {
      closed.push(String(args?.terminalId));
      return { success: true };
    }
    throw new Error(`unexpected ${cmd}`);
  });
  return closed;
}

function renderUseTerminalPages(): ReturnType<typeof useTerminalPages> {
  let out: ReturnType<typeof useTerminalPages> | null = null;
  function Probe() {
    out = useTerminalPages();
    return null;
  }
  renderToStaticMarkup(createElement(Probe));
  expect(out).not.toBeNull();
  return out!;
}

describe("useTerminalPages removeEmptyPage (the hook's own callback)", () => {
  beforeEach(() => {
    invokeMock.mockReset();
    const store = new Map<string, string>();
    (globalThis as Record<string, unknown>).localStorage = {
      getItem: (k: string) => store.get(k) ?? null,
      setItem: (k: string, v: string) => void store.set(k, v),
      removeItem: (k: string) => void store.delete(k),
      key: (i: number) => [...store.keys()][i] ?? null,
      get length() {
        return store.size;
      },
      clear: () => store.clear(),
    };
    instanceStorage.setJSON(PAGES_STORAGE_KEY, [
      { id: "p-work", name: "work", createdAt: 1 },
      { id: "p-old", name: "old", createdAt: 2 },
    ]);
  });

  it("removeEmptyPage keeps an occupied page and closes no terminal", async () => {
    const closed = runner([{ id: "t-live", pageId: "p-old" }]);
    const hook = renderUseTerminalPages();

    expect(await hook.removeEmptyPage("p-old")).toBe(false);
    expect(closed).toEqual([]);
    expect(invokeMock.mock.calls.map((c) => c[0])).not.toContain("terminal_close");
  });

  it("removeEmptyPage removes an empty page and closes no terminal", async () => {
    const closed = runner([{ id: "t-live", pageId: "p-work" }]);
    const hook = renderUseTerminalPages();

    expect(await hook.removeEmptyPage("p-old")).toBe(true);
    expect(closed).toEqual([]);
    expect(invokeMock.mock.calls.map((c) => c[0])).not.toContain("terminal_close");
  });
});
