/**
 * useOsHealth — the module singleton and the banner's dwell / dismissal state
 * (plan `2026-10-19-fleet-is-blind-to-kernel-object-leaks-and-os-process-creation-latency`
 * Phase 5). The runner's vitest config is `environment: "node"`, so the Tauri
 * `invoke` / `listen` are mocked and the 10-minute dwell is driven by a fake
 * clock.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import {
  advanceTracker,
  bannerVisible,
  dismissOsHealthBanner,
  INITIAL_TRACKER,
  isOsHealthPayload,
  OS_HEALTH_EVENT,
  OS_HEALTH_GET_COMMAND,
  POLL_MS,
  resetOsHealthForTests,
  subscribeOsHealth,
  type OsHealthPayload,
  type OsHealthState,
  type OsHealthVerdict,
} from "./useOsHealth";

const mockInvoke = invoke as unknown as ReturnType<typeof vi.fn>;
const mockListen = listen as unknown as ReturnType<typeof vi.fn>;

function payload(verdict: OsHealthVerdict, over: Partial<OsHealthPayload> = {}): OsHealthPayload {
  return {
    verdict,
    reasons: [],
    readings: null,
    platform: "windows",
    ...over,
  };
}

const MIN = 60_000;

describe("advanceTracker / bannerVisible (pure)", () => {
  it("shows only once breach has held the full dwell", () => {
    let t = advanceTracker(INITIAL_TRACKER, "breach", 0);
    expect(bannerVisible(t, "breach", 10 * MIN - 1, 600)).toBe(false);
    t = advanceTracker(t, "breach", 5 * MIN);
    expect(t.breachSince).toBe(0);
    expect(bannerVisible(t, "breach", 10 * MIN, 600)).toBe(true);
  });

  it("a warn restarts the dwell; an unknown neither restarts nor shows it", () => {
    let t = advanceTracker(INITIAL_TRACKER, "breach", 0);
    t = advanceTracker(t, "warn", 6 * MIN);
    t = advanceTracker(t, "breach", 7 * MIN);
    expect(bannerVisible(t, "breach", 16 * MIN, 600)).toBe(false);
    expect(bannerVisible(t, "breach", 17 * MIN, 600)).toBe(true);

    let u = advanceTracker(INITIAL_TRACKER, "breach", 0);
    u = advanceTracker(u, "unknown", 5 * MIN);
    expect(u.breachSince).toBe(0);
    expect(bannerVisible(u, "unknown", 11 * MIN, 600)).toBe(false);
    expect(bannerVisible(u, "breach", 11 * MIN, 600)).toBe(true);
  });

  it("a dismissal holds through warn and breach and is forgotten only on ok", () => {
    let t = { ...advanceTracker(INITIAL_TRACKER, "breach", 0), dismissed: true };
    expect(bannerVisible(t, "breach", 20 * MIN, 600)).toBe(false);
    t = advanceTracker(t, "warn", 21 * MIN);
    t = advanceTracker(t, "breach", 22 * MIN);
    expect(bannerVisible(t, "breach", 40 * MIN, 600)).toBe(false);
    t = advanceTracker(t, "ok", 41 * MIN);
    expect(t).toEqual(INITIAL_TRACKER);
  });

  it("guards the payload shape", () => {
    expect(isOsHealthPayload(payload("ok"))).toBe(true);
    expect(isOsHealthPayload({ ...payload("ok"), verdict: "fine" })).toBe(false);
    expect(isOsHealthPayload({ verdict: "ok" })).toBe(false);
    expect(isOsHealthPayload(null)).toBe(false);
  });
});

describe("the module singleton", () => {
  let current: OsHealthVerdict = "ok";
  let eventHandler: ((e: { payload: unknown }) => void) | null = null;
  let seen: OsHealthState[] = [];
  let unsubscribe: () => void = () => {};

  beforeEach(() => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-10-20T12:00:00Z"));
    resetOsHealthForTests();
    current = "ok";
    eventHandler = null;
    seen = [];
    mockInvoke.mockReset();
    mockListen.mockReset();
    mockInvoke.mockImplementation(async () => payload(current));
    mockListen.mockImplementation(async (_name: string, fn: (e: { payload: unknown }) => void) => {
      eventHandler = fn;
      return () => {};
    });
  });

  afterEach(() => {
    unsubscribe();
    resetOsHealthForTests();
    vi.useRealTimers();
  });

  const last = () => seen[seen.length - 1];

  async function start() {
    unsubscribe = subscribeOsHealth((s) => seen.push(s));
    await vi.advanceTimersByTimeAsync(0);
  }

  it("reads the getter, listens for the event, and polls every 30 s", async () => {
    await start();
    expect(mockInvoke).toHaveBeenCalledWith(OS_HEALTH_GET_COMMAND);
    expect(mockListen).toHaveBeenCalledWith(OS_HEALTH_EVENT, expect.any(Function));
    expect(last().payload?.verdict).toBe("ok");
    const calls = mockInvoke.mock.calls.length;
    await vi.advanceTimersByTimeAsync(POLL_MS);
    expect(mockInvoke.mock.calls.length).toBe(calls + 1);
  });

  it("shows the banner only after breach has held 10 minutes on the fake clock", async () => {
    current = "breach";
    await start();
    expect(last().payload?.verdict).toBe("breach");
    expect(last().bannerVisible).toBe(false);
    await vi.advanceTimersByTimeAsync(9 * MIN + 30_000);
    expect(last().bannerVisible).toBe(false);
    await vi.advanceTimersByTimeAsync(30_000);
    expect(last().bannerVisible).toBe(true);
  });

  it("honours a served bannerDwellSecs", async () => {
    mockInvoke.mockImplementation(async () => payload(current, { bannerDwellSecs: 60 }));
    current = "breach";
    await start();
    await vi.advanceTimersByTimeAsync(60_000);
    expect(last().bannerVisible).toBe(true);
  });

  it("an event clearing the verdict hides the banner at once", async () => {
    current = "breach";
    await start();
    await vi.advanceTimersByTimeAsync(10 * MIN);
    expect(last().bannerVisible).toBe(true);
    eventHandler?.({ payload: payload("ok") });
    expect(last().bannerVisible).toBe(false);
  });

  it("a dismissal lasts until the verdict next clears", async () => {
    current = "breach";
    await start();
    await vi.advanceTimersByTimeAsync(10 * MIN);
    expect(last().bannerVisible).toBe(true);
    dismissOsHealthBanner();
    expect(last().bannerVisible).toBe(false);
    // Still breaching an hour later: still dismissed.
    await vi.advanceTimersByTimeAsync(60 * MIN);
    expect(last().bannerVisible).toBe(false);
    // Clears, then breaches again: the dwell runs afresh and the banner returns.
    current = "ok";
    await vi.advanceTimersByTimeAsync(POLL_MS);
    current = "breach";
    await vi.advanceTimersByTimeAsync(POLL_MS);
    expect(last().bannerVisible).toBe(false);
    await vi.advanceTimersByTimeAsync(10 * MIN);
    expect(last().bannerVisible).toBe(true);
  });

  it("an unknown verdict never shows the banner", async () => {
    current = "unknown";
    await start();
    await vi.advanceTimersByTimeAsync(30 * MIN);
    expect(last().payload?.verdict).toBe("unknown");
    expect(last().bannerVisible).toBe(false);
  });

  it("ignores a malformed payload", async () => {
    await start();
    eventHandler?.({ payload: { verdict: "nope" } });
    expect(last().payload?.verdict).toBe("ok");
  });
});
