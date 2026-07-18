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

  it("suffixes the label with the first 8 chars of the bound Claude session id", () => {
    const spec = zoneHeaderElementSpec(0, "powershell.exe", "abcdef12-3456-7890-abcd-ef1234567890");
    expect(spec).toEqual({
      id: "terminal-zone-header-0",
      label: "Zone 1: powershell.exe (abcdef12)",
    });
  });

  it("short-id suffix disambiguates zones sharing an identical PTY title", () => {
    const a = zoneHeaderElementSpec(0, "same shell path", "11111111-aaaa");
    const b = zoneHeaderElementSpec(1, "same shell path", "22222222-bbbb");
    expect(a.label).not.toBe(b.label);
  });

  it("omits the suffix when no session id is bound", () => {
    expect(zoneHeaderElementSpec(2, "bash").label).toBe("Zone 3: bash");
  });

  it("omits the suffix for an empty-string session id", () => {
    expect(zoneHeaderElementSpec(2, "bash", "").label).toBe("Zone 3: bash");
  });
});
