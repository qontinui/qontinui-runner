/**
 * The conditional `predicts` behind the terminal page's effect signatures
 * (plan `2026-10-22-runner-ui-bridge-makes-an-agent-guess-its-own-vocabulary`,
 * Phase 2). Coverage of WHICH actions carry a signature lives in
 * `src/lib/ui-bridge/action-effect-coverage.test.ts`; this file pins WHAT the
 * branching ones predict, with hand-written expectations.
 */

import { describe, expect, it } from "vitest";
import type { ActionParams, SemanticSnapshot } from "@qontinui/ui-bridge";

import {
  countLabelledZones,
  CREATE_PLAIN_TERMINAL_SIGNATURE,
  CREATE_TERMINAL_SIGNATURE,
  LAUNCH_CREATE_PLAIN_SIGNATURE,
  predictMoveTerminal,
  predictPopOutActiveTerminal,
  predictPopOutPage,
  zoneViewSignature,
  predictWorkMode,
  predictZoneView,
  PTY_SPAWN_SETTLE_MS,
  type ZoneLabelInputs,
} from "./terminalPageSignatures";
import {
  loadProfileSignature,
  predictLoadProfile,
  predictSelectLayout,
  selectLayoutSignature,
} from "./zonePickerSignatures";
import type { ZoneProfile } from "./zoneProfileStorage";

function snap(...ids: string[]): SemanticSnapshot {
  return { elements: ids.map((id) => ({ id })) } as unknown as SemanticSnapshot;
}

function p(params: Record<string, unknown>): ActionParams {
  return { action: "x", params };
}

const GRID = snap("terminal-input-a", "terminal-input-b", "terminal-zone-header-0");
const ZONE_SURFACE = ["terminal-input-*", "terminal-zone-header-*"];

describe("spawn signatures", () => {
  it("create-plain predicts a terminal-input element appearing, scoped to the zone surface", () => {
    expect(LAUNCH_CREATE_PLAIN_SIGNATURE.id).toBe("terminal-launch-menu.create-plain");
    expect(LAUNCH_CREATE_PLAIN_SIGNATURE.predicts(p({ count: 2 }), GRID)).toEqual({
      elementsAppear: [{ id: "terminal-input-*" }],
    });
    expect(LAUNCH_CREATE_PLAIN_SIGNATURE.scope.elementIds).toEqual(ZONE_SURFACE);
    expect(CREATE_PLAIN_TERMINAL_SIGNATURE.predicts(p({}), snap())).toEqual({
      elementsAppear: [{ id: "terminal-input-*" }],
    });
  });

  it("create-terminal shares the zone-surface scope (auto-assignment may add a header)", () => {
    expect(CREATE_TERMINAL_SIGNATURE.scope.elementIds).toEqual(ZONE_SURFACE);
  });

  it("spawns wait out the PTY spawn settle window, not the SDK fallback band", () => {
    expect(LAUNCH_CREATE_PLAIN_SIGNATURE.settleMs).toBe(PTY_SPAWN_SETTLE_MS);
    expect(CREATE_TERMINAL_SIGNATURE.settleMs).toBe(PTY_SPAWN_SETTLE_MS);
    expect(loadProfileSignature({ current: { profiles: {}, liveTabCount: 0 } }).settleMs).toBe(PTY_SPAWN_SETTLE_MS);
    expect(PTY_SPAWN_SETTLE_MS).toBeGreaterThanOrEqual(1000);
  });
});

describe("pop-out-page", () => {
  it("the active page predicts its tab elements leaving only when it has terminals", () => {
    expect(predictPopOutPage(p({}), GRID, "default", 2)).toEqual({
      elementsDisappear: [{ id: "terminal-input-*" }],
    });
    expect(predictPopOutPage(p({}), GRID, "default", 0)).toEqual({});
    expect(predictPopOutPage(p({ pageId: "default" }), GRID, "default", 0)).toEqual({});
  });

  it("another page falls back to whether this window renders any tab", () => {
    expect(predictPopOutPage(p({ pageId: "other" }), GRID, "default", 0)).toEqual({
      elementsDisappear: [{ id: "terminal-input-*" }],
    });
    expect(predictPopOutPage(p({ pageId: "other" }), snap(), "default", 3)).toEqual({});
  });
});

describe("move-terminal-to-window", () => {
  it("in an unbound window, a tab rendered here and sent elsewhere disappears", () => {
    expect(
      predictMoveTerminal(p({ terminalId: "a", windowLabel: "term-1" }), GRID, "main", null),
    ).toEqual({ elementsDisappear: [{ id: "terminal-input-a" }] });
  });

  it("a tab sent here is not predicted: its page may be bound to another window", () => {
    expect(
      predictMoveTerminal(p({ terminalId: "z", windowLabel: "main" }), GRID, "main", null),
    ).toEqual({});
  });

  it("in a page-bound window the binding outranks the move, so nothing changes", () => {
    expect(
      predictMoveTerminal(p({ terminalId: "a", windowLabel: "main" }), GRID, "term-1", "page-1"),
    ).toEqual({});
  });

  it("a move that does not touch this window, or a refused bag, predicts nothing", () => {
    expect(
      predictMoveTerminal(p({ terminalId: "a", windowLabel: "main" }), GRID, "main", null),
    ).toEqual({});
    expect(
      predictMoveTerminal(p({ terminalId: "z", windowLabel: "term-2" }), GRID, "main", null),
    ).toEqual({});
    expect(predictMoveTerminal(p({ terminalId: "a" }), GRID, "main", null)).toEqual({});
  });
});

describe("pop-out-active-terminal", () => {
  it("leaves an unbound window, and stays in a page-bound one", () => {
    expect(predictPopOutActiveTerminal(null)).toEqual({
      elementsDisappear: [{ id: "terminal-input-*" }],
    });
    expect(predictPopOutActiveTerminal("page-1")).toEqual({});
  });
});

describe("signatures read live state when predict runs, not when they were built", () => {
  // `useUIComponent` keeps the signature object it was first given, so a
  // value captured at build time would stay at its mount-time value forever.
  it("load-profile sees profiles loaded after the signature was built", () => {
    const live = { current: { profiles: {} as Record<string, ZoneProfile>, liveTabCount: 0 } };
    const sig = loadProfileSignature(live);
    live.current = {
      profiles: { p: { sessions: [{}, {}] } as unknown as ZoneProfile },
      liveTabCount: 0,
    };
    expect(sig.predicts(p({ name: "p" }), GRID)).toEqual({
      elementsAppear: [{ id: "terminal-input-*" }],
    });
  });

  it("select-layout sees the tab count at predict time", () => {
    const tabs = { current: 0 };
    const sig = selectLayoutSignature(tabs);
    tabs.current = 3;
    expect(sig.predicts(p({ layoutId: "single" }), GRID)).toEqual({});
  });

  it("restore-zone sees a zone maximized after the signature was built", () => {
    const facts = {
      current: { maximizedZone: null as number | null, zoneCount: 4, labelledZonesWhenTiled: 2 },
    };
    const sig = zoneViewSignature("restore", facts);
    facts.current = { ...facts.current, maximizedZone: 1 };
    expect(sig.predicts(p({}), GRID)).toEqual({
      elementsAppear: [{ id: "terminal-zone-header-*" }],
    });
  });
});

describe("zone view (maximize / restore / toggle)", () => {
  const tiled = { maximizedZone: null, zoneCount: 4, labelledZonesWhenTiled: 2 };
  const maximized = { maximizedZone: 1, zoneCount: 4, labelledZonesWhenTiled: 2 };

  it("maximizing from the tiled grid removes the zone headers", () => {
    expect(predictZoneView("maximize", p({ zoneIndex: 1 }), GRID, tiled)).toEqual({
      elementsDisappear: [{ id: "terminal-zone-header-*" }],
    });
  });

  it("restoring brings headers back only when the tiled grid renders some", () => {
    expect(predictZoneView("restore", p({}), snap(), maximized)).toEqual({
      elementsAppear: [{ id: "terminal-zone-header-*" }],
    });
    expect(predictZoneView("restore", p({}), GRID, tiled)).toEqual({});
    expect(
      predictZoneView("restore", p({}), snap(), { ...maximized, labelledZonesWhenTiled: 0 }),
    ).toEqual({});
    expect(
      predictZoneView("restore", p({}), snap(), { ...maximized, labelledZonesWhenTiled: null }),
    ).toEqual({});
  });

  it("toggle follows the keyboard rule; an out-of-range index predicts nothing", () => {
    expect(predictZoneView("toggle", p({ zoneIndex: 1 }), snap(), maximized)).toEqual({
      elementsAppear: [{ id: "terminal-zone-header-*" }],
    });
    expect(
      predictZoneView("toggle", p({ zoneIndex: 1 }), snap(), {
        ...maximized,
        labelledZonesWhenTiled: 0,
      }),
    ).toEqual({});
    expect(predictZoneView("toggle", p({ zoneIndex: 2 }), GRID, tiled)).toEqual({
      elementsDisappear: [{ id: "terminal-zone-header-*" }],
    });
    expect(predictZoneView("maximize", p({ zoneIndex: 9 }), GRID, tiled)).toEqual({});
  });
});

describe("set-terminal-work-mode", () => {
  it("entering tmux from a grid with headers removes them", () => {
    expect(predictWorkMode(p({ mode: "tmux" }), GRID, "grid")).toEqual({
      elementsDisappear: [{ id: "terminal-zone-header-*" }],
    });
    expect(predictWorkMode(p({}), GRID, "grid")).toEqual({
      elementsDisappear: [{ id: "terminal-zone-header-*" }],
    });
  });

  it("predicts nothing it cannot see: leaving tmux, a no-op request, a malformed one", () => {
    expect(predictWorkMode(p({}), snap("terminal-input-a"), "tmux")).toEqual({});
    expect(predictWorkMode(p({ mode: "grid" }), GRID, "grid")).toEqual({});
    expect(predictWorkMode(p({ mode: "tmux" }), snap("terminal-input-a"), "grid")).toEqual({});
    expect(predictWorkMode(p({ mode: "tiles" }), GRID, "grid")).toEqual({});
  });
});

describe("countLabelledZones mirrors ZoneGrid's header rule", () => {
  const base: ZoneLabelInputs = {
    layoutId: "grid-2x2",
    zoneCount: 4,
    assignments: { 0: "a", 1: "b", 3: "gone" },
    tabs: [{ id: "a" }, { id: "b" }, { id: "plan", type: "plan" }],
    focusedZone: 0,
    pinnedZones: new Set<number>(),
    compactView: false,
  };

  it("counts occupied zones only (an assignment to a missing tab is empty)", () => {
    expect(countLabelledZones(base)).toBe(2);
  });

  it("forced compact drops unfocused, unpinned terminal zones", () => {
    expect(countLabelledZones({ ...base, compactView: true })).toBe(1);
    expect(countLabelledZones({ ...base, compactView: true, pinnedZones: new Set([1]) })).toBe(2);
  });

  it("a single-zone layout renders none; flow-grid is unknown", () => {
    expect(countLabelledZones({ ...base, zoneCount: 1 })).toBe(0);
    expect(countLabelledZones({ ...base, layoutId: "flow-grid" })).toBeNull();
  });
});

describe("zone pickers", () => {
  it("select-layout to a single-zone preset removes headers only when auto-grow cannot undo it", () => {
    expect(predictSelectLayout(p({ layoutId: "single" }), GRID, 1)).toEqual({
      elementsDisappear: [{ id: "terminal-zone-header-*" }],
    });
    // Two tabs overflow a single zone: auto-grow re-expands, headers stay.
    expect(predictSelectLayout(p({ layoutId: "single" }), GRID, 2)).toEqual({});
    expect(predictSelectLayout(p({ layoutId: "no-such-layout" }), GRID, 1)).toEqual({});
  });

  it("select-layout resolves a window count like the handler, and predicts nothing for a named page", () => {
    expect(predictSelectLayout(p({ layoutId: "1" }), GRID, 1)).toEqual({
      elementsDisappear: [{ id: "terminal-zone-header-*" }],
    });
    // A pageId may name an off-screen page: its headers are not in this registry.
    expect(predictSelectLayout(p({ layoutId: "single", pageId: "page-2" }), GRID, 1)).toEqual({});
  });

  it("load-profile predicts spawned tabs only when the profile has more sessions than live tabs", () => {
    const profile = {
      layoutId: "grid-2x2",
      labels: {},
      notes: {},
      pins: [],
      autoApprovePatterns: [],
      sessions: [
        { zoneIndex: 0, claudeSessionId: "s0" },
        { zoneIndex: 1, claudeSessionId: "s1" },
      ],
    } as ZoneProfile;
    expect(predictLoadProfile(p({ name: "two" }), { two: profile }, 1)).toEqual({
      elementsAppear: [{ id: "terminal-input-*" }],
    });
    expect(predictLoadProfile(p({ name: "two" }), { two: profile }, 2)).toEqual({});
    expect(predictLoadProfile(p({ name: "missing" }), { two: profile }, 0)).toEqual({});
  });
});
