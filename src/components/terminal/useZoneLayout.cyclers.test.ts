/**
 * Zone focus cyclers — plan 2026-08-23-single-source-derived-facts item 5.
 *
 * `focusNextNeedsInput` clamped its walk to `min(zones, tabs)` while the status
 * strip's hand-copied `focusNextError` walked every zone. Assignments are not
 * dense, so with 4 zones and tabs only in zones 2 and 3 the needs-input pill
 * read "2 need input · Tab to cycle" and Tab reached neither. Every cycler now
 * runs through ONE walk, `findNextZone`. The first half pins that pure walk;
 * the second drives the HOOK itself, so the bindings on top of it — the zone
 * count it is handed, the `changed:false` rule, the needs-input -> error
 * fallback, and the un-maximize — are tested where they live.
 *
 * vitest runs `environment: "node"` with no DOM and no React test renderer, so
 * the hook is driven through a minimal hooks harness: `react` is mocked with a
 * slot-indexed `useState` whose setters really update state, `useCallback`
 * returns its function, `useRef` a ref, and `useEffect` is inert (the effects
 * only reconcile/persist, and the persisted fixture is already reconciled).
 * Each `render()` re-invokes the hook against the current state, exactly as
 * React would re-render after a state update.
 */

import { beforeEach, describe, expect, it, vi } from "vitest";

// ---------------------------------------------------------------------------
// Minimal hooks harness
// ---------------------------------------------------------------------------

const harness = vi.hoisted(() => {
  const slots: unknown[] = [];
  let cursor = 0;
  return {
    slots,
    reset() {
      slots.length = 0;
      cursor = 0;
    },
    beginRender() {
      cursor = 0;
    },
    useState<T>(init: T | (() => T)) {
      const slot = cursor++;
      if (!(slot in slots)) {
        slots[slot] = typeof init === "function" ? (init as () => T)() : init;
      }
      const set = (v: T | ((prev: T) => T)) => {
        slots[slot] = typeof v === "function" ? (v as (p: T) => T)(slots[slot] as T) : v;
      };
      return [slots[slot] as T, set] as const;
    },
  };
});

vi.mock("react", () => ({
  useState: harness.useState,
  useCallback: <F>(fn: F) => fn,
  useRef: <T>(init: T) => ({ current: init }),
  useEffect: () => {},
}));

const persisted = vi.hoisted(() => ({ value: null as unknown }));
vi.mock("@/lib/instance-storage", () => ({
  instanceStorage: {
    getJSON: () => persisted.value,
    setJSON: () => {},
  },
}));

import {
  findNextZone,
  useZoneLayout,
  type SessionState,
  type ZoneAssignments,
} from "./useZoneLayout";

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

// ---------------------------------------------------------------------------
// Hook-level: the bindings on top of the walk
// ---------------------------------------------------------------------------

/**
 * A `quad` (4 zones) holding TWO tabs parked in zones 2 and 3. Two tabs is
 * the point: a hook that handed the walk `tabIds.length` (2) instead of
 * `layout.zones.length` (4) would only ever visit the empty zones 0 and 1.
 */
function mountSparseQuad(opts: { focusedZone?: number; assignments?: ZoneAssignments } = {}) {
  harness.reset();
  persisted.value = {
    layoutId: "quad",
    assignments: opts.assignments ?? { 2: "tab-x", 3: "tab-y" },
    focusedZone: opts.focusedZone ?? 0,
  };
  const tabIds = ["tab-x", "tab-y"];
  // Named like a component: it is the harness's stand-in for one, re-invoked
  // per "render" against the current hook state.
  function ZoneLayoutHost() {
    harness.beginRender();
    return useZoneLayout(tabIds, "cyclers-test", "main");
  }
  return ZoneLayoutHost;
}

describe("useZoneLayout — state cyclers (hook level)", () => {
  beforeEach(() => harness.reset());

  it("focusNextNeedsInput walks the LAYOUT's zones, reaching tabs parked in zones 2 and 3", () => {
    const render = mountSparseQuad();
    expect(render().layout.zones).toHaveLength(4);
    const states: Record<string, SessionState> = { "tab-x": "needs-input", "tab-y": "needs-input" };

    expect(render().focusNextNeedsInput(states)).toBe(true);
    expect(render().focusedZone).toBe(2);
    expect(render().focusNextNeedsInput(states)).toBe(true);
    expect(render().focusedZone).toBe(3);
    // wraps
    expect(render().focusNextNeedsInput(states)).toBe(true);
    expect(render().focusedZone).toBe(2);
  });

  it("focusNextError reaches the errored tabs on the same sparse fixture", () => {
    const render = mountSparseQuad();
    const states: Record<string, SessionState> = { "tab-x": "error", "tab-y": "working" };
    expect(render().focusNextError(states)).toBe(true);
    expect(render().focusedZone).toBe(2);
    // Only one errored zone: the walk lands back on it.
    expect(render().focusNextError(states)).toBe(true);
    expect(render().focusedZone).toBe(2);
  });

  it("focusNextInState(..., 'error') is what focusNextError binds", () => {
    const render = mountSparseQuad();
    const states: Record<string, SessionState> = { "tab-x": "working", "tab-y": "error" };
    expect(render().focusNextInState(states, "error")).toBe(true);
    expect(render().focusedZone).toBe(3);
    // ...and it does NOT match other states.
    expect(render().focusNextInState(states, "needs-input")).toBe(false);
    expect(render().focusedZone).toBe(3);
  });

  it("focusNextNeedsInput falls back to an errored zone when none is waiting", () => {
    const render = mountSparseQuad();
    const states: Record<string, SessionState> = { "tab-x": "working", "tab-y": "error" };
    expect(render().focusNextNeedsInput(states)).toBe(true);
    expect(render().focusedZone).toBe(3);
  });

  it("prefers needs-input over error when both exist", () => {
    const render = mountSparseQuad();
    const states: Record<string, SessionState> = { "tab-x": "error", "tab-y": "needs-input" };
    expect(render().focusNextNeedsInput(states)).toBe(true);
    expect(render().focusedZone).toBe(3);
  });

  it("un-maximizes on a hit", () => {
    const render = mountSparseQuad();
    render().setMaximizedZone(1);
    expect(render().maximizedZone).toBe(1);
    expect(render().focusNextError({ "tab-x": "error" })).toBe(true);
    expect(render().maximizedZone).toBeNull();
  });

  it("negative control: no tab in the target state -> false, focus and maximize unchanged", () => {
    const render = mountSparseQuad({ focusedZone: 1 });
    render().setMaximizedZone(1);
    const states: Record<string, SessionState> = { "tab-x": "working", "tab-y": "idle" };
    expect(render().focusNextNeedsInput(states)).toBe(false);
    expect(render().focusNextError(states)).toBe(false);
    expect(render().focusedZone).toBe(1);
    expect(render().maximizedZone).toBe(1);
  });
});

describe("useZoneLayout — keyboard zone navigation (hook level)", () => {
  beforeEach(() => harness.reset());

  it("focusNextZone / focusPrevZone move between the occupied zones 2 and 3", () => {
    const render = mountSparseQuad();
    expect(render().focusNextZone()).toEqual({ changed: true });
    expect(render().focusedZone).toBe(2);
    expect(render().focusNextZone()).toEqual({ changed: true });
    expect(render().focusedZone).toBe(3);
    expect(render().focusPrevZone()).toEqual({ changed: true });
    expect(render().focusedZone).toBe(2);
  });

  it("reports changed:false when the only occupied zone is already focused", () => {
    const render = mountSparseQuad({ focusedZone: 3, assignments: { 3: "tab-y" } });
    expect(render().focusNextZone()).toEqual({ changed: false });
    expect(render().focusPrevZone()).toEqual({ changed: false });
    expect(render().focusedZone).toBe(3);
  });

  it("moves (changed:true) from an empty zone to the single occupied one", () => {
    const render = mountSparseQuad({ focusedZone: 0, assignments: { 3: "tab-y" } });
    expect(render().focusNextZone()).toEqual({ changed: true });
    expect(render().focusedZone).toBe(3);
  });
});
