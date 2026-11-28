// @vitest-environment jsdom
/**
 * The deferred-kill close, through the REAL `useTerminalManager` hook.
 *
 * `pendingCloseReducer.test.ts` pins the pure transitions; this renders the
 * hook (React 19 `createRoot` + `act`, no Testing Library — same harness as
 * `useSessionReview.test.ts`) inside the app's `ToastProvider`, with Tauri's
 * `invoke` / `listen` mocked and `setTimeout` faked, and pins what the hook
 * actually sends to the backend: when `terminal_close` goes out, how many
 * times, and what the toast's Undo does.
 *
 * The mocked backend lists one terminal, `t1`, and stops listing it once a
 * `terminal_close` for it arrives — so a resurrected tab can only come from
 * the re-sync reading a still-live PTY whose close is pending.
 */

import { act, createElement, useLayoutEffect } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ToastAction, ToastType } from "@/hooks/useToast";

interface BackendTerminal {
  id: string;
  title: string;
  pid: number;
  isAlive: boolean;
  exitCode: number | null;
  createdAt: number;
  pageId: string;
}
let backend: BackendTerminal[] = [];
const invokeCalls: { cmd: string; args: unknown }[] = [];

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn((cmd: string, args?: Record<string, unknown>) => {
    invokeCalls.push({ cmd, args });
    if (cmd === "terminal_list") {
      return Promise.resolve({ success: true, data: { terminals: backend } });
    }
    if (cmd === "terminal_close") {
      backend = backend.filter((t) => t.id !== args?.terminalId);
    }
    return Promise.resolve({ success: true, data: {} });
  }),
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(() => Promise.resolve(() => {})),
}));

import { ToastProvider } from "@/contexts/ToastContext";
import { useTerminalManager } from "./useTerminalManager";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

type Manager = ReturnType<typeof useTerminalManager>;
const out: { current: Manager | null } = { current: null };
function manager(): Manager {
  if (!out.current) throw new Error("Probe has not committed");
  return out.current;
}

const toasts: { message: string; type: ToastType; action?: ToastAction }[] = [];
const showToast = (message: string, type: ToastType, action?: ToastAction) => {
  toasts.push({ message, type, action });
};

function Probe() {
  const m = useTerminalManager("default");
  useLayoutEffect(() => {
    out.current = m;
  });
  return null;
}

let container: HTMLDivElement;
let root: Root;
let mounted = false;

/**
 * Let the mocked IPC promises and their `.then` chains commit.
 *
 * Only `setTimeout` / `clearTimeout` are faked, so promise microtasks still run
 * normally — but only when something awaits. Every mocked `invoke` resolves
 * immediately, and no chain in the hook is longer than a handful of `.then`
 * hops, so ten awaited ticks inside one async `act` drain them all (and let
 * the resulting state commit). Timer-driven work (the 10 s expiry, the 400 ms
 * exit debounce) is NOT advanced here: tests move the clock explicitly with
 * `vi.advanceTimersByTime`.
 */
async function flush() {
  await act(async () => {
    for (let i = 0; i < 10; i++) await Promise.resolve();
  });
}

const kills = () => invokeCalls.filter((c) => c.cmd === "terminal_close").length;
const tabIds = () => manager().tabs.map((t) => t.id);

async function mountWith(terminal: Partial<BackendTerminal> = {}) {
  backend = [
    {
      id: "t1",
      title: "Terminal 1",
      pid: 42,
      isAlive: true,
      exitCode: null,
      createdAt: Date.now(),
      pageId: "default",
      ...terminal,
    },
  ];
  act(() => {
    root.render(createElement(ToastProvider, { showToast }, createElement(Probe)));
  });
  mounted = true;
  // The mount re-sync reads `terminal_list` and adds `t1` as a tab.
  await flush();
  expect(tabIds()).toEqual(["t1"]);
}

describe("useTerminalManager deferred close (real hook)", () => {
  beforeEach(() => {
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
    invokeCalls.length = 0;
    toasts.length = 0;
    out.current = null;
    container = document.createElement("div");
    root = createRoot(container);
    mounted = false;
  });
  afterEach(() => {
    if (mounted) act(() => root.unmount());
    vi.useRealTimers();
  });

  it("a default close removes the tab at once but sends terminal_close only after 10 s, once", async () => {
    await mountWith();
    act(() => {
      manager().closeTerminal("t1");
    });
    expect(tabIds()).toEqual([]);
    expect(kills()).toBe(0);
    expect(toasts).toHaveLength(1);
    expect(toasts[0].message).toBe("Closed Terminal 1");
    expect(toasts[0].type).toBe("info");
    expect(toasts[0].action?.label).toBe("Undo");

    act(() => {
      vi.advanceTimersByTime(9_999);
    });
    expect(kills()).toBe(0);

    act(() => {
      vi.advanceTimersByTime(1);
    });
    expect(kills()).toBe(1);
    await flush();
    act(() => {
      vi.advanceTimersByTime(30_000);
    });
    await flush();
    expect(kills()).toBe(1);
    expect(tabIds()).toEqual([]);
  });

  it("Undo inside the window restores the tab and never sends terminal_close", async () => {
    await mountWith();
    act(() => {
      manager().closeTerminal("t1");
    });
    expect(tabIds()).toEqual([]);
    act(() => {
      toasts[0].action?.onClick();
    });
    // Restored by Undo itself, before the follow-up re-sync's IPC settles —
    // the re-sync would also list t1 again, which must not mask a broken Undo.
    expect(tabIds()).toEqual(["t1"]);
    await flush();
    expect(tabIds()).toEqual(["t1"]);
    act(() => {
      vi.advanceTimersByTime(30_000);
    });
    await flush();
    expect(kills()).toBe(0);
    expect(tabIds()).toEqual(["t1"]);
  });

  it("a re-sync while the close is pending does not resurrect the tab", async () => {
    await mountWith();
    act(() => {
      manager().closeTerminal("t1");
    });
    // The backend still lists t1: its PTY is alive until the window expires.
    expect(backend.map((t) => t.id)).toEqual(["t1"]);
    await act(async () => {
      await manager().resyncTabs();
    });
    await flush();
    expect(tabIds()).toEqual([]);
    expect(kills()).toBe(0);
  });

  it("unmounting with a close pending sends its terminal_close at once", async () => {
    await mountWith();
    act(() => {
      manager().closeTerminal("t1");
    });
    expect(kills()).toBe(0);
    act(() => root.unmount());
    mounted = false;
    expect(kills()).toBe(1);
    act(() => {
      vi.advanceTimersByTime(30_000);
    });
    expect(kills()).toBe(1);
  });

  it("{ immediate: true } sends terminal_close at once and raises no toast", async () => {
    await mountWith();
    act(() => {
      manager().closeTerminal("t1", { immediate: true });
    });
    expect(tabIds()).toEqual([]);
    expect(kills()).toBe(1);
    expect(toasts).toHaveLength(0);
  });

  it("Undo then an immediate re-close (before the next commit) defers again, not kill-now", async () => {
    await mountWith();
    act(() => {
      manager().closeTerminal("t1");
    });
    // Undo and the second close in ONE act: no commit (and so no post-commit
    // `tabsRef` effect) runs between them.
    act(() => {
      toasts[0].action?.onClick();
      manager().closeTerminal("t1");
    });
    expect(kills()).toBe(0);
    expect(toasts).toHaveLength(2);
    expect(toasts[1].action?.label).toBe("Undo");
    act(() => {
      vi.advanceTimersByTime(10_000);
    });
    expect(kills()).toBe(1);
  });

  it("a claudeSessionId pinned during the window is in the close record the expiry sends", async () => {
    await mountWith();
    act(() => {
      manager().closeTerminal("t1");
    });
    act(() => {
      manager().updateTab("t1", { claudeSessionId: "sess-late" });
    });
    act(() => {
      vi.advanceTimersByTime(10_000);
    });
    const records = invokeCalls.filter((c) => c.cmd === "terminal_session_record_close");
    expect(records).toHaveLength(1);
    expect(records[0].args).toEqual({
      claudeSessionId: "sess-late",
      terminalId: "t1",
      reason: "explicit",
    });
  });

  it("dismissing an exited session kills at once with no Undo toast", async () => {
    await mountWith({ isAlive: false, exitCode: 0 });
    act(() => {
      manager().closeTerminal("t1");
    });
    expect(kills()).toBe(1);
    expect(toasts).toHaveLength(0);
  });
});
