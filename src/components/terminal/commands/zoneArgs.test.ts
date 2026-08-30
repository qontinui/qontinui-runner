/**
 * Zone-argument validation for the zone-taking terminal commands
 * (`/focus`, `/maximize`, `/close`, `/restart`, `/swap`) and the
 * `/layout` preset list.
 *
 * The contract these pin down is the one that used to be missing: a
 * SUPPLIED-but-unusable argument must never be indistinguishable from an
 * absent one. `/focus bogus` silently focused the current zone and
 * `/close 99` silently closed the ACTIVE session — both reported success.
 *
 * Runs under vitest's `environment: "node"` (no jsdom), so these exercise
 * the exported pure helpers rather than the hook — same split as
 * `spawnTenant.test.ts`.
 */

import { describe, expect, it } from "vitest";

import { LAYOUT_IDS, readZoneArg, resolveZoneTarget } from "./useTerminalCommands";

describe("readZoneArg", () => {
  it("reports an omitted field as absent, not as a zero", () => {
    expect(readZoneArg({})).toEqual({ kind: "absent" });
    expect(readZoneArg({ zone: undefined })).toEqual({ kind: "absent" });
    expect(readZoneArg({ zone: null })).toEqual({ kind: "absent" });
    expect(readZoneArg({ zone: "  " })).toEqual({ kind: "absent" });
  });

  it("reads a numeric zone from either the number or its string form", () => {
    expect(readZoneArg({ zone: 3 })).toEqual({ kind: "zone", zone: 3 });
    expect(readZoneArg({ zone: "3" })).toEqual({ kind: "zone", zone: 3 });
    expect(readZoneArg({ zone: " 3 " })).toEqual({ kind: "zone", zone: 3 });
    expect(readZoneArg({ zone: 3.7 })).toEqual({ kind: "zone", zone: 3 });
  });

  it("reports a supplied-but-unparseable value as INVALID, not absent", () => {
    expect(readZoneArg({ zone: "bogus" })).toEqual({ kind: "invalid", raw: "bogus" });
    expect(readZoneArg({ zone: NaN })).toEqual({ kind: "invalid", raw: "NaN" });
    expect(readZoneArg({ zone: {} })).toMatchObject({ kind: "invalid" });
  });

  it("reads an alternate field name (the /swap a/b operands)", () => {
    expect(readZoneArg({ a: 1, b: "x" }, "a")).toEqual({ kind: "zone", zone: 1 });
    expect(readZoneArg({ a: 1, b: "x" }, "b")).toEqual({ kind: "invalid", raw: "x" });
  });
});

describe("resolveZoneTarget", () => {
  // focusedZone = 1 (0-based), grid of 4 zones.
  const focused = 1;
  const zones = 4;

  it("falls back to the focused zone when no zone was supplied", () => {
    expect(resolveZoneTarget({}, focused, zones)).toEqual({
      kind: "ok",
      index: focused,
      supplied: false,
    });
  });

  it("converts a supplied 1-based zone to a 0-based index", () => {
    expect(resolveZoneTarget({ zone: 3 }, focused, zones)).toEqual({
      kind: "ok",
      index: 2,
      supplied: true,
    });
  });

  it("REFUSES a supplied-but-unparseable zone instead of using the focused one", () => {
    expect(resolveZoneTarget({ zone: "bogus" }, focused, zones)).toEqual({
      kind: "invalid-zone",
      raw: "bogus",
    });
  });

  it("reports an out-of-range zone AND that it was supplied", () => {
    expect(resolveZoneTarget({ zone: 99 }, focused, zones)).toEqual({
      kind: "out-of-range",
      supplied: true,
    });
    expect(resolveZoneTarget({ zone: 0 }, focused, zones)).toEqual({
      kind: "out-of-range",
      supplied: true,
    });
    expect(resolveZoneTarget({ zone: -1 }, focused, zones)).toEqual({
      kind: "out-of-range",
      supplied: true,
    });
  });

  it("marks an out-of-range DEFAULT as not supplied, so /close can still fall back", () => {
    // Focused zone sitting outside a shrunken grid — nothing was named,
    // so the caller is free to use the active session.
    expect(resolveZoneTarget({}, 7, zones)).toEqual({ kind: "out-of-range", supplied: false });
  });

  it("resolves the /swap operands through the same three states", () => {
    expect(resolveZoneTarget({ a: 2 }, focused, zones, "a")).toEqual({
      kind: "ok",
      index: 1,
      supplied: true,
    });
    expect(resolveZoneTarget({ a: "nope" }, focused, zones, "a")).toEqual({
      kind: "invalid-zone",
      raw: "nope",
    });
  });
});

describe("LAYOUT_IDS", () => {
  it("is derived from the layout presets, so /layout can't drift from them", () => {
    // The ids the ZoneLayoutPicker offers plus the synthesized flow grid —
    // exactly the set `setLayoutId` accepts.
    expect(LAYOUT_IDS).toContain("single");
    expect(LAYOUT_IDS).toContain("split");
    expect(LAYOUT_IDS).toContain("triptych");
    expect(LAYOUT_IDS).toContain("quad");
    expect(LAYOUT_IDS).toContain("six-pack");
    expect(LAYOUT_IDS).toContain("full-grid");
    expect(LAYOUT_IDS).toContain("flow-grid");
  });

  it("does not contain a bogus preset", () => {
    expect(LAYOUT_IDS).not.toContain("bogus");
  });
});
