/**
 * The restart cohort's one restore queue (plan
 * `2026-10-21-operator-prepare-for-restart-and-progressive-restore`, Phase 7).
 * The queue's effects are injected, so every rule runs here without Tauri.
 */

import { describe, it, expect, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

import {
  COHORT_NOT_RESTORING,
  GUARD_RETRY_MS,
  RestoreQueue,
  bulkSkipNotice,
  dismissRestoreCohort,
  restorePhaseText,
  verdictFromReply,
  type ResumeVerdict,
} from "./useRestoreQueue";
import type { RestorePlaceholder } from "./restorePlaceholders";

const row = (sid: string, o: Partial<RestorePlaceholder> = {}): RestorePlaceholder => ({
  claudeSessionId: sid,
  prepareId: "prep-1",
  pageId: "default",
  zoneIndex: 0,
  name: sid,
  accountLabel: null,
  configDir: "/cfg",
  workingDir: "/w",
  planSlug: null,
  effortLevel: "high",
  effortSource: "screen",
  held: false,
  finished: false,
  stateAtPrepare: "idle",
  limitResetAt: null,
  blocker: null,
  ...o,
});

/** A controllable fake: each resume waits until the test settles it. */
function harness() {
  const pending = new Map<string, (v: ResumeVerdict) => void>();
  const calls: string[] = [];
  const timers: Array<{ fn: () => void; ms: number; cleared: boolean }> = [];
  const back: Array<[string, string | null]> = [];
  const marked: string[] = [];
  const cleared: string[] = [];
  /** The prepares the webview still knows as restoring. */
  const live = new Set(["prep-1"]);
  const named: string[] = [];
  const queue = new RestoreQueue({
    resume: (id, prepareId) => {
      calls.push(id);
      named.push(prepareId);
      return new Promise((resolve) => pending.set(id, resolve));
    },
    cohortLive: (prepareId) => live.has(prepareId),
    markPending: (id) => marked.push(id),
    clearPending: (id) => cleared.push(id),
    onBack: (r, t) => back.push([r.claudeSessionId, t]),
    setTimer: (fn, ms) => {
      const t = { fn, ms, cleared: false };
      timers.push(t);
      return t;
    },
    clearTimer: (h) => {
      (h as { cleared: boolean }).cleared = true;
    },
  });
  const settle = async (id: string, v: ResumeVerdict) => {
    const resolve = pending.get(id);
    if (!resolve) throw new Error(`no resume in flight for ${id}`);
    pending.delete(id);
    resolve(v);
    // Let the queue's continuation run.
    await new Promise((r) => setTimeout(r, 0));
  };
  return { queue, calls, timers, back, marked, cleared, settle, pending, live, named };
}

describe("RestoreQueue", () => {
  it("restores one at a time and continues after a failure", async () => {
    const h = harness();
    h.queue.enqueue([row("a"), row("b")]);
    // A click while a restore is in flight waits its turn.
    h.queue.enqueue([row("c")]);
    expect(h.calls).toEqual(["a"]);
    expect(h.queue.current()).toBe("a");
    expect(h.queue.phaseOf("b")).toEqual({ phase: "queued" });
    expect(h.queue.phaseOf("a")).toEqual({ phase: "restoring" });

    await h.settle("a", { outcome: "failed", reason: "no_claude_resume_process_observed" });
    expect(h.queue.phaseOf("a")).toEqual({
      phase: "failed",
      reason: "no_claude_resume_process_observed",
    });
    expect(h.calls).toEqual(["a", "b"]);

    await h.settle("b", { outcome: "resumed", terminalId: "t-b" });
    expect(h.queue.phaseOf("b")).toEqual({ phase: "back" });
    expect(h.back).toEqual([["b", "t-b"]]);
    expect(h.cleared).toEqual(["b"]);
    expect(h.calls).toEqual(["a", "b", "c"]);
    expect(h.marked).toEqual(["a", "b", "c"]);
  });

  it("enqueues a session pressed twice only once", async () => {
    const h = harness();
    expect(h.queue.enqueue([row("a")])).toBe(1);
    expect(h.queue.enqueue([row("a")])).toBe(0);
    expect(h.queue.enqueue([row("b"), row("b")])).toBe(1);
    await h.settle("a", { outcome: "resumed", terminalId: null });
    // Back is final: another click enqueues nothing.
    expect(h.queue.enqueue([row("a")])).toBe(0);
    await h.settle("b", { outcome: "resumed", terminalId: null });
    expect(h.calls).toEqual(["a", "b"]);
  });

  it("lets Retry re-enqueue a failed row", async () => {
    const h = harness();
    h.queue.enqueue([row("a")]);
    await h.settle("a", { outcome: "failed", reason: "x" });
    expect(h.queue.enqueue([row("a")])).toBe(1);
    expect(h.calls).toEqual(["a", "a"]);
  });

  it("bulk skips held, finished, blocked and usage-limited rows and says how many", () => {
    const h = harness();
    const r = h.queue.enqueueBulk([
      row("a"),
      row("held", { held: true }),
      row("unread", { held: null }),
      row("done", { finished: true }),
      row("blocked", { blocker: "boot_restore_owns_row" }),
      row("limited", { stateAtPrepare: "limit_hit", limitResetAt: Date.now() + 3_600_000 }),
    ]);
    expect(r).toEqual({
      enqueued: 1,
      skippedHeld: 2,
      skippedFinished: 1,
      skippedBlocked: 1,
      skippedLimitHit: 1,
    });
    expect(bulkSkipNotice(r)).toBe(
      "skipped 2 held · skipped 1 finished · skipped 1 not restorable yet · " +
        "skipped 1 usage-limited (restore by hand)",
    );
    expect(
      bulkSkipNotice({
        enqueued: 3,
        skippedHeld: 0,
        skippedFinished: 0,
        skippedBlocked: 0,
        skippedLimitHit: 0,
      }),
    ).toBeNull();
    expect(h.calls).toEqual(["a"]);
  });

  it("a finished row restores when asked by name (the resume core lifts the mark)", () => {
    const h = harness();
    h.queue.enqueue([row("done", { finished: true })]);
    expect(h.calls).toEqual(["done"]);
  });

  it("bulk orders by page (strip order), then zone", async () => {
    const h = harness();
    h.queue.enqueueBulk(
      [
        row("p2z0", { pageId: "p2", zoneIndex: 0 }),
        row("p1z3", { pageId: "p1", zoneIndex: 3 }),
        row("p1z1", { pageId: "p1", zoneIndex: 1 }),
      ],
      ["p1", "p2"],
    );
    for (const id of ["p1z1", "p1z3", "p2z0"]) {
      expect(h.calls.at(-1)).toBe(id);
      await h.settle(id, { outcome: "resumed", terminalId: null });
    }
  });

  it("a guard refusal waits 15 s, keeps its place and asks again", async () => {
    const h = harness();
    h.queue.enqueue([row("a"), row("b")]);
    await h.settle("a", { outcome: "busy", lane: "thread_count" });
    expect(h.queue.phaseOf("a")).toEqual({ phase: "waiting", reason: "box busy (thread_count)" });
    expect(restorePhaseText(h.queue.phaseOf("a"))).toBe("waiting: box busy (thread_count)");
    // The whole queue waits: the box is busy for every spawn.
    expect(h.calls).toEqual(["a"]);
    expect(h.timers.map((t) => t.ms)).toEqual([GUARD_RETRY_MS]);
    h.timers[0].fn();
    expect(h.calls).toEqual(["a", "a"]);
    await h.settle("a", { outcome: "busy", lane: "thread_count" });
    expect(h.timers).toHaveLength(2);
    h.timers[1].fn();
    await h.settle("a", { outcome: "resumed", terminalId: "t-a" });
    expect(h.calls).toEqual(["a", "a", "a", "b"]);
  });

  // Finding 4 (review): a cohort dismissed from the PANEL (whose dismiss never
  // touched the queue) must not go on being restored row by row.
  it("a cohort that ended mid-Restore-all restores none of its remaining rows", async () => {
    const h = harness();
    h.queue.enqueueBulk([row("a"), row("b"), row("c")]);
    expect(h.calls).toEqual(["a"]);
    expect(h.named).toEqual(["prep-1"]);
    // Dismissed while `a` is in flight: the store no longer holds prep-1.
    h.live.delete("prep-1");
    await h.settle("a", { outcome: "failed", reason: "x" });
    expect(h.calls).toEqual(["a"]);
    expect(h.queue.phaseOf("b")).toBeUndefined();
    expect(h.queue.phaseOf("c")).toBeUndefined();
  });

  it("follows the cohort store: rows of another prepare are dropped at once", async () => {
    const h = harness();
    h.queue.enqueue([row("a"), row("b"), row("c", { prepareId: "prep-2" })]);
    h.queue.retainPrepare("prep-1");
    expect(h.queue.phaseOf("c")).toBeUndefined();
    expect(h.queue.phaseOf("b")).toEqual({ phase: "queued" });
    h.queue.retainPrepare(null);
    expect(h.queue.phaseOf("b")).toBeUndefined();
    await h.settle("a", { outcome: "resumed", terminalId: null });
    expect(h.calls).toEqual(["a"]);
  });

  it("a restore the backend refuses because its cohort ended is dropped, not failed", async () => {
    const h = harness();
    h.queue.enqueue([row("a")]);
    await h.settle("a", { outcome: "gone" });
    expect(h.queue.phaseOf("a")).toBeUndefined();
    expect(h.cleared).toEqual(["a"]);
  });

  it("clear drops the queue and cancels a busy-box wait", async () => {
    const h = harness();
    h.queue.enqueue([row("a"), row("b")]);
    await h.settle("a", { outcome: "busy", lane: "x" });
    h.queue.clear();
    expect(h.timers[0].cleared).toBe(true);
    expect(h.queue.phaseOf("b")).toBeUndefined();
  });
});

describe("verdictFromReply", () => {
  const ok = (row: object) => ({
    status: 200,
    body: { success: true, data: { results: [row] } },
  });
  it("reads resumed with the new pane", () => {
    expect(
      verdictFromReply(ok({ id: "a", outcome: "resumed", evidence: { terminal_id: "t-1" } }), "a"),
    ).toEqual({ outcome: "resumed", terminalId: "t-1" });
  });
  it("reads a resource-guard skip as a busy box, any other skip as a failure", () => {
    expect(
      verdictFromReply(
        ok({ id: "a", outcome: "skipped", reason: "resource_guard:thread_count" }),
        "a",
      ),
    ).toEqual({ outcome: "busy", lane: "thread_count" });
    expect(
      verdictFromReply(ok({ id: "a", outcome: "skipped", reason: "live_terminal" }), "a"),
    ).toEqual({ outcome: "failed", reason: "live_terminal" });
    expect(
      verdictFromReply(
        ok({ id: "a", outcome: "skipped", reason: `${COHORT_NOT_RESTORING}(prep-1)` }),
        "a",
      ),
    ).toEqual({ outcome: "gone" });
  });
  it("never reads a refused call or a missing row as resumed", () => {
    expect(
      verdictFromReply({ status: 503, body: { success: false, error: "lifecycle store" } }, "a"),
    ).toEqual({ outcome: "failed", reason: "lifecycle store" });
    expect(verdictFromReply(ok({ id: "other", outcome: "resumed" }), "a")).toEqual({
      outcome: "failed",
      reason: "no verdict for this session",
    });
  });
});

describe("dismissRestoreCohort", () => {
  // The backend's dismiss clears the restore-pending markers itself (one
  // door for every surface); the webview ends the cohort through `endCohort`.
  it("dismisses, then ends the cohort through the one shared end", async () => {
    const ended: string[] = [];
    const dismissed: string[] = [];
    const err = await dismissRestoreCohort("prep-1", {
      dismiss: async (id) => {
        dismissed.push(id);
      },
      store: { endCohort: (id) => ended.push(id) },
    });
    expect(err).toBeNull();
    expect(dismissed).toEqual(["prep-1"]);
    expect(ended).toEqual(["prep-1"]);
  });

  it("changes nothing when the backend refuses", async () => {
    const ended: string[] = [];
    const err = await dismissRestoreCohort("prep-1", {
      dismiss: async () => {
        throw new Error("not restoring");
      },
      store: { endCohort: (id) => ended.push(id) },
    });
    expect(err).toContain("not restoring");
    expect(ended).toEqual([]);
  });
});
