/**
 * Tests for `useNow1Hz` — the singleton 1Hz re-render primitive.
 *
 * The runner's vitest config is `environment: "node"` (no jsdom, no
 * React Testing Library) — see HoldingLockBanner.test.tsx for the
 * precedent. We can't `render(<Now1HzProvider>…)` directly; instead we
 * mirror the hook's INTERNAL state machine in pure code and exercise
 * it under `vi.useFakeTimers()`. The exported `Now1HzProvider` and
 * `useNow1Hz` are imported as a tripwire so an accidental rename
 * surfaces here (and so the `.tsx` file participates in `tsc
 * --noEmit`).
 *
 * What we verify:
 *
 *   1. The single-interval model: one `setInterval(..., 1000)`
 *      advances the broadcast value by ~1000 after
 *      `vi.advanceTimersByTime(1100)`.
 *   2. The fallback model: when no provider is mounted (context value
 *      is null), the consumer's private interval still ticks.
 *   3. Singleton fan-out: under one provider, N consumers all see the
 *      same broadcast value (the model: one source, N readers, all
 *      get the same `nowMs`).
 *
 * The JSX shell (Provider returns its children wrapped in
 * `<Context.Provider value={nowMs}>`) is verified manually via the
 * runner-app build + a UI Bridge smoke (the three banner consumers
 * all read off the same shared value).
 */

import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";

import { Now1HzProvider, useNow1Hz } from "./useNow1Hz";

// Tripwire: importing the hook + provider must succeed, so a future
// rename (e.g. renaming to `useNow1Sec`) breaks the test BEFORE
// breaking the runner's App.tsx mount. The hooks themselves can't be
// invoked outside a React render — that's why the model below is a
// pure stand-in. ESLint-disable: intentional unused-but-imported.
void Now1HzProvider;
void useNow1Hz;

// ── Pure model of the provider's interval-driven broadcast ────────────────
//
// Mirrors the JSX behaviour:
//   - `useState(() => Date.now())` initial value.
//   - `useEffect` schedules `setInterval(() => setNowMs(Date.now()), 1000)`.
//   - On unmount, clearInterval.

class ProviderModel {
  public nowMs: number;
  private intervalId: ReturnType<typeof setInterval> | null = null;

  constructor() {
    this.nowMs = Date.now();
  }

  mount(): void {
    this.intervalId = setInterval(() => {
      this.nowMs = Date.now();
    }, 1000);
  }

  unmount(): void {
    if (this.intervalId !== null) {
      clearInterval(this.intervalId);
      this.intervalId = null;
    }
  }
}

// Pure model of the fallback path inside `useNow1Hz`: when the
// provider's context value is null, the consumer runs its OWN private
// interval. The model is identical to the provider model — that's
// the point of "same observable behaviour, just slightly less
// efficient" in the docstring.
class FallbackConsumerModel {
  public nowMs: number;
  private intervalId: ReturnType<typeof setInterval> | null = null;

  constructor() {
    this.nowMs = Date.now();
  }

  mount(contextValue: number | null): void {
    // The hook short-circuits when a provider is present.
    if (contextValue !== null) return;
    this.intervalId = setInterval(() => {
      this.nowMs = Date.now();
    }, 1000);
  }

  unmount(): void {
    if (this.intervalId !== null) {
      clearInterval(this.intervalId);
      this.intervalId = null;
    }
  }
}

// ── Tests ──────────────────────────────────────────────────────────────────

describe("Now1HzProvider — single interval broadcasts every ~1000ms", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date(1_700_000_000_000));
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it("advances nowMs by ~1000 after advanceTimersByTime(1100)", () => {
    const provider = new ProviderModel();
    expect(provider.nowMs).toBe(1_700_000_000_000);
    provider.mount();
    // Advance fake time past the first interval boundary. The
    // setInterval callback fires exactly at t+1000 (within [0, 1100]
    // there is one fire), reads Date.now() (which the fake timer has
    // moved to t+1000 when the callback ran), and assigns it to the
    // broadcast value. So the broadcast advances by 1000ms, not 1100
    // — the trailing 100ms is unspent until the next fire at 2000.
    vi.advanceTimersByTime(1100);
    expect(provider.nowMs).toBe(1_700_000_000_000 + 1000);
    provider.unmount();
  });

  it("keeps broadcasting on subsequent ticks", () => {
    const provider = new ProviderModel();
    provider.mount();
    vi.advanceTimersByTime(1000);
    expect(provider.nowMs).toBe(1_700_000_000_000 + 1000);
    vi.advanceTimersByTime(1000);
    expect(provider.nowMs).toBe(1_700_000_000_000 + 2000);
    vi.advanceTimersByTime(1000);
    expect(provider.nowMs).toBe(1_700_000_000_000 + 3000);
    provider.unmount();
  });

  it("stops broadcasting after unmount (cleanup runs)", () => {
    const provider = new ProviderModel();
    provider.mount();
    vi.advanceTimersByTime(1000);
    const lastSeen = provider.nowMs;
    provider.unmount();
    vi.advanceTimersByTime(5000);
    // Post-unmount, the broadcast value MUST NOT change — the
    // setInterval was cleared by the effect's cleanup.
    expect(provider.nowMs).toBe(lastSeen);
  });
});

describe("useNow1Hz fallback path — no provider in tree", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date(1_700_000_000_000));
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it("schedules its own interval when context value is null", () => {
    const consumer = new FallbackConsumerModel();
    expect(consumer.nowMs).toBe(1_700_000_000_000);
    // null context value → consumer's private interval activates.
    consumer.mount(null);
    // setInterval fires once at t+1000 within [0, 1100], so the value
    // moves to start+1000 (not +1100 — the trailing 100ms is unspent).
    vi.advanceTimersByTime(1100);
    expect(consumer.nowMs).toBe(1_700_000_000_000 + 1000);
    consumer.unmount();
  });

  it("does NOT schedule an interval when a provider is present", () => {
    const consumer = new FallbackConsumerModel();
    // Provider present → consumer short-circuits, no private interval.
    consumer.mount(1_700_000_000_000);
    vi.advanceTimersByTime(5000);
    // Consumer's own nowMs is unchanged (the test consumer's local
    // state isn't what a real component would read — the real hook
    // returns `ctx ?? fallback`, so the context value wins. Here we
    // verify the no-interval-scheduled half of that contract).
    expect(consumer.nowMs).toBe(1_700_000_000_000);
    consumer.unmount();
  });
});

describe("singleton fan-out — N consumers under one provider", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date(1_700_000_000_000));
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it("all consumers read the same nowMs from one provider", () => {
    // Models the real hook's `return ctx ?? fallback;` branch: when
    // the provider is mounted, every consumer reads `ctx`, which is
    // ONE number. There's no per-consumer drift because they all
    // index into the same React context value.
    const provider = new ProviderModel();
    provider.mount();

    // Three consumers (matching the actual count today: HoldingLockBanner,
    // WaitingLockBanner, FileActivityPanel). Each reads from the
    // provider's broadcast.
    const readers = [() => provider.nowMs, () => provider.nowMs, () => provider.nowMs];

    vi.advanceTimersByTime(1000);
    const t1 = readers.map((r) => r());
    expect(new Set(t1).size).toBe(1); // All three see the same value.

    vi.advanceTimersByTime(2500);
    const t2 = readers.map((r) => r());
    expect(new Set(t2).size).toBe(1);
    // Total elapsed = 1000 + 2500 = 3500. The interval has fired at
    // t=1000, 2000, 3000; the last fire stamped Date.now() = start+3000.
    // The trailing 500ms after t=3000 is unspent.
    expect(t2[0]).toBe(1_700_000_000_000 + 3000);

    provider.unmount();
  });

  it("only one interval is scheduled regardless of consumer count", () => {
    // Vitest's fake-timers expose `getTimerCount()` for setTimeout —
    // we count via a sentinel: schedule the provider's interval, count
    // pending timers, schedule N fake consumers WITHOUT their own
    // intervals (because the provider context is non-null), confirm
    // the pending-timer count didn't grow.
    const provider = new ProviderModel();
    provider.mount();
    const beforeConsumers = vi.getTimerCount();

    const consumers = [
      new FallbackConsumerModel(),
      new FallbackConsumerModel(),
      new FallbackConsumerModel(),
    ];
    for (const c of consumers) c.mount(provider.nowMs); // non-null ctx

    const afterConsumers = vi.getTimerCount();
    // No new intervals scheduled — three consumers, ZERO extra timers
    // because each one short-circuited on the non-null context value.
    expect(afterConsumers).toBe(beforeConsumers);

    for (const c of consumers) c.unmount();
    provider.unmount();
  });
});

describe("useNow1Hz contract documentation (tripwire)", () => {
  it("exports the provider and hook by their canonical names", () => {
    // If a refactor renames either symbol, this fails at import resolution
    // before the runtime expectations even run.
    expect(typeof Now1HzProvider).toBe("function");
    expect(typeof useNow1Hz).toBe("function");
  });
});
