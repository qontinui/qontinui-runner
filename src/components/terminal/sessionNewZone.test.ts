/**
 * `planNewZone` — where the session search palette's "open in a new zone"
 * puts a session (plan
 * `2026-10-13-session-manager-chrome-style-tab-organization-and-operator-search`,
 * Phase 4).
 */

import { describe, expect, it } from "vitest";

import { firstEmptyZone, planNewZone } from "./sessionNewZone";

describe("planNewZone", () => {
  it("tmux mode shows the session in its one viewport", () => {
    expect(
      planNewZone({ workMode: "tmux", layoutId: "single", zoneCount: 1, assignments: { 0: "t" } }),
    ).toEqual({ kind: "tmux" });
  });

  it("takes the first empty zone when there is one", () => {
    expect(
      planNewZone({
        workMode: "grid",
        layoutId: "quad",
        zoneCount: 4,
        assignments: { 0: "a", 2: "c" },
      }),
    ).toEqual({ kind: "empty", zone: 1 });
  });

  it("grows the grid one step when every zone is taken; the new zone follows the old ones", () => {
    expect(
      planNewZone({
        workMode: "grid",
        layoutId: "split",
        zoneCount: 2,
        assignments: { 0: "a", 1: "b" },
      }),
    ).toEqual({ kind: "grow", layoutId: "quad", zone: 2 });
  });

  it("says full — never a silent plain open — when the grid cannot grow", () => {
    expect(
      planNewZone({
        workMode: "grid",
        layoutId: "flow-grid",
        zoneCount: 12,
        assignments: Object.fromEntries(Array.from({ length: 12 }, (_, i) => [i, `t${i}`])),
      }),
    ).toEqual({ kind: "full" });
  });
});

describe("firstEmptyZone", () => {
  it("is -1 when every zone is held", () => {
    expect(firstEmptyZone(2, { 0: "a", 1: "b" })).toBe(-1);
    expect(firstEmptyZone(0, {})).toBe(-1);
    expect(firstEmptyZone(3, { 0: "a", 2: "c" })).toBe(1);
  });
});
