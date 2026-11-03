/**
 * The first-run wizard must never empty the Claude account roster.
 *
 * `save_claude_config_dirs` replaces the machine-global roster with exactly the
 * list it is given, so the step's save decision is the guard: an empty
 * selection saves nothing. The component needs jsdom to render, which this
 * repo's vitest setup (`environment: "node"`) does not have, so the decision is
 * exported and tested directly (the TierStep pattern).
 */

import { describe, expect, it } from "vitest";

import { claudeDirsToSave, type DiscoveredDir } from "./ClaudeConfigStep";

const dir = (path: string, source = "auto-detected"): DiscoveredDir => ({
  path,
  label: path.split("/").pop() ?? path,
  source,
});

describe("claudeDirsToSave", () => {
  it("saves nothing when discovery found nothing (the Linux wipe)", () => {
    expect(claudeDirsToSave([], new Set())).toBeNull();
  });

  it("saves nothing when every found dir was unchecked", () => {
    const dirs = [dir("/home/u/.claude-a", "configured"), dir("/home/u/.claude-b")];
    expect(claudeDirsToSave(dirs, new Set())).toBeNull();
  });

  it("saves the checked dirs in discovery order", () => {
    const dirs = [
      dir("/home/u/.claude-a", "configured"),
      dir("/home/u/.claude-b"),
      dir("/home/u/.claude-c"),
    ];
    expect(claudeDirsToSave(dirs, new Set(["/home/u/.claude-c", "/home/u/.claude-a"]))).toEqual([
      "/home/u/.claude-a",
      "/home/u/.claude-c",
    ]);
  });

  it("ignores a checked path that is no longer listed (a removed manual dir)", () => {
    const dirs = [dir("/home/u/.claude-a")];
    expect(claudeDirsToSave(dirs, new Set(["/home/u/.claude-a", "/gone"]))).toEqual([
      "/home/u/.claude-a",
    ]);
  });
});
