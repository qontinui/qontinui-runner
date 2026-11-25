/**
 * Selecting a TAB-sourced session row never drives the transcript panel
 * (plan `2026-10-11-runner-terminal-tmux-work-mode`, Phase 4 — the
 * integration fix carried over from Phase 3).
 *
 * A tab-sourced row (a plain shell, a session before its first message) is
 * keyed by its TAB id and has no transcript. Its click selects that id as the
 * current session so the card highlights, and before this fix an open
 * transcript panel then rendered the PREVIOUS session's messages under it; a
 * tab row whose tab was gone opened the transcript panel outright on a tab id.
 *
 * `onFocusSessionTab` is an inline closure in `TerminalSessionContext`, so its
 * two decisions live in pure functions asserted here: `routeOpenSession`
 * (where a click goes) and `rightPanelModeAfterRowSelect` (what the panel does).
 */

import { describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => undefined) }));

import { rightPanelModeAfterRowSelect } from "./sessionManagerLiveRows";
import { routeOpenSession, type UnifiedSession } from "./useSessionManager";

type Panel = "transcript" | "workflow" | "analysis" | "findings" | "file-ownership";

const transcriptRow = (zoneTabId: string | null) =>
  ({
    source: "transcript",
    zoneTabId,
    _transcript: {},
  }) as unknown as Pick<UnifiedSession, "source" | "zoneTabId" | "_transcript">;

const tabRow = (zoneTabId: string) =>
  ({ source: "tab", zoneTabId, _transcript: null }) as unknown as Pick<
    UnifiedSession,
    "source" | "zoneTabId" | "_transcript"
  >;

const tabs = [
  { id: "t-shell", isAlive: true },
  { id: "t-claude", isAlive: true },
  { id: "t-dead", isAlive: false },
];

describe("routeOpenSession", () => {
  it("focuses the live tab behind a tab row", () => {
    expect(routeOpenSession(tabRow("t-shell"), tabs)).toEqual({
      kind: "focus-tab",
      tabId: "t-shell",
    });
  });

  it("focuses the live tab behind a transcript row", () => {
    expect(routeOpenSession(transcriptRow("t-claude"), tabs)).toEqual({
      kind: "focus-tab",
      tabId: "t-claude",
    });
  });

  it("opens the transcript of a transcript row with no live tab", () => {
    expect(routeOpenSession(transcriptRow(null), tabs)).toEqual({ kind: "transcript" });
    expect(routeOpenSession(transcriptRow("t-dead"), tabs)).toEqual({ kind: "transcript" });
  });

  it("does nothing for a tab row whose tab is gone — there is no transcript to open", () => {
    expect(routeOpenSession(tabRow("t-dead"), tabs)).toEqual({ kind: "none" });
    expect(routeOpenSession(tabRow("t-missing"), tabs)).toEqual({ kind: "none" });
  });
});

describe("rightPanelModeAfterRowSelect", () => {
  it("closes an open transcript panel when a tab row is selected", () => {
    expect(rightPanelModeAfterRowSelect<Panel>("tab", "transcript")).toBeNull();
  });

  it.each<Panel | null>([null, "workflow", "analysis", "findings", "file-ownership"])(
    "leaves a %s panel alone when a tab row is selected",
    (mode) => {
      expect(rightPanelModeAfterRowSelect<Panel>("tab", mode)).toBe(mode);
    },
  );

  it.each<Panel | null>([null, "transcript", "workflow", "analysis", "findings", "file-ownership"])(
    "leaves a %s panel alone when a transcript row is selected",
    (mode) => {
      expect(rightPanelModeAfterRowSelect<Panel>("transcript", mode)).toBe(mode);
    },
  );
});
