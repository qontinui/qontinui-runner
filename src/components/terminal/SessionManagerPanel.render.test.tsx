// @vitest-environment jsdom
/**
 * The Session Manager panel's cards, entered where the page enters them: the
 * REAL `SessionManagerPanel` rendered in React with the props `TerminalPage`
 * hands it, its real `SessionCard` children included. Each card shows its
 * row's two status words — activity and location — as text and as
 * `data-session-activity` / `data-session-location` (plan
 * `2026-10-13-session-manager-chrome-style-tab-organization-and-operator-search`,
 * Phase 1), so a hidden running session reads "Hidden here", never "in zone".
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
import {
  SESSION_ACTIVITIES,
  injectedStatus,
  type SessionActivity,
  type SessionLocation,
} from "./sessionStatusModel";
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
function runningTabRow(
  tabId: string,
  title: string,
  location: SessionLocation = "hidden-here",
  activity: SessionActivity = "working",
): UnifiedSession {
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
    status: injectedStatus(location, activity),
    zoneTabId: tabId,
    zoneIndex: null,
    remoteDeviceLabel: null,
    startedAt: null,
    durationMs: null,
    lastMessageType: "",
    lastMessageTimestamp: "",
    lastMessagePreview: "",
    workSummaryHint: "",
  };
}

/** The manager surface the panel reads, over a fixed list of rows. */
function managerOver(
  sessions: UnifiedSession[],
  close: {
    closableTabId?: (s: UnifiedSession) => string | null;
    closeSession?: (s: UnifiedSession) => void;
  } = {},
  extra: { unidentifiedCount?: number | null } = {},
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
    selectAllVisible: noop,
    clearSelection: noop,
    bulkResume: noop,
    pinnedIds: new Set<string>(),
    togglePin: noop,
    sessionLabels: {},
    setSessionLabel: noop,
    allAccounts: [],
    savedConfigDirs: [],
    activityCounts: Object.fromEntries(
      SESSION_ACTIVITIES.map((a) => [a, sessions.filter((s) => s.status.activity === a).length]),
    ),
    noOutputCount: 0,
    needsInputCount: 0,
    unidentifiedCount: extra.unidentifiedCount === undefined ? 0 : extra.unidentifiedCount,
  } as unknown as UseSessionManagerReturn;
}

/** The status attributes each rendered card carries, by its row title. */
function cardStatuses(): Record<string, [string | null, string | null]> {
  const out: Record<string, [string | null, string | null]> = {};
  for (const card of Array.from(container.querySelectorAll("[data-session-location]"))) {
    const title = card.textContent ?? "";
    const key = title.includes("shown-shell")
      ? "shown"
      : title.includes("hidden-shell")
        ? "hidden"
        : title;
    out[key] = [
      card.getAttribute("data-session-location"),
      card.getAttribute("data-session-activity"),
    ];
  }
  return out;
}

describe("SessionManagerPanel — two-axis status on every card", () => {
  it("a hidden running session reads 'Hidden here', the shown one 'On screen'", () => {
    const rows = [
      runningTabRow("tab-shown", "shown-shell", "on-screen"),
      runningTabRow("tab-hidden", "hidden-shell", "hidden-here"),
    ];
    function Page() {
      return SessionManagerPanel({
        manager: managerOver(rows),
        selectedSessionId: null,
        onScreenTabId: "tab-shown",
      });
    }
    act(() => root.render(createElement(Page)));

    expect(cardStatuses()).toEqual({
      shown: ["on-screen", "working"],
      hidden: ["hidden-here", "working"],
    });
    // The words are rendered, not only the attributes (never color alone).
    const words = Array.from(container.querySelectorAll("[data-testid='session-card-status']")).map(
      (el) => el.textContent,
    );
    expect(words).toEqual(["Working·On screen", "Working·Hidden here"]);
    // No card carries the retired single-axis attribute.
    expect(container.querySelector("[data-session-status]")).toBeNull();
  });

  it("groups by the activity word from the one label table", () => {
    const rows = [
      runningTabRow("tab-a", "a-shell", "hidden-here", "needs-you"),
      runningTabRow("tab-b", "b-shell", "hidden-here", "idle-at-prompt"),
    ];
    function Page() {
      return SessionManagerPanel({ manager: managerOver(rows), selectedSessionId: null });
    }
    act(() => root.render(createElement(Page)));
    const text = container.textContent ?? "";
    expect(text).toContain("Needs you(1)");
    expect(text).toContain("Idle at prompt(1)");
  });

  it("reports unidentified Claude processes as a count, and UNKNOWN as unknown — never zero", () => {
    const rows = [runningTabRow("tab-a", "a-shell")];
    function Page({ n }: { n: number | null }) {
      return SessionManagerPanel({
        manager: managerOver(rows, {}, { unidentifiedCount: n }),
        selectedSessionId: null,
      });
    }
    act(() => root.render(createElement(Page, { n: 3 })));
    expect(container.textContent).toContain("3 unidentified Claude processes");
    act(() => root.render(createElement(Page, { n: null })));
    expect(container.textContent).toContain("Unidentified Claude processes: unknown");
    act(() => root.render(createElement(Page, { n: 0 })));
    expect(container.textContent).not.toContain("nidentified");
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
