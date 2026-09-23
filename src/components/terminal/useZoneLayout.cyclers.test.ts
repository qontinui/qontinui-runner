/**
 * Zone focus cyclers — plan 2026-08-23-single-source-derived-facts item 5.
 *
 * `focusNextNeedsInput` clamped its walk to `min(zones, tabs)` while the status
 * strip's hand-copied `focusNextError` walked every zone. Assignments are not
 * dense, so with 4 zones and tabs only in zones 2 and 3 the needs-input pill
 * read "2 need input · Tab to cycle" and Tab reached neither. Every cycler now
 * runs through ONE walk, `findNextZone`, which these tests pin (vitest runs
 * `environment: "node"`, so the hook's thin bindings are exercised through the
 * pure walk they delegate to — repo precedent `useZoneLayout.restore.test.ts`).
 */

import { describe, expect, it } from "vitest";

import { findNextZone, type SessionState, type ZoneAssignments } from "./useZoneLayout";

const ZONES = 4;
/** Sparse: zones 0 and 1 empty, tabs parked in zones 2 and 3. */
const SPARSE: ZoneAssignments = { 2: "tab-x", 3: "tab-y" };

function inState(states: Record<string, SessionState>, state: SessionState) {
  return (tabId: string) => states[tabId] === state;
}

/** Drive the cycler `n` times from `start`, returning the focus sequence. */
function cycle(
  start: number,
  n: number,
  matches: (tabId: string) => boolean,
  assignments: ZoneAssignments = SPARSE,
  direction: 1 | -1 = 1,
): (number | null)[] {
  const seq: (number | null)[] = [];
  let focused = start;
  for (let i = 0; i < n; i++) {
    const next = findNextZone(ZONES, focused, assignments, matches, direction);
    seq.push(next);
    if (next !== null) focused = next;
  }
  return seq;
}

describe("findNextZone — state cycling over sparse assignments", () => {
  it("needs-input: reaches both tabs parked in zones 2 and 3, then wraps", () => {
    const states: Record<string, SessionState> = { "tab-x": "needs-input", "tab-y": "needs-input" };
    // The old clamp (maxZones = min(4, 2) = 2) walked only zones 0-1 and
    // returned nothing here.
    expect(cycle(0, 3, inState(states, "needs-input"))).toEqual([2, 3, 2]);
  });

  it("error: the identical fixture in `error` yields the identical focus sequence", () => {
    const needs: Record<string, SessionState> = { "tab-x": "needs-input", "tab-y": "needs-input" };
    const errors: Record<string, SessionState> = { "tab-x": "error", "tab-y": "error" };
    expect(cycle(0, 3, inState(errors, "error"))).toEqual(
      cycle(0, 3, inState(needs, "needs-input")),
    );
  });

  it("negative control: no tab in the target state returns null (focus stays put)", () => {
    const states: Record<string, SessionState> = { "tab-x": "working", "tab-y": "idle" };
    expect(findNextZone(ZONES, 1, SPARSE, inState(states, "needs-input"))).toBeNull();
    expect(findNextZone(ZONES, 1, SPARSE, inState(states, "error"))).toBeNull();
  });

  it("skips a non-matching occupied zone and keeps walking", () => {
    const states: Record<string, SessionState> = { "tab-x": "working", "tab-y": "needs-input" };
    expect(cycle(0, 2, inState(states, "needs-input"))).toEqual([3, 3]);
  });

  it("returns null for a layout with no zones", () => {
    expect(findNextZone(0, 0, {}, () => true)).toBeNull();
  });
});

describe("findNextZone — keyboard zone navigation (any occupied zone)", () => {
  it("next: cycles between the occupied zones 2 and 3, never landing on empty 0/1", () => {
    // The old `focusableZoneCount = min(4, 2) = 2` cycled 0 <-> 1 — both empty.
    expect(cycle(0, 3, () => true)).toEqual([2, 3, 2]);
  });

  it("prev: walks backwards over the same occupied zones", () => {
    expect(cycle(0, 3, () => true, SPARSE, -1)).toEqual([3, 2, 3]);
  });

  it("dense layouts behave exactly as before", () => {
    const dense: ZoneAssignments = { 0: "a", 1: "b", 2: "c", 3: "d" };
    expect(cycle(0, 4, () => true, dense)).toEqual([1, 2, 3, 0]);
    expect(cycle(0, 4, () => true, dense, -1)).toEqual([3, 2, 1, 0]);
  });

  it("a single occupied zone has nowhere else to go (returns the focused zone itself)", () => {
    // The hook reports `changed: false` when the walk lands back on focusedZone.
    expect(findNextZone(ZONES, 2, { 2: "only" }, () => true)).toBe(2);
  });
});
