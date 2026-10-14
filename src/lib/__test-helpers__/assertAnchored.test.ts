import path from "node:path";
import { pathToFileURL } from "node:url";

import { afterEach, describe, it, expect, vi } from "vitest";

import { argsOf, assertAnchored, callsOf, subjectDir } from "./assertAnchored";

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
  // Built from a native absolute path: a literal `file:///some/dir/…` has no
  // drive letter, and `fileURLToPath` rejects it on Windows.
  const dir = path.resolve("some", "dir");
  const url = pathToFileURL(path.join(dir, "x.test.ts")).href;
  const ownDir = dir + path.sep;
  const saved = process.env.MP_STAGE;

  afterEach(() => {
    if (saved === undefined) delete process.env.MP_STAGE;
    else process.env.MP_STAGE = saved;
    vi.restoreAllMocks();
  });

  it("is the test's own directory when MP_STAGE is unset", () => {
    delete process.env.MP_STAGE;
    expect(subjectDir(url)).toBe(ownDir);
  });

  it("is the test's own directory when MP_STAGE is the empty string", () => {
    process.env.MP_STAGE = "";
    expect(subjectDir(url)).toBe(ownDir);
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

  it("scans template interpolation as code, so a nested backtick cannot end the outer template", () => {
    // Before `${…}` was parsed, the inner backtick closed the outer template,
    // the `)` inside the nested template was read as code, and the slice ended
    // as "f(`${`)" — a quietly wrong call text, not an error.
    const code = 'f(`${`)`} ${ { k: g("(") }.k }`, 1); f(2)';
    expect(callsOf(code, "f(")).toEqual(['f(`${`)`} ${ { k: g("(") }.k }`, 1)', "f(2)"]);
  });

  it("throws on a literal that never closes, including inside an interpolation", () => {
    expect(() => callsOf('f("abc)', "f(")).toThrow(/unterminated string literal/);
    expect(() => callsOf("f(`a ${`b} c)", "f(")).toThrow(/unterminated string literal/);
  });

  it("throws when the call never closes or is absent", () => {
    expect(() => callsOf("f(1", "f(")).toThrow(/unbalanced/);
    expect(() => callsOf("g()", "f(")).toThrow(/no f\( call found/);
  });
});

describe("argsOf", () => {
  it("splits top-level arguments only, flattening whitespace", () => {
    expect(argsOf("f(a ?? b,\n  g(x, y), [1, 2], { k: 1, j: 2 })")).toEqual([
      "a ?? b",
      "g(x, y)",
      "[1, 2]",
      "{ k: 1, j: 2 }",
    ]);
  });

  it("does not split on a comma inside a string or template literal", () => {
    expect(argsOf("f('a, b', \"c, d\", `e, ${h(`,`, 2)}`)")).toEqual([
      "'a, b'",
      '"c, d"',
      "`e, ${h(`,`, 2)}`",
    ]);
  });

  it("drops a trailing empty argument and refuses a non-call", () => {
    expect(argsOf("f(a, b,\n)")).toEqual(["a", "b"]);
    expect(() => argsOf("f")).toThrow(/not a call/);
  });
});
