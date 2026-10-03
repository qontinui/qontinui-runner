import { afterEach, describe, it, expect, vi } from "vitest";

import { assertAnchored, callsOf, subjectDir } from "./assertAnchored";

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

describe("subjectDir", () => {
  const url = "file:///some/dir/x.test.ts";
  const saved = process.env.MP_STAGE;

  afterEach(() => {
    if (saved === undefined) delete process.env.MP_STAGE;
    else process.env.MP_STAGE = saved;
    vi.restoreAllMocks();
  });

  it("is the test's own directory when MP_STAGE is unset", () => {
    delete process.env.MP_STAGE;
    expect(subjectDir(url)).toBe("/some/dir/");
  });

  it("is the test's own directory when MP_STAGE is the empty string", () => {
    process.env.MP_STAGE = "";
    expect(subjectDir(url)).toBe("/some/dir/");
  });

  it("honours MP_STAGE and warns, naming the staged directory", () => {
    process.env.MP_STAGE = "/stage/copy";
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    expect(subjectDir(url)).toBe("/stage/copy/");
    expect(warn).toHaveBeenCalledTimes(1);
    expect(String(warn.mock.calls[0][0])).toContain("/stage/copy");
  });
});

describe("callsOf", () => {
  it("is not thrown off by parentheses inside string literals", () => {
    const code = `a(1); f("(", 'x)', \`)(\`, "\\")") ; f(2)`;
    expect(callsOf(code, "f(")).toEqual([`f("(", 'x)', \`)(\`, "\\")")`, "f(2)"]);
  });

  it("throws when the call never closes or is absent", () => {
    expect(() => callsOf("f(1", "f(")).toThrow(/unbalanced/);
    expect(() => callsOf("g()", "f(")).toThrow(/no f\( call found/);
  });
});
