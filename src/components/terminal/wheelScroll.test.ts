import { describe, expect, it } from "vitest";
import { wheelToLineDelta, DEFAULT_CELL_HEIGHT_PX } from "./wheelScroll";

describe("wheelToLineDelta", () => {
  it("returns 0 when there is no vertical or horizontal delta", () => {
    expect(wheelToLineDelta({ deltaY: 0, deltaX: 0, deltaMode: 0 }, 40)).toBe(0);
    expect(wheelToLineDelta({ deltaY: NaN, deltaX: 0, deltaMode: 0 }, 40)).toBe(0);
  });

  it("treats line-mode (deltaMode 1) deltaY as a direct line count", () => {
    expect(wheelToLineDelta({ deltaY: 3, deltaX: 0, deltaMode: 1 }, 40)).toBe(3);
    expect(wheelToLineDelta({ deltaY: -3, deltaX: 0, deltaMode: 1 }, 40)).toBe(-3);
  });

  it("converts pixel-mode (deltaMode 0) deltaY to lines via cell height", () => {
    // 100px / 20px-per-line = 5 lines.
    expect(wheelToLineDelta({ deltaY: 100, deltaX: 0, deltaMode: 0 }, 40, 20)).toBe(5);
    expect(wheelToLineDelta({ deltaY: -100, deltaX: 0, deltaMode: 0 }, 40, 20)).toBe(-5);
  });

  it("uses the default cell height when none is supplied", () => {
    expect(
      wheelToLineDelta({ deltaY: DEFAULT_CELL_HEIGHT_PX, deltaX: 0, deltaMode: 0 }, 40),
    ).toBeCloseTo(1);
  });

  it("preserves the sign of deltaY (scrollLines convention: + down, - up)", () => {
    expect(wheelToLineDelta({ deltaY: 50, deltaX: 0, deltaMode: 0 }, 40, 20)).toBeGreaterThan(0);
    expect(wheelToLineDelta({ deltaY: -50, deltaX: 0, deltaMode: 0 }, 40, 20)).toBeLessThan(0);
  });

  it("falls back to deltaX when deltaY is 0 (Windows/Chromium Shift→horizontal remap)", () => {
    // Shift+wheel on Windows arrives as horizontal scroll: deltaY 0, deltaX set.
    expect(wheelToLineDelta({ deltaY: 0, deltaX: 100, deltaMode: 0 }, 40, 20)).toBe(5);
    expect(wheelToLineDelta({ deltaY: 0, deltaX: -100, deltaMode: 0 }, 40, 20)).toBe(-5);
  });

  it("prefers deltaY over deltaX when both are present", () => {
    expect(wheelToLineDelta({ deltaY: 40, deltaX: 999, deltaMode: 0 }, 40, 20)).toBe(2);
  });

  it("returns a fractional delta for sub-line pixel scrolls (trackpad)", () => {
    // 4px with a 20px line is 0.2 of a line — the caller accumulates these.
    expect(wheelToLineDelta({ deltaY: 4, deltaX: 0, deltaMode: 0 }, 40, 20)).toBeCloseTo(0.2);
  });

  it("scrolls nearly a full screen in page mode (deltaMode 2), keeping 1 row overlap", () => {
    expect(wheelToLineDelta({ deltaY: 1, deltaX: 0, deltaMode: 2 }, 40)).toBe(39);
    expect(wheelToLineDelta({ deltaY: -2, deltaX: 0, deltaMode: 2 }, 40)).toBe(-78);
  });

  it("falls back to the default cell height when given a non-positive one", () => {
    expect(
      wheelToLineDelta({ deltaY: DEFAULT_CELL_HEIGHT_PX, deltaX: 0, deltaMode: 0 }, 40, 0),
    ).toBeCloseTo(1);
    expect(
      wheelToLineDelta({ deltaY: DEFAULT_CELL_HEIGHT_PX, deltaX: 0, deltaMode: 0 }, 40, -5),
    ).toBeCloseTo(1);
  });

  it("accumulates to a whole line across several trackpad ticks", () => {
    // Simulate the caller's fractional-carry loop: five 0.2-line ticks → 1 line.
    let accum = 0;
    let scrolled = 0;
    for (let i = 0; i < 5; i++) {
      accum += wheelToLineDelta({ deltaY: 4, deltaX: 0, deltaMode: 0 }, 40, 20);
      const whole = Math.trunc(accum);
      accum -= whole;
      scrolled += whole;
    }
    expect(scrolled).toBe(1);
  });
});
