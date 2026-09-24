/**
 * Event→UI-state mapping tests for the renderer-memory watchdog listener
 * (plan `2026-06-09-runner-renderer-memory-watchdog-and-twin-slo` Phase 1,
 * §6 Q2 and §6 Q3).
 *
 * The runner's vitest config is `environment: "node"` with no jsdom (see
 * `HoldingLockBanner.test.tsx`'s header for the precedent), so these drive the
 * exported pure reducer — which is where every decision lives — rather than
 * rendering the component. The JSX shell is verified by UI Bridge against the
 * `data-ui-bridge-id`s baked into `RendererMemoryWatchdogNotices.tsx`.
 *
 * Covered:
 *   1. Every `kind` the Rust side emits maps to the right surface, and only
 *      that surface: `reload_warning` → countdown, `storming` → banner,
 *      `reload_result` → neither (it retires the countdown and reports via the
 *      ordinary toast queue).
 *   2. A REPEATED storm event does not duplicate the banner — the escalation
 *      re-emits on every breaching tick while latching only its loud log, so
 *      the single slot, the preserved `sinceMs` and the identity bail-out are
 *      the load-bearing properties.
 *   3. The countdown counts DOWN and elapses on its own — §6 Q2's "proceed
 *      whether or not acknowledged".
 *   4. An unknown future `kind` is ignored rather than mapped to a guess.
 */

import { describe, it, expect } from "vitest";

import {
  INITIAL_RENDERER_WATCHDOG_UI_STATE,
  RENDERER_MEMORY_WATCHDOG_EVENT,
  WARNING_LINGER_SECS,
  advanceRendererWatchdogClock,
  isWarningElapsed,
  isWarningStale,
  reduceRendererWatchdogEvent,
  reloadResultToastType,
  warningSecondsRemaining,
  type RendererWatchdogEvent,
  type RendererWatchdogUiState,
} from "./useRendererMemoryWatchdog";
import {
  formatCountdownLabel,
  formatStormAge,
  formatWorkingSet,
} from "@/components/RendererMemoryWatchdogNotices";

const T0 = 1_700_000_000_000;

/** A `reload_warning` exactly as `heal()` emits it (camelCase per serde). */
function warningEvent(overrides: Partial<RendererWatchdogEvent> = {}): RendererWatchdogEvent {
  return {
    kind: "reload_warning",
    breach: "fast_slope",
    totalWsBytes: 1_600_000_000,
    countdownSecs: 10,
    reloadTotal: 0,
    message:
      "Reclaiming renderer memory — reloading in 10 s. Terminal sessions are preserved. " +
      "(renderer memory climbing 84.00 MB/min (> 30.00 over 10 min))",
    ...overrides,
  };
}

/** A `storming` event exactly as `escalate_storm()` emits it. */
function stormEvent(overrides: Partial<RendererWatchdogEvent> = {}): RendererWatchdogEvent {
  return {
    kind: "storming",
    breach: "total_ceiling",
    totalWsBytes: 1_700_000_000,
    countdownSecs: 0,
    reloadTotal: 2,
    message:
      "Renderer memory leak the reload can't outrun — restart recommended. " +
      "(total WebView2 working set 1700000000 B > 1500000000 B)",
    ...overrides,
  };
}

/** A `reload_result` event exactly as `heal()` emits it after a verified heal. */
function resultEvent(overrides: Partial<RendererWatchdogEvent> = {}): RendererWatchdogEvent {
  return {
    kind: "reload_result",
    breach: "fast_slope",
    totalWsBytes: 820_000_000,
    countdownSecs: 0,
    reloadTotal: 1,
    reclaimedBytes: 780_000_000,
    message: "Renderer reloaded — reclaimed 780000000 bytes (1600000000 B → 820000000 B).",
    ...overrides,
  };
}

describe("the event channel", () => {
  it("names the same channel as renderer_watchdog::WATCHDOG_EVENT", () => {
    expect(RENDERER_MEMORY_WATCHDOG_EVENT).toBe("renderer-memory-watchdog");
  });
});

describe("reduceRendererWatchdogEvent — one surface per emitted kind", () => {
  it("reload_warning arms the countdown and raises no banner (§6 Q2)", () => {
    const s = reduceRendererWatchdogEvent(INITIAL_RENDERER_WATCHDOG_UI_STATE, warningEvent(), T0);
    expect(s.storm).toBeNull();
    expect(s.warning).not.toBeNull();
    expect(s.warning).toMatchObject({
      breach: "fast_slope",
      countdownSecs: 10,
      totalWsBytes: 1_600_000_000,
      reloadTotal: 0,
      startedAtMs: T0,
    });
    // The Rust side owns the phrasing; the listener carries it verbatim.
    expect(s.warning?.message).toBe(warningEvent().message);
  });

  it("storming raises the persistent banner and raises no countdown (§6 Q3)", () => {
    const s = reduceRendererWatchdogEvent(INITIAL_RENDERER_WATCHDOG_UI_STATE, stormEvent(), T0);
    expect(s.warning).toBeNull();
    expect(s.storm).toMatchObject({
      breach: "total_ceiling",
      totalWsBytes: 1_700_000_000,
      reloadTotal: 2,
      sinceMs: T0,
    });
    expect(s.storm?.message).toContain("restart recommended");
  });

  it("reload_result retires the countdown and raises neither surface", () => {
    const armed = reduceRendererWatchdogEvent(
      INITIAL_RENDERER_WATCHDOG_UI_STATE,
      warningEvent(),
      T0,
    );
    const s = reduceRendererWatchdogEvent(armed, resultEvent(), T0 + 12_000);
    expect(s.warning).toBeNull();
    expect(s.storm).toBeNull();
  });

  it("reload_result with nothing to retire returns the same state object", () => {
    const s = reduceRendererWatchdogEvent(INITIAL_RENDERER_WATCHDOG_UI_STATE, resultEvent(), T0);
    expect(s).toBe(INITIAL_RENDERER_WATCHDOG_UI_STATE);
  });

  it("reload_result does NOT clear a storm — heal() escalates right after one", () => {
    // `heal()` emits `reload_result` and THEN calls `escalate_storm` when the
    // reclaim was below `min_reclaim_bytes`, so a result is not evidence the
    // storm ended. Order the two the way the Rust side does.
    const afterResult = reduceRendererWatchdogEvent(
      INITIAL_RENDERER_WATCHDOG_UI_STATE,
      resultEvent({ reclaimedBytes: 1_000 }),
      T0,
    );
    const stormed = reduceRendererWatchdogEvent(afterResult, stormEvent(), T0 + 1);
    const nextResult = reduceRendererWatchdogEvent(
      stormed,
      resultEvent({ reloadTotal: 2 }),
      T0 + 60_000,
    );
    expect(nextResult.storm).not.toBeNull();
    expect(nextResult.storm?.sinceMs).toBe(T0 + 1);
  });

  it("a storm clears a live countdown — reloads have stopped", () => {
    const armed = reduceRendererWatchdogEvent(
      INITIAL_RENDERER_WATCHDOG_UI_STATE,
      warningEvent(),
      T0,
    );
    const s = reduceRendererWatchdogEvent(armed, stormEvent(), T0 + 10_000);
    expect(s.warning).toBeNull();
    expect(s.storm).not.toBeNull();
  });

  it("ignores an unknown future kind rather than guessing a surface", () => {
    const s = reduceRendererWatchdogEvent(
      INITIAL_RENDERER_WATCHDOG_UI_STATE,
      { ...stormEvent(), kind: "some_future_kind" },
      T0,
    );
    expect(s).toBe(INITIAL_RENDERER_WATCHDOG_UI_STATE);
  });

  it("a second reload_warning replaces the first — one webview, one heal", () => {
    const first = reduceRendererWatchdogEvent(
      INITIAL_RENDERER_WATCHDOG_UI_STATE,
      warningEvent(),
      T0,
    );
    const second = reduceRendererWatchdogEvent(
      first,
      warningEvent({ breach: "slow_slope", countdownSecs: 30, reloadTotal: 1 }),
      T0 + 600_000,
    );
    expect(second.warning).toMatchObject({
      breach: "slow_slope",
      countdownSecs: 30,
      reloadTotal: 1,
      startedAtMs: T0 + 600_000,
    });
  });
});

describe("the storm banner is idempotent under re-emission (§6 Q3)", () => {
  it("a repeated identical storm event does not duplicate the banner", () => {
    // `escalate_storm` re-emits on EVERY breaching tick while latching only its
    // loud log, so the listener sees this event over and over.
    let s: RendererWatchdogUiState = INITIAL_RENDERER_WATCHDOG_UI_STATE;
    const first = reduceRendererWatchdogEvent(s, stormEvent(), T0);
    s = first;
    for (let tick = 1; tick <= 25; tick += 1) {
      s = reduceRendererWatchdogEvent(s, stormEvent(), T0 + tick * 30_000);
    }
    // One slot, so "duplicate" is structurally impossible — and nothing new
    // arrived, so even the state object is untouched (React skips the render).
    expect(s).toBe(first);
    expect(s.storm).toBe(first.storm);
    expect(s.storm?.sinceMs).toBe(T0);
  });

  it("re-emission refreshes the numbers in place and keeps the original sinceMs", () => {
    const first = reduceRendererWatchdogEvent(INITIAL_RENDERER_WATCHDOG_UI_STATE, stormEvent(), T0);
    const later = reduceRendererWatchdogEvent(
      first,
      stormEvent({ totalWsBytes: 1_900_000_000 }),
      T0 + 120_000,
    );
    expect(later).not.toBe(first);
    expect(later.storm?.totalWsBytes).toBe(1_900_000_000);
    // Still ONE banner, and it has been up since the first escalation.
    expect(later.storm?.sinceMs).toBe(T0);
    expect(formatStormAge(later.storm!, T0 + 120_000)).toBe("2m");
  });

  it("a storm re-emitted while a countdown is live still clears the countdown once", () => {
    const stormed = reduceRendererWatchdogEvent(
      INITIAL_RENDERER_WATCHDOG_UI_STATE,
      stormEvent(),
      T0,
    );
    const armed = reduceRendererWatchdogEvent(stormed, warningEvent(), T0 + 1_000);
    expect(armed.warning).not.toBeNull();
    const again = reduceRendererWatchdogEvent(stormed, stormEvent(), T0 + 2_000);
    expect(again.warning).toBeNull();
    expect(again.storm?.sinceMs).toBe(T0);
  });
});

describe("the countdown elapses on its own (§6 Q2)", () => {
  const armed = reduceRendererWatchdogEvent(INITIAL_RENDERER_WATCHDOG_UI_STATE, warningEvent(), T0);
  const warning = armed.warning!;

  it("counts down second by second", () => {
    expect(warningSecondsRemaining(warning, T0)).toBe(10);
    expect(warningSecondsRemaining(warning, T0 + 1_000)).toBe(9);
    expect(warningSecondsRemaining(warning, T0 + 1_999)).toBe(9);
    expect(warningSecondsRemaining(warning, T0 + 9_000)).toBe(1);
  });

  it("floors at zero and never goes negative — nobody has to acknowledge it", () => {
    expect(warningSecondsRemaining(warning, T0 + 10_000)).toBe(0);
    expect(warningSecondsRemaining(warning, T0 + 90_000)).toBe(0);
    expect(isWarningElapsed(warning, T0 + 9_999)).toBe(false);
    expect(isWarningElapsed(warning, T0 + 10_000)).toBe(true);
  });

  it("renders a shrinking number, then hands over", () => {
    expect(formatCountdownLabel(warning, T0)).toBe("Reloading in 10s");
    expect(formatCountdownLabel(warning, T0 + 7_000)).toBe("Reloading in 3s");
    expect(formatCountdownLabel(warning, T0 + 10_000)).toBe("Reloading now…");
  });

  it("is retired only after the reload attempt has had its window", () => {
    // The ladder can decline, wedge, exhaust or fail, and emits nothing on any
    // of those arms — so the toast must retire itself rather than sit at 0s.
    expect(isWarningStale(warning, T0 + 10_000)).toBe(false);
    expect(isWarningStale(warning, T0 + (10 + WARNING_LINGER_SECS) * 1000)).toBe(true);
    expect(advanceRendererWatchdogClock(armed, T0 + 10_000)).toBe(armed);
    expect(advanceRendererWatchdogClock(armed, T0 + 60_000).warning).toBeNull();
  });

  it("the clock never invents a surface where there was none", () => {
    expect(advanceRendererWatchdogClock(INITIAL_RENDERER_WATCHDOG_UI_STATE, T0 + 1e9)).toBe(
      INITIAL_RENDERER_WATCHDOG_UI_STATE,
    );
    const stormed = reduceRendererWatchdogEvent(
      INITIAL_RENDERER_WATCHDOG_UI_STATE,
      stormEvent(),
      T0,
    );
    // A storm is persistent: no clock tick retires it.
    expect(advanceRendererWatchdogClock(stormed, T0 + 1e9)).toBe(stormed);
  });
});

describe("reload_result toast severity", () => {
  it("a heal that reclaimed memory is good news", () => {
    expect(reloadResultToastType(resultEvent())).toBe("success");
  });
  it("a heal that reclaimed nothing, or went backwards, is not", () => {
    expect(reloadResultToastType(resultEvent({ reclaimedBytes: 0 }))).toBe("info");
    expect(reloadResultToastType(resultEvent({ reclaimedBytes: -4_000_000 }))).toBe("info");
    // `reclaimedBytes` is `skip_serializing_if` — ABSENT, not null.
    const { reclaimedBytes: _omitted, ...withoutField } = resultEvent();
    expect(reloadResultToastType(withoutField)).toBe("info");
  });
});

describe("formatWorkingSet", () => {
  it("renders MB under a gigabyte and GB above it", () => {
    expect(formatWorkingSet(788 * 1024 * 1024)).toBe("788 MB");
    expect(formatWorkingSet(1_610_612_736)).toBe("1.50 GB");
  });
  it("says unknown rather than 0 MB for an unreadable sample", () => {
    expect(formatWorkingSet(0)).toBe("unknown");
    expect(formatWorkingSet(Number.NaN)).toBe("unknown");
  });
});
