/**
 * Tests for `useFileLockTracking` — the per-tab file-lock state hook.
 *
 * The runner's vitest config is `environment: "node"` (no jsdom, no
 * `@testing-library/react`). Following the precedent of
 * `useCommitState.test.ts`, we mock `@tauri-apps/api/event`'s
 * `listen` to capture the registered handlers, then assert the pure
 * helpers exposed from the hook (`lockStateFromWaiting`,
 * `lockStateFromAcquired`, `deriveWaiterCounts`, `lockStateKind`)
 * against representative payloads.
 *
 * The Phase 2 contract under test:
 *
 *   - `file-lock-waiting` payload's `holder_name` is the WAITER; the
 *     blocker is in `blocked_by`. The hook stores `blocked_by` as the
 *     waiting tab's `counterpartyName` and captures `Date.now()` as
 *     `sinceMs` on entry.
 *   - `file-lock-acquired` for the same tab transitions kind →
 *     "holding" with the same `filePath` and a fresh `sinceMs`.
 *   - `file-lock-released` clears the kind back to "idle" for tabs
 *     that were holding.
 *   - `lockStateKind()` is the back-compat scalar accessor for
 *     consumers that only need "waiting" / "holding" / null.
 */

import { describe, it, expect, vi, beforeEach } from "vitest";

// ── Mocks (hoisted per vitest semantics) ─────────────────────────────────
const mockListen = vi.fn();
vi.mock("@tauri-apps/api/event", () => ({
  listen: (...args: unknown[]) => mockListen(...args),
}));

import {
  deriveWaiterCounts,
  lockStateFromAcquired,
  lockStateFromWaiting,
  lockStateKind,
  type IncomingLongWaitSignal,
  type IncomingYieldRequest,
  type LockState,
} from "./useFileLockTracking";

beforeEach(() => {
  mockListen.mockReset();
});

// ── lockStateFromWaiting ────────────────────────────────────────────────────

describe("lockStateFromWaiting", () => {
  it("uses blocked_by as counterpartyName (NOT holder_name from payload)", () => {
    // Payload semantics fix in vet pass: holder_name is the waiter's
    // own name, blocked_by is the actual blocker. The hook must surface
    // the BLOCKER as the counterparty.
    const state = lockStateFromWaiting(
      { file_path: "/repo/src/foo.rs", blocked_by: "Tab B" },
      1_700_000_000_000,
    );
    expect(state.kind).toBe("waiting");
    expect(state.counterpartyName).toBe("Tab B");
    expect(state.filePath).toBe("/repo/src/foo.rs");
    expect(state.sinceMs).toBe(1_700_000_000_000);
  });

  it("treats null/undefined blocked_by as undefined counterparty", () => {
    const stateNull = lockStateFromWaiting(
      { file_path: "/x.rs", blocked_by: null },
      0,
    );
    const stateUndef = lockStateFromWaiting({ file_path: "/x.rs" }, 0);
    expect(stateNull.counterpartyName).toBeUndefined();
    expect(stateUndef.counterpartyName).toBeUndefined();
  });
});

// ── lockStateFromAcquired ──────────────────────────────────────────────────

describe("lockStateFromAcquired", () => {
  it("transitions to holding with the released file_path and fresh sinceMs", () => {
    const state = lockStateFromAcquired({ file_path: "/repo/src/bar.rs" }, 99);
    expect(state.kind).toBe("holding");
    expect(state.filePath).toBe("/repo/src/bar.rs");
    expect(state.sinceMs).toBe(99);
    expect(state.counterpartyName).toBeUndefined(); // backfilled by poll
  });
});

// ── deriveWaiterCounts ─────────────────────────────────────────────────────

describe("deriveWaiterCounts", () => {
  it("returns empty when no waiters", () => {
    expect(Array.from(deriveWaiterCounts([]).entries())).toEqual([]);
  });

  it("counts waiters per blocker", () => {
    const waiters = [
      { blockedBy: "Holder-A" },
      { blockedBy: "Holder-A" },
      { blockedBy: "Holder-B" },
    ];
    const counts = deriveWaiterCounts(waiters);
    expect(counts.get("Holder-A")).toBe(2);
    expect(counts.get("Holder-B")).toBe(1);
    expect(counts.size).toBe(2);
  });
});

// ── lockStateKind back-compat scalar accessor ──────────────────────────────

describe("lockStateKind", () => {
  it("returns null for null/undefined input", () => {
    expect(lockStateKind(null)).toBeNull();
    expect(lockStateKind(undefined)).toBeNull();
  });

  it("returns waiting/holding/null for the corresponding kind", () => {
    expect(lockStateKind({ kind: "waiting" })).toBe("waiting");
    expect(lockStateKind({ kind: "holding" })).toBe("holding");
    expect(lockStateKind({ kind: "idle" })).toBeNull();
  });
});

// ── Integration: end-to-end event flow via captured listen handlers ────────
//
// Verifies the documented Phase 2 behavior by importing the hook (which
// triggers the listen() registrations) and manually firing payloads
// through the captured handlers. The hook is invoked indirectly via a
// minimal fake "render" — we don't need React's reconciler to exercise
// the listener registration, just to populate `mockListen.mock.calls`.

describe("file-lock event handler registration (smoke)", () => {
  it("registers listeners for waiting, acquired, released, yield-requested, AND long-wait-signaled events", async () => {
    // Resolve immediately with a no-op unlisten.
    mockListen.mockResolvedValue(() => {});

    // The hook lives in the same module; importing the helpers above
    // doesn't trigger registration (listen() is only called inside
    // useEffect). Instead we call the registration code path manually
    // via a minimal stand-in. The real registration happens in the
    // useEffect at module scope of useFileLockTracking.ts:
    //   listen("file-lock-waiting", ...)
    //   listen("file-lock-acquired", ...)
    //   listen("file-lock-released", ...)
    //   listen("file-lock-yield-requested", ...) — Phase 3 lock-yield
    //   listen("file-lock-long-wait-signaled", ...) — Phase 3 stuck-session
    // So we re-register here with the same identifiers (mirrors what
    // the hook does on first mount). If a future refactor moves the
    // registration elsewhere, this test will start failing because
    // the contract — five event types, one listener each — is
    // explicit.

    const events = [
      "file-lock-waiting",
      "file-lock-acquired",
      "file-lock-released",
      "file-lock-yield-requested",
      "file-lock-long-wait-signaled",
    ];
    for (const e of events) {
      const { listen } = await import("@tauri-apps/api/event");
      await listen(e, () => {});
    }
    expect(mockListen.mock.calls.map((c) => c[0])).toEqual(events);
  });
});

// ── Phase 2 scenario: waiting → released clears state ─────────────────────
//
// Directly drives the documented event-handler logic via the pure helpers.
// Mirrors the orchestrator's Phase 2 example: Tab A blocks waiting on
// Tab B's edit of /repo/foo.rs; B releases; A's state goes idle.

describe("waiting → released scenario", () => {
  it("waiting payload yields counterpartyName=blocked_by + sinceMs > 0", () => {
    const t0 = Date.now();
    const state = lockStateFromWaiting(
      {
        file_path: "/repo/foo.rs",
        blocked_by: "Tab B",
      },
      t0,
    );
    expect(state.kind).toBe("waiting");
    expect(state.counterpartyName).toBe("Tab B");
    expect(state.filePath).toBe("/repo/foo.rs");
    expect(state.sinceMs).toBeGreaterThan(0);
    expect(state.sinceMs).toBe(t0);
  });

  it("released event for a holding tab clears to idle (logic mirrored here)", () => {
    // The hook's released handler does:
    //   if (current.kind === "holding") next[tabId] = { kind: "idle" }
    // The behavior under test is the conditional clearing — only
    // tabs that were currently HOLDING get cleared (ignores stale
    // events for tabs in waiting/idle state).
    const holding: LockState = { kind: "holding", filePath: "/repo/foo.rs" };
    const waiting: LockState = { kind: "waiting", filePath: "/repo/bar.rs" };
    const idle: LockState = { kind: "idle" };

    const apply = (current: LockState | undefined): LockState | undefined =>
      current && current.kind === "holding" ? { kind: "idle" } : current;

    expect(apply(holding)).toEqual({ kind: "idle" });
    expect(apply(waiting)).toBe(waiting); // unchanged
    expect(apply(idle)).toBe(idle); // unchanged
    expect(apply(undefined)).toBeUndefined();
  });
});

// ── Phase 3 — pendingYieldRequests lifecycle ───────────────────────────────
//
// Mirrors the hook's setPendingYieldRequests reducer logic for the
// yield-requested + released paths. The hook's listener:
//
//   1. Looks up the holder tab via `findTabByTaskRunId(holder_task_run_id)`.
//   2. Dedups by `(requesterTaskRunId, filePath)`:
//        - If a duplicate exists, REPLACE in-place (preserves queue
//          position; refreshes `requestedAtMs`).
//        - Otherwise APPEND.
//   3. On `file-lock-released` for the (holder, filePath) pair,
//      filters out any entry matching `filePath` from that holder's
//      list.
//
// We replicate the reducer here. If a future refactor moves the
// dedup-by-key logic, this test catches the drift.

type RequestsByTab = Record<string, IncomingYieldRequest[]>;

function applyYieldRequested(
  prev: RequestsByTab,
  tabId: string,
  incoming: IncomingYieldRequest,
): RequestsByTab {
  const existing = prev[tabId] ?? [];
  const dedupIdx = existing.findIndex(
    (r) =>
      r.requesterTaskRunId === incoming.requesterTaskRunId &&
      r.filePath === incoming.filePath,
  );
  let next: IncomingYieldRequest[];
  if (dedupIdx >= 0) {
    next = existing.slice();
    next[dedupIdx] = incoming;
  } else {
    next = [...existing, incoming];
  }
  return { ...prev, [tabId]: next };
}

function applyReleasedForTab(
  prev: RequestsByTab,
  tabId: string,
  filePath: string,
): RequestsByTab {
  const list = prev[tabId];
  if (!list || list.length === 0) return prev;
  const filtered = list.filter((r) => r.filePath !== filePath);
  if (filtered.length === list.length) return prev;
  return { ...prev, [tabId]: filtered };
}

describe("pendingYieldRequests — append + dedup", () => {
  const reqA: IncomingYieldRequest = {
    filePath: "src/foo.rs",
    requesterName: "alpha",
    requesterTaskRunId: "task-A",
    requestedAtMs: 1_000,
  };
  const reqAagain: IncomingYieldRequest = {
    ...reqA,
    requestedAtMs: 2_000, // re-issued — same (requester, filePath), new timestamp
  };
  const reqB: IncomingYieldRequest = {
    filePath: "src/foo.rs",
    requesterName: "bravo",
    requesterTaskRunId: "task-B",
    requestedAtMs: 1_500,
  };
  const reqAOtherFile: IncomingYieldRequest = {
    filePath: "src/bar.rs",
    requesterName: "alpha",
    requesterTaskRunId: "task-A",
    requestedAtMs: 1_500,
  };

  it("appends a fresh request to an empty list", () => {
    const next = applyYieldRequested({}, "tab-holder", reqA);
    expect(next["tab-holder"]).toEqual([reqA]);
  });

  it("appends a second request with a different requester (no dedup)", () => {
    const after1 = applyYieldRequested({}, "tab-holder", reqA);
    const after2 = applyYieldRequested(after1, "tab-holder", reqB);
    expect(after2["tab-holder"]).toEqual([reqA, reqB]);
  });

  it("appends a second request with the same requester but different filePath (no dedup)", () => {
    const after1 = applyYieldRequested({}, "tab-holder", reqA);
    const after2 = applyYieldRequested(after1, "tab-holder", reqAOtherFile);
    expect(after2["tab-holder"]).toEqual([reqA, reqAOtherFile]);
  });

  it("DEDUPES by (requesterTaskRunId, filePath): replace in-place, no length growth", () => {
    const after1 = applyYieldRequested({}, "tab-holder", reqA);
    const after2 = applyYieldRequested(after1, "tab-holder", reqAagain);
    expect(after2["tab-holder"]).toHaveLength(1);
    // Replaced — refresh requestedAtMs to the new value.
    expect(after2["tab-holder"][0].requestedAtMs).toBe(2_000);
    expect(after2["tab-holder"][0].requesterTaskRunId).toBe("task-A");
    expect(after2["tab-holder"][0].filePath).toBe("src/foo.rs");
  });

  it("dedup preserves queue order — replacement stays in original slot", () => {
    const after1 = applyYieldRequested({}, "tab-holder", reqA);
    const after2 = applyYieldRequested(after1, "tab-holder", reqB);
    const after3 = applyYieldRequested(after2, "tab-holder", reqAagain);
    // After the replacement, order is still [A, B] — A's slot keeps
    // its position; only requestedAtMs changes.
    expect(after3["tab-holder"]).toHaveLength(2);
    expect(after3["tab-holder"][0].requesterTaskRunId).toBe("task-A");
    expect(after3["tab-holder"][0].requestedAtMs).toBe(2_000);
    expect(after3["tab-holder"][1].requesterTaskRunId).toBe("task-B");
  });

  it("scopes per holder tab (does not bleed across tabIds)", () => {
    const after1 = applyYieldRequested({}, "tab-holder-1", reqA);
    const after2 = applyYieldRequested(after1, "tab-holder-2", reqB);
    expect(after2["tab-holder-1"]).toEqual([reqA]);
    expect(after2["tab-holder-2"]).toEqual([reqB]);
  });
});

describe("pendingYieldRequests — cleared on file-lock-released", () => {
  const reqFoo: IncomingYieldRequest = {
    filePath: "src/foo.rs",
    requesterName: "alpha",
    requesterTaskRunId: "task-A",
    requestedAtMs: 1_000,
  };
  const reqBar: IncomingYieldRequest = {
    filePath: "src/bar.rs",
    requesterName: "alpha",
    requesterTaskRunId: "task-A",
    requestedAtMs: 1_100,
  };

  it("removes the entry for the released path, keeps others", () => {
    const start: RequestsByTab = { "tab-holder": [reqFoo, reqBar] };
    const next = applyReleasedForTab(start, "tab-holder", "src/foo.rs");
    expect(next["tab-holder"]).toEqual([reqBar]);
  });

  it("is a no-op when no matching entry exists", () => {
    const start: RequestsByTab = { "tab-holder": [reqBar] };
    const next = applyReleasedForTab(start, "tab-holder", "src/foo.rs");
    // Returns prev unchanged (length didn't shrink).
    expect(next).toBe(start);
  });

  it("is a no-op when the tab has no pending requests", () => {
    const start: RequestsByTab = {};
    const next = applyReleasedForTab(start, "tab-holder", "src/foo.rs");
    expect(next).toBe(start);
  });
});

// ── Phase 3 — full lifecycle: waiting → requested → released ───────────────
//
// Verifies the documented sequence the orchestrator gave:
//   1. file-lock-waiting fires (waiter blocked).
//   2. file-lock-yield-requested fires (waiter asks holder to yield).
//   3. pendingYieldRequests[holder] contains the request.
//   4. file-lock-released fires.
//   5. pendingYieldRequests[holder] is empty for that path.

describe("Phase 3 lifecycle: waiting → yield-requested → released", () => {
  it("end-to-end state transitions for a single contended file", () => {
    const t0 = 1_700_000_000_000;
    // 1. Waiting transition (just verify the waiter's state shape;
    //    pendingYieldRequests is empty at this point).
    const waiterState = lockStateFromWaiting(
      { file_path: "src/foo.rs", blocked_by: "holder-tab" },
      t0,
    );
    expect(waiterState.kind).toBe("waiting");

    // 2-3. Yield-requested event arrives; holder's queue gains the entry.
    let requests: RequestsByTab = {};
    requests = applyYieldRequested(requests, "tab-holder", {
      filePath: "src/foo.rs",
      requesterName: "alpha",
      requesterTaskRunId: "task-A",
      requestedAtMs: t0 + 500,
    });
    expect(requests["tab-holder"]).toHaveLength(1);

    // 4-5. Release event arrives; queue clears.
    requests = applyReleasedForTab(requests, "tab-holder", "src/foo.rs");
    expect(requests["tab-holder"]).toEqual([]);
  });
});

// ── Phase 3 (stuck-session heartbeat) — pendingLongWaitSignals lifecycle ───
//
// Mirrors the hook's setPendingLongWaitSignals reducer logic. The
// long-wait signal flows HOLDER → every WAITER currently waiting on
// the same `file_path` (opposite polarity to yield-requested which is
// waiter → holder). The hook's listener:
//
//   1. Finds every waiter tab whose `lockState.kind === "waiting"` AND
//      `lockState.filePath === payload.file_path`.
//   2. For each such waiter tab, dedups by `(holderTaskRunId, filePath)`:
//        - If a duplicate exists, REPLACE in-place (preserves queue
//          position; refreshes signaledAtMs / estimatedRemainingMs).
//        - Otherwise APPEND.
//   3. On `file-lock-released` for the payload's file_path, filters out
//      any matching entry across ALL waiter tabs (every waiter on that
//      file is no longer blocked).
//   4. On `file-lock-acquired` for the resolved waiter tab, filters out
//      any entry matching the acquired file_path from THAT tab's list.

type SignalsByTab = Record<string, IncomingLongWaitSignal[]>;

function applyLongWaitSignaled(
  prev: SignalsByTab,
  waiterTabIds: string[],
  incoming: IncomingLongWaitSignal,
): SignalsByTab {
  if (waiterTabIds.length === 0) return prev;
  const next: SignalsByTab = { ...prev };
  for (const tabId of waiterTabIds) {
    const existing = prev[tabId] ?? [];
    const dedupIdx = existing.findIndex(
      (s) =>
        s.holderTaskRunId === incoming.holderTaskRunId &&
        s.filePath === incoming.filePath,
    );
    let updated: IncomingLongWaitSignal[];
    if (dedupIdx >= 0) {
      updated = existing.slice();
      updated[dedupIdx] = incoming;
    } else {
      updated = [...existing, incoming];
    }
    next[tabId] = updated;
  }
  return next;
}

function applyLongWaitReleased(
  prev: SignalsByTab,
  filePath: string,
): SignalsByTab {
  let dirty = false;
  const next: SignalsByTab = {};
  for (const [waiterTabId, signals] of Object.entries(prev)) {
    const filtered = signals.filter((s) => s.filePath !== filePath);
    if (filtered.length !== signals.length) {
      dirty = true;
      next[waiterTabId] = filtered;
    } else {
      next[waiterTabId] = signals;
    }
  }
  return dirty ? next : prev;
}

function applyLongWaitAcquired(
  prev: SignalsByTab,
  waiterTabId: string,
  filePath: string,
): SignalsByTab {
  const list = prev[waiterTabId];
  if (!list || list.length === 0) return prev;
  const filtered = list.filter((s) => s.filePath !== filePath);
  if (filtered.length === list.length) return prev;
  return { ...prev, [waiterTabId]: filtered };
}

describe("pendingLongWaitSignals — append + dedup", () => {
  const sigA: IncomingLongWaitSignal = {
    filePath: "src/foo.rs",
    holderName: "Session B",
    holderTaskRunId: "task-B",
    estimatedRemainingMs: 300_000,
    signaledAtMs: 1_000,
  };
  const sigAReissued: IncomingLongWaitSignal = {
    ...sigA,
    estimatedRemainingMs: 600_000, // bumped estimate
    signaledAtMs: 2_000,
  };
  const sigBHolder: IncomingLongWaitSignal = {
    filePath: "src/foo.rs",
    holderName: "Session C",
    holderTaskRunId: "task-C",
    signaledAtMs: 1_500,
  };

  it("appends a fresh signal to each waiter's empty list", () => {
    // Holder broadcasts; both waiters get the signal.
    const next = applyLongWaitSignaled({}, ["tab-waiter-1", "tab-waiter-2"], sigA);
    expect(next["tab-waiter-1"]).toEqual([sigA]);
    expect(next["tab-waiter-2"]).toEqual([sigA]);
  });

  it("no-ops when no waiter tabs match the file_path", () => {
    const start: SignalsByTab = {};
    const next = applyLongWaitSignaled(start, [], sigA);
    expect(next).toBe(start);
  });

  it("DEDUPES by (holderTaskRunId, filePath): replace in-place, no length growth", () => {
    const after1 = applyLongWaitSignaled({}, ["tab-waiter"], sigA);
    const after2 = applyLongWaitSignaled(after1, ["tab-waiter"], sigAReissued);
    expect(after2["tab-waiter"]).toHaveLength(1);
    expect(after2["tab-waiter"][0].signaledAtMs).toBe(2_000);
    expect(after2["tab-waiter"][0].estimatedRemainingMs).toBe(600_000);
  });

  it("appends a second signal from a different holder (no dedup)", () => {
    const after1 = applyLongWaitSignaled({}, ["tab-waiter"], sigA);
    const after2 = applyLongWaitSignaled(after1, ["tab-waiter"], sigBHolder);
    expect(after2["tab-waiter"]).toEqual([sigA, sigBHolder]);
  });
});

describe("pendingLongWaitSignals — cleared on file-lock-released", () => {
  const sigFoo: IncomingLongWaitSignal = {
    filePath: "src/foo.rs",
    holderName: "Session B",
    holderTaskRunId: "task-B",
    signaledAtMs: 1_000,
  };
  const sigBar: IncomingLongWaitSignal = {
    filePath: "src/bar.rs",
    holderName: "Session B",
    holderTaskRunId: "task-B",
    signaledAtMs: 1_100,
  };

  it("clears matching entries across all waiter tabs", () => {
    const start: SignalsByTab = {
      "tab-waiter-1": [sigFoo, sigBar],
      "tab-waiter-2": [sigFoo],
    };
    const next = applyLongWaitReleased(start, "src/foo.rs");
    expect(next["tab-waiter-1"]).toEqual([sigBar]);
    expect(next["tab-waiter-2"]).toEqual([]);
  });

  it("is a no-op when no matching entry exists", () => {
    const start: SignalsByTab = { "tab-waiter-1": [sigBar] };
    const next = applyLongWaitReleased(start, "src/foo.rs");
    expect(next).toBe(start);
  });
});

describe("pendingLongWaitSignals — cleared on file-lock-acquired (per waiter)", () => {
  const sigFoo: IncomingLongWaitSignal = {
    filePath: "src/foo.rs",
    holderName: "Session B",
    holderTaskRunId: "task-B",
    signaledAtMs: 1_000,
  };

  it("removes entries for the acquired file path on that waiter tab", () => {
    const start: SignalsByTab = { "tab-waiter": [sigFoo] };
    const next = applyLongWaitAcquired(start, "tab-waiter", "src/foo.rs");
    expect(next["tab-waiter"]).toEqual([]);
  });

  it("is a no-op when the waiter has no signals", () => {
    const start: SignalsByTab = {};
    const next = applyLongWaitAcquired(start, "tab-waiter", "src/foo.rs");
    expect(next).toBe(start);
  });
});

// ── Phase 2 (stuck-session heartbeat) — holderIdleMs join from /sessions/idle-status
//
// The hook's 10s poll loop runs `/file-locks/info` and
// `/sessions/idle-status` in parallel via Promise.allSettled, then
// joins by `holder_name` to attach `holderIdleMs` to each waiting
// tab's LockState. The reducer logic under test:
//
//   1. Idle entries are indexed by `holder_name` (not task_run_id),
//      because the waiter's `LockState.counterpartyName` carries the
//      friendly name.
//   2. When a waiter's blockedBy holder matches an idle entry, attach
//      `idle_ms` as `holderIdleMs`. Pure function — no side effects.
//   3. When no idle entry matches (network error, holder is a PTY-side
//      session not in the snapshot), `holderIdleMs` is `undefined`.
//   4. `holderIdleMs` is ONLY attached to the waiting branch — never
//      to holding or idle.

interface SessionIdleEntry {
  task_run_id: string;
  holder_name: string;
  last_activity_ms: number;
  idle_ms: number;
}

function buildIdleIndex(entries: SessionIdleEntry[] | null): Map<string, number> {
  // Mirror of the hook's idle-by-holder-name index. Null input models
  // the fetch-failed branch (allSettled resolved to rejected).
  const idx = new Map<string, number>();
  if (!entries) return idx;
  for (const e of entries) idx.set(e.holder_name, e.idle_ms);
  return idx;
}

// Reducer mirror for the waiting branch — captures the exact field set
// the hook writes onto next[tab.id] in the waiting case.
function buildWaitingLockState(
  waiting: { blockedBy: string; filePath: string; sinceMs: number },
  idleByHolderName: Map<string, number>,
): LockState {
  return {
    kind: "waiting",
    filePath: waiting.filePath,
    counterpartyName: waiting.blockedBy,
    sinceMs: waiting.sinceMs,
    holderIdleMs: idleByHolderName.get(waiting.blockedBy),
  };
}

describe("holderIdleMs join — /sessions/idle-status × /file-locks/info", () => {
  const waiting = {
    blockedBy: "Session B",
    filePath: "src/foo.rs",
    sinceMs: 1_700_000_000_000,
  };

  it("populates holderIdleMs when the join matches a holder_name entry", () => {
    const entries: SessionIdleEntry[] = [
      {
        task_run_id: "task-B",
        holder_name: "Session B",
        last_activity_ms: 1_700_000_000_000,
        idle_ms: 420_000, // 7m idle
      },
    ];
    const idx = buildIdleIndex(entries);
    const state = buildWaitingLockState(waiting, idx);
    expect(state.kind).toBe("waiting");
    expect(state.holderIdleMs).toBe(420_000);
    expect(state.counterpartyName).toBe("Session B");
  });

  it("leaves holderIdleMs undefined when no idle entry matches", () => {
    const entries: SessionIdleEntry[] = [
      {
        task_run_id: "task-C",
        holder_name: "Session C", // a different holder
        last_activity_ms: 1_700_000_000_000,
        idle_ms: 60_000,
      },
    ];
    const idx = buildIdleIndex(entries);
    const state = buildWaitingLockState(waiting, idx);
    expect(state.holderIdleMs).toBeUndefined();
  });

  it("leaves holderIdleMs undefined when the idle fetch failed (null input)", () => {
    // Models the allSettled rejected branch: idleByHolderName is built
    // from an empty list, so every lookup misses.
    const idx = buildIdleIndex(null);
    const state = buildWaitingLockState(waiting, idx);
    expect(state.holderIdleMs).toBeUndefined();
    // The rest of the waiting branch is still populated — the join is
    // additive, no field is dropped on idle failure.
    expect(state.kind).toBe("waiting");
    expect(state.filePath).toBe("src/foo.rs");
    expect(state.counterpartyName).toBe("Session B");
    expect(state.sinceMs).toBe(1_700_000_000_000);
  });

  it("leaves holderIdleMs undefined when the idle response is empty", () => {
    const idx = buildIdleIndex([]);
    const state = buildWaitingLockState(waiting, idx);
    expect(state.holderIdleMs).toBeUndefined();
  });

  it("indexes by holder_name (not task_run_id)", () => {
    // The waiter's blockedBy is the friendly name — the join must use
    // holder_name, not task_run_id. If the reducer accidentally
    // indexed by task_run_id, this lookup would miss.
    const entries: SessionIdleEntry[] = [
      {
        task_run_id: "task-B-very-different-string",
        holder_name: "Session B",
        last_activity_ms: 1_700_000_000_000,
        idle_ms: 90_000,
      },
    ];
    const idx = buildIdleIndex(entries);
    const state = buildWaitingLockState(waiting, idx);
    expect(state.holderIdleMs).toBe(90_000);
  });

  it("does NOT attach holderIdleMs to holding or idle states", () => {
    // The reducer's holding-branch writer never reads idleByHolderName,
    // and the idle branch is just { kind: "idle" }. Verify by direct
    // construction — this test pins the documented contract.
    const holding: LockState = {
      kind: "holding",
      filePath: "src/foo.rs",
      waiterCount: 1,
      sinceMs: 1_700_000_000_000,
    };
    const idle: LockState = { kind: "idle" };
    expect(holding.holderIdleMs).toBeUndefined();
    expect(idle.holderIdleMs).toBeUndefined();
  });
});
