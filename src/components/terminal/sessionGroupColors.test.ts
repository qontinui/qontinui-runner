/**
 * The group color roles (D4 of plan
 * `2026-10-13-session-manager-chrome-style-tab-organization-and-operator-search`)
 * and the zone tags that now draw from them in place of the retired
 * `GROUP_COLORS`, which minted the attention layer's red and amber.
 */

import { readFileSync } from "fs";
import { join } from "path";

import { describe, expect, it } from "vitest";

import { SESSION_GROUP_COLOR_VAR, groupColorAt, groupColorTint } from "./sessionGroupColors";
import { SESSION_GROUP_COLOR_ROLES } from "./sessionOrgTypes";

describe("groupColorAt", () => {
  it("cycles the six roles in declared order", () => {
    const six = SESSION_GROUP_COLOR_ROLES.map((r) => SESSION_GROUP_COLOR_VAR[r]);
    expect([0, 1, 2, 3, 4, 5].map(groupColorAt)).toEqual(six);
    expect(groupColorAt(6)).toBe(six[0]);
    expect(groupColorAt(-1)).toBe(six[5]);
  });

  it("is only ever a design-token variable, never a hex literal", () => {
    for (let i = 0; i < 12; i++) expect(groupColorAt(i)).toMatch(/^var\(--color-[a-z-]+\)$/);
  });
});

describe("groupColorTint", () => {
  it("mixes the variable with transparency instead of appending a hex alpha", () => {
    expect(groupColorTint("var(--color-info)", 8)).toBe(
      "color-mix(in srgb, var(--color-info) 8%, transparent)",
    );
  });
});

describe("zone tags", () => {
  const source = readFileSync(join(__dirname, "useZoneLabelsAndTags.ts"), "utf8");

  it("color from the group roles; the old attention-hue palette is gone", () => {
    expect(source).toContain("groupColorAt(i)");
    expect(source).not.toContain("GROUP_COLORS");
    expect(source).not.toMatch(/#f7768e|#e0af68/i);
  });
});
