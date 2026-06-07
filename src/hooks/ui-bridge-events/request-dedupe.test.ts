/**
 * RequestDedupe tests — exactly-once request_id claiming + bounded memory.
 */

import { describe, it, expect } from "vitest";
import { RequestDedupe, MAX_TRACKED } from "./request-dedupe";

describe("RequestDedupe", () => {
  it("claims a fresh request_id exactly once", () => {
    const d = new RequestDedupe();
    expect(d.claim("req-1")).toBe(true);
    expect(d.claim("req-1")).toBe(false);
    expect(d.claim("req-1")).toBe(false);
  });

  it("treats distinct ids independently", () => {
    const d = new RequestDedupe();
    expect(d.claim("a")).toBe(true);
    expect(d.claim("b")).toBe(true);
    expect(d.claim("a")).toBe(false);
    expect(d.claim("b")).toBe(false);
  });

  it("never claims empty or non-string ids", () => {
    const d = new RequestDedupe();
    expect(d.claim("")).toBe(false);
    // @ts-expect-error testing runtime guard against bad input
    expect(d.claim(undefined)).toBe(false);
    // @ts-expect-error testing runtime guard against bad input
    expect(d.claim(null)).toBe(false);
    expect(d.size()).toBe(0);
  });

  it("models N accumulated listeners → one execution", () => {
    // Each "listener" calls claim() for the same event. Only the first wins.
    const d = new RequestDedupe();
    const N = 4; // the observed fan-out on a fresh single-window instance
    const wins = Array.from({ length: N }, () => d.claim("storm-id")).filter(Boolean);
    expect(wins).toHaveLength(1);
  });

  it("is bounded: evicts oldest beyond the cap", () => {
    const d = new RequestDedupe(3);
    expect(d.claim("1")).toBe(true);
    expect(d.claim("2")).toBe(true);
    expect(d.claim("3")).toBe(true);
    expect(d.size()).toBe(3);
    // "2" and "3" are still tracked at this point.
    expect(d.claim("2")).toBe(false);
    expect(d.claim("3")).toBe(false);
    // 4th distinct id evicts "1" (oldest). Set is now {2,3,4}.
    expect(d.claim("4")).toBe(true);
    expect(d.size()).toBe(3);
    // "1" was evicted, so it is claimable again (definitionally stale).
    expect(d.claim("1")).toBe(true);
    // That claim evicted "2" (now oldest), so "2" is claimable again too;
    // "3" and "4" remain tracked.
    expect(d.claim("3")).toBe(false);
    expect(d.claim("4")).toBe(false);
  });

  it("default cap matches MAX_TRACKED", () => {
    const d = new RequestDedupe();
    for (let i = 0; i < MAX_TRACKED + 50; i++) {
      d.claim(`id-${i}`);
    }
    expect(d.size()).toBe(MAX_TRACKED);
  });

  it("reset clears tracked ids", () => {
    const d = new RequestDedupe();
    d.claim("x");
    d.reset();
    expect(d.size()).toBe(0);
    expect(d.claim("x")).toBe(true);
  });
});
