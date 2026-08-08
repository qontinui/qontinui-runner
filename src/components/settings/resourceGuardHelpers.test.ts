/**
 * Tests for the `ResourceGuardSettings` pure helpers.
 *
 * The runner's vitest config is `environment: "node"` (no jsdom), so — as
 * `appFreshnessHelpers.test.ts` and `LockYieldPolicySettings.test.tsx` do — we
 * test the exported pure helpers rather than rendering.
 *
 * The cases that matter are the ones spanning the UNIT BOUNDARY: the panel
 * edits GiB, the runner stores bytes, and the shipped critical default (1.5
 * GiB) is the value a naive integer conversion silently destroys.
 */

import { describe, it, expect } from "vitest";

import {
  GIB,
  bytesToGib,
  clampGib,
  clampInt,
  effectiveSessionFloorsGib,
  gibToBytes,
  parseRepoAllowlist,
  sessionFloorsAreInverted,
  SESSION_FLOOR_CAP_GIB,
  SESSION_FLOOR_DEFAULT_CRITICAL_GIB,
  SESSION_FLOOR_DEFAULT_WARN_GIB,
  SESSION_FLOOR_MAX_GIB,
  SESSION_FLOOR_MIN_GIB,
} from "./resourceGuardHelpers";

describe("byte ↔ GiB conversion", () => {
  it("round-trips the shipped defaults exactly", () => {
    // 3 GiB warn / 1.5 GiB critical — `settings::SessionGuardSettings`.
    expect(bytesToGib(3 * GIB)).toBe(3);
    expect(bytesToGib(1.5 * GIB)).toBe(1.5);
    expect(gibToBytes(3)).toBe(3 * GIB);
    expect(gibToBytes(1.5)).toBe(1.5 * GIB);
  });

  it("treats an unreadable or non-positive figure as 0, never as a huge number", () => {
    expect(bytesToGib(Number.NaN)).toBe(0);
    expect(bytesToGib(-1)).toBe(0);
    expect(gibToBytes(Number.NaN)).toBe(0);
    expect(gibToBytes(-1)).toBe(0);
  });

  it("emits whole bytes — the Rust field is a u64", () => {
    expect(Number.isInteger(gibToBytes(0.33))).toBe(true);
  });
});

describe("input clamping", () => {
  it("keeps fractional GiB intact", () => {
    // Flooring here would rewrite the shipped 1.5 GiB critical default to 1
    // the moment the panel loaded it.
    expect(clampGib(1.5, SESSION_FLOOR_MIN_GIB, SESSION_FLOOR_MAX_GIB, 3)).toBe(1.5);
  });

  it("falls back on non-numeric input and clamps to the band", () => {
    expect(clampGib(Number.NaN, SESSION_FLOOR_MIN_GIB, SESSION_FLOOR_MAX_GIB, 3)).toBe(3);
    expect(clampGib(0, SESSION_FLOOR_MIN_GIB, SESSION_FLOOR_MAX_GIB, 3)).toBe(
      SESSION_FLOOR_MIN_GIB,
    );
    expect(clampGib(9999, SESSION_FLOOR_MIN_GIB, SESSION_FLOOR_MAX_GIB, 3)).toBe(
      SESSION_FLOOR_MAX_GIB,
    );
  });

  it("floors integer fields", () => {
    expect(clampInt(2.9, 1, 16, 1)).toBe(2);
    expect(clampInt(Number.NaN, 1, 16, 1)).toBe(1);
  });
});

describe("floor ordering", () => {
  it("flags only a strictly higher critical floor", () => {
    // Mirrors `commands::resource_guard_settings::session_floors_are_inverted`.
    expect(sessionFloorsAreInverted(1, 3)).toBe(true);
    expect(sessionFloorsAreInverted(2, 2)).toBe(false);
    expect(sessionFloorsAreInverted(3, 1.5)).toBe(false);
  });
});

describe("effective (enforced) floors", () => {
  it("shows the built-in default for every value the runner would discard", () => {
    // The whole 0.25–3 GiB warn range is inert — enforcement is
    // `max(configured, hardcoded)` — which is exactly what the panel now says
    // out loud instead of displaying a floor that never applies.
    expect(effectiveSessionFloorsGib(SESSION_FLOOR_MIN_GIB, SESSION_FLOOR_MIN_GIB)).toEqual({
      warnGib: SESSION_FLOOR_DEFAULT_WARN_GIB,
      criticalGib: SESSION_FLOOR_DEFAULT_CRITICAL_GIB,
    });
    expect(effectiveSessionFloorsGib(1, 0.5)).toEqual({
      warnGib: SESSION_FLOOR_DEFAULT_WARN_GIB,
      criticalGib: SESSION_FLOOR_DEFAULT_CRITICAL_GIB,
    });
  });

  it("passes a tightened floor through untouched", () => {
    expect(effectiveSessionFloorsGib(8, 4)).toEqual({ warnGib: 8, criticalGib: 4 });
  });

  it("caps an unreachable floor at the runner's ceiling", () => {
    // `resource_guard::SESSION_FLOOR_MAX_BYTES`. The input accepts up to 128
    // GiB; a floor no box can clear would refuse every unattended spawn
    // forever, so the runner clamps it and the panel must not claim otherwise.
    expect(effectiveSessionFloorsGib(SESSION_FLOOR_MAX_GIB, SESSION_FLOOR_MAX_GIB)).toEqual({
      warnGib: SESSION_FLOOR_CAP_GIB,
      criticalGib: SESSION_FLOOR_CAP_GIB,
    });
  });

  it("clamps the block floor down to the warn floor, never the reverse", () => {
    // Mirrors `resource_guard::coerce_ladder`: raising warn to meet critical
    // would enforce a warn floor nobody asked for.
    expect(effectiveSessionFloorsGib(4, 9)).toEqual({ warnGib: 4, criticalGib: 4 });
  });

  it("never returns an inverted ladder or a non-numeric floor", () => {
    for (const warn of [Number.NaN, 0, 0.25, 1.5, 3, 7, 12, 128, 1e9]) {
      for (const critical of [Number.NaN, 0, 0.25, 1.5, 3, 7, 12, 128, 1e9]) {
        const eff = effectiveSessionFloorsGib(warn, critical);
        expect(Number.isFinite(eff.warnGib)).toBe(true);
        expect(Number.isFinite(eff.criticalGib)).toBe(true);
        expect(eff.criticalGib).toBeLessThanOrEqual(eff.warnGib);
        expect(eff.warnGib).toBeLessThanOrEqual(SESSION_FLOOR_CAP_GIB);
        expect(eff.warnGib).toBeGreaterThanOrEqual(SESSION_FLOOR_DEFAULT_WARN_GIB);
      }
    }
  });
});

describe("repo allowlist parsing", () => {
  it("splits on commas and newlines and drops blanks", () => {
    expect(parseRepoAllowlist("qontinui/qontinui-runner,\n qontinui/coord ,\n\n")).toEqual([
      "qontinui/qontinui-runner",
      "qontinui/coord",
    ]);
  });

  it("an empty textarea is an empty allowlist, not one blank entry", () => {
    // `ci_node`'s contract: an EMPTY allowlist means nothing is runnable. A
    // stray "" entry would read as a configured repo that matches nothing.
    expect(parseRepoAllowlist("  \n , \n")).toEqual([]);
  });
});
