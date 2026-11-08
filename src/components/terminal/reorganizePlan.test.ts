import { describe, expect, it } from "vitest";
import { normalizeReorganizeGroups } from "./reorganizePlan";

describe("normalizeReorganizeGroups", () => {
  it("trims each group name so an edited name matches the existing page", () => {
    const out = normalizeReorganizeGroups([
      { name: "Foo ", sessionIds: ["a"] },
      { name: "  Bar", sessionIds: ["b", "c"] },
    ]);
    expect(out).toEqual([
      { name: "Foo", sessionIds: ["a"] },
      { name: "Bar", sessionIds: ["b", "c"] },
    ]);
  });

  it("leaves session ids and order untouched and does not mutate the input", () => {
    const input = [{ name: " X ", sessionIds: ["s1", "s2"] }];
    const out = normalizeReorganizeGroups(input);
    expect(input[0].name).toBe(" X ");
    expect(out[0].sessionIds).toBe(input[0].sessionIds);
  });
});
