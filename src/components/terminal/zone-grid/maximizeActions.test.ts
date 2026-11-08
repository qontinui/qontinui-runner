import { describe, it, expect } from "vitest";
import {
  MAXIMIZE_IN_TMUX_ERROR,
  parseZoneIndex,
  planMaximizeZone,
  planToggleMaximizeZone,
  buildMaximizeResult,
  buildZoneViewState,
} from "./maximizeActions";

/**
 * Plan `2026-08-19-session-info-dropdown-mount-gaps-remediation`, D2.
 *
 * These assert the two properties the action envelope exists for: an
 * out-of-range or wrongly-typed zone is a NAMED refusal rather than a silent
 * no-op, and a successful call reports the transition it actually made.
 */
describe("parseZoneIndex", () => {
  it("accepts an in-range integer", () => {
    expect(parseZoneIndex(0, 4)).toEqual({ ok: true, next: 0 });
    expect(parseZoneIndex(3, 4)).toEqual({ ok: true, next: 3 });
  });

  it("names the bound when the zone is out of range", () => {
    const r = parseZoneIndex(4, 4);
    expect(r.ok).toBe(false);
    if (r.ok) throw new Error("unreachable");
    expect(r.error).toContain("out of range");
    expect(r.error).toContain("4 zone(s)");
    expect(r.error).toContain("0..3");
  });

  it("refuses a negative zone", () => {
    expect(parseZoneIndex(-1, 4).ok).toBe(false);
  });

  it("refuses a numeric STRING rather than coercing it", () => {
    // A caller's typo must not be indistinguishable from a correct call.
    const r = parseZoneIndex("1", 4);
    expect(r.ok).toBe(false);
    if (r.ok) throw new Error("unreachable");
    expect(r.error).toContain("must be an integer");
    expect(r.error).toContain('"1"');
  });

  it("refuses a non-integer number, undefined and null", () => {
    expect(parseZoneIndex(1.5, 4).ok).toBe(false);
    expect(parseZoneIndex(undefined, 4).ok).toBe(false);
    expect(parseZoneIndex(null, 4).ok).toBe(false);
  });

  it("refuses every zone when the layout has none", () => {
    expect(parseZoneIndex(0, 0).ok).toBe(false);
  });
});

describe("planMaximizeZone", () => {
  it("resolves to the requested zone regardless of what is already maximized", () => {
    expect(planMaximizeZone(2, 4)).toEqual({ ok: true, next: 2 });
  });
});

describe("planToggleMaximizeZone", () => {
  it("maximizes a zone that is not currently maximized", () => {
    expect(planToggleMaximizeZone(2, 4, null)).toEqual({ ok: true, next: 2 });
    expect(planToggleMaximizeZone(2, 4, 1)).toEqual({ ok: true, next: 2 });
  });

  it("restores when the requested zone is already the maximized one", () => {
    expect(planToggleMaximizeZone(2, 4, 2)).toEqual({ ok: true, next: null });
  });

  it("validates the zone before consulting the current state", () => {
    expect(planToggleMaximizeZone(9, 4, 9).ok).toBe(false);
  });
});

describe("buildMaximizeResult", () => {
  it("reports the transition, so a caller can prove the action did something", () => {
    expect(buildMaximizeResult(null, 2, 4)).toEqual({
      maximizedZone: 2,
      previousMaximizedZone: null,
      zoneCount: 4,
      changed: true,
    });
  });

  it("reports changed:false when the requested state was already current", () => {
    // Not an error — the request IS satisfied — but a caller asserting "my
    // action changed something" must be able to tell the two apart.
    expect(buildMaximizeResult(2, 2, 4).changed).toBe(false);
    expect(buildMaximizeResult(null, null, 4).changed).toBe(false);
  });

  it("reports a restore as a real transition", () => {
    expect(buildMaximizeResult(3, null, 4)).toEqual({
      maximizedZone: null,
      previousMaximizedZone: 3,
      zoneCount: 4,
      changed: true,
    });
  });
});

describe("maximize in tmux work mode", () => {
  // tmux mode shows one session at full size and `useZoneLayout` ignores
  // every maximize request there: reporting `changed: true` would be a lie.
  it("maximize-zone is refused by name, not reported as a change", () => {
    expect(planMaximizeZone(0, 1, "tmux")).toEqual({ ok: false, error: MAXIMIZE_IN_TMUX_ERROR });
    expect(MAXIMIZE_IN_TMUX_ERROR).toContain("tmux work mode");
    expect(MAXIMIZE_IN_TMUX_ERROR).toContain("set-terminal-work-mode");
  });

  it("toggle-maximize-zone is refused the same way", () => {
    expect(planToggleMaximizeZone(0, 1, null, "tmux")).toEqual({
      ok: false,
      error: MAXIMIZE_IN_TMUX_ERROR,
    });
  });

  it("grid mode plans as before", () => {
    expect(planMaximizeZone(1, 4, "grid")).toEqual({ ok: true, next: 1 });
    expect(planToggleMaximizeZone(1, 4, 1, "grid")).toEqual({ ok: true, next: null });
  });
});

/**
 * Plan `2026-11-08-tenant-badge-parity-and-oracle-fixtures`, Phase 4: the
 * `get-zone-view-state` envelope carries `pageId`, `assignments` and
 * `workMode`, which the session-info oracle builds its fixtures from instead of
 * an evaluated `localStorage` read. Literal envelopes, not recomputed ones.
 */
describe("buildZoneViewState", () => {
  it("returns every field, including pageId, assignments and workMode", () => {
    expect(
      buildZoneViewState({
        maximizedZone: null,
        zoneCount: 4,
        layoutId: "quad",
        pageId: "page-7",
        assignments: { 0: "term-a", 1: "term-b", 2: "term-c" },
        workMode: "grid",
      }),
    ).toEqual({
      maximizedZone: null,
      zoneCount: 4,
      layoutId: "quad",
      pageId: "page-7",
      assignments: { 0: "term-a", 1: "term-b", 2: "term-c" },
      workMode: "grid",
    });
  });

  it("reports tmux work mode and a maximized zone as given", () => {
    expect(
      buildZoneViewState({
        maximizedZone: 1,
        zoneCount: 2,
        layoutId: "split",
        pageId: "default",
        assignments: {},
        workMode: "tmux",
      }),
    ).toEqual({
      maximizedZone: 1,
      zoneCount: 2,
      layoutId: "split",
      pageId: "default",
      assignments: {},
      workMode: "tmux",
    });
  });

  it("copies the assignments, so the envelope cannot alias the live map", () => {
    const live: Record<number, string> = { 0: "term-a" };
    const env = buildZoneViewState({
      maximizedZone: null,
      zoneCount: 1,
      layoutId: "single",
      pageId: "p",
      assignments: live,
      workMode: "grid",
    });
    env.assignments[1] = "term-x";
    expect(live).toEqual({ 0: "term-a" });
    expect(env.assignments).not.toBe(live);
  });
});
