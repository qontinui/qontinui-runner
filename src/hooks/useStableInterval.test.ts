/**
 * Tests for `StableIntervalController` — the poll driver behind `AiOutputTab`'s
 * 1 s executor-status check (plan
 * `2026-07-28-runner-many-sessions-performance.md` §A5f).
 *
 * The regression these lock down: the old effect depended on the whole `lines`
 * array AND on a `useCallback` keyed on `lines`, so every streamed line tore
 * down the interval, re-armed it, and fired an immediate
 * `fetch(/task-runs/running)` + `invoke("get_executor_status")`. The controller
 * must arm exactly once per enable transition, no matter how often the callback
 * identity changes.
 *
 * The runner's vitest config is `environment: "node"`, so the React hook shell
 * cannot be rendered here — the controller it delegates to is the unit under
 * test, and `useStableInterval` is imported as a rename tripwire.
 */

import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";

import { StableIntervalController, useStableInterval } from "./useStableInterval";

// Tripwire: a rename of the hook must break this file before it breaks
// AiOutputTab's poll.
void useStableInterval;

describe("StableIntervalController", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("arms once and keeps ticking", () => {
    const controller = new StableIntervalController(1000);
    const tick = vi.fn();
    controller.setCallback(tick);
    controller.setEnabled(true);

    vi.advanceTimersByTime(3000);
    expect(tick).toHaveBeenCalledTimes(3);
    expect(controller.armCount).toBe(1);
    controller.dispose();
  });

  it("does NOT re-arm when the callback identity churns per streamed line", () => {
    const controller = new StableIntervalController(1000);
    let calls = 0;
    controller.setCallback(() => {
      calls++;
    });
    controller.setEnabled(true);

    // Simulate 500 streamed lines, each producing a brand new callback closure
    // (what a `useCallback([lines])` did) and a repeated "still enabled" sync
    // (what the effect did).
    for (let i = 0; i < 500; i++) {
      controller.setCallback(() => {
        calls++;
      });
      controller.setEnabled(true);
    }

    expect(controller.armCount).toBe(1);
    expect(calls).toBe(0); // no immediate fire per line

    vi.advanceTimersByTime(1000);
    expect(calls).toBe(1);
    controller.dispose();
  });

  it("always calls through to the LATEST callback", () => {
    const controller = new StableIntervalController(1000);
    const first = vi.fn();
    const second = vi.fn();
    controller.setCallback(first);
    controller.setEnabled(true);
    controller.setCallback(second);

    vi.advanceTimersByTime(1000);
    expect(first).not.toHaveBeenCalled();
    expect(second).toHaveBeenCalledTimes(1);
    controller.dispose();
  });

  it("disarms on disable and re-arms on the next enable", () => {
    const controller = new StableIntervalController(1000);
    const tick = vi.fn();
    controller.setCallback(tick);

    controller.setEnabled(true);
    expect(controller.isArmed).toBe(true);
    vi.advanceTimersByTime(1000);

    controller.setEnabled(false);
    expect(controller.isArmed).toBe(false);
    vi.advanceTimersByTime(5000);
    expect(tick).toHaveBeenCalledTimes(1);

    controller.setEnabled(true);
    expect(controller.armCount).toBe(2);
    vi.advanceTimersByTime(1000);
    expect(tick).toHaveBeenCalledTimes(2);
    controller.dispose();
  });

  it("fireOnEnable runs the callback once on the arm transition only", () => {
    const controller = new StableIntervalController(1000, { fireOnEnable: true });
    const tick = vi.fn();
    controller.setCallback(tick);

    controller.setEnabled(true);
    expect(tick).toHaveBeenCalledTimes(1);

    for (let i = 0; i < 100; i++) controller.setEnabled(true);
    expect(tick).toHaveBeenCalledTimes(1);
    controller.dispose();
  });

  it("changes cadence without leaking the old interval", () => {
    const controller = new StableIntervalController(1000);
    const tick = vi.fn();
    controller.setCallback(tick);
    controller.setEnabled(true);

    controller.setIntervalMs(200);
    vi.advanceTimersByTime(1000);
    expect(tick).toHaveBeenCalledTimes(5);
    expect(controller.armCount).toBe(2);
    controller.dispose();
  });

  it("fire() runs an out-of-band check WITHOUT re-arming or shifting the cadence", () => {
    // The turn-start re-fire: `fireOnEnable` only covers the disabled→enabled
    // edge, so a new turn in a session that already has output would otherwise
    // leave `isAiWorking` poll-bound for a full second. This must stay cheap —
    // if it re-armed, it would be the per-line storm A5f removed.
    const controller = new StableIntervalController(1000);
    const tick = vi.fn();
    controller.setCallback(tick);
    controller.setEnabled(true);
    expect(controller.armCount).toBe(1);

    vi.advanceTimersByTime(600);
    controller.fire();
    expect(tick).toHaveBeenCalledTimes(1);
    expect(controller.armCount).toBe(1);

    // The interval's own tick still lands on its original schedule.
    vi.advanceTimersByTime(400);
    expect(tick).toHaveBeenCalledTimes(2);
    controller.dispose();
  });

  it("fire() after dispose is inert", () => {
    const controller = new StableIntervalController(1000);
    const tick = vi.fn();
    controller.setCallback(tick);
    controller.setEnabled(true);
    controller.dispose();

    controller.fire();
    expect(tick).not.toHaveBeenCalled();
  });

  it("dispose() stops everything", () => {
    const controller = new StableIntervalController(1000);
    const tick = vi.fn();
    controller.setCallback(tick);
    controller.setEnabled(true);
    controller.dispose();

    vi.advanceTimersByTime(10_000);
    expect(tick).not.toHaveBeenCalled();
    expect(controller.isArmed).toBe(false);
  });
});
