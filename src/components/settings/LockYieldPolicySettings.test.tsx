/**
 * Pure-helper tests for `LockYieldPolicySettings`.
 *
 * The runner's vitest config uses `environment: "node"` (no jsdom) —
 * see sibling tests like `HoldingLockBanner.test.tsx` for the
 * precedent. We exercise the exported `clampNumber` pure helper +
 * exported defaults / constants for value-binding semantics.
 */

import { describe, it, expect } from "vitest";

import {
  clampNumber,
  IDLE_THRESHOLD_MIN,
  IDLE_THRESHOLD_MAX,
  MIN_WAIT_MIN,
  MIN_WAIT_MAX,
} from "./lockYieldPolicyHelpers";

describe("clampNumber", () => {
  it("returns n unchanged when within bounds", () => {
    expect(clampNumber(50, 10, 100, 60)).toBe(50);
    expect(clampNumber(10, 10, 100, 60)).toBe(10);
    expect(clampNumber(100, 10, 100, 60)).toBe(100);
  });
  it("clamps to min when below", () => {
    expect(clampNumber(5, 10, 100, 60)).toBe(10);
    expect(clampNumber(-1, 10, 100, 60)).toBe(10);
  });
  it("clamps to max when above", () => {
    expect(clampNumber(101, 10, 100, 60)).toBe(100);
    expect(clampNumber(9999, 10, 100, 60)).toBe(100);
  });
  it("floors fractional inputs", () => {
    expect(clampNumber(50.7, 10, 100, 60)).toBe(50);
    expect(clampNumber(10.99, 10, 100, 60)).toBe(10);
  });
  it("falls back when input is NaN", () => {
    expect(clampNumber(Number.NaN, 10, 100, 60)).toBe(60);
  });
  it("falls back when input is non-finite", () => {
    expect(clampNumber(Number.POSITIVE_INFINITY, 10, 100, 60)).toBe(60);
    expect(clampNumber(Number.NEGATIVE_INFINITY, 10, 100, 60)).toBe(60);
  });
});

describe("LockYieldPolicySettings UI bounds (settings panel contract)", () => {
  it("idle-threshold bounds match the spec (10-3600s)", () => {
    expect(IDLE_THRESHOLD_MIN).toBe(10);
    expect(IDLE_THRESHOLD_MAX).toBe(3600);
  });
  it("min-wait bounds match the spec (5-600s)", () => {
    expect(MIN_WAIT_MIN).toBe(5);
    expect(MIN_WAIT_MAX).toBe(600);
  });
});
