// @vitest-environment jsdom
/**
 * The Session Manager's rows and its row click, entered where the page enters
 * them: the REAL `useSessionManager`, mounted in React with the inputs
 * `TerminalSessionContext` hands it. In tmux work mode the panel is the only
 * way to reach an off-screen session (plan
 * `2026-10-11-runner-terminal-tmux-work-mode`, D5), so EVERY live tab needs a
 * row — a plain shell or a session before its first message included, which
 * has no transcript — and clicking that row must route to the tab, which the
 * page turns into a swap (`onFocusSessionTab` → `requestReveal`).
 */

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(() => Promise.resolve(null)) }));
vi.mock("@/lib/runner-api", () => ({ resolvePort: () => 0 }));
vi.mock("@/lib/instance-storage", () => ({
  instanceStorage: {
    getJSON: (_key: string, fallback: unknown) => fallback,
    setJSON: () => {},
    getItem: () => null,
    setItem: () => {},
  },
}));
vi.mock("./useLiveClaudeSessionNames", () => ({ useLiveClaudeSessionNames: () => new Map() }));

import type { TerminalTab } from "./useTerminalManager";
import { useSessionManager, type UseSessionManagerReturn } from "./useSessionManager";

let container: HTMLDivElement;
let root: Root;

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  vi.stubGlobal(
    "fetch",
    vi.fn(() => Promise.reject(new Error("no runner in a unit test"))),
  );
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
  vi.unstubAllGlobals();
});

describe("useSessionManager — live tabs with no transcript", () => {
  it("lists a plain shell tab as a row, and clicking it focuses that tab", () => {
    const shell: TerminalTab = {
      id: "tab-shell",
      title: "bash",
      pid: 7,
      isAlive: true,
      exitCode: null,
    };
    const onFocusSessionTab = vi.fn();
    const onSelectSession = vi.fn();
    let manager: UseSessionManagerReturn | null = null;
    function Page() {
      manager = useSessionManager({
        tabs: [shell],
        sessionStates: { "tab-shell": "needs-input" },
        staleTabs: new Set<string>(),
        transcriptSessions: [],
        sessionsLoading: false,
        desktopNotify: false,
        onRefreshSessions: () => {},
        onResumeSession: () => {},
        onSelectSession,
        onFocusSessionTab,
      });
      return null;
    }
    act(() => root.render(createElement(Page)));

    const row = manager!.sessions.find((s) => s.zoneTabId === "tab-shell");
    expect(row).toBeDefined();
    expect(row!.liveStatus).toBe("needs-input");

    act(() => manager!.openSession(row!));
    expect(onFocusSessionTab).toHaveBeenCalledWith("tab-shell", "tab-shell", "tab");
    expect(onSelectSession).not.toHaveBeenCalled();
  });

  it("does not report an idle, silent plain shell as stuck", () => {
    // Never typed into, off screen, and silent past the 60 s staleness sweep:
    // the page puts it in `staleTabs`. The live check showed this row as
    // `frozen` — "Session appears stuck — may need resume".
    const shell: TerminalTab = {
      id: "tab-idle-shell",
      title: "bash",
      pid: 8,
      isAlive: true,
      exitCode: null,
    };
    let manager: UseSessionManagerReturn | null = null;
    function Page() {
      manager = useSessionManager({
        tabs: [shell],
        sessionStates: { "tab-idle-shell": "idle" },
        staleTabs: new Set<string>(["tab-idle-shell"]),
        transcriptSessions: [],
        sessionsLoading: false,
        desktopNotify: false,
        onRefreshSessions: () => {},
        onResumeSession: () => {},
        onSelectSession: () => {},
        onFocusSessionTab: () => {},
      });
      return null;
    }
    act(() => root.render(createElement(Page)));

    const row = manager!.sessions.find((s) => s.zoneTabId === "tab-idle-shell");
    expect(row?.source).toBe("tab");
    expect(row?.liveStatus).not.toBe("frozen");
    expect(manager!.frozenCount).toBe(0);
  });
});

describe("useSessionManager — closing a session from its row", () => {
  function mountManager(tabs: TerminalTab[], onCloseSessionTab?: (tabId: string) => void) {
    let manager: UseSessionManagerReturn | null = null;
    function Page() {
      manager = useSessionManager({
        tabs,
        sessionStates: {},
        staleTabs: new Set<string>(),
        transcriptSessions: [],
        sessionsLoading: false,
        desktopNotify: false,
        onRefreshSessions: () => {},
        onResumeSession: () => {},
        onSelectSession: () => {},
        onFocusSessionTab: () => {},
        onCloseSessionTab,
      });
      return null;
    }
    act(() => root.render(createElement(Page)));
    return manager!;
  }

  const shell: TerminalTab = { id: "tab-x", title: "bash", pid: 9, isAlive: true, exitCode: null };

  it("closes the row's live tab through the page's close path", () => {
    const close = vi.fn();
    const manager = mountManager([shell], close);
    const row = manager.sessions.find((s) => s.zoneTabId === "tab-x")!;
    expect(manager.closableTabId(row)).toBe("tab-x");
    act(() => manager.closeSession(row));
    expect(close.mock.calls).toEqual([["tab-x"]]);
  });

  it("offers no close for a row with no live tab, nor without a close path", () => {
    const close = vi.fn();
    const manager = mountManager([shell], close);
    const row = manager.sessions.find((s) => s.zoneTabId === "tab-x")!;
    const transcriptOnly = { ...row, source: "transcript" as const, zoneTabId: null };
    expect(manager.closableTabId(transcriptOnly as typeof row)).toBeNull();
    act(() => manager.closeSession(transcriptOnly as typeof row));
    expect(close).not.toHaveBeenCalled();
    expect(mountManager([shell]).closableTabId(row)).toBeNull();
  });
});
