/**
 * Tests for the shared path-comparison helpers. These back every "is this
 * terminal in the project folder?" decision on the projects surface, where a
 * false mismatch means offering to close a healthy session.
 */

import { describe, it, expect } from "vitest";

import { normalizePathForCompare, samePath } from "./pathCompare";

describe("normalizePathForCompare", () => {
  it("folds separator direction, trailing separators and case", () => {
    expect(normalizePathForCompare("D:\\Projects\\Pizza")).toBe("d:/projects/pizza");
    expect(normalizePathForCompare("D:/projects/pizza/")).toBe("d:/projects/pizza");
    expect(normalizePathForCompare("D:\\projects\\pizza\\\\")).toBe("d:/projects/pizza");
  });

  it("never folds a root path to the empty string", () => {
    // "" would compare equal to every falsy path — the one fold that must not
    // happen.
    expect(normalizePathForCompare("/")).toBe("/");
    expect(normalizePathForCompare("//")).toBe("/");
    expect(normalizePathForCompare("\\")).toBe("/");
  });
});

describe("samePath", () => {
  it("matches the same directory written by different producers", () => {
    // Left: a Tauri spawn cwd (verbatim, backslashes). Right: a folder-picker
    // project root (forward slashes, trailing separator, different case).
    expect(
      samePath("D:\\qontinui-root\\qontinui-runner", "d:/qontinui-root/QONTINUI-RUNNER/"),
    ).toBe(true);
  });

  it("does not conflate sibling directories that share a prefix", () => {
    expect(samePath("D:\\foo", "D:\\foobar")).toBe(false);
  });

  it("treats an unknown path as never equal", () => {
    expect(samePath(undefined, "D:\\foo")).toBe(false);
    expect(samePath("D:\\foo", null)).toBe(false);
    expect(samePath("", "")).toBe(false);
  });
});
