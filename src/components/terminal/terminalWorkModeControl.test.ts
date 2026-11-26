/**
 * The one work-mode rule the CommandBar toggle, Ctrl+Shift+Y, `/terminal.mode`
 * and the UI Bridge `set-terminal-work-mode` action share (plan
 * `2026-10-11-runner-terminal-tmux-work-mode`, Phase 4).
 */

import { describe, expect, it } from "vitest";

import type { TerminalWorkMode } from "./terminalWorkMode";
import {
  applyWorkModeRequest,
  describeWorkModeChange,
  parseWorkModeRequest,
  resolveWorkModeRequest,
  workModeLabel,
  type WorkModeRequest,
} from "./terminalWorkModeControl";

/** A target that records every call, in order. */
function target(workMode: TerminalWorkMode, showSidebar: boolean) {
  const calls: string[] = [];
  return {
    calls,
    t: {
      workMode,
      showSidebar,
      enterTmuxMode: () => calls.push("enter"),
      exitTmuxMode: () => calls.push("exit"),
      setShowSidebar: (open: boolean) => calls.push(`sidebar:${open}`),
    },
  };
}

describe("parseWorkModeRequest", () => {
  it.each([
    [undefined, "toggle"],
    [null, "toggle"],
    ["", "toggle"],
    ["  ", "toggle"],
    ["grid", "grid"],
    ["tmux", "tmux"],
    ["TMUX", "tmux"],
    [" Grid ", "grid"],
  ])("reads %j as %s", (raw, request) => {
    expect(parseWorkModeRequest(raw)).toEqual({ ok: true, request });
  });

  it.each([["tile"], ["single"], [1], [true], [{}]])(
    "refuses %j with the accepted set named, never as a toggle",
    (raw) => {
      expect(parseWorkModeRequest(raw)).toEqual({
        ok: false,
        error: `mode must be one of: grid, tmux — got ${JSON.stringify(raw)}`,
      });
    },
  );
});

describe("resolveWorkModeRequest", () => {
  it.each<[WorkModeRequest, TerminalWorkMode, TerminalWorkMode]>([
    ["toggle", "grid", "tmux"],
    ["toggle", "tmux", "grid"],
    ["grid", "grid", "grid"],
    ["grid", "tmux", "grid"],
    ["tmux", "grid", "tmux"],
    ["tmux", "tmux", "tmux"],
  ])("%s from %s → %s", (request, current, next) => {
    expect(resolveWorkModeRequest(request, current)).toBe(next);
  });
});

describe("applyWorkModeRequest", () => {
  it("enters tmux from grid and opens a closed session manager", () => {
    const { calls, t } = target("grid", false);
    expect(applyWorkModeRequest(t, "toggle")).toEqual({
      previousMode: "grid",
      mode: "tmux",
      changed: true,
      sidebarOpened: true,
      message: "entered tmux mode — session manager opened",
    });
    expect(calls).toEqual(["enter", "sidebar:true"]);
  });

  it("enters tmux without touching a session manager that is already open", () => {
    const { calls, t } = target("grid", true);
    expect(applyWorkModeRequest(t, "tmux")).toEqual({
      previousMode: "grid",
      mode: "tmux",
      changed: true,
      sidebarOpened: false,
      message: "entered tmux mode",
    });
    expect(calls).toEqual(["enter"]);
  });

  it("leaves tmux for the grid and leaves the sidebar as it is", () => {
    const { calls, t } = target("tmux", true);
    expect(applyWorkModeRequest(t, "toggle")).toEqual({
      previousMode: "tmux",
      mode: "grid",
      changed: true,
      sidebarOpened: false,
      message: "left tmux mode — grid restored",
    });
    expect(calls).toEqual(["exit"]);
  });

  it("is a no-op for grid when already in grid", () => {
    const { calls, t } = target("grid", false);
    expect(applyWorkModeRequest(t, "grid")).toEqual({
      previousMode: "grid",
      mode: "grid",
      changed: false,
      sidebarOpened: false,
      message: "already in grid mode",
    });
    expect(calls).toEqual([]);
  });

  it("re-opens a closed navigator when tmux is asked for while already on", () => {
    const { calls, t } = target("tmux", false);
    expect(applyWorkModeRequest(t, "tmux")).toEqual({
      previousMode: "tmux",
      mode: "tmux",
      changed: false,
      sidebarOpened: true,
      message: "already in tmux mode — session manager opened",
    });
    expect(calls).toEqual(["sidebar:true"]);
  });

  it("does nothing at all for tmux when tmux and the navigator are both on", () => {
    const { calls, t } = target("tmux", true);
    expect(applyWorkModeRequest(t, "tmux").changed).toBe(false);
    expect(calls).toEqual([]);
  });
});

describe("labels and sentences", () => {
  it("names each mode the way the toggle shows it", () => {
    expect(workModeLabel("grid")).toBe("Grid");
    expect(workModeLabel("tmux")).toBe("tmux");
  });

  it("says the grid came back when tmux is left", () => {
    expect(describeWorkModeChange("grid", true, false)).toBe("left tmux mode — grid restored");
  });
});
