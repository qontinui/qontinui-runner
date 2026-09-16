/**
 * The hidden-worker reversal — BEHAVIOUR, not markup.
 *
 * Round 1 shipped the reversal (close a Conductor worker cell, get it back from
 * the chip) with its stateful half pinned only by
 * `expect(source).toContain("restoreHiddenWorkers()")`: reverting the hook
 * logic while leaving the JSX kept every test green, and a prettier reflow
 * broke the test without any behaviour changing. The reversal's state
 * transitions now live in `hiddenWorkerReducer.ts` as pure functions, and this
 * is the coverage that actually fails when the reversal stops working.
 *
 * (The runner's vitest environment is `node` — no jsdom, no React Testing
 * Library — so a hook cannot be driven directly; extracting the pure reducer is
 * the shape that works here.)
 */

import { describe, it, expect } from "vitest";
import {
  EMPTY_HIDDEN_WORKER_STATE,
  beginRestore,
  forgetAdoptedWorker,
  hideWorker,
  recordRestoreMisses,
  workerDismissalKeys,
} from "./hiddenWorkerReducer";
import type { HiddenWorker } from "./useTerminalManager";

const worker = (partial: Partial<HiddenWorker> = {}): HiddenWorker => ({
  tabId: "tab-1",
  taskRunId: "trid-1",
  title: "worker: refactor parser",
  hiddenAtMs: 1000,
  ...partial,
});

describe("hideWorker", () => {
  it("records the dismissal AND the chip row together", () => {
    // Without the dismissal the live adoption probe re-adds the tab on the
    // worker's next `ai-output` line and "close" is a three-second hide.
    // Without the row the operator has no way back at all.
    const state = hideWorker(EMPTY_HIDDEN_WORKER_STATE, worker());
    expect(state.hidden.map((w) => w.tabId)).toEqual(["tab-1"]);
    expect(state.dismissed.has("tab-1")).toBe(true);
    expect(state.dismissed.has("trid-1")).toBe(true);
  });

  it("dismisses under BOTH ids, because the probe is keyed by task run id", () => {
    expect(workerDismissalKeys({ tabId: "a", taskRunId: "b" })).toEqual(["a", "b"]);
    // A tab with no task run id, or one equal to the tab id, is one key.
    expect(workerDismissalKeys({ tabId: "a", taskRunId: null })).toEqual(["a"]);
    expect(workerDismissalKeys({ tabId: "a", taskRunId: "a" })).toEqual(["a"]);
  });

  it("is idempotent on tabId (React may invoke an updater twice)", () => {
    const once = hideWorker(EMPTY_HIDDEN_WORKER_STATE, worker());
    const twice = hideWorker(once, worker({ hiddenAtMs: 2000 }));
    expect(twice.hidden).toHaveLength(1);
    expect(twice.hidden[0].hiddenAtMs).toBe(1000);
  });

  it("mutates nothing it was given", () => {
    const before = hideWorker(EMPTY_HIDDEN_WORKER_STATE, worker());
    hideWorker(before, worker({ tabId: "tab-2", taskRunId: "trid-2" }));
    expect(before.hidden).toHaveLength(1);
    expect(before.dismissed.has("trid-2")).toBe(false);
    expect(EMPTY_HIDDEN_WORKER_STATE.hidden).toHaveLength(0);
    expect(EMPTY_HIDDEN_WORKER_STATE.dismissed.size).toBe(0);
  });
});

describe("beginRestore", () => {
  const two = hideWorker(
    hideWorker(EMPTY_HIDDEN_WORKER_STATE, worker()),
    worker({ tabId: "tab-2", taskRunId: "trid-2", title: "worker: tests" }),
  );

  it("clears the dismissal so the adoption probe may run again", () => {
    // This is the whole reversal: while the id is dismissed, `maybeAdoptWorker`
    // returns false before doing anything at all.
    const { state, restoring } = beginRestore(two);
    expect(restoring.map((w) => w.tabId)).toEqual(["tab-1", "tab-2"]);
    expect(state.dismissed.size).toBe(0);
    expect(state.hidden).toHaveLength(0);
  });

  it("names the probe-throttle keys so a QUIET worker comes back now", () => {
    // Without dropping the throttle the adoption read waits for the worker's
    // next event — and a finished-but-live worker has none, so an explicit
    // "show" would silently do nothing, which is the failure being fixed.
    expect(beginRestore(two).probeKeys).toEqual(["trid-1", "trid-2"]);
    // A row with no task run id throttles under its tab id.
    const noTrid = hideWorker(EMPTY_HIDDEN_WORKER_STATE, worker({ taskRunId: null }));
    expect(beginRestore(noTrid).probeKeys).toEqual(["tab-1"]);
  });

  it("restores only the named tabs, leaving the rest hidden AND dismissed", () => {
    const { state, restoring } = beginRestore(two, ["tab-2"]);
    expect(restoring.map((w) => w.tabId)).toEqual(["tab-2"]);
    expect(state.hidden.map((w) => w.tabId)).toEqual(["tab-1"]);
    expect(state.dismissed.has("tab-1")).toBe(true);
    expect(state.dismissed.has("trid-1")).toBe(true);
    expect(state.dismissed.has("tab-2")).toBe(false);
  });

  it("is a no-op — same state object — when nothing matches", () => {
    const result = beginRestore(two, ["tab-nope"]);
    expect(result.restoring).toHaveLength(0);
    expect(result.state).toBe(two);
    expect(beginRestore(EMPTY_HIDDEN_WORKER_STATE).restoring).toHaveLength(0);
  });
});

describe("recordRestoreMisses", () => {
  const two = hideWorker(
    hideWorker(EMPTY_HIDDEN_WORKER_STATE, worker()),
    worker({ tabId: "tab-2", taskRunId: "trid-2", title: "worker: tests" }),
  );

  it("re-lists an unadoptable worker with a timestamp, so the chip can say so", () => {
    const { state, restoring } = beginRestore(two);
    const after = recordRestoreMisses(state, [restoring[1]], 4242);
    expect(after.hidden.map((w) => w.tabId)).toEqual(["tab-2"]);
    expect(after.hidden[0].restoreMissedAtMs).toBe(4242);
  });

  it("leaves the DISMISSAL cleared, so a transient miss self-heals", () => {
    // The miss is often transient (a failed `terminal_session_list_open`, or a
    // record not yet written). Re-dismissing would make the operator's click
    // permanent in exactly the case where waiting would have worked.
    const { state, restoring } = beginRestore(two);
    const after = recordRestoreMisses(state, restoring, 1);
    expect(after.dismissed.size).toBe(0);
  });

  it("does not duplicate a row that is already listed, and no-ops on none", () => {
    const after = recordRestoreMisses(two, [worker()], 5);
    expect(after.hidden).toHaveLength(2);
    expect(recordRestoreMisses(two, [], 5)).toBe(two);
  });
});

describe("forgetAdoptedWorker", () => {
  it("drops the row once the worker is back on screen", () => {
    const { state, restoring } = beginRestore(
      hideWorker(EMPTY_HIDDEN_WORKER_STATE, worker()),
    );
    const missed = recordRestoreMisses(state, restoring, 1);
    expect(missed.hidden).toHaveLength(1);
    // The live probe adopted it on the worker's next event.
    const adopted = forgetAdoptedWorker(missed, "tab-1", "trid-1");
    expect(adopted.hidden).toHaveLength(0);
  });

  it("matches on either id", () => {
    const state = hideWorker(EMPTY_HIDDEN_WORKER_STATE, worker());
    expect(forgetAdoptedWorker(state, "other-tab", "trid-1").hidden).toHaveLength(0);
    expect(forgetAdoptedWorker(state, "tab-1", "other-trid").hidden).toHaveLength(0);
  });

  it("returns the SAME state when there is nothing to drop", () => {
    const state = hideWorker(EMPTY_HIDDEN_WORKER_STATE, worker());
    expect(forgetAdoptedWorker(state, "tab-9", "trid-9")).toBe(state);
  });
});

describe("the full close → show → miss → adopt round trip", () => {
  it("ends where it started, with the dismissal gone", () => {
    // The sequence the operator actually performs, as one assertion: close the
    // cell, click "show", have the adoption miss, then have the live probe pick
    // the worker up on its next event.
    const closed = hideWorker(EMPTY_HIDDEN_WORKER_STATE, worker());
    expect(closed.dismissed.has("trid-1")).toBe(true);

    const { state: restoring, restoring: entries, probeKeys } = beginRestore(closed);
    expect(probeKeys).toEqual(["trid-1"]);
    expect(restoring.dismissed.has("trid-1")).toBe(false);

    const missed = recordRestoreMisses(restoring, entries, 99);
    expect(missed.hidden[0].restoreMissedAtMs).toBe(99);
    expect(missed.dismissed.has("trid-1")).toBe(false);

    const adopted = forgetAdoptedWorker(missed, "tab-1", "trid-1");
    expect(adopted.hidden).toHaveLength(0);
    expect(adopted.dismissed.size).toBe(0);
  });
});
