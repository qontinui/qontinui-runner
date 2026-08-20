/**
 * Tests for the declarative performance-caps field descriptors and their
 * clamping helpers (many-sessions plan Phase 8).
 *
 * The clamping rules are the load-bearing part: they run at SAVE, not per
 * keystroke, and a regression back to per-keystroke clamping silently makes
 * the six-digit scrollback field impossible to type into.
 */

import { describe, it, expect } from "vitest";

import {
  PERF_CAP_FIELDS,
  clampPerfCaps,
  clampPerfField,
  isOutOfRange,
  type PerfCapNumberField,
} from "./performanceCapsConfig";
import { DEFAULT_PERF_CAPS, type PerfCaps } from "@/lib/perfCaps";

function numberField(key: keyof PerfCaps): PerfCapNumberField {
  const f = PERF_CAP_FIELDS.find((x) => x.key === key);
  if (!f || f.type !== "number") throw new Error(`no number field for ${String(key)}`);
  return f;
}

describe("descriptors", () => {
  it("cover every cap exactly once", () => {
    const keys = PERF_CAP_FIELDS.map((f) => f.key).sort();
    expect(keys).toEqual(Object.keys(DEFAULT_PERF_CAPS).sort());
    expect(new Set(keys).size).toBe(keys.length);
  });

  /**
   * Every shipped default must be inside its own declared range, or the panel
   * would flag a stock install as out of range and "adjust" it on save.
   */
  it("declare ranges that contain the shipped defaults", () => {
    for (const field of PERF_CAP_FIELDS) {
      if (field.type !== "number") continue;
      const value = DEFAULT_PERF_CAPS[field.key] as number;
      expect(field.min).toBeLessThanOrEqual(value);
      expect(field.max).toBeGreaterThanOrEqual(value);
      expect(isOutOfRange(field, value)).toBe(false);
    }
  });
});

describe("clampPerfField", () => {
  it("clamps to the declared bounds", () => {
    const scrollback = numberField("scrollback_capacity_bytes");
    expect(clampPerfField(scrollback, 4)).toBe(scrollback.min);
    expect(clampPerfField(scrollback, 1e12)).toBe(scrollback.max);
    expect(clampPerfField(scrollback, 4_194_304)).toBe(4_194_304);
  });

  it("rounds and rejects non-numbers", () => {
    const sessions = numberField("max_sessions_warn");
    expect(clampPerfField(sessions, 12.7)).toBe(13);
    expect(clampPerfField(sessions, Number.NaN)).toBe(sessions.min);
    expect(clampPerfField(sessions, "40")).toBe(sessions.min);
  });
});

describe("clampPerfCaps", () => {
  it("clamps every number field and leaves the others alone", () => {
    const draft: PerfCaps = {
      ...DEFAULT_PERF_CAPS,
      // Mid-typing values a per-keystroke clamp would have destroyed.
      scrollback_capacity_bytes: 4,
      grid_scan_interval_ms: 1,
      max_sessions_warn: 99_999,
      share_terminal_output: false,
      redact_terminal_secrets: true,
    };
    const out = clampPerfCaps(draft, PERF_CAP_FIELDS);

    expect(out.scrollback_capacity_bytes).toBe(numberField("scrollback_capacity_bytes").min);
    expect(out.grid_scan_interval_ms).toBe(numberField("grid_scan_interval_ms").min);
    expect(out.max_sessions_warn).toBe(numberField("max_sessions_warn").max);
    // Non-number fields pass through untouched.
    expect(out.share_terminal_output).toBe(false);
    expect(out.redact_terminal_secrets).toBe(true);
  });

  it("is a no-op on the stock defaults", () => {
    expect(clampPerfCaps({ ...DEFAULT_PERF_CAPS }, PERF_CAP_FIELDS)).toEqual(DEFAULT_PERF_CAPS);
  });

  it("does not mutate the draft it was given", () => {
    const draft: PerfCaps = { ...DEFAULT_PERF_CAPS, max_sessions_warn: 99_999 };
    clampPerfCaps(draft, PERF_CAP_FIELDS);
    expect(draft.max_sessions_warn).toBe(99_999);
  });
});

describe("honesty labelling", () => {
  /**
   * A knob whose consumer has not landed must SAY so. If a consumer lands and
   * the label is not removed, the panel under-promises — annoying but safe; if
   * a knob is added without a label and without a consumer, the panel lies.
   * This pins the current set.
   *
   * The two flush intervals left this list when the merge-train plan's D3
   * wired them into `TerminalSession::spawn`. They had been mislabelled in the
   * other direction for a while: the label named Phase 5, Phase 5 landed, and
   * the knobs stayed inert because the emission path read a compiled-in
   * 250 ms constant instead of the setting.
   */
  it("marks exactly the not-yet-consumed knobs", () => {
    const pending = PERF_CAP_FIELDS.filter((f) => f.pendingPhase)
      .map((f) => f.key)
      .sort();
    expect(pending).toEqual(["max_webgl_panes"]);
  });

  it("marks the grid-scan knob as needing a restart", () => {
    const restart = PERF_CAP_FIELDS.filter((f) => f.requiresRestart).map((f) => f.key);
    expect(restart).toEqual(["grid_scan_interval_ms"]);
  });
});
