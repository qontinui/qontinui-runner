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
  CREATE_PLAIN_TERMINAL_SIGNATURE,
  LAUNCH_CREATE_PLAIN_SIGNATURE,
  POP_OUT_PAGE_SIGNATURE,
  predictMoveTerminal,
  predictZoneView,
} from "./terminalPageSignatures";
import { predictLoadProfile, predictSelectLayout } from "./zonePickerSignatures";
import type { ZoneProfile } from "./zoneProfileStorage";

function snap(...ids: string[]): SemanticSnapshot {
  return { elements: ids.map((id) => ({ id })) } as unknown as SemanticSnapshot;
}

function p(params: Record<string, unknown>): ActionParams {
  return { action: "x", params };
}

const GRID = snap("terminal-input-a", "terminal-input-b", "terminal-zone-header-0");

describe("spawn signatures", () => {
  it("create-plain predicts a terminal-input element appearing, scoped to the zone surface", () => {
    expect(LAUNCH_CREATE_PLAIN_SIGNATURE.id).toBe("terminal-launch-menu.create-plain");
    expect(LAUNCH_CREATE_PLAIN_SIGNATURE.predicts(p({ count: 2 }), GRID)).toEqual({
      elementsAppear: [{ id: "terminal-input-*" }],
    });
    expect(LAUNCH_CREATE_PLAIN_SIGNATURE.scope.elementIds).toEqual([
      "terminal-input-*",
      "terminal-zone-header-*",
    ]);
    expect(CREATE_PLAIN_TERMINAL_SIGNATURE.predicts(p({}), snap())).toEqual({
      elementsAppear: [{ id: "terminal-input-*" }],
    });
  });

  it("pop-out-page predicts tab elements leaving only when there are any", () => {
    expect(POP_OUT_PAGE_SIGNATURE.predicts(p({}), GRID)).toEqual({
      elementsDisappear: [{ id: "terminal-input-*" }],
    });
    expect(POP_OUT_PAGE_SIGNATURE.predicts(p({}), snap())).toEqual({});
  });
});

describe("move-terminal-to-window", () => {
  it("a tab rendered here and sent elsewhere disappears", () => {
    expect(
      predictMoveTerminal(p({ terminalId: "a", windowLabel: "term-1" }), GRID, "main"),
    ).toEqual({ elementsDisappear: [{ id: "terminal-input-a" }] });
  });

  it("a tab not rendered here and sent here appears", () => {
    expect(predictMoveTerminal(p({ terminalId: "z", windowLabel: "main" }), GRID, "main")).toEqual({
      elementsAppear: [{ id: "terminal-input-z" }],
    });
  });

  it("a move that does not touch this window, or a refused bag, predicts nothing", () => {
    expect(predictMoveTerminal(p({ terminalId: "a", windowLabel: "main" }), GRID, "main")).toEqual(
      {},
    );
    expect(
      predictMoveTerminal(p({ terminalId: "z", windowLabel: "term-2" }), GRID, "main"),
    ).toEqual({});
    expect(predictMoveTerminal(p({ terminalId: "a" }), GRID, "main")).toEqual({});
  });
});

describe("zone view (maximize / restore / toggle)", () => {
  const tiled = { maximizedZone: null, zoneCount: 4 };
  const maximized = { maximizedZone: 1, zoneCount: 4 };

  it("maximizing from the tiled grid removes the zone headers", () => {
    expect(predictZoneView("maximize", p({ zoneIndex: 1 }), GRID, tiled)).toEqual({
      elementsDisappear: [{ id: "terminal-zone-header-*" }],
    });
  });

  it("restoring from the maximized view brings the headers back", () => {
    expect(predictZoneView("restore", p({}), snap(), maximized)).toEqual({
      elementsAppear: [{ id: "terminal-zone-header-*" }],
    });
    expect(predictZoneView("restore", p({}), GRID, tiled)).toEqual({});
  });

  it("toggle follows the keyboard rule; an out-of-range index predicts nothing", () => {
    expect(predictZoneView("toggle", p({ zoneIndex: 1 }), snap(), maximized)).toEqual({
      elementsAppear: [{ id: "terminal-zone-header-*" }],
    });
    expect(predictZoneView("toggle", p({ zoneIndex: 2 }), GRID, tiled)).toEqual({
      elementsDisappear: [{ id: "terminal-zone-header-*" }],
    });
    expect(predictZoneView("maximize", p({ zoneIndex: 9 }), GRID, tiled)).toEqual({});
  });
});

describe("zone pickers", () => {
  it("select-layout to a single-zone preset removes the zone headers", () => {
    expect(predictSelectLayout(p({ layoutId: "single" }), GRID)).toEqual({
      elementsDisappear: [{ id: "terminal-zone-header-*" }],
    });
    expect(predictSelectLayout(p({ layoutId: "no-such-layout" }), GRID)).toEqual({});
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
