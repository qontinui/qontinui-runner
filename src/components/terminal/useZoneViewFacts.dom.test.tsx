// @vitest-environment jsdom
/**
 * `useZoneViewFacts` wires the page's zone layout into `zoneViewFactsFrom`.
 * `terminalSignatures.test.ts` pins what the pure function answers. This file
 * pins that the hook hands it the inputs the auto-trim reads (`layoutOrigin`,
 * `workMode`). Without them, a restore that releases an owed trim would predict
 * zone headers that the trim then removes.
 *
 * The two contexts the hook reads are mocked with the shapes it uses. The
 * scenario is the one the restore defect was found on: a runner-grown `quad`,
 * zone 1 maximized, three of its four tabs closed.
 */

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { ZoneViewFacts } from "./terminalPageSignatures";

const session = vi.hoisted(() => ({
  value: {} as Record<string, unknown>,
}));

vi.mock("./contexts", () => ({
  useTerminalSession: () => session.value,
  useUIStateCx: () => ({ state: { viewMode: "grid" } }),
}));

import { useZoneViewFacts } from "./useZoneViewFacts";

declare global {
  var IS_REACT_ACT_ENVIRONMENT: boolean | undefined;
}

let root: Root | null = null;
let container: HTMLDivElement | null = null;

beforeEach(() => {
  globalThis.IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => root?.unmount());
  container?.remove();
  root = null;
  container = null;
});

function factsFor(layoutOrigin: "auto" | "operator"): ZoneViewFacts | null {
  session.value = {
    tabs: [{ id: "b" }],
    zoneLayout: {
      layout: { zones: [{}, {}, {}, {}] },
      layoutId: "quad",
      layoutOrigin,
      workMode: "grid",
      maximizedZone: 1,
      assignments: { 1: "b" },
      focusedZone: 1,
    },
    labelsAndTags: { pinnedZones: new Set<number>() },
  };
  let facts: ZoneViewFacts | null = null;
  function Probe() {
    facts = useZoneViewFacts();
    return null;
  }
  act(() => root?.render(<Probe />));
  return facts;
}

describe("useZoneViewFacts", () => {
  it("reports the tiled header count unknown when restoring would release a trim", () => {
    expect(factsFor("auto")).toEqual({
      maximizedZone: 1,
      zoneCount: 4,
      labelledZonesWhenTiled: null,
    });
  });

  it("counts the tiled headers when no trim can apply (the operator chose the layout)", () => {
    expect(factsFor("operator")).toEqual({
      maximizedZone: 1,
      zoneCount: 4,
      labelledZonesWhenTiled: 1,
    });
  });
});
