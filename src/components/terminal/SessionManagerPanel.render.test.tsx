// @vitest-environment jsdom
/**
 * The Session Manager panel's cards, entered where the page enters them: the
 * REAL `SessionManagerPanel` rendered in React with the props `TerminalPage`
 * hands it, its real `SessionCard` children included. In tmux work mode only
 * the shown tab sits in a zone, so every other running session of this window
 * must NOT read "Active in zone" (plan `2026-10-11-runner-terminal-tmux-work-mode`,
 * Bug 3) — its card says it is off screen. Grid mode keeps the old wording.
 *
 * The panel's IO-bound siblings (steward, worktrees, fleet picker, past
 * sessions) are stubbed: they are not what this test observes.
 */

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(() => Promise.resolve(null)) }));
vi.mock("@/lib/runner-api", () => ({ resolvePort: () => 0 }));
vi.mock("./StewardControl", () => ({ StewardControl: () => null }));
vi.mock("../worktrees/WorktreesPanel", () => ({ WorktreesPanel: () => null }));
vi.mock("./FleetSessionPicker", () => ({ FleetSessionPicker: () => null }));
vi.mock("./PastSessionsView", () => ({ PastSessionsView: () => null }));

import { SessionManagerPanel } from "./SessionManagerPanel";
import type { UnifiedSession, UseSessionManagerReturn } from "./useSessionManager";

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

/** A running tab-sourced row of this window, keyed by its tab id. */
function runningTabRow(tabId: string, title: string): UnifiedSession {
  return {
    source: "tab",
    _transcript: null,
    sessionId: tabId,
    accountLabel: "",
    configDir: "",
    projectPath: "",
    projectLabel: "",
    displayName: title,
    resumeName: null,
    registryName: null,
    firstMessagePreview: null,
    messageCount: 0,
    lastModified: "",
    hasPlans: false,
    liveStatus: "active-in-zone",
    zoneTabId: tabId,
    zoneIndex: null,
    remoteDeviceLabel: null,
    startedAt: null,
    durationMs: null,
    lastMessageType: "",
    lastMessageTimestamp: "",
    lastMessagePreview: "",
    workSummaryHint: "",
    likelyFrozen: false,
    isOrphaned: false,
  };
}

/** The manager surface the panel reads, over a fixed list of rows. */
function managerOver(
  sessions: UnifiedSession[],
  close: {
    closableTabId?: (s: UnifiedSession) => string | null;
    closeSession?: (s: UnifiedSession) => void;
  } = {},
): UseSessionManagerReturn {
  const noop = () => {};
  return {
    sessions,
    loading: false,
    accountUsage: [],
    launchCommands: {},
    defaultLaunchCommand: null,
    fileLocks: [],
    searchQuery: "",
    setSearchQuery: noop,
    statusFilter: "all",
    setStatusFilter: noop,
    accountFilter: "all",
    setAccountFilter: noop,
    groupBy: "status",
    setGroupBy: noop,
    sortBy: "recent",
    setSortBy: noop,
    refresh: noop,
    resumeSession: noop,
    viewTranscript: noop,
    openSession: noop,
    copySessionId: noop,
    closableTabId: close.closableTabId ?? (() => null),
    closeSession: close.closeSession ?? noop,
    selectedIds: new Set<string>(),
    toggleSelect: noop,
    selectAllFrozen: noop,
    clearSelection: noop,
    bulkResume: noop,
    pinnedIds: new Set<string>(),
    togglePin: noop,
    sessionLabels: {},
    setSessionLabel: noop,
    allAccounts: [],
    savedConfigDirs: [],
    frozenCount: 0,
    needsInputCount: 0,
    activeCount: sessions.length,
  } as unknown as UseSessionManagerReturn;
}

/** The `data-session-status` each rendered card carries, by its row title. */
function cardStatuses(): Record<string, string | null> {
  const out: Record<string, string | null> = {};
  for (const card of Array.from(container.querySelectorAll("[data-session-status]"))) {
    const title = card.textContent ?? "";
    const key = title.includes("shown-shell")
      ? "shown"
      : title.includes("hidden-shell")
        ? "hidden"
        : title;
    out[key] = card.getAttribute("data-session-status");
  }
  return out;
}

describe("SessionManagerPanel — tmux mode off-screen wording", () => {
  it("in tmux mode a running session that is not on screen reads off screen, not in zone", () => {
    const rows = [
      runningTabRow("tab-shown", "shown-shell"),
      runningTabRow("tab-hidden", "hidden-shell"),
    ];
    // The page mounts the panel as JSX; calling the component from a wrapper
    // component renders the same tree (its hooks belong to the wrapper).
    function Page() {
      return SessionManagerPanel({
        manager: managerOver(rows),
        selectedSessionId: null,
        onScreenTabId: "tab-shown",
        workMode: "tmux",
      });
    }
    act(() => root.render(createElement(Page)));

    const statuses = cardStatuses();
    expect(statuses.hidden).toBe("active-off-screen");
    expect(statuses.shown).toBe("active-in-zone");
    // Only the shown session's card carries the "on screen" marker.
    const onScreenCards = Array.from(container.querySelectorAll("[data-session-status]")).filter(
      (card) => (card.textContent ?? "").includes("on screen"),
    );
    expect(onScreenCards.map((card) => card.textContent?.includes("shown-shell"))).toEqual([true]);
    // The status dot's tooltip is the card's label.
    const labels = Array.from(container.querySelectorAll("[data-session-status] [title]")).map(
      (el) => el.getAttribute("title"),
    );
    expect(labels).toContain("Active, off screen");
  });
});

describe("SessionManagerPanel — closing a session from its row", () => {
  it("a row with a live tab offers a close button (UI Bridge id), which closes that session", () => {
    const rows = [runningTabRow("tab-a", "shell-a"), runningTabRow("tab-b", "shell-b")];
    const closeSession = vi.fn();
    function Page() {
      return SessionManagerPanel({
        manager: managerOver(rows, {
          // Only tab-a still has a live tab in this window.
          closableTabId: (s) => (s.zoneTabId === "tab-a" ? "tab-a" : null),
          closeSession,
        }),
        selectedSessionId: null,
        onScreenTabId: "tab-b",
        workMode: "tmux",
      });
    }
    act(() => root.render(createElement(Page)));

    // The UI Bridge id is spelled out: it is the contract an agent drives.
    const close = container.querySelector(
      "[data-ui-bridge-id='terminal.session-card-close-tab-a']",
    ) as HTMLElement | null;
    expect(close).not.toBeNull();
    expect(
      container.querySelector("[data-ui-bridge-id='terminal.session-card-close-tab-b']"),
    ).toBeNull();
    act(() => close!.click());
    expect(closeSession).toHaveBeenCalledTimes(1);
    expect(closeSession.mock.calls[0][0].zoneTabId).toBe("tab-a");
  });
});
