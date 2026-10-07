/**
 * The session-switch race in `useSessionReview`'s gated reads.
 *
 * The runner's vitest config is `environment: "node"` with no React Testing
 * Library (see `useCommitState.test.ts`), so the hook is not rendered here.
 * The guard it relies on — `isCurrentSession` — is pure and exported; these
 * tests lock it down, then replay the race (a debounce timer armed for session
 * A firing after the surface moved to B, and a mutation for A settling after
 * the switch) against a harness that wires the guard exactly as `refresh` does.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));

import { isCurrentSession } from "./useSessionReview";

describe("isCurrentSession", () => {
  it("allows a refresh captured for the id being rendered", () => {
    expect(isCurrentSession("a", "a")).toBe(true);
  });

  it("refuses a refresh captured for a previous id", () => {
    expect(isCurrentSession("a", "b")).toBe(false);
  });

  it("refuses when either side has no session", () => {
    expect(isCurrentSession(null, null)).toBe(false);
    expect(isCurrentSession(null, "a")).toBe(false);
    expect(isCurrentSession("a", null)).toBe(false);
  });
});

/**
 * The slice of `useGatedRead` the race touches: a `refresh` closure per
 * session id, the shared `fetchedFor` / in-flight / tagged-state refs, and the
 * live-id ref the guard reads.
 */
function harness() {
  const state = {
    currentId: "a" as string | null,
    fetchedFor: null as string | null,
    taggedFor: null as string | null,
    inFlight: null as AbortController | null,
  };
  const makeRefresh = (sessionId: string | null) => () => {
    if (!isCurrentSession(sessionId, state.currentId)) return;
    state.inFlight?.abort();
    state.inFlight = new AbortController();
    state.fetchedFor = sessionId;
    state.taggedFor = sessionId;
  };
  return { state, makeRefresh };
}

describe("session switch race", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it("a debounce armed for A that fires after the switch to B leaves B's read alone", () => {
    const { state, makeRefresh } = harness();
    const refreshA = makeRefresh("a");
    // commit-state-changed for A: the debounce arms with A's closure.
    setTimeout(refreshA, 750);

    // Switch to B before it fires; B's fetch effect issues B's read.
    state.currentId = "b";
    makeRefresh("b")();
    const bRead = state.inFlight;
    expect(state.fetchedFor).toBe("b");

    vi.advanceTimersByTime(750);

    expect(bRead?.signal.aborted).toBe(false);
    expect(state.inFlight).toBe(bRead);
    expect(state.fetchedFor).toBe("b");
    expect(state.taggedFor).toBe("b");
  });

  it("a mutation for A that settles after the switch does not re-read A", async () => {
    const { state, makeRefresh } = harness();
    const refreshReviewA = makeRefresh("a");
    let resolveOp!: () => void;
    const op = new Promise<void>((r) => {
      resolveOp = r;
    });
    // `settle`: the op runs, its `finally` re-reads the review store.
    const settled = op.finally(refreshReviewA);

    state.currentId = "b";
    makeRefresh("b")();
    const bRead = state.inFlight;

    resolveOp();
    await settled;

    expect(bRead?.signal.aborted).toBe(false);
    expect(state.fetchedFor).toBe("b");
    expect(state.taggedFor).toBe("b");
  });

  it("without the switch, A's debounce still refreshes A", () => {
    const { state, makeRefresh } = harness();
    setTimeout(makeRefresh("a"), 750);
    vi.advanceTimersByTime(750);
    expect(state.fetchedFor).toBe("a");
  });
});
