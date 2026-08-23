import { describe, it, expect, vi } from "vitest";
import { registerWhenReady } from "./registerWhenReady";

/**
 * Fake interval timers so the retry ladder is driven by the test, not by wall
 * clock. Deliberately hand-rolled rather than `vi.useFakeTimers()`: the unit
 * under test takes its timers by injection precisely so this file never has
 * to reach into global state.
 */
function fakeTimers() {
  let next = 1;
  const live = new Map<number, () => void>();
  return {
    api: {
      setInterval: (fn: () => void, _ms: number) => {
        const handle = next++;
        live.set(handle, fn);
        return handle as unknown as ReturnType<typeof setInterval>;
      },
      clearInterval: (handle: ReturnType<typeof setInterval>) => {
        live.delete(handle as unknown as number);
      },
    },
    /** Fire every live interval once. */
    tick(times = 1) {
      for (let i = 0; i < times; i++) {
        for (const fn of [...live.values()]) fn();
      }
    },
    get liveCount() {
      return live.size;
    },
  };
}

describe("registerWhenReady", () => {
  it("registers synchronously and allocates no timer when already ready", () => {
    const timers = fakeTimers();
    const attempt = vi.fn(() => true);

    registerWhenReady({ attempt, timers: timers.api });

    expect(attempt).toHaveBeenCalledTimes(1);
    expect(timers.liveCount).toBe(0);
  });

  /**
   * The regression this whole module exists for.
   *
   * A live terminal pane whose xterm input (or whose bridge registry) is not
   * ready at mount got exactly ONE attempt and then nothing — leaving the
   * pane permanently unregistered and every terminal custom action on it
   * unreachable. Revert `registerWhenReady` to a single attempt and this test
   * goes red.
   */
  it("keeps retrying until the input finally attaches", () => {
    const timers = fakeTimers();
    let ready = false;
    const attempt = vi.fn(() => ready);

    registerWhenReady({ attempt, timers: timers.api });
    expect(attempt).toHaveBeenCalledTimes(1); // the mount-time miss

    timers.tick(4);
    expect(attempt).toHaveBeenCalledTimes(5);
    expect(timers.liveCount).toBe(1); // still trying

    ready = true; // the textarea attaches / the registry wires up
    timers.tick();

    expect(attempt).toHaveBeenCalledTimes(6);
    expect(timers.liveCount).toBe(0); // landed → loop stops
  });

  it("stops attempting once registration lands", () => {
    const timers = fakeTimers();
    let ready = false;
    const attempt = vi.fn(() => ready);

    registerWhenReady({ attempt, timers: timers.api });
    ready = true;
    timers.tick();
    const callsAtLanding = attempt.mock.calls.length;

    timers.tick(10);
    expect(attempt).toHaveBeenCalledTimes(callsAtLanding);
  });

  it("cancel stops the loop and is idempotent", () => {
    const timers = fakeTimers();
    const attempt = vi.fn(() => false);

    const cancel = registerWhenReady({ attempt, timers: timers.api });
    timers.tick();
    const before = attempt.mock.calls.length;

    cancel();
    cancel(); // second call must not throw
    timers.tick(5);

    expect(attempt).toHaveBeenCalledTimes(before);
    expect(timers.liveCount).toBe(0);
  });

  it("gives up once, loudly, when the budget is exhausted", () => {
    const timers = fakeTimers();
    const attempt = vi.fn(() => false);
    const onGiveUp = vi.fn();

    registerWhenReady({
      attempt,
      intervalMs: 100,
      timeoutMs: 300,
      onGiveUp,
      timers: timers.api,
    });

    timers.tick(3); // 100 + 100 + 100 = the whole budget
    expect(onGiveUp).toHaveBeenCalledTimes(1);
    expect(onGiveUp).toHaveBeenCalledWith(300);
    expect(timers.liveCount).toBe(0);

    timers.tick(5);
    expect(onGiveUp).toHaveBeenCalledTimes(1);
  });

  it("a cancel issued after the loop already stopped is harmless", () => {
    const timers = fakeTimers();
    const cancel = registerWhenReady({ attempt: () => true, timers: timers.api });
    expect(() => cancel()).not.toThrow();
  });
});
