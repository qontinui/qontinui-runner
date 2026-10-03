import { describe, it, expect } from "vitest";

import { assertAnchored } from "./assertAnchored";

describe("assertAnchored", () => {
  const src = "alpha beta gamma beta";

  it("returns the index of a present marker", () => {
    expect(assertAnchored(src, "beta")).toBe(6);
  });

  it("honours `from`", () => {
    expect(assertAnchored(src, "beta", 7)).toBe(17);
  });

  it("throws, naming the marker, when the marker is absent", () => {
    expect(() => assertAnchored(src, "delta")).toThrow(/marker not found: "delta"/);
  });

  it("throws, with a separately-worded error, when the marker is absent after `from`", () => {
    expect(() => assertAnchored(src, "alpha", 1)).toThrow(/at or after offset 1: "alpha"/);
  });
});
