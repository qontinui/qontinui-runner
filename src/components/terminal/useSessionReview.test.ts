// @vitest-environment jsdom
/**
 * The session-switch race in `useSessionReview`'s gated reads.
 *
 * The REAL hook is rendered (React 19 `createRoot` + `act`, no Testing
 * Library) with both reads and the review mutation mocked as deferred
 * promises, and the Tauri event bus mocked so a test can fire
 * `commit-state-changed` / `session-review-changed` for a given session.
 *
 * Races replayed: a debounce timer armed for session A firing after the
 * surface moved to B, and a mutation for A settling after the switch. Each
 * would abort B's in-flight read (and re-read A) without the guards in
 * `useGatedRead.refresh` / the debounce-timer cleanup.
 */

import { act, createElement, useLayoutEffect } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

type Handler = (e: { payload: Record<string, unknown> }) => void;
const listeners = new Map<string, Set<Handler>>();

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn((event: string, handler: Handler) => {
    let set = listeners.get(event);
    if (!set) {
      set = new Set();
      listeners.set(event, set);
    }
    set.add(handler);
    return Promise.resolve(() => {
      set.delete(handler);
    });
  }),
}));

interface Deferred<T> {
  promise: Promise<T>;
  resolve: (v: T) => void;
  reject: (e: unknown) => void;
}
function deferred<T>(): Deferred<T> {
  let resolve!: (v: T) => void;
  let reject!: (e: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

/** One recorded call of a mocked read: the id, its signal, and its deferred. */
interface Call<T> {
  id: string;
  signal: AbortSignal;
  d: Deferred<T>;
}
const changesCalls: Call<unknown>[] = [];
const reviewCalls: Call<unknown>[] = [];
const markCalls: { id: string; d: Deferred<void> }[] = [];

vi.mock("./workerFileChanges", () => ({
  fetchSessionFileChanges: vi.fn((id: string, signal: AbortSignal) => {
    const d = deferred<unknown>();
    changesCalls.push({ id, signal, d });
    return d.promise;
  }),
}));

vi.mock("./sessionReviewApi", () => ({
  fetchSessionReview: vi.fn((id: string, signal: AbortSignal) => {
    const d = deferred<unknown>();
    reviewCalls.push({ id, signal, d });
    return d.promise;
  }),
  markHunksRead: vi.fn((id: string) => {
    const d = deferred<void>();
    markCalls.push({ id, d });
    return d.promise;
  }),
  createReviewNote: vi.fn(),
  patchReviewNote: vi.fn(),
  sendReview: vi.fn(),
  insertReview: vi.fn(),
}));

import { isCurrentSession, useSessionReview, type SessionReviewHandle } from "./useSessionReview";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

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

let container: HTMLDivElement;
let root: Root;
/** The latest committed handle, published by `Probe` after each commit. */
const out: { current: SessionReviewHandle | null } = { current: null };
function handle(): SessionReviewHandle {
  if (!out.current) throw new Error("Probe has not committed");
  return out.current;
}

function Probe({ sessionId }: { sessionId: string | null }) {
  const h = useSessionReview(sessionId, { visible: true });
  useLayoutEffect(() => {
    out.current = h;
  });
  return null;
}

function render(sessionId: string | null) {
  act(() => {
    root.render(createElement(Probe, { sessionId }));
  });
}

function fire(event: string, payload: Record<string, unknown>) {
  act(() => {
    for (const h of listeners.get(event) ?? []) h({ payload });
  });
}

/** Settle a deferred and let its `.then` chain commit. */
async function settle(fn: () => void) {
  await act(async () => {
    fn();
    await Promise.resolve();
  });
}

const latest = <T>(calls: Call<T>[], id: string): Call<T> => {
  const c = calls.filter((x) => x.id === id).at(-1);
  if (!c) throw new Error(`no call for ${id}`);
  return c;
};

const changesBody = (id: string) => ({ taskRunId: id, files: [] });
const reviewBody = (id: string) => ({ sessionId: id, notes: [], hunks: [] });

describe("useSessionReview session switch (real hook)", () => {
  beforeEach(() => {
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
    listeners.clear();
    changesCalls.length = 0;
    reviewCalls.length = 0;
    markCalls.length = 0;
    out.current = null;
    container = document.createElement("div");
    root = createRoot(container);
  });
  afterEach(() => {
    act(() => root.unmount());
    vi.useRealTimers();
  });

  it("a debounce armed for A that would fire after the switch to B leaves B's read alone", async () => {
    render("a");
    expect(changesCalls.map((c) => c.id)).toEqual(["a"]);

    // commit-state-changed for A arms the 750 ms debounce with A's refresh.
    fire("commit-state-changed", { task_run_id: "a" });

    render("b");
    const bChanges = latest(changesCalls, "b");
    const changesCountAfterSwitch = changesCalls.length;

    act(() => {
      vi.advanceTimersByTime(750);
    });

    expect(bChanges.signal.aborted).toBe(false);
    expect(changesCalls.length).toBe(changesCountAfterSwitch);

    await settle(() => bChanges.d.resolve(changesBody("b")));
    expect(handle().sessionId).toBe("b");
    expect(handle().changes.status).toBe("ok");
  });

  it("without the switch, A's debounce still re-reads A", () => {
    render("a");
    fire("commit-state-changed", { task_run_id: "a" });
    expect(changesCalls.filter((c) => c.id === "a")).toHaveLength(1);
    act(() => {
      vi.advanceTimersByTime(750);
    });
    expect(changesCalls.filter((c) => c.id === "a")).toHaveLength(2);
  });

  it("a mutation for A that settles after the switch neither aborts B's review read nor re-reads A", async () => {
    render("a");
    await settle(() => latest(reviewCalls, "a").d.resolve(reviewBody("a")));

    let markDone!: Promise<void>;
    act(() => {
      markDone = handle().markRead([], true);
    });
    expect(markCalls.map((c) => c.id)).toEqual(["a"]);

    render("b");
    const bReview = latest(reviewCalls, "b");
    const aReviewReads = reviewCalls.filter((c) => c.id === "a").length;

    await act(async () => {
      markCalls[0].d.resolve();
      await markDone;
    });

    expect(bReview.signal.aborted).toBe(false);
    expect(reviewCalls.filter((c) => c.id === "a").length).toBe(aReviewReads);

    await settle(() => bReview.d.resolve(reviewBody("b")));
    expect(handle().review.status).toBe("ok");
  });

  it("a failed review read surfaces the review fallback message", async () => {
    render("a");
    await settle(() => latest(reviewCalls, "a").d.reject(new Error("")));
    const review = handle().review;
    expect(review.status).toBe("error");
    if (review.status !== "error") throw new Error("unreachable");
    expect(review.error).toBe("Failed to load the session's review notes");
    expect(review.previous).toBeNull();
  });
});
