import { describe, expect, it } from "vitest";

import {
  FANOUT_LEDGER_LOAD_FAILED,
  FANOUT_LEDGER_NOT_LOADED,
  isFanoutRunList,
  parseFanoutEnvelope,
  type FanoutMemberView,
  type FanoutRunView,
} from "./fanoutApi";
import {
  activeFanoutRuns,
  canReleaseMember,
  cancellableCount,
  cancelledNote,
  capClampNote,
  createFreshnessGate,
  fanoutRunSummary,
  fanoutStripVisible,
  memberNumber,
  memberStateLabel,
  mergeRunUpdate,
  nextCap,
  readStateFromResult,
  reasonLabel,
  retryLabel,
  runAgeLabel,
  runName,
  summarizeReasons,
  unknownStripText,
} from "./fanoutStripModel";

function member(index: number, state: FanoutMemberView["state"], reason: string | null = null) {
  return {
    index,
    title: `m${index}`,
    prompt: "p",
    state,
    terminalId: state === "admitted" ? `term-${index}` : null,
    claudeSessionId: null,
    reason,
    admittedAt: null,
    releasedAt: null,
  } satisfies FanoutMemberView;
}

function run(members: FanoutMemberView[], overrides: Partial<FanoutRunView> = {}): FanoutRunView {
  const counts = { queued: 0, admitted: 0, released: 0, cancelled: 0, refused: 0 };
  for (const m of members) counts[m.state] += 1;
  return {
    id: "abcdef12-3456-7890-abcd-ef1234567890",
    tenantId: null,
    templateSlug: "port-feature",
    templateVersion: 1,
    maxConcurrent: 2,
    configDirPolicy: { kind: "bestHeadroom" },
    workingDir: "/repo",
    createdAt: "2026-10-03T10:00:00Z",
    state: "active",
    counts,
    members,
    ...overrides,
  };
}

describe("read state", () => {
  it("a 503 FANOUT_LEDGER_NOT_LOADED (the boot settle) is a quiet state that never forces the strip", () => {
    const result = parseFanoutEnvelope(
      503,
      {
        success: false,
        error: "the fan-out ledger has not been loaded yet — its runs are UNKNOWN until then",
        code: FANOUT_LEDGER_NOT_LOADED,
      },
      isFanoutRunList,
      "GET /fanout",
    );
    const s = readStateFromResult(result);
    expect(s).toEqual({
      kind: "settling",
      reason: "the fan-out ledger has not been loaded yet — its runs are UNKNOWN until then",
    });
    expect(fanoutStripVisible(s)).toBe(false);
  });

  it("a 503 FANOUT_LEDGER_LOAD_FAILED (PG unreadable) is UNKNOWN, shown in the strip", () => {
    const result = parseFanoutEnvelope(
      503,
      {
        success: false,
        error: "the fan-out ledger could not be loaded from PostgreSQL (connection refused)",
        code: FANOUT_LEDGER_LOAD_FAILED,
      },
      isFanoutRunList,
      "GET /fanout",
    );
    const s = readStateFromResult(result);
    expect(s).toEqual({
      kind: "unknown",
      status: 503,
      error: "the fan-out ledger could not be loaded from PostgreSQL (connection refused)",
      code: FANOUT_LEDGER_LOAD_FAILED,
    });
    expect(fanoutStripVisible(s)).toBe(true);
    if (s.kind !== "unknown") throw new Error("unreachable");
    expect(unknownStripText(s.error)).toMatch(/^fan-out UNKNOWN — the fan-out ledger could not/);
  });

  it("a failed read is UNKNOWN with the error, never an empty list", () => {
    const s = readStateFromResult({ ok: false, status: 503, error: "dispatcher not running" });
    expect(s).toEqual({ kind: "unknown", status: 503, error: "dispatcher not running" });
    expect(fanoutStripVisible(s)).toBe(true);
  });

  it("renders nothing while loading or with no ACTIVE runs", () => {
    expect(fanoutStripVisible({ kind: "loading" })).toBe(false);
    expect(fanoutStripVisible({ kind: "ok", runs: [] })).toBe(false);
    expect(
      fanoutStripVisible({
        kind: "ok",
        runs: [run([member(0, "released")], { state: "completed" })],
      }),
    ).toBe(false);
    expect(fanoutStripVisible({ kind: "ok", runs: [run([member(0, "queued")])] })).toBe(true);
  });

  it("activeFanoutRuns drops completed runs", () => {
    const a = run([member(0, "queued")], { id: "a" });
    const c = run([], { id: "c", state: "completed" });
    expect(activeFanoutRuns([a, c]).map((r) => r.id)).toEqual(["a"]);
  });
});

describe("unknownStripText", () => {
  it("says UNKNOWN and keeps the error to one strip width", () => {
    expect(unknownStripText("GET /fanout: HTTP 503")).toBe(
      "fan-out UNKNOWN — GET /fanout: HTTP 503",
    );
    const long = unknownStripText(
      "the fan-out dispatcher is not running on this runner ".repeat(3),
    );
    expect(long.startsWith("fan-out UNKNOWN — ")).toBe(true);
    expect(long.endsWith("…")).toBe(true);
  });
});

describe("mergeRunUpdate", () => {
  it("replaces a known run by id and prepends an unseen one", () => {
    const a = run([member(0, "queued")], { id: "a" });
    const b = run([member(0, "queued")], { id: "b" });
    const a2 = run([member(0, "admitted")], { id: "a" });
    const s1 = mergeRunUpdate({ kind: "ok", runs: [a, b] }, a2);
    expect(s1.kind === "ok" && s1.runs.map((r) => r.counts.admitted)).toEqual([1, 0]);
    const c = run([], { id: "c" });
    const s2 = mergeRunUpdate(s1, c);
    expect(s2.kind === "ok" && s2.runs.map((r) => r.id)).toEqual(["c", "a", "b"]);
  });

  it("one run's update does not make an UNKNOWN or loading scheduler known", () => {
    const u = { kind: "unknown", error: "x", status: null } as const;
    expect(mergeRunUpdate(u, run([]))).toBe(u);
    const l = { kind: "loading" } as const;
    expect(mergeRunUpdate(l, run([]))).toBe(l);
  });
});

describe("labels", () => {
  it("summarizes a run: running and queued always, refused with its reason", () => {
    const r = run([
      member(0, "admitted"),
      member(1, "admitted"),
      member(2, "queued"),
      member(3, "queued"),
      member(4, "refused", "low memory: 300 MB free below the 512 MB floor"),
    ]);
    expect(fanoutRunSummary(r)).toBe(
      "run port-feature — 2 running · 2 queued · 1 refused (low memory: 300 MB free below the 512 MB floor)",
    );
  });

  it("names a shared queued reason in operator words and omits zero refused", () => {
    const r = run([member(0, "queued", "runner_draining"), member(1, "queued", "runner_draining")]);
    expect(fanoutRunSummary(r)).toBe("run port-feature — 0 running · 2 queued (runner draining)");
  });

  it("collapses several distinct reasons to the first plus a count", () => {
    const ms = [
      member(0, "refused", "a"),
      member(1, "refused", "b"),
      member(2, "refused", "a"),
      member(3, "refused", "c"),
    ];
    expect(summarizeReasons(ms, "refused")).toBe("a +2 more");
    expect(summarizeReasons(ms, "queued")).toBeNull();
  });

  it("maps stable wire reasons and truncates free text", () => {
    expect(reasonLabel("fanout_bound_occupied")).toBe("fan-out bound full");
    expect(reasonLabel(null)).toBeNull();
    const long = reasonLabel("x".repeat(100));
    expect(long?.length).toBe(60);
    expect(long?.endsWith("…")).toBe(true);
  });

  it("falls back to a short id when the run has no template slug", () => {
    expect(runName(run([], { templateSlug: null }))).toBe("abcdef12");
  });

  it("labels a member with its reason", () => {
    expect(memberStateLabel(member(0, "released", "terminal_exit"))).toBe(
      "released (terminal exited)",
    );
    expect(memberStateLabel(member(1, "queued"))).toBe("queued");
  });
});

describe("controls", () => {
  it("Release-slot is offered only for admitted members", () => {
    expect(canReleaseMember(member(0, "admitted"))).toBe(true);
    for (const s of ["queued", "released", "cancelled", "refused"] as const) {
      expect(canReleaseMember(member(0, s))).toBe(false);
    }
  });

  it("Cancel-queued counts what the route cancels: queued AND refused", () => {
    expect(
      cancellableCount(run([member(0, "queued"), member(1, "refused"), member(2, "admitted")])),
    ).toBe(2);
  });

  it("counts cancelled members from the server's answer, not the click-time count", () => {
    const before = run([member(0, "queued"), member(1, "queued")]);
    // Member 0 was admitted between the click and the answer; only 1 cancelled.
    const after = run([member(0, "admitted"), member(1, "cancelled", "cancelled")]);
    expect(cancelledNote(before, after)).toBe("cancelled 1 waiting member");
  });

  it("the cap never steps below 1", () => {
    expect(nextCap(1, -1)).toBe(1);
    expect(nextCap(3, -1)).toBe(2);
    expect(nextCap(3, 1)).toBe(4);
  });

  it("notes the server's clamp after a PATCH, and nothing when there was none", () => {
    expect(capClampNote(20, { run: { maxConcurrent: 15 }, fanoutBound: 15, clampedFrom: 20 })).toBe(
      "asked for 20, clamped to 15 (fan-out bound 15)",
    );
    expect(
      capClampNote(4, { run: { maxConcurrent: 4 }, fanoutBound: 15, clampedFrom: null }),
    ).toBeNull();
  });
});

describe("memberNumber", () => {
  it("shows the preview's row number, not the renumbered posted position", () => {
    // Preview rows #1, #3, #6 were ticked: the server holds them at 0, 1, 2.
    expect(memberNumber({ index: 1, previewIndex: 2 })).toBe(3);
    expect(memberNumber({ index: 2, previewIndex: 5 })).toBe(6);
  });

  it("falls back to the posted position when no preview index was echoed", () => {
    expect(memberNumber({ index: 1, previewIndex: null })).toBe(2);
    expect(memberNumber({ index: 4 })).toBe(5);
  });
});

describe("freshness gate", () => {
  it("drops a poll that started before an event the state already shows", () => {
    const g = createFreshnessGate();
    const slow = g.begin();
    g.markApplied(); // an event lands while the poll is in flight
    expect(g.acceptPoll(slow)).toBe(false);
    // A poll started after it is applied.
    const fresh = g.begin();
    expect(g.acceptPoll(fresh)).toBe(true);
  });

  it("drops an older poll that answers after a newer one", () => {
    const g = createFreshnessGate();
    const a = g.begin();
    const b = g.begin();
    expect(g.acceptPoll(b)).toBe(true);
    expect(g.acceptPoll(a)).toBe(false);
  });

  it("applies polls in order when nothing else happened", () => {
    const g = createFreshnessGate();
    expect(g.acceptPoll(g.begin())).toBe(true);
    expect(g.acceptPoll(g.begin())).toBe(true);
  });
});

describe("backoff and age labels", () => {
  const now = Date.parse("2026-10-03T12:00:00Z");

  it("says when a refused member returns to the queue", () => {
    const m = {
      ...member(0, "refused", "resource_guard:critical: low memory"),
      refusals: 3,
      nextRetryAt: "2026-10-03T12:00:40Z",
    };
    expect(retryLabel(m, now)).toBe("retry in 40s (refusal 3)");
    expect(memberStateLabel(m, now)).toBe(
      "refused (resource_guard:critical: low memory · retry in 40s (refusal 3))",
    );
    expect(retryLabel({ ...m, nextRetryAt: "2026-10-03T11:59:00Z" }, now)).toBe(
      "retrying (refusal 3)",
    );
    expect(retryLabel({ ...m, refusals: 1 }, now)).toBe("retry in 40s");
    expect(retryLabel({ ...m, state: "queued" }, now)).toBeNull();
  });

  it("names the new release reasons", () => {
    expect(reasonLabel("spawn_unconfirmed")).toBe("spawn unconfirmed (runner restarted)");
    expect(reasonLabel("stale_after_restart")).toBe("stale after restart");
  });

  it("shows how old a run is", () => {
    expect(runAgeLabel({ createdAt: "2026-10-03T11:59:15Z" }, now)).toBe("started 45s ago");
    expect(runAgeLabel({ createdAt: "2026-10-03T09:00:00Z" }, now)).toBe("started 3h ago");
    expect(runAgeLabel({ createdAt: "2026-09-29T12:00:00Z" }, now)).toBe("started 4d ago");
    expect(runAgeLabel({ createdAt: "not a date" }, now)).toBeNull();
  });
});
