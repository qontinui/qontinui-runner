/**
 * `StructuredSessionCell` — the honesty contract of the structured-session cell
 * (a Conductor worker, or an operator's structured launch with its permission
 * card, plan 2026-09-20-ai-session-handling-is-claude-shaped Phase 9).
 *
 * The runner's vitest config is `environment: "node"` with no React Testing
 * Library, so (as `StreamingMessageView.test.tsx` does) the pure presentational
 * pieces are rendered with `renderToStaticMarkup` and the state logic is
 * exercised through its exported pure helpers. The container's wiring into
 * `ZoneGrid` is pinned at source level.
 */

import { describe, it, expect } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import {
  ConversationView,
  FileChangesPanel,
  SteeringLedger,
  WorkerStateBadge,
  describeWorkerState,
  armDirectSend,
  consumeDirectSendArm,
  deliveryForSendOutcome,
  deliveryLabel,
  IDLE_DIRECT_SEND_ARM,
  isWorkerEndState,
  reconcileDirectSendArm,
  settleQueuedOnTransition,
  shouldFetchChanges,
  PermissionCard,
  addPermissionRequest,
  formatToolInput,
  kindCopy,
  removePermissionRequest,
  resolvedLabel,
  respondArgs,
  PERMISSION_INPUT_MAX_CHARS,
  type PermissionRequest,
  type SteeringEntry,
} from "./StructuredSessionCell";
import { isOperatorOwnedStructuredTab, workerTabFromRecord } from "./useTerminalManager";
import type { FileChangesRead, SessionFileChangesResponse } from "./workerFileChanges";

describe("describeWorkerState", () => {
  it("is UNKNOWN until a read settles, and UNKNOWN when it failed — whatever the state value", () => {
    expect(describeWorkerState("ready", "pending")).toEqual({
      label: "attaching…",
      tone: "unknown",
    });
    expect(describeWorkerState("closed", "failed")).toEqual({ label: "UNKNOWN", tone: "unknown" });
    expect(describeWorkerState("processing", "failed").tone).toBe("unknown");
  });

  it("maps the SessionManager states once read", () => {
    expect(describeWorkerState("processing", "ok").tone).toBe("busy");
    expect(describeWorkerState("ready", "ok").tone).toBe("idle");
    expect(describeWorkerState("closed", "ok")).toEqual({ label: "finished", tone: "done" });
    expect(describeWorkerState("error", "ok").tone).toBe("error");
    expect(describeWorkerState("connecting", "ok").tone).toBe("starting");
  });
});

describe("settleQueuedOnTransition", () => {
  const queued: SteeringEntry = { id: "q", text: "do x", atMs: 1, delivery: "queued" };
  const sent: SteeringEntry = { id: "s", text: "do y", atMs: 2, delivery: "sent" };

  it("flips the OLDEST queued entry to delivered on the ready → processing edge", () => {
    const after = settleQueuedOnTransition([queued, sent], "ready", "processing");
    expect(after.map((e) => e.delivery)).toEqual(["delivered", "sent"]);
  });

  it("settles ONE queued message per edge, FIFO — never the whole queue", () => {
    // `send_next_pending_message` (claude_session/dispatcher.rs) does a single
    // `pop_front` per turn end, and MAX_PENDING_MESSAGES > 1. Flipping both A
    // and B on one edge told the operator B had landed while it was still
    // queued — or lost, if the worker finished first.
    const a: SteeringEntry = { id: "a", text: "first", atMs: 1, delivery: "queued" };
    const b: SteeringEntry = { id: "b", text: "second", atMs: 2, delivery: "queued" };

    const afterFirstTurn = settleQueuedOnTransition([a, b], "ready", "processing");
    expect(afterFirstTurn.map((e) => [e.id, e.delivery])).toEqual([
      ["a", "delivered"],
      ["b", "queued"],
    ]);

    const afterSecondTurn = settleQueuedOnTransition(afterFirstTurn, "ready", "processing");
    expect(afterSecondTurn.map((e) => [e.id, e.delivery])).toEqual([
      ["a", "delivered"],
      ["b", "delivered"],
    ]);

    // A third edge with nothing queued invents no delivery.
    expect(
      settleQueuedOnTransition(afterSecondTurn, "ready", "processing").map((e) => e.delivery),
    ).toEqual(["delivered", "delivered"]);
  });

  it("picks the oldest queued entry even with later rows around it", () => {
    // Order in the ledger IS send order, so the first `queued` row is the head
    // of the backend's own FIFO regardless of what sits around it.
    const entries: SteeringEntry[] = [
      { id: "old-sent", text: "0", atMs: 0, delivery: "sent" },
      { id: "q1", text: "1", atMs: 1, delivery: "queued" },
      { id: "failed", text: "2", atMs: 2, delivery: "failed", error: "nope" },
      { id: "q2", text: "3", atMs: 3, delivery: "queued" },
    ];
    expect(settleQueuedOnTransition(entries, "ready", "processing").map((e) => e.delivery)).toEqual(
      ["sent", "delivered", "failed", "queued"],
    );
  });

  it("leaves every entry alone on any other edge", () => {
    for (const [prev, next] of [
      ["processing", "ready"],
      ["processing", "processing"],
      ["ready", "ready"],
      ["connecting", "processing"],
      ["connecting", "ready"],
    ] as const) {
      expect(settleQueuedOnTransition([queued, sent], prev, next).map((e) => e.delivery)).toEqual([
        "queued",
        "sent",
      ]);
    }
  });

  it("settles every queued entry to UNDELIVERED when the worker ENDS", () => {
    // The fix. The backend pops the queue at a TURN END; a worker that
    // finishes or dies mid-turn goes `processing -> closed` and has no next
    // turn end, so anything still queued is lost. Leaving those rows at
    // `queued` kept the ledger promising "delivered when the current turn
    // ends" about a message that will never be delivered.
    const a: SteeringEntry = { id: "a", text: "first", atMs: 1, delivery: "queued" };
    const b: SteeringEntry = { id: "b", text: "second", atMs: 2, delivery: "queued" };
    const after = settleQueuedOnTransition([sent, a, b], "processing", "closed");
    expect(after.map((e) => [e.id, e.delivery])).toEqual([
      ["s", "sent"],
      ["a", "undelivered"],
      ["b", "undelivered"],
    ]);
  });

  it("treats error and not_found as ends too, and settles from any live state", () => {
    for (const end of ["closed", "error", "not_found"] as const) {
      for (const from of ["processing", "ready", "interrupting"] as const) {
        expect(settleQueuedOnTransition([queued], from, end).map((e) => e.delivery)).toEqual([
          "undelivered",
        ]);
      }
    }
  });

  it("does not re-settle when the worker was already ended", () => {
    // `closed -> not_found` is not a fresh end; nothing about it is new
    // evidence, and a `sent` row must never be rewritten by it.
    const settled: SteeringEntry = { id: "u", text: "x", atMs: 1, delivery: "undelivered" };
    expect(
      settleQueuedOnTransition([settled, sent], "closed", "not_found").map((e) => e.delivery),
    ).toEqual(["undelivered", "sent"]);
  });

  it("never invents a delivery for an end edge — only queued rows move", () => {
    const sending: SteeringEntry = { id: "x", text: "z", atMs: 3, delivery: "sending" };
    const failed: SteeringEntry = { id: "f", text: "w", atMs: 4, delivery: "failed", error: "no" };
    expect(
      settleQueuedOnTransition([sending, failed], "processing", "closed").map((e) => e.delivery),
    ).toEqual(["sending", "failed"]);
  });

  it("does NOT settle a queued entry on the edge an immediate send caused", () => {
    // Nit 4. M1 is queued while the worker is Processing; the worker reaches
    // Ready; the operator sends M2 at that instant so it goes straight out
    // (`queued: false`) and drives `ready -> processing` itself. The backend
    // popped nothing, so crediting M1 with a delivery would be a lie — M1 is
    // still sitting in the `VecDeque`.
    const m1: SteeringEntry = { id: "m1", text: "first", atMs: 1, delivery: "queued" };
    const m2: SteeringEntry = { id: "m2", text: "second", atMs: 2, delivery: "sent" };
    expect(
      settleQueuedOnTransition([m1, m2], "ready", "processing", true).map((e) => e.delivery),
    ).toEqual(["queued", "sent"]);
    // The very next real turn end still settles it.
    expect(
      settleQueuedOnTransition([m1, m2], "ready", "processing", false).map((e) => e.delivery),
    ).toEqual(["delivered", "sent"]);
  });

  it("suppression applies ONLY to the ready → processing edge", () => {
    // A direct send cannot make a worker's END mean anything else.
    expect(
      settleQueuedOnTransition([queued], "processing", "closed", true).map((e) => e.delivery),
    ).toEqual(["undelivered"]);
  });

  it("labels each delivery state distinctly, naming the failure", () => {
    expect(deliveryLabel(queued)).toContain("queued");
    expect(deliveryLabel(sent)).toBe("sent");
    expect(deliveryLabel({ ...sent, delivery: "delivered" })).toBe("delivered");
    expect(deliveryLabel({ ...sent, delivery: "failed", error: "queue full" })).toBe(
      "failed: queue full",
    );
    // The honesty that matters, in BOTH directions. The label must not read as
    // a delivery — and it must not read as a confident NON-delivery either:
    // the coalesced-`Ready` window means an entry can still be `queued` at the
    // end edge after the backend popped and re-sent it, so "not delivered"
    // would be a false claim that costs the operator a duplicated steering
    // message. It states the uncertainty and names the cause.
    const undelivered = deliveryLabel({ ...queued, delivery: "undelivered" });
    expect(undelivered).toContain("unconfirmed");
    expect(undelivered).toContain("worker ended");
    expect(undelivered).not.toContain("not delivered");
  });

  it("returns the SAME array when a transition changes nothing", () => {
    // The effect hands EVERY transition to this function rather than keeping a
    // second copy of the edge rules; identity is what lets React bail out of
    // the re-render, so it is part of the contract, not an optimisation.
    const entries: SteeringEntry[] = [queued, sent];
    expect(settleQueuedOnTransition(entries, "processing", "ready")).toBe(entries);
    expect(settleQueuedOnTransition(entries, "ready", "processing", true)).toBe(entries);
    const nothingQueued: SteeringEntry[] = [sent];
    expect(settleQueuedOnTransition(nothingQueued, "processing", "closed")).toBe(nothingQueued);
    expect(settleQueuedOnTransition(nothingQueued, "ready", "processing")).toBe(nothingQueued);
  });

  it("isWorkerEndState names exactly the states from which no queue can drain", () => {
    for (const s of ["closed", "error", "not_found"] as const) {
      expect(isWorkerEndState(s)).toBe(true);
    }
    for (const s of [
      "connecting",
      "initializing",
      "ready",
      "processing",
      "interrupting",
      "restoring",
      "disconnected",
    ] as const) {
      expect(isWorkerEndState(s)).toBe(false);
    }
  });
});

describe("deliveryForSendOutcome", () => {
  it("records a message queued into a worker that ALREADY ENDED as undelivered", () => {
    // The end-edge settle only moves rows that are already `queued`, and a row
    // is `sending` for the whole `send_user_message` invoke. So a worker that
    // dies while a send is in flight goes past the end edge with nothing to
    // settle, and writing `queued` afterwards strands a permanent "delivered
    // when the current turn ends" on a dead worker — in exactly the window
    // where a steering message is most likely to be lost.
    for (const end of ["closed", "error", "not_found"] as const) {
      expect(deliveryForSendOutcome({ ok: true, queued: true, state: null }, end)).toEqual({
        delivery: "undelivered",
      });
    }
  });

  it("records an ordinary queue as queued while the worker is alive", () => {
    for (const live of ["processing", "ready", "interrupting", "restoring"] as const) {
      expect(deliveryForSendOutcome({ ok: true, queued: true, state: null }, live)).toEqual({
        delivery: "queued",
      });
    }
  });

  it("an immediate send is `sent` even if the worker ended straight after", () => {
    // It really did go out; the worker ending afterwards does not unsend it.
    expect(deliveryForSendOutcome({ ok: true, queued: false, state: null }, "closed")).toEqual({
      delivery: "sent",
    });
  });

  it("carries a failure through with its reason", () => {
    expect(deliveryForSendOutcome({ ok: false, error: "queue full" }, "ready")).toEqual({
      delivery: "failed",
      error: "queue full",
    });
  });
});

describe("the direct-send arm", () => {
  it("arms only when the worker looked idle at issue time", () => {
    expect(armDirectSend("ready")).toEqual({ pending: true, spent: false });
    for (const s of ["processing", "closed", "interrupting", "connecting"] as const) {
      expect(armDirectSend(s)).toEqual({ pending: false, spent: false });
    }
  });

  it("suppresses the edge the direct send caused, and records that it did", () => {
    const { arm, causedByDirectSend } = consumeDirectSendArm(
      armDirectSend("ready"),
      "ready",
      "processing",
    );
    expect(causedByDirectSend).toBe(true);
    expect(arm).toEqual({ pending: false, spent: true });
  });

  it("is retired by ANY other observed transition, without suppressing it", () => {
    // A stale arm is the dangerous state: left pending, it would suppress a
    // genuine `pop_front` an arbitrary number of turns later and report a
    // delivered message as lost.
    for (const [prev, next] of [
      ["ready", "closed"],
      ["processing", "ready"],
      ["ready", "error"],
    ] as const) {
      const { arm, causedByDirectSend } = consumeDirectSendArm(armDirectSend("ready"), prev, next);
      expect(causedByDirectSend).toBe(false);
      expect(arm.pending).toBe(false);
      expect(arm.spent).toBe(false);
    }
  });

  it("leaves an unarmed arm alone, and never suppresses on one", () => {
    const { arm, causedByDirectSend } = consumeDirectSendArm(
      IDLE_DIRECT_SEND_ARM,
      "ready",
      "processing",
    );
    expect(causedByDirectSend).toBe(false);
    expect(arm).toBe(IDLE_DIRECT_SEND_ARM);
  });

  it("puts the settle BACK when the outcome proves the edge was a real pop", () => {
    // The interleaving: M1 is queued; the worker reaches a turn end; the
    // operator sends M2 at that instant so the client arms; the backend pops M1
    // and re-sends it, driving `ready -> processing` while the M2 invoke is
    // still outstanding, so the arm suppresses M1's settle; M2's outcome then
    // comes back `queued: true` — the client's `ready` was wrong and the edge
    // it swallowed was M1's delivery.
    const consumed = consumeDirectSendArm(armDirectSend("ready"), "ready", "processing");
    expect(consumed.causedByDirectSend).toBe(true);
    const reconciled = reconcileDirectSendArm(consumed.arm, /* wentOutImmediately */ false);
    expect(reconciled.resettle).toBe(true);
    expect(reconciled.arm).toEqual({ pending: false, spent: false });
  });

  it("does NOT re-settle when the send really did go out immediately", () => {
    const consumed = consumeDirectSendArm(armDirectSend("ready"), "ready", "processing");
    expect(reconcileDirectSendArm(consumed.arm, true)).toEqual({
      arm: { pending: false, spent: false },
      resettle: false,
    });
  });

  it("KEEPS a pending, unconsumed arm when the send went out immediately", () => {
    // The ordinary immediate-send path, and the one the arm exists for. This
    // reconcile runs on the awaited continuation (a microtask); the effect that
    // calls `consumeDirectSendArm` runs after a commit. The edge's only route
    // to the UI is the same round-trip that resolves the send, so the reconcile
    // lands FIRST and the `ready -> processing` edge has not been observed yet.
    // Disarming here would hand that edge to `settleQueuedOnTransition` with
    // `causedByDirectSend === false` and credit a still-queued predecessor with
    // a delivery that never happened.
    const armed = armDirectSend("ready");
    const reconciled = reconcileDirectSendArm(armed, true);
    expect(reconciled.resettle).toBe(false);
    expect(reconciled.arm).toEqual({ pending: true, spent: false });

    // Still-armed, so the edge it was waiting for IS suppressed when it finally
    // arrives — and is retired by that same observation, so nothing goes stale.
    const consumed = consumeDirectSendArm(reconciled.arm, "ready", "processing");
    expect(consumed.causedByDirectSend).toBe(true);
    expect(consumed.arm.pending).toBe(false);
  });

  it("an unarmed arm survives the reconcile as an unarmed arm", () => {
    // A send issued while the worker was busy never armed, so an immediate
    // outcome must not conjure a suppression for the next edge.
    const reconciled = reconcileDirectSendArm(armDirectSend("processing"), true);
    expect(reconciled).toEqual({ arm: { pending: false, spent: false }, resettle: false });
  });

  it("does NOT re-settle a suppression that was never applied", () => {
    // Queued outcome, but no edge was suppressed — nothing to put back.
    expect(reconcileDirectSendArm(armDirectSend("ready"), false).resettle).toBe(false);
    expect(reconcileDirectSendArm(IDLE_DIRECT_SEND_ARM, false).resettle).toBe(false);
  });

  it("the re-settle lands on the OLDER queued row, not on the in-flight one", () => {
    // The row for the send being reconciled is still `sending`, so
    // `settleQueuedOnTransition` cannot see it — which is why the repair is
    // issued before that row is marked.
    const m1: SteeringEntry = { id: "m1", text: "first", atMs: 1, delivery: "queued" };
    const m2: SteeringEntry = { id: "m2", text: "second", atMs: 2, delivery: "sending" };
    expect(
      settleQueuedOnTransition([m1, m2], "ready", "processing").map((e) => [e.id, e.delivery]),
    ).toEqual([
      ["m1", "delivered"],
      ["m2", "sending"],
    ]);
  });
});

describe("WorkerStateBadge", () => {
  it("renders UNKNOWN (with the error as title) when the read failed", () => {
    const html = renderToStaticMarkup(
      <WorkerStateBadge
        state="closed"
        readStatus="failed"
        lastReadError="state read failed: boom"
      />,
    );
    expect(html).toContain("UNKNOWN");
    expect(html).toContain('data-worker-state="unknown"');
    expect(html).toContain("state read failed: boom");
    expect(html).not.toContain("finished");
  });

  it("renders the live state once read", () => {
    const html = renderToStaticMarkup(
      <WorkerStateBadge state="processing" readStatus="ok" lastReadError={null} />,
    );
    expect(html).toContain("working");
    expect(html).toContain('data-worker-state="processing"');
  });
});

describe("SteeringLedger", () => {
  it("shows queued and sent entries with their delivery state", () => {
    const html = renderToStaticMarkup(
      <SteeringLedger
        entries={[
          { id: "1", text: "first", atMs: 0, delivery: "sent" },
          { id: "2", text: "second", atMs: 0, delivery: "queued" },
        ]}
      />,
    );
    expect(html).toContain('data-delivery="sent"');
    expect(html).toContain('data-delivery="queued"');
    expect(html).toContain("delivered when the current turn ends");
  });

  it("renders nothing with no entries", () => {
    expect(renderToStaticMarkup(<SteeringLedger entries={[]} />)).toBe("");
  });
});

const okResponse = (
  files: SessionFileChangesResponse["files"],
  extra: Partial<SessionFileChangesResponse> = {},
): SessionFileChangesResponse => ({
  sessionId: "w1",
  files,
  filesTruncated: false,
  omittedFiles: 0,
  baseKind: null,
  baseSha: null,
  readAtMs: 0,
  ...extra,
});

describe("shouldFetchChanges", () => {
  const base = { taskRunId: "w1", fetchedFor: null as string | null, stale: false };

  it("reads nothing while the cell is not visible", () => {
    expect(shouldFetchChanges({ ...base, visible: false })).toBe(false);
    expect(shouldFetchChanges({ ...base, visible: false, stale: true })).toBe(false);
    expect(shouldFetchChanges({ ...base, visible: false, fetchedFor: "w1", stale: true })).toBe(
      false,
    );
  });

  it("reads on FIRST becoming visible, and not again while nothing is owed", () => {
    expect(shouldFetchChanges({ ...base, visible: true })).toBe(true);
    expect(shouldFetchChanges({ ...base, visible: true, fetchedFor: "w1" })).toBe(false);
  });

  it("reads again when a refresh fell due while hidden, or the worker changed", () => {
    expect(shouldFetchChanges({ ...base, visible: true, fetchedFor: "w1", stale: true })).toBe(
      true,
    );
    expect(shouldFetchChanges({ ...base, visible: true, fetchedFor: "other" })).toBe(true);
  });
});

describe("FileChangesPanel", () => {
  const noop = () => {};

  it("renders a failed read as UNKNOWN with the error, never as an empty list", () => {
    const read: FileChangesRead = { status: "error", error: "HTTP 500", atMs: 0, previous: null };
    const html = renderToStaticMarkup(<FileChangesPanel read={read} onRefresh={noop} />);
    expect(html).toContain('data-file-changes-status="error"');
    expect(html).toContain("UNKNOWN");
    expect(html).toContain("HTTP 500");
    expect(html).not.toContain("No snapshotted edits");
  });

  it("labels a stale previous read as such while the fresh one failed", () => {
    const previous = okResponse([
      {
        filePath: "/repo/a.ts",
        status: "modified",
        before: "a\n",
        after: "b\n",
        beforeBytes: 2,
        afterBytes: 2,
        beforeSha256: "x",
        afterSha256: "y",
        truncated: false,
        takenAt: null,
        detail: null,
        beforeSource: "snapshot",
        baseKind: null,
        baseSha: null,
      },
    ]);
    const read: FileChangesRead = { status: "error", error: "boom", atMs: 0, previous };
    const html = renderToStaticMarkup(<FileChangesPanel read={read} onRefresh={noop} />);
    expect(html).toContain("may be stale");
    expect(html).toContain("a.ts");
    expect(html).toContain("UNKNOWN");
  });

  it("says when the backend cut the list, rather than presenting it as whole", () => {
    const read: FileChangesRead = {
      status: "ok",
      response: okResponse(
        [
          {
            filePath: "/repo/a.ts",
            status: "modified",
            before: "a\n",
            after: "b\n",
            beforeBytes: 2,
            afterBytes: 2,
            beforeSha256: "x",
            afterSha256: "y",
            truncated: false,
            takenAt: null,
            detail: null,
            beforeSource: "snapshot",
            baseKind: null,
            baseSha: null,
          },
        ],
        { filesTruncated: true, omittedFiles: 12 },
      ),
    };
    const html = renderToStaticMarkup(<FileChangesPanel read={read} onRefresh={noop} />);
    expect(html).toContain('data-file-changes-cut="true"');
    expect(html).toContain("12 more path");
    expect(html).toContain("NOT shown");
  });

  it("says a successful empty read is genuinely empty", () => {
    const read: FileChangesRead = { status: "ok", response: okResponse([]) };
    const html = renderToStaticMarkup(<FileChangesPanel read={read} onRefresh={noop} />);
    expect(html).toContain("No snapshotted edits");
    expect(html).not.toContain("UNKNOWN");
  });

  it("shows an unreadable file as UNKNOWN with the backend detail, beside real changes", () => {
    const read: FileChangesRead = {
      status: "ok",
      response: okResponse([
        {
          filePath: "/repo/b.ts",
          status: "unreadable",
          before: null,
          after: null,
          beforeBytes: null,
          afterBytes: null,
          beforeSha256: null,
          afterSha256: null,
          truncated: false,
          takenAt: null,
          detail: "pre-edit snapshot blob is missing on disk",
          beforeSource: "snapshot",
          baseKind: null,
          baseSha: null,
        },
        {
          filePath: "/repo/c.ts",
          status: "created",
          before: null,
          after: "new\n",
          beforeBytes: null,
          afterBytes: 4,
          beforeSha256: null,
          afterSha256: "z",
          truncated: false,
          takenAt: null,
          detail: null,
          beforeSource: "git_base",
          baseKind: "head",
          baseSha: "abc",
        },
      ]),
    };
    const html = renderToStaticMarkup(<FileChangesPanel read={read} onRefresh={noop} />);
    expect(html).toContain("blob is missing on disk");
    expect(html).toContain('data-file-change="unreadable"');
    expect(html).toContain('data-file-change="created"');
    expect(html).toContain("+1");
    expect(html).toContain("2 changed");
  });

  it("never shows a path with no established before side as a creation", () => {
    // A PTY-hosted session's path with no pre-edit snapshot and no git base:
    // diffing it from empty would claim the whole file is new.
    const read: FileChangesRead = {
      status: "ok",
      response: okResponse([
        {
          filePath: "/repo/d.ts",
          status: "created",
          before: null,
          after: "existing\n",
          beforeBytes: null,
          afterBytes: 9,
          beforeSha256: null,
          afterSha256: "w",
          truncated: false,
          takenAt: null,
          detail: null,
          beforeSource: "none",
          baseKind: null,
          baseSha: null,
        },
      ]),
    };
    // Called as a function (it holds no hooks) so the render enters through
    // the panel itself, exactly as the worker cell and review panel mount it.
    const html = renderToStaticMarkup(FileChangesPanel({ read, onRefresh: noop }));
    expect(html).toContain("d.ts");
    expect(html).toContain("UNKNOWN — no pre-edit snapshot and no git base to diff against");
    expect(html).not.toContain("+1");
  });

  it("names the git base the changes are diffed against in the panel header", () => {
    const read: FileChangesRead = {
      status: "ok",
      response: okResponse([], { baseKind: "head", baseSha: "abc1234567890" }),
    };
    // Called as a function (it holds no hooks) so the render enters through
    // the panel itself, exactly as the worker cell and review panel mount it.
    const html = renderToStaticMarkup(FileChangesPanel({ read, onRefresh: noop }));
    expect(html).toContain('data-review-base="head"');
    expect(html).toContain("vs HEAD abc123456 — committed work not shown");
  });
});

describe("ConversationView", () => {
  const base = {
    streamingContent: "",
    streamingDroppedChars: 0,
    isProcessing: false,
    toolActivity: null,
  };

  it("renders a failed read as UNKNOWN, not as an empty transcript", () => {
    const html = renderToStaticMarkup(
      <ConversationView
        {...base}
        messages={[]}
        readStatus="failed"
        lastReadError="history read failed: pg down"
      />,
    );
    expect(html).toContain("UNKNOWN");
    expect(html).toContain("pg down");
    expect(html).not.toContain("No transcript yet");
  });

  it("only calls an empty transcript empty after a successful read", () => {
    const pending = renderToStaticMarkup(
      <ConversationView {...base} messages={[]} readStatus="pending" lastReadError={null} />,
    );
    expect(pending).toContain("attaching");
    expect(pending).not.toContain("No transcript yet");
    const ok = renderToStaticMarkup(
      <ConversationView {...base} messages={[]} readStatus="ok" lastReadError={null} />,
    );
    expect(ok).toContain("No transcript yet");
  });

  it("renders the in-flight tail through StreamingMessageView while processing", () => {
    const html = renderToStaticMarkup(
      <ConversationView
        {...base}
        messages={[{ role: "user", content: "hello" }]}
        streamingContent="working on it"
        isProcessing
        readStatus="ok"
        lastReadError={null}
      />,
    );
    expect(html).toContain("hello");
    expect(html).toContain("working on it");
    expect(html).toContain("animate-pulse"); // the caret
  });
});

describe("ZoneGrid wiring", () => {
  const source = readFileSync(resolve(__dirname, "./ZoneGrid.tsx"), "utf8");

  it("mounts StructuredSessionCell at both TerminalInstance sites (maximized + zoned)", () => {
    expect(source.match(/<StructuredSessionCell\s/g)?.length).toBe(2);
  });

  it("never gives a worker tab the hidden TerminalInstance mount", () => {
    expect(source).toContain("!t.sessionBacked");
  });

  it("passes the zone's visibility through to every StructuredSessionCell mount", () => {
    // The cell's file-changes read is gated on `visible`; a mount that hard-
    // coded `visible` would defeat that silently.
    expect(source.match(/<StructuredSessionCell[^>]*visible=\{/g)?.length).toBe(2);
  });
});

describe("StructuredSessionCell wiring", () => {
  const source = readFileSync(resolve(__dirname, "./StructuredSessionCell.tsx"), "utf8");

  it("hands the cell's visibility to the file-changes hook", () => {
    // Pins the fix: `visible` must reach `useSessionReview` (formerly `useWorkerFileChanges`), not just the
    // `data-visible` attribute it used to be spent on.
    expect(source).toMatch(
      /useSessionReview\(taskRunId,\s*\{\s*visible,\s*sessionState:\s*session\.sessionState\s*\}\)/,
    );
  });
});

describe("permission card (Phase 9)", () => {
  // The request Claude Code 2.1.285 sent in the Phase 2 probe
  // (`can_use_tool_sdk_allow_with_session_state_events`), as the runner
  // re-emits it on `session-permission-request`.
  const request: PermissionRequest = {
    sessionId: "trid-1",
    requestId: "00000000-0000-4000-8000-000000000011",
    toolName: "Write",
    displayName: "Write",
    description: "probe.txt",
    toolUseId: "toolu_redacted_01",
    input: { file_path: "/redacted/cwd/probe.txt", content: "hello" },
    suggestions: [{ type: "setMode", mode: "acceptEdits", destination: "session" }],
    requestedAt: "2026-10-03T10:00:00+00:00",
    expiresAt: "2026-10-03T10:10:00+00:00",
    timeoutSecs: 600,
  };

  it("renders the tool, its arguments and the three answers", () => {
    const html = renderToStaticMarkup(
      <PermissionCard request={request} onAnswer={() => {}} busy={false} error={null} />,
    );
    expect(html).toContain("Permission requested: Write");
    expect(html).toContain("probe.txt");
    expect(html).toContain("&quot;file_path&quot;: &quot;/redacted/cwd/probe.txt&quot;");
    expect(html).toContain('data-permission-answer="allow"');
    expect(html).toContain('data-permission-answer="deny"');
    expect(html).toContain('data-permission-answer="deny_interrupt"');
    expect(html).toContain("Deny &amp; interrupt");
    expect(html).toContain("denied automatically at");
    expect(html).not.toContain('disabled=""');
  });

  it("disables the answers while one is in flight and shows a failed answer", () => {
    const html = renderToStaticMarkup(
      <PermissionCard request={request} onAnswer={() => {}} busy error="no live session trid-1" />,
    );
    expect(html.match(/disabled=""/g)?.length).toBe(3);
    expect(html).toContain("The answer was not delivered: no live session trid-1");
  });

  it("bounds the arguments it renders and says how much it held back", () => {
    const big = { content: "x".repeat(PERMISSION_INPUT_MAX_CHARS * 2) };
    const { text, truncatedChars } = formatToolInput(big);
    expect(text.length).toBe(PERMISSION_INPUT_MAX_CHARS);
    expect(truncatedChars).toBeGreaterThan(0);
    const html = renderToStaticMarkup(
      <PermissionCard request={{ ...request, input: big }} onAnswer={() => {}} busy={false} error={null} />,
    );
    expect(html).toContain(`${truncatedChars} more characters not shown`);
    expect(formatToolInput({ a: 1 })).toEqual({ text: '{\n  "a": 1\n}', truncatedChars: 0 });
  });

  it("maps each answer onto respond_session_permission's arguments", () => {
    expect(respondArgs("s", "r", "allow")).toEqual({
      sessionId: "s",
      requestId: "r",
      decision: "allow",
      interrupt: false,
    });
    expect(respondArgs("s", "r", "deny")).toMatchObject({ decision: "deny", interrupt: false });
    expect(respondArgs("s", "r", "deny_interrupt")).toMatchObject({
      decision: "deny",
      interrupt: true,
    });
  });

  it("keeps one card per request, oldest first, and drops it when resolved", () => {
    const later = { ...request, requestId: "r2", requestedAt: "2026-10-03T10:05:00+00:00" };
    let list = addPermissionRequest([], later);
    list = addPermissionRequest(list, request);
    expect(list.map((r) => r.requestId)).toEqual([request.requestId, "r2"]);
    expect(addPermissionRequest(list, request)).toBe(list); // the mount read + the event
    const after = removePermissionRequest(list, request.requestId);
    expect(after.map((r) => r.requestId)).toEqual(["r2"]);
    expect(removePermissionRequest(after, "unknown")).toBe(after);
  });

  it("says a timeout was a fail-closed deny, never a silent disappearance", () => {
    const base = { sessionId: "s", requestId: "r", toolName: "Bash", interrupt: false };
    expect(resolvedLabel({ ...base, outcome: "timed_out" })).toContain("denied Bash (fail closed)");
    expect(resolvedLabel({ ...base, outcome: "denied", interrupt: true })).toContain("interrupted");
    expect(resolvedLabel({ ...base, outcome: "allowed" })).toBe("Allowed Bash.");
    expect(resolvedLabel({ ...base, outcome: "session_ended" })).toContain("ended");
  });

  it("labels the cell by what it hosts", () => {
    expect(kindCopy("worker").label).toBe("Worker");
    expect(kindCopy("structured").label).toBe("Structured");
    expect(kindCopy("structured").steerNow).not.toContain("worker");
  });

  it("marks a structured launch's record as prompting, and a worker's as not", () => {
    const rec = {
      claudeSessionId: "t",
      terminalId: "t",
      taskRunId: "t",
      title: "Claude Code (structured)",
      openedAt: 1,
    };
    expect(
      workerTabFromRecord({ ...rec, lane: "structured", bypassPermissions: false })
        ?.promptsForPermission,
    ).toBe(true);
    // A Conductor worker's record carries no bypass flag: never "asks".
    expect(workerTabFromRecord({ ...rec, lane: "structured" })?.promptsForPermission).toBeUndefined();
  });

  it("closing a structured launch's tab ends it; closing a worker's only hides it", () => {
    const rec = { claudeSessionId: "t", terminalId: "t", taskRunId: "t", openedAt: 1 };
    const launched = workerTabFromRecord({ ...rec, lane: "structured", bypassPermissions: false });
    const worker = workerTabFromRecord({ ...rec, lane: "structured" });
    expect(isOperatorOwnedStructuredTab(launched ?? undefined)).toBe(true);
    expect(isOperatorOwnedStructuredTab(worker ?? undefined)).toBe(false);
    expect(isOperatorOwnedStructuredTab(undefined)).toBe(false);
  });

  it("mounts the card and the permission channels in the cell", () => {
    const source = readFileSync(resolve(__dirname, "./StructuredSessionCell.tsx"), "utf8");
    expect(source).toContain('"session-permission-request"');
    expect(source).toContain('"session-permission-resolved"');
    expect(source).toContain('"session_pending_permissions"');
    expect(source).toContain('"respond_session_permission"');
    expect(source).toMatch(/<PermissionCard\s/);
  });
});
