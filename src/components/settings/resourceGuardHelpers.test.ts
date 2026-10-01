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

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, it, expect } from "vitest";

import {
  GIB,
  bytesToGib,
  clampGib,
  clampInt,
  concurrencyAboveSuggestionWarning,
  MAX_CONCURRENT_BUILDS_MAX,
  MAX_CONCURRENT_BUILDS_MIN,
  parseConcurrencyInput,
  effectiveSessionFloorsGib,
  gibToBytes,
  parseRepoAllowlist,
  sessionFloorsAreInverted,
  SESSION_FLOOR_CAP_GIB,
  SESSION_FLOOR_DEFAULT_CRITICAL_GIB,
  SESSION_FLOOR_DEFAULT_WARN_GIB,
  SESSION_FLOOR_MAX_GIB,
  SESSION_FLOOR_MIN_GIB,
  THREAD_CEILING_ABS_MAX,
  THREAD_CEILING_INPUT_MIN,
  parseThreadCeilingInput,
  threadCeilingSourceText,
  threadCeilingsAreInverted,
  type ThreadCeilingsReport,
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

describe("thread ceilings — the panel renders the runner's fold, never its own", () => {
  it("a ceiling is transposed when critical sits BELOW warn — the inverse comparison", () => {
    // `commands::resource_guard_settings::thread_ceilings_are_inverted`.
    expect(threadCeilingsAreInverted(256, 200)).toBe(true);
    expect(threadCeilingsAreInverted(256, 256)).toBe(false);
    expect(threadCeilingsAreInverted(256, 400)).toBe(false);
    // Half a pair is never a transposition: the unset half is the machine
    // default, which moves with the box and which the runner's fold coerces.
    expect(threadCeilingsAreInverted(1000, null)).toBe(false);
    expect(threadCeilingsAreInverted(null, 210)).toBe(false);
    expect(threadCeilingsAreInverted(null, null)).toBe(false);
  });

  it("disagrees with the floor predicate on every non-equal pair", () => {
    // The bug this guards: copying `sessionFloorsAreInverted` onto the ceiling
    // lane accepts exactly the pair that deletes the warn band.
    for (const [warn, critical] of [
      [256, 400],
      [400, 256],
      [50, 2048],
    ]) {
      expect(threadCeilingsAreInverted(warn, critical)).not.toBe(
        sessionFloorsAreInverted(warn, critical),
      );
    }
  });

  it("the upper bound IS the runner's THREAD_CEILING_ABS_MAX, read from source", () => {
    const src = readFileSync(
      fileURLToPath(new URL("../../../src-tauri/src/resource_guard.rs", import.meta.url)),
      "utf8",
    );
    const m = src.match(/const\s+THREAD_CEILING_ABS_MAX\s*:\s*usize\s*=\s*(\d+)\s*;/);
    if (!m) throw new Error("THREAD_CEILING_ABS_MAX not found in resource_guard.rs");
    expect(THREAD_CEILING_ABS_MAX).toBe(Number(m[1]));
  });

  it("commits clamped explicit values and reverts empty/invalid drafts, never to null", () => {
    expect(parseThreadCeilingInput("600", null)).toBe(600);
    expect(parseThreadCeilingInput("1", 300)).toBe(THREAD_CEILING_INPUT_MIN);
    expect(parseThreadCeilingInput("99999", 300)).toBe(THREAD_CEILING_ABS_MAX);
    // An explicit value stays explicit, an unset one stays unset: only the
    // "Use machine default" button writes null.
    expect(parseThreadCeilingInput("", 300)).toBe(300);
    expect(parseThreadCeilingInput("abc", 300)).toBe(300);
    expect(parseThreadCeilingInput("", null)).toBeNull();
  });

  /** merytshost under the 2026-10-01 load, as the runner reports it. */
  const report = (over: Partial<ThreadCeilingsReport> = {}): ThreadCeilingsReport => ({
    enabled: true,
    warn: 555,
    critical: 747,
    provenance: { warn: "scaled", critical: "scaled" },
    local: { warn: null, critical: null },
    fleet: { warn: null, critical: null },
    floor: { warn: 276, critical: 420 },
    shift: 20,
    clampMin: 220,
    absMax: 2048,
    scaled: {
      warn: 555,
      critical: 747,
      sessionArm: { warn: 555, critical: 747 },
      poolArm: { warn: 604, critical: 748 },
      sessionCapacity: { warn: 192, critical: 288 },
      baselineUsed: 171,
      perSessionThreadsUsed: 2,
    },
    scaledUnknown: null,
    inputs: {
      cores: 48,
      memTotalBytes: 368_000_000_000,
      baseline: 171,
      perSessionThreads: 2,
      sessionThreadsNow: 328,
    },
    ladderCoerced: false,
    ...over,
  });

  it("names the term that decided each enforced ceiling, from the runner's report", () => {
    expect(threadCeilingSourceText("warn", report())).toMatch(/48 cores.*session capacity/);
    const light = report({
      warn: 296,
      scaled: { ...report().scaled!, warn: 296, poolArm: { warn: 296, critical: 440 } },
    });
    expect(threadCeilingSourceText("warn", light)).toMatch(/blocking-pool headroom/);
    expect(
      threadCeilingSourceText(
        "warn",
        report({
          provenance: { warn: "floor", critical: "floor" },
          scaled: null,
          scaledUnknown: "cores_unknown",
        }),
      ),
    ).toMatch(/unavailable \(cores unknown\)/);
    expect(
      threadCeilingSourceText(
        "warn",
        report({ provenance: { warn: "local", critical: "scaled" } }),
      ),
    ).toMatch(/your value/);
    expect(
      threadCeilingSourceText(
        "critical",
        report({ provenance: { warn: "scaled", critical: "fleet" } }),
      ),
    ).toMatch(/fleet ceiling/);
    expect(
      threadCeilingSourceText(
        "warn",
        report({ provenance: { warn: "clamp_min", critical: "scaled" } }),
      ),
    ).toMatch(/220/);
    expect(
      threadCeilingSourceText(
        "warn",
        report({ provenance: { warn: "clamp_max", critical: "scaled" } }),
      ),
    ).toMatch(/2048/);
    expect(
      threadCeilingSourceText(
        "critical",
        report({ provenance: { warn: "local", critical: "ladder" } }),
      ),
    ).toMatch(/warn ceiling/);
  });
});

describe("max concurrent builds bound", () => {
  /** Read a `const NAME: u32 = N;` out of the Rust validator's source. */
  function rustConst(name: string): number {
    const src = readFileSync(
      fileURLToPath(
        new URL("../../../src-tauri/src/ci_node/settings_directive.rs", import.meta.url),
      ),
      "utf8",
    );
    const m = src.match(new RegExp(`const\\s+${name}\\s*:\\s*u32\\s*=\\s*(\\d+)\\s*;`));
    if (!m) throw new Error(`${name} not found in settings_directive.rs`);
    return Number(m[1]);
  }

  it("equals the Rust validator's bounds, read from source", () => {
    expect(MAX_CONCURRENT_BUILDS_MAX).toBe(rustConst("MAX_CONCURRENT_BUILDS_MAX"));
    expect(MAX_CONCURRENT_BUILDS_MIN).toBe(rustConst("MAX_CONCURRENT_BUILDS_MIN"));
  });

  it("commits clamped explicit values and reverts empty/invalid drafts, never to null", () => {
    expect(parseConcurrencyInput("17", null)).toBe(17);
    expect(parseConcurrencyInput("0", 4)).toBe(1);
    expect(parseConcurrencyInput("999", 4)).toBe(64);
    // Empty or garbage on blur reverts to what was there — an explicit value
    // stays explicit, and an unset one stays unset. Only the "Use suggested"
    // button writes null.
    expect(parseConcurrencyInput("", 4)).toBe(4);
    expect(parseConcurrencyInput("abc", 4)).toBe(4);
    expect(parseConcurrencyInput("", null)).toBeNull();
  });

  it("warns above the suggestion, naming the limiting term, and never otherwise", () => {
    const big = { suggested: 12, cpus: 48, mem_gib: 368, limiting_term: "cores" as const };
    const msi = { suggested: 2, cpus: 16, mem_gib: 31, limiting_term: "memory" as const };
    expect(concurrencyAboveSuggestionWarning(null, big)).toBeNull();
    expect(concurrencyAboveSuggestionWarning(12, big)).toBeNull();
    expect(concurrencyAboveSuggestionWarning(4, null)).toBeNull();
    expect(concurrencyAboveSuggestionWarning(13, big)).toMatch(/cores/);
    expect(concurrencyAboveSuggestionWarning(3, msi)).toMatch(/memory/);
  });
});
