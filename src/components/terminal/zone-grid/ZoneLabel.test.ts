/**
 * Tests for the zone-header UI Bridge registration spec (boot-restore
 * remediation item 8): session/zone identity must be discoverable via
 * `ai/find` — the registered element's label carries the session title and a
 * stable per-zone id. The `useUIElement` call itself needs a DOM; the spec
 * builder is pure, so we assert the contract here (vitest `node` env, same
 * precedent as the sibling restore tests).
 */

import { describe, it, expect } from "vitest";
import { zoneHeaderElementSpec } from "./ZoneLabel";

describe("zoneHeaderElementSpec", () => {
  it("labels the element with the session title so ai/find by title resolves", () => {
    const spec = zoneHeaderElementSpec(0, "FIXTURE-ONSCREEN-1");
    expect(spec.label).toContain("FIXTURE-ONSCREEN-1");
  });

  it("uses a stable per-zone element id (1-based zone numbering in the label)", () => {
    expect(zoneHeaderElementSpec(3, "Claude 4")).toEqual({
      id: "terminal-zone-header-3",
      label: "Zone 4: Claude 4",
    });
  });

  it("ids are unique per zone — two zones never collide in the registry", () => {
    expect(zoneHeaderElementSpec(0, "same title").id).not.toBe(
      zoneHeaderElementSpec(1, "same title").id,
    );
  });
});
