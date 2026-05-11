/**
 * Tests for the gating logic of `useMidSessionProbe`.
 *
 * The hook itself is composed of an inner debounce + fetch loop that's
 * hard to exercise from `environment: "node"` (no jsdom, no React
 * Testing Library). The decision-making lives in the exported pure
 * helper `shouldFireProbe` — we test the matrix of gates there so any
 * accidental shift in the gating logic (e.g. dropping the AI-session
 * check) gets caught in CI.
 */

import { describe, it, expect } from "vitest";
import {
  shouldFireProbe,
  MID_SESSION_MIN_LINE_LEN,
  MID_SESSION_RATE_LIMIT_MS,
} from "./useMidSessionProbe";

const NOW = 1_000_000_000;
const LONG_LINE = "edit src/components/foo/bar.ts";
const SHORT_LINE = "ls -la"; // < 10 chars

describe("shouldFireProbe", () => {
  it("returns false when the user setting is disabled", () => {
    const result = shouldFireProbe(
      LONG_LINE,
      { claudeSessionId: "abc-123" },
      NOW,
      { enabled: false },
    );
    expect(result).toBe(false);
  });

  it(`returns false when line is under ${MID_SESSION_MIN_LINE_LEN} chars`, () => {
    expect(SHORT_LINE.length).toBeLessThan(MID_SESSION_MIN_LINE_LEN);
    const result = shouldFireProbe(
      SHORT_LINE,
      { claudeSessionId: "abc-123" },
      NOW,
      { enabled: true },
    );
    expect(result).toBe(false);
  });

  it("returns false when the tab has no claudeSessionId", () => {
    const result = shouldFireProbe(LONG_LINE, {}, NOW, { enabled: true });
    expect(result).toBe(false);
  });

  it(`returns false when within ${MID_SESSION_RATE_LIMIT_MS}ms of previous probe`, () => {
    const result = shouldFireProbe(
      LONG_LINE,
      { claudeSessionId: "abc-123", lastProbeAt: NOW - 500 },
      NOW,
      { enabled: true },
    );
    expect(result).toBe(false);
  });

  it("returns true when all gates pass (no previous probe)", () => {
    const result = shouldFireProbe(
      LONG_LINE,
      { claudeSessionId: "abc-123" },
      NOW,
      { enabled: true },
    );
    expect(result).toBe(true);
  });

  it("returns true when last probe was outside the rate-limit window", () => {
    const result = shouldFireProbe(
      LONG_LINE,
      { claudeSessionId: "abc-123", lastProbeAt: NOW - (MID_SESSION_RATE_LIMIT_MS + 1) },
      NOW,
      { enabled: true },
    );
    expect(result).toBe(true);
  });

  it("returns true at the exact rate-limit boundary (>= rate limit ms passed)", () => {
    // Implementation uses strict `<` for the rate-limit check so a probe
    // can fire as soon as RATE_LIMIT_MS has elapsed, not RATE_LIMIT_MS+1.
    const result = shouldFireProbe(
      LONG_LINE,
      { claudeSessionId: "abc-123", lastProbeAt: NOW - MID_SESSION_RATE_LIMIT_MS },
      NOW,
      { enabled: true },
    );
    expect(result).toBe(true);
  });
});
