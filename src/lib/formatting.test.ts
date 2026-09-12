/**
 * Tests for `formatRelativeTime`, which had no coverage at all.
 *
 * The property that matters here is a NEGATIVE one: this function must never
 * invent freshness. It is fed heartbeats and last-activity timestamps, so
 * "just now" is a liveness claim and an instant it cannot support must not
 * produce one.
 */

import { describe, it, expect, vi, afterEach } from "vitest";

import { formatRelativeTime } from "./formatting";

const NOW = new Date("2026-09-12T12:00:00.000Z");

function at(offsetMs: number): string {
  return new Date(NOW.getTime() + offsetMs).toISOString();
}

afterEach(() => {
  vi.useRealTimers();
});

function withFrozenClock<T>(fn: () => T): T {
  vi.useFakeTimers();
  vi.setSystemTime(NOW);
  return fn();
}

describe("formatRelativeTime — a FUTURE instant is never 'just now'", () => {
  /**
   * THE DISCRIMINATING TEST. `diffMs` is `now - date`, and `seconds < 60` had
   * no lower bound, so every negative difference — a skew of ten minutes or of
   * a thousand years — rendered as "just now". Revert the `diffMs < 0` guard
   * and all three of these fail.
   */
  it("renders an instant minutes ahead as an absolute time, not as freshness", () => {
    const out = withFrozenClock(() => formatRelativeTime(at(10 * 60 * 1000)));
    expect(out).not.toBe("just now");
    expect(out).toBe(new Date(at(10 * 60 * 1000)).toLocaleString());
  });

  it("renders an instant days ahead the same way", () => {
    const out = withFrozenClock(() => formatRelativeTime(at(5 * 24 * 60 * 60 * 1000)));
    expect(out).not.toBe("just now");
  });

  it("renders a far-future instant the same way", () => {
    const out = withFrozenClock(() => formatRelativeTime("3000-01-01T00:00:00.000Z"));
    expect(out).not.toBe("just now");
  });

  it("shows the TIME as well as the date, so a small skew is visible", () => {
    // `toLocaleDateString` would collapse a skew of minutes into today's date
    // and read as an ordinary row.
    const out = withFrozenClock(() => formatRelativeTime(at(90 * 1000)));
    expect(out).toContain(new Date(at(90 * 1000)).toLocaleString());
  });
});

describe("formatRelativeTime — the past is unchanged", () => {
  it("keeps 'just now' for an instant inside the last minute", () => {
    expect(withFrozenClock(() => formatRelativeTime(at(-30 * 1000)))).toBe("just now");
    // The boundary itself: exactly `now` is not in the future.
    expect(withFrozenClock(() => formatRelativeTime(at(0)))).toBe("just now");
  });

  it("counts minutes, hours and days", () => {
    expect(withFrozenClock(() => formatRelativeTime(at(-5 * 60 * 1000)))).toBe("5m ago");
    expect(withFrozenClock(() => formatRelativeTime(at(-3 * 60 * 60 * 1000)))).toBe("3h ago");
    expect(withFrozenClock(() => formatRelativeTime(at(-2 * 24 * 60 * 60 * 1000)))).toBe("2d ago");
  });

  it("falls back to a date past a week", () => {
    const ts = at(-30 * 24 * 60 * 60 * 1000);
    expect(withFrozenClock(() => formatRelativeTime(ts))).toBe(new Date(ts).toLocaleDateString());
  });

  it("renders an absent timestamp as a dash, never as an instant", () => {
    expect(formatRelativeTime(null)).toBe("-");
    expect(formatRelativeTime(undefined)).toBe("-");
    expect(formatRelativeTime("")).toBe("-");
  });
});
