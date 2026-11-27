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
  isRowOnScreen,
  mergeLiveRows,
  visibleTabIdsForLayout,
  type LiveRowTab,
} from "./sessionManagerLiveRows";
import { sessionCardActions, sessionCardStatusDisplay, sessionCardTooltip } from "./SessionCard";
import { injectedStatus } from "./sessionStatusModel";
import type { TranscriptSessionRow } from "./useSessionManager";

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
    status: injectedStatus("on-screen", "working"),
    zoneTabId: "t-claude",
    zoneIndex: null,
    remoteDeviceLabel: null,
    startedAt: null,
    durationMs: null,
    lastMessageType: "",
    lastMessageTimestamp: "2026-10-11T10:00:00.000Z",
    lastMessagePreview: "",
    workSummaryHint: "",
    _transcript: {} as TranscriptSessionRow["_transcript"],
    ...over,
  };
}

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
        zoneTabId: "t-shell",
        zoneIndex: null,
        remoteDeviceLabel: null,
        startedAt: "2026-10-11T09:30:00.000Z",
        durationMs: null,
        lastMessageType: "",
        lastMessageTimestamp: "2026-10-11T09:30:00.000Z",
        lastMessagePreview: "",
        workSummaryHint: "",
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
      registryNames: new Map([["s-1", "renamed by operator"]]),
    });
    expect(named.displayName).toBe("renamed by operator");
    expect(cwdTitled.displayName).toBe("Terminal");
  });

  it("leaves status to the classifier: a tab row carries none of its own", () => {
    const [row] = mergeLiveRows([], { tabs: [tab({ id: "t-shell" })] });
    expect(row.source).toBe("tab");
    expect("status" in row).toBe(false);
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
    });
    expect(rows.map((r) => [r.source, r.sessionId, r.displayName])).toEqual([
      ["tab", "t-new", "new session"],
    ]);
  });
});

describe("SessionCard actions by row source", () => {
  it("a tab-sourced row hides every transcript-only action", () => {
    const [base] = buildTabRows({
      tabs: [tab({ id: "t-shell" })],
      representedTabIds: new Set(),
    });
    const row = { ...base, status: injectedStatus("hidden-here", "needs-you") };
    expect(sessionCardActions(row)).toEqual({
      resume: false,
      viewTranscript: false,
      copySessionId: false,
      promote: false,
      commit: false,
    });
  });

  it("a live transcript row keeps all of them", () => {
    expect(
      sessionCardActions(transcriptRow({ status: injectedStatus("hidden-here", "needs-you") })),
    ).toEqual({
      resume: true,
      viewTranscript: true,
      copySessionId: true,
      promote: true,
      commit: true,
    });
  });

  it("a not-running transcript row can be resumed but not promoted or committed", () => {
    expect(
      sessionCardActions(transcriptRow({ status: injectedStatus("not-running", "exited") })),
    ).toEqual({
      resume: true,
      viewTranscript: true,
      copySessionId: true,
      promote: false,
      commit: false,
    });
  });

  it("a tab-sourced row's tooltip names a terminal, not a session id", () => {
    const [row] = buildTabRows({
      tabs: [tab({ id: "t-shell", spawnName: "build shell" })],
      representedTabIds: new Set(),
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

describe("visibleTabIdsForLayout — the On screen / Hidden here split", () => {
  const assignments = { 0: "t-a", 1: "t-b" };

  it("tmux mode: only the shown tab is visible", () => {
    expect([
      ...visibleTabIdsForLayout({
        workMode: "tmux",
        shownTabId: "t-b",
        assignments,
        maximizedZone: null,
      }),
    ]).toEqual(["t-b"]);
    expect(
      visibleTabIdsForLayout({
        workMode: "tmux",
        shownTabId: null,
        assignments,
        maximizedZone: null,
      }).size,
    ).toBe(0);
  });

  it("grid mode: every zoned tab, or only the maximized zone's", () => {
    expect(
      [
        ...visibleTabIdsForLayout({
          workMode: "grid",
          shownTabId: null,
          assignments,
          maximizedZone: null,
        }),
      ].sort(),
    ).toEqual(["t-a", "t-b"]);
    expect([
      ...visibleTabIdsForLayout({
        workMode: "grid",
        shownTabId: null,
        assignments,
        maximizedZone: 1,
      }),
    ]).toEqual(["t-b"]);
  });
});

describe("the card's status words", () => {
  it("a hidden running session reads its location, never 'in zone'", () => {
    expect(sessionCardStatusDisplay(injectedStatus("hidden-here", "working"))).toMatchObject({
      location: "hidden-here",
      activity: "working",
      locationLabel: "Hidden here",
      activityLabel: "Working",
    });
  });

  it("renders data-session-location + data-session-activity, not data-session-status", () => {
    const source = readFileSync(resolve(__dirname, "./SessionCard.tsx"), "utf8");
    expect(source).toContain("data-session-location={statusInfo.location}");
    expect(source).toContain("data-session-activity={statusInfo.activity}");
    expect(source).not.toContain("data-session-status");
  });
});
