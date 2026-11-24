/**
 * The session manager lists every live session (plan
 * `2026-10-11-runner-terminal-tmux-work-mode`, D5 / Phase 3): a live tab with
 * no transcript row gets a tab-sourced row, and that row offers none of the
 * transcript-only actions.
 *
 * vitest runs `environment: "node"` (no jsdom), so the card's action set is
 * asserted through `sessionCardActions`, the pure predicate its JSX renders
 * from — every gated button and menu item reads one of its fields.
 */

import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";

import {
  buildTabRows,
  isRowOffScreen,
  isRowOnScreen,
  mergeLiveRows,
  sessionCardStatus,
  tabLiveStatus,
  type LiveRowTab,
} from "./sessionManagerLiveRows";
import { sessionCardActions, sessionCardStatusInfo, sessionCardTooltip } from "./SessionCard";
import type { TranscriptSessionRow } from "./useSessionManager";
import type { SessionState } from "./useZoneLayout";

function tab(over: Partial<LiveRowTab> & { id: string }): LiveRowTab {
  return { title: "bash", isAlive: true, ...over };
}

function transcriptRow(over: Partial<TranscriptSessionRow> = {}): TranscriptSessionRow {
  return {
    source: "transcript",
    sessionId: "b770ae37-1ffa-4888-a5d1-89d058307adf",
    accountLabel: "gmail",
    configDir: "/home/u/.claude-gmail",
    projectPath: "/work/qontinui-runner",
    projectLabel: "qontinui-runner",
    displayName: "fix the scheduler",
    resumeName: null,
    registryName: null,
    firstMessagePreview: null,
    messageCount: 12,
    lastModified: "2026-10-11T10:00:00.000Z",
    hasPlans: false,
    liveStatus: "active-in-zone",
    zoneTabId: "t-claude",
    zoneIndex: null,
    remoteDeviceLabel: null,
    startedAt: null,
    durationMs: null,
    lastMessageType: "",
    lastMessageTimestamp: "2026-10-11T10:00:00.000Z",
    lastMessagePreview: "",
    workSummaryHint: "",
    likelyFrozen: false,
    isOrphaned: false,
    _transcript: {} as TranscriptSessionRow["_transcript"],
    ...over,
  };
}

const none: Record<string, SessionState> = {};
const noStale = new Set<string>();

describe("buildTabRows", () => {
  it("lists a plain shell tab as a tab-sourced row keyed by its tab id", () => {
    const rows = buildTabRows({
      tabs: [
        tab({
          id: "t-shell",
          spawnName: "build shell",
          workingDir: "/work/qontinui-web",
          createdAt: Date.UTC(2026, 9, 11, 9, 30),
        }),
      ],
      representedTabIds: new Set(),
      sessionStates: none,
    });
    expect(rows).toEqual([
      {
        source: "tab",
        sessionId: "t-shell",
        accountLabel: "default",
        configDir: "",
        projectPath: "/work/qontinui-web",
        projectLabel: "qontinui-web",
        displayName: "build shell",
        resumeName: null,
        registryName: null,
        firstMessagePreview: null,
        messageCount: 0,
        lastModified: "2026-10-11T09:30:00.000Z",
        hasPlans: false,
        liveStatus: "active-in-zone",
        zoneTabId: "t-shell",
        zoneIndex: null,
        remoteDeviceLabel: null,
        startedAt: "2026-10-11T09:30:00.000Z",
        durationMs: null,
        lastMessageType: "",
        lastMessageTimestamp: "2026-10-11T09:30:00.000Z",
        lastMessagePreview: "",
        workSummaryHint: "",
        likelyFrozen: false,
        isOrphaned: false,
        _transcript: null,
      },
    ]);
  });

  it("names a row through the titlebar resolver: registry name, then spawn name, never the cwd", () => {
    const [named, cwdTitled] = buildTabRows({
      tabs: [
        tab({ id: "t-1", claudeSessionId: "s-1", spawnName: "spawned", title: "x" }),
        tab({ id: "t-2", title: "/home/u/project", workingDir: "/home/u/project" }),
      ],
      representedTabIds: new Set(),
      sessionStates: none,
      registryNames: new Map([["s-1", "renamed by operator"]]),
    });
    expect(named.displayName).toBe("renamed by operator");
    expect(cwdTitled.displayName).toBe("Terminal");
  });

  it("carries the tab's state chip, so a hidden session needing input reads as such", () => {
    const rows = buildTabRows({
      tabs: [tab({ id: "a" }), tab({ id: "b" }), tab({ id: "c" }), tab({ id: "d" })],
      representedTabIds: new Set(),
      sessionStates: { a: "needs-input", b: "error", c: "completed" },
    });
    expect(rows.map((r) => [r.sessionId, r.liveStatus])).toEqual([
      ["a", "needs-input"],
      ["b", "error"],
      ["c", "completed"],
      ["d", "active-in-zone"],
    ]);
  });

  it("never reads an idle plain shell as stuck, however long it has been silent", () => {
    // The live check: a never-typed-in, off-screen shell tab showed `frozen`
    // and "Session appears stuck — may need resume". Silence is a shell's idle
    // state, and a tab row has nothing to resume.
    const staleShell = tab({ id: "t-shell" });
    const [row] = mergeLiveRows([], {
      tabs: [staleShell],
      sessionStates: { "t-shell": "idle" },
    });
    expect(row.source).toBe("tab");
    expect(row.liveStatus).toBe("active-in-zone");
    expect(row.likelyFrozen).toBe(false);
  });

  it("keeps the stuck heuristic for a transcript row joined to a silent tab", () => {
    // A Claude conversation that has written messages and then went silent
    // mid-run is what the staleness arm is for — unchanged by tab rows.
    expect(tabLiveStatus("t-claude", { "t-claude": "idle" }, new Set(["t-claude"]))).toBe("frozen");
  });

  it("lists a remote session tab, labelled with its device", () => {
    const [row] = buildTabRows({
      tabs: [
        tab({
          id: "t-remote",
          spawnName: "fleet worker",
          remote: {
            deviceId: "dev-1",
            deviceLabel: "spaceship",
            sessionId: "s-remote",
            remoteTerminalId: "rt-1",
            historyAvailable: false,
          },
        }),
      ],
      representedTabIds: new Set(),
      sessionStates: none,
    });
    expect(row.source).toBe("tab");
    expect(row.zoneTabId).toBe("t-remote");
    expect(row.remoteDeviceLabel).toBe("spaceship");
  });

  it("leaves out exited tabs, synthetic fixture tabs and tabs a transcript row already lists", () => {
    const rows = buildTabRows({
      tabs: [
        tab({ id: "t-dead", isAlive: false }),
        tab({ id: "synthetic-abc" }),
        tab({ id: "t-flagged", __synthetic: true }),
        tab({ id: "t-claude" }),
        tab({ id: "t-shell" }),
      ],
      representedTabIds: new Set(["t-claude"]),
      sessionStates: none,
    });
    expect(rows.map((r) => r.sessionId)).toEqual(["t-shell"]);
  });
});

describe("mergeLiveRows", () => {
  it("gives every live tab exactly one row: transcript rows first, then tab rows", () => {
    const transcript = transcriptRow({ zoneTabId: "t-claude" });
    const rows = mergeLiveRows([transcript], {
      tabs: [
        tab({ id: "t-claude", claudeSessionId: transcript.sessionId }),
        tab({ id: "t-shell" }),
      ],
      sessionStates: none,
    });
    expect(rows.map((r) => [r.source, r.zoneTabId])).toEqual([
      ["transcript", "t-claude"],
      ["tab", "t-shell"],
    ]);
  });

  it("lists a Claude tab whose transcript has no messages yet as a tab row", () => {
    // `message_count > 0` filtered its transcript out, so no row joins the tab.
    const rows = mergeLiveRows([], {
      tabs: [tab({ id: "t-new", claudeSessionId: "s-new", spawnName: "new session" })],
      sessionStates: none,
    });
    expect(rows.map((r) => [r.source, r.sessionId, r.displayName])).toEqual([
      ["tab", "t-new", "new session"],
    ]);
  });
});

describe("tabLiveStatus", () => {
  it("prefers the tracked state over staleness, and is active otherwise", () => {
    expect(tabLiveStatus("a", { a: "needs-input" }, new Set(["a"]))).toBe("needs-input");
    expect(tabLiveStatus("a", {}, new Set(["a"]))).toBe("frozen");
    expect(tabLiveStatus("a", { a: "working" }, noStale)).toBe("active-in-zone");
  });
});

describe("SessionCard actions by row source", () => {
  it("a tab-sourced row hides every transcript-only action", () => {
    const [row] = buildTabRows({
      tabs: [tab({ id: "t-shell" })],
      representedTabIds: new Set(),
      sessionStates: { "t-shell": "needs-input" },
    });
    expect(sessionCardActions(row)).toEqual({
      resume: false,
      viewTranscript: false,
      copySessionId: false,
      promote: false,
      commit: false,
      bulkSelect: false,
    });
  });

  it("a live transcript row keeps all of them", () => {
    expect(sessionCardActions(transcriptRow({ liveStatus: "needs-input" }))).toEqual({
      resume: true,
      viewTranscript: true,
      copySessionId: true,
      promote: true,
      commit: true,
      bulkSelect: true,
    });
  });

  it("a dormant transcript row can be resumed but not promoted or committed", () => {
    expect(sessionCardActions(transcriptRow({ liveStatus: "dormant" }))).toEqual({
      resume: true,
      viewTranscript: true,
      copySessionId: true,
      promote: false,
      commit: false,
      bulkSelect: true,
    });
  });

  it("a tab-sourced row's tooltip names a terminal, not a session id", () => {
    const [row] = buildTabRows({
      tabs: [tab({ id: "t-shell", spawnName: "build shell" })],
      representedTabIds: new Set(),
      sessionStates: none,
    });
    expect(sessionCardTooltip(row)).toBe("Terminal t-shell\nbuild shell");
  });
});

describe("isRowOnScreen", () => {
  it("marks only the row naming tmux mode's shown tab", () => {
    expect(isRowOnScreen({ zoneTabId: "t-1" }, "t-1")).toBe(true);
    expect(isRowOnScreen({ zoneTabId: "t-2" }, "t-1")).toBe(false);
    expect(isRowOnScreen({ zoneTabId: null }, "t-1")).toBe(false);
  });

  it("marks nothing in grid mode (no shown tab)", () => {
    expect(isRowOnScreen({ zoneTabId: "t-1" }, null)).toBe(false);
  });
});

describe("off-screen wording in tmux mode", () => {
  it("marks every row of this window but the shown one as off screen, in tmux only", () => {
    const shown = transcriptRow({ zoneTabId: "t-a" });
    const parked = transcriptRow({ zoneTabId: "t-b" });
    const noTab = transcriptRow({ zoneTabId: null, liveStatus: "dormant" });
    expect(isRowOffScreen(shown, "tmux", "t-a")).toBe(false);
    expect(isRowOffScreen(parked, "tmux", "t-a")).toBe(true);
    // A session not open in this window is not "off screen" — it is not here.
    expect(isRowOffScreen(noTab, "tmux", "t-a")).toBe(false);
    // tmux with nothing shown yet: every row of this window is off screen.
    expect(isRowOffScreen(shown, "tmux", null)).toBe(true);
    // Grid mode never says off screen.
    expect(isRowOffScreen(parked, "grid", null)).toBe(false);
  });

  it("an off-screen running session reads 'Active, off screen', never 'in zone'", () => {
    const parked = transcriptRow({ zoneTabId: "t-b" });
    expect(sessionCardStatus("active-in-zone", true)).toBe("active-off-screen");
    expect(sessionCardStatusInfo(parked, true)).toMatchObject({
      status: "active-off-screen",
      label: "Active, off screen",
    });
  });

  it("keeps the grid wording, and every state chip, unchanged", () => {
    expect(sessionCardStatusInfo(transcriptRow(), false)).toMatchObject({
      status: "active-in-zone",
      label: "Active in zone",
    });
    // A chip that describes the session, not its place, stays as it is.
    for (const status of ["needs-input", "error", "completed", "frozen"] as const) {
      expect(sessionCardStatus(status, true)).toBe(status);
    }
  });

  it("the card renders the shown status as its data-session-status", () => {
    const source = readFileSync(resolve(__dirname, "./SessionCard.tsx"), "utf8");
    expect(source).toContain("const statusInfo = sessionCardStatusInfo(session, isOffScreen);");
    expect(source).toContain("data-session-status={cardStatus}");
    const panel = readFileSync(resolve(__dirname, "./SessionManagerPanel.tsx"), "utf8");
    expect(panel).toContain("isOffScreen={isRowOffScreen(session, workMode, onScreenTabId)}");
    const page = readFileSync(resolve(__dirname, "./TerminalPage.tsx"), "utf8");
    expect(page).toMatch(
      /onScreenTabId=\{zoneLayout\.shownTabId\}\s*workMode=\{zoneLayout\.workMode\}/,
    );
  });
});
