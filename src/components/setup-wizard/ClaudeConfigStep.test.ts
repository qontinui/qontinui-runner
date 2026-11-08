/**
 * The setup wizard's Claude step only ever ADDS accounts: `claudeDirsToAdd`
 * picks the checked dirs that are not already configured. The rendered step
 * (Finish, Add Directory, configured rows) is covered by
 * `ClaudeConfigStep.render.test.tsx`.
 */

import { describe, expect, it } from "vitest";

import { CONFIGURED_SOURCE, claudeDirsToAdd, type DiscoveredDir } from "./ClaudeConfigStep";

const dir = (path: string, source = "auto-detected"): DiscoveredDir => ({
  path,
  label: path.split("/").pop() ?? path,
  source,
});

describe("claudeDirsToAdd", () => {
  it("adds nothing when discovery found nothing", () => {
    expect(claudeDirsToAdd([], new Set())).toEqual([]);
  });

  it("never re-adds a configured dir, checked or not", () => {
    const dirs = [dir("/home/u/.claude-a", CONFIGURED_SOURCE), dir("/home/u/.claude-b")];
    expect(claudeDirsToAdd(dirs, new Set(["/home/u/.claude-a"]))).toEqual([]);
    expect(claudeDirsToAdd(dirs, new Set())).toEqual([]);
  });

  it("adds the checked new dirs in discovery order", () => {
    const dirs = [
      dir("/home/u/.claude-a", CONFIGURED_SOURCE),
      dir("/home/u/.claude-b"),
      dir("/home/u/.claude-c"),
      dir("/picked", "manual"),
    ];
    expect(
      claudeDirsToAdd(dirs, new Set(["/picked", "/home/u/.claude-c", "/home/u/.claude-a"])),
    ).toEqual(["/home/u/.claude-c", "/picked"]);
  });

  it("ignores a checked path that is no longer listed (a removed manual dir)", () => {
    const dirs = [dir("/home/u/.claude-b")];
    expect(claudeDirsToAdd(dirs, new Set(["/home/u/.claude-b", "/gone"]))).toEqual([
      "/home/u/.claude-b",
    ]);
  });
});
