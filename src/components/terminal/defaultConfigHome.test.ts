/**
 * The default Claude home is never typed as `CLAUDE_CONFIG_DIR`, however it is
 * spelled (trailing slash, backslashes, Windows case), and the path itself
 * comes from the backend.
 */

import { describe, expect, it, vi } from "vitest";

const invokeMock = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => invokeMock(...args),
}));

import {
  accountRosterOf,
  isDefaultConfigHome,
  loadDefaultConfigHome,
  mayBeDefaultConfigHome,
  resolveAccountDir,
  typedConfigDir,
} from "./defaultConfigHome";

describe("typedConfigDir", () => {
  it("drops the default home, keeps every other account", () => {
    expect(typedConfigDir("/home/u/.claude", "/home/u/.claude", false)).toBeUndefined();
    expect(typedConfigDir("/home/u/.claude/", "/home/u/.claude", false)).toBeUndefined();
    expect(typedConfigDir("/home/u/.claude-x", "/home/u/.claude", false)).toBe("/home/u/.claude-x");
    expect(typedConfigDir(undefined, "/home/u/.claude", false)).toBeUndefined();
    expect(typedConfigDir("  ", "/home/u/.claude", false)).toBeUndefined();
  });

  it("compares Windows paths slash- and case-insensitively", () => {
    expect(isDefaultConfigHome("C:\\Users\\U\\.claude", "C:/Users/u/.claude", true)).toBe(true);
    expect(isDefaultConfigHome("/home/U/.claude", "/home/u/.claude", false)).toBe(false);
  });

  it("with no known home, nothing is treated as the default", () => {
    expect(typedConfigDir("/home/u/.claude", null, false)).toBe("/home/u/.claude");
  });
});

describe("loadDefaultConfigHome", () => {
  it("reads the backend's path once", async () => {
    invokeMock.mockResolvedValue("/home/u/.claude");
    expect(await loadDefaultConfigHome()).toBe("/home/u/.claude");
    expect(await loadDefaultConfigHome()).toBe("/home/u/.claude");
    expect(invokeMock).toHaveBeenCalledTimes(1);
    expect(invokeMock).toHaveBeenCalledWith("claude_default_config_home");
  });
});

describe("resolveAccountDir — the one rule for resuming under a recorded dir", () => {
  it("the default home is typed as nothing and is NOT shell-checked (it is never typed)", () => {
    const home = "/home/Zoë O'Brien/.claude";
    expect(resolveAccountDir(home, home, false)).toEqual({
      kind: "resolved",
      recordDir: home,
      typedDir: undefined,
    });
  });

  it("any other dir is typed as itself when shell-safe, refused when not", () => {
    expect(resolveAccountDir("/home/u/.claude-x", "/home/u/.claude", false)).toEqual({
      kind: "resolved",
      recordDir: "/home/u/.claude-x",
      typedDir: "/home/u/.claude-x",
    });
    expect(resolveAccountDir("/home/u/$(x)", "/home/u/.claude", false)).toEqual({
      kind: "unknown",
      reason: "config-dir-rejected",
    });
  });

  it("with the home unreadable, a dir that may be it is unknown — never typed on a guess", () => {
    expect(resolveAccountDir("/home/u/.claude/", null, false)).toEqual({
      kind: "unknown",
      reason: "default-home-unknown",
    });
    expect(resolveAccountDir("C:\\Users\\U\\.CLAUDE", null, true)).toEqual({
      kind: "unknown",
      reason: "default-home-unknown",
    });
    expect(mayBeDefaultConfigHome("/home/u/.claude-x", false)).toBe(false);
  });
});

describe("accountRosterOf — the dirs the chooser offers to type", () => {
  const dirs = ["/home/u/.claude", "/home/u/.claude-x", "/home/u/.claude-y"];

  it("excludes the default home (it has its own untyped entry)", () => {
    expect(accountRosterOf(dirs, "/home/u/.claude", false)).toEqual([
      "/home/u/.claude-x",
      "/home/u/.claude-y",
    ]);
  });

  it("with the home unknown, excludes every dir that may be it instead of listing it to type", () => {
    expect(accountRosterOf(dirs, null, false)).toEqual(["/home/u/.claude-x", "/home/u/.claude-y"]);
  });
});
