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

import { readTextArg, textArg } from "./parse";
import { LAYOUT_IDS, readCountArg, readZoneArg, resolveZoneTarget } from "./useTerminalCommands";

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

/**
 * `count` is the same contract as `zone`, and it was the one field the
 * contract had never been applied to. There was NO coverage at all:
 * `grep -rn "invalid-count" src` matched only the three `count < 1`
 * guards, and neither this file nor `spawnVerdict.test.ts` mentioned a
 * non-numeric count. `/spawn abc` therefore rendered `[ok] /spawn ✓`
 * after silently creating ONE terminal.
 */
describe("readCountArg", () => {
  it("reports an omitted field as absent, so the schema default can apply", () => {
    expect(readCountArg({})).toEqual({ kind: "absent" });
    expect(readCountArg({ count: undefined })).toEqual({ kind: "absent" });
    expect(readCountArg({ count: null })).toEqual({ kind: "absent" });
    expect(readCountArg({ count: "  " })).toEqual({ kind: "absent" });
  });

  it("reads an integer count from either the number or its string form", () => {
    expect(readCountArg({ count: 3 })).toEqual({ kind: "count", count: 3 });
    expect(readCountArg({ count: "3" })).toEqual({ kind: "count", count: 3 });
    expect(readCountArg({ count: " 3 " })).toEqual({ kind: "count", count: 3 });
  });

  it("reports a supplied-but-unparseable count as INVALID, not absent", () => {
    // The live defect: `parse.ts::coerceToken` leaves "abc" a string, and
    // `typeof args.count === "number" ? args.count : 1` collapsed that to
    // the same 1 a bare `/spawn` produces.
    expect(readCountArg({ count: "abc" })).toEqual({ kind: "invalid", raw: "abc" });
    expect(readCountArg({ count: NaN })).toEqual({ kind: "invalid", raw: "NaN" });
    expect(readCountArg({ count: Infinity })).toMatchObject({ kind: "invalid" });
    expect(readCountArg({ count: {} })).toMatchObject({ kind: "invalid" });
  });

  it("rejects a NON-INTEGER count rather than rounding it", () => {
    // `/spawn 2.7` created THREE terminals and reported success, because
    // `spawnVerdict(ids, 2.7)` asks `3 < 2.7` — false.
    expect(readCountArg({ count: 2.7 })).toEqual({ kind: "invalid", raw: "2.7" });
    expect(readCountArg({ count: "2.7" })).toEqual({ kind: "invalid", raw: "2.7" });
  });

  it("reads a non-default field name", () => {
    expect(readCountArg({ n: 4 }, "n")).toEqual({ kind: "count", count: 4 });
  });
});

/**
 * The string half of the same class. `coerceToken` turns a clean numeric
 * literal into a `number`, and every `typeof args.x === "string" ? x : ""`
 * read that as ABSENT — so `/spawn-with 2 5` answered "command is
 * required" for a command that was supplied, and `/spawn-ai 2 3` silently
 * launched the "best" account instead of failing on the unknown one.
 */
describe("readTextArg", () => {
  it("reports an omitted or blank field as absent", () => {
    expect(readTextArg({}, "command")).toEqual({ kind: "absent" });
    expect(readTextArg({ command: null }, "command")).toEqual({ kind: "absent" });
    expect(readTextArg({ command: "   " }, "command")).toEqual({ kind: "absent" });
  });

  it("keeps a supplied token that coerceToken turned into a number", () => {
    expect(readTextArg({ command: 5 }, "command")).toEqual({ kind: "text", text: "5" });
    expect(textArg({ account: 3 }, "account")).toBe("3");
  });

  it("passes a normal string through untouched", () => {
    expect(readTextArg({ command: "htop" }, "command")).toEqual({ kind: "text", text: "htop" });
  });
});
