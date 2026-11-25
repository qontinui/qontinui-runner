/**
 * The "Since restart" roster model (plan
 * `2026-10-04-runner-session-roster-restore-picker`, Phases 4-5): pre-check
 * selection, the sequential resume queue, the strip's copy, Finish/Unfinish
 * row state, and the pre-rebuild preview. Pure — the runner's vitest env is
 * `node` — plus a source read pinning the stable UI-Bridge ids.
 */

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { describe, expect, it, vi } from "vitest";

import type {
  LedgerEntry,
  LedgerGeneration,
  LedgerOutcome,
  LedgerReport,
} from "@/lib/session-ledger";
import {
  EMPTY_SELECTION,
  buildRow,
  buildRows,
  defaultGenerationFile,
  describeProgress,
  finishButtonModel,
  pageRestoreSettled,
  pastSessionFinishId,
  preCheckedIds,
  previewModel,
  reconcileSelection,
  APP_NAVIGATE_EVENT,
  requestTerminalView,
  resumeTargetFor,
  ResumeQueue,
  RESUME_CANCELLED,
  runResumeQueue,
  selectedResumable,
  sinceRestartCheckId,
  sinceRestartRowId,
  stripModel,
  thisBootGeneration,
  type ResumeProgress,
  type RowContext,
} from "./sinceRestart";

function outcome(id: string, over: Partial<LedgerOutcome> = {}): LedgerOutcome {
  return {
    claudeSessionId: id,
    terminalId: `term-${id}`,
    pageId: "p1",
    zoneIndex: 0,
    displayName: `name-${id}`,
    sessionName: `name-${id}`,
    nameSource: null,
    title: "claude",
    accountLabel: "gmail",
    configDir: "C:/claude/.claude-gmail",
    configDirKnown: true,
    provider: "claude",
    workingDir: "D:/repo/sub",
    worktreePath: "D:/repo",
    planSlug: null,
    workUnitId: null,
    wipState: null,
    wipRef: null,
    custodySessionMismatch: false,
    lastSeenAt: 1_000,
    restorable: true,
    finished: false,
    outcome: "missing",
    reason: "no-attempt",
    resumeDir: "D:/repo/sub",
    resumeCommand: `cd "D:/repo/sub" && CLAUDE_CONFIG_DIR="C:/claude/.claude-gmail" claude --resume ${id}`,
    resumeAccount: { known: true, configDir: "C:/claude/.claude-gmail" },
    ...over,
  };
}

const back = (id: string, over: Partial<LedgerOutcome> = {}) =>
  outcome(id, { outcome: "back", reason: null, ...over });
const finished = (id: string) => outcome(id, { outcome: "finished", reason: null, finished: true });
const closedByUser = (id: string) => outcome(id, { outcome: "closed-by-user", reason: null });

/**
 * The serde field name of a generation's size. Spelled through a constant
 * because this directory bans DECLARING a bare `sessionCount` (it names no
 * population — eslint `no-restricted-syntax`); here it is someone else's field.
 */
const SESSION_COUNT = "sessionCount" as const;

function generation(
  file: string,
  parts: {
    returned?: LedgerOutcome[];
    missing?: LedgerOutcome[];
    finished?: LedgerOutcome[];
    closedByUser?: LedgerOutcome[];
  },
  verdict: LedgerGeneration["verdict"] = "partial",
): LedgerGeneration {
  const returned = parts.returned ?? [];
  const missing = parts.missing ?? [];
  const fin = parts.finished ?? [];
  const closed = parts.closedByUser ?? [];
  return {
    file,
    rotatedAtMs: 5_000,
    bootAtMs: 1_000,
    thisBoot: false,
    capturedAtMs: 4_000,
    capturedAt: "x",
    reason: "poll",
    cleanShutdown: true,
    [SESSION_COUNT]: returned.length + missing.length + fin.length + closed.length,
    verdict,
    returned,
    missing,
    finished: fin,
    closedByUser: closed,
  };
}

function entry(id: string, over: Partial<LedgerEntry> = {}): LedgerEntry {
  return {
    claudeSessionId: id,
    terminalId: `term-${id}`,
    pageId: "p1",
    zoneIndex: 0,
    title: "claude",
    sessionName: `name-${id}`,
    nameSource: null,
    accountLabel: "gmail",
    configDir: null,
    resumeConfigDir: null,
    resumeCommand: null,
    provider: "claude",
    workingDir: "D:/repo",
    lastSeenAt: 1_000,
    finished: false,
    restorable: true,
    worktreePath: null,
    planSlug: null,
    workUnitId: null,
    wipState: null,
    wipRef: null,
    custodySessionMismatch: false,
    ...over,
  };
}

/** `generations[0]` is marked this boot's, as the backend serves it, unless `thisBootRetired` is false. */
function report(
  generations: LedgerGeneration[],
  current: LedgerEntry[] = [],
  thisBootRetired = true,
): LedgerReport {
  generations = generations.map((g, i) => ({ ...g, thisBoot: thisBootRetired && i === 0 }));
  return {
    status: "ok",
    reason: null,
    generatedAt: 10_000,
    priorCapturedAt: null,
    priorReason: null,
    expected: [],
    returned: [],
    missing: [],
    finished: [],
    closedByUser: [],
    verdict: "partial",
    current: {
      ledgerVersion: 1,
      capturedAtMs: 10_000,
      capturedAt: "x",
      reason: "read",
      bootAtMs: 9_000,
      shutdownAt: null,
      cleanShutdown: true,
      sessions: current,
    },
    generations,
    savedAtMs: 9_500,
    savedMatchesCurrent: true,
    note: "",
  };
}

function ctx(over: Partial<RowContext> & { complete?: string[]; deferred?: string[] } = {}) {
  return {
    progress: {
      completePages: new Set(over.complete ?? ["p1"]),
      deferredPages: new Set(over.deferred ?? []),
      knownPageIds: ["p1", "p2"],
    },
    needsAccount: over.needsAccount ?? new Map(),
    accountChoices: over.accountChoices ?? new Map(),
  } satisfies RowContext;
}

describe("page restore settlement", () => {
  it("settles a known page only once its own restore drained", () => {
    const c = ctx({ complete: ["p1"] });
    expect(pageRestoreSettled("p1", c.progress)).toBe(true);
    expect(pageRestoreSettled("p2", c.progress)).toBe(false);
  });
  it("settles an orphan page (no live page) once ANY page restored — it was adopted", () => {
    expect(pageRestoreSettled("gone", ctx({ complete: ["p1"] }).progress)).toBe(true);
    expect(pageRestoreSettled("gone", ctx({ complete: [] }).progress)).toBe(false);
  });
});

describe("rows", () => {
  it("labels each outcome and sorts what needs the operator first", () => {
    const gen = generation("g", {
      returned: [back("b")],
      missing: [outcome("m"), outcome("w", { pageId: "p2" })],
      finished: [finished("f")],
    });
    const rows = buildRows(gen, ctx());
    expect(rows.map((r) => [r.outcome.claudeSessionId, r.state, r.label])).toEqual([
      ["m", "missing", "missing: no-attempt"],
      ["w", "waiting-page", "waiting: page not opened"],
      ["b", "back", "back"],
      ["f", "finished", "finished"],
    ]);
  });

  it("reads `needs-account` from the boot restore, with its reason, and never selects it", () => {
    const row = buildRow(
      outcome("n"),
      ctx({ needsAccount: new Map([["n", "ambiguous-transcript"]]) }),
    );
    expect(row.state).toBe("needs-account");
    expect(row.detail).toMatch(/several account directories/);
    expect(row.account.known).toBe(false);
    expect(row.selectable).toBe(false);
  });

  it("reads `needs-account` from an unknown ledger account too", () => {
    const row = buildRow(outcome("u", { resumeAccount: { known: false, configDir: null } }), ctx());
    expect(row.state).toBe("needs-account");
    expect(row.blockedReason).toBe("choose an account first");
  });

  it("a chosen account makes the row resumable under THAT account", () => {
    const chosen = { known: true, configDir: "C:/claude/.claude-extra" };
    const row = buildRow(
      outcome("n"),
      ctx({
        needsAccount: new Map([["n", "no-transcript-holder"]]),
        accountChoices: new Map([["n", chosen]]),
      }),
    );
    expect(row.state).toBe("missing");
    expect(row.accountChosen).toBe(true);
    expect(row.account).toEqual(chosen);
    expect(row.selectable).toBe(true);
  });

  it("marks rows on a drain-deferred page as waiting on the drain", () => {
    const row = buildRow(outcome("d"), ctx({ deferred: ["p1"] }));
    expect(row.state).toBe("waiting-drain");
    expect(row.selectable).toBe(false);
  });

  it("refuses a never-restorable row and a row with no directory", () => {
    expect(buildRow(outcome("x", { reason: "not-restorable" }), ctx()).selectable).toBe(false);
    expect(buildRow(outcome("y", { resumeDir: null }), ctx()).blockedReason).toBe(
      "no working directory was recorded",
    );
  });

  it("resumes in the backend's resume dir — the same one the copy line cd's into", () => {
    expect(buildRow(outcome("a"), ctx()).resumeDir).toBe("D:/repo/sub");
    expect(buildRow(outcome("b", { resumeDir: "D:/repo" }), ctx()).resumeDir).toBe("D:/repo");
  });

  it("labels a session the operator closed since as `closed by you` — tickable, never missing", () => {
    const row = buildRow(closedByUser("c"), ctx());
    expect(row.state).toBe("closed-by-user");
    expect(row.label).toBe("closed by you");
    expect(row.selectable).toBe(true);
    const unknown = buildRow(
      outcome("u", {
        outcome: "closed-by-user",
        reason: null,
        resumeAccount: { known: false, configDir: null },
      }),
      ctx(),
    );
    expect(unknown.selectable).toBe(false);
    expect(unknown.blockedReason).toBe("the account it ran under is unknown");
  });
});

describe("pre-check selection", () => {
  const gen = generation("g", {
    returned: [back("b")],
    missing: [
      outcome("known"),
      outcome("unknown-restorability", {
        reason: "restorability-unknown",
        restorable: null,
      }),
      outcome("needs", { resumeAccount: { known: false, configDir: null } }),
      outcome("waiting", { pageId: "p2" }),
    ],
    finished: [finished("f")],
    closedByUser: [closedByUser("closed")],
  });

  it("pre-checks only unfinished, missing, KNOWN-resumable rows — never one closed by the user", () => {
    expect([...preCheckedIds(buildRows(gen, ctx()))]).toEqual(["known"]);
  });

  it("pre-checks NOTHING in an older generation; the operator may still tick its rows", () => {
    const rows = buildRows(gen, ctx());
    const sel = reconcileSelection(EMPTY_SELECTION, "g", rows, false);
    expect([...sel.ids]).toEqual([]);
    expect(
      selectedResumable(rows, new Set(["known"])).map((r) => r.outcome.claudeSessionId),
    ).toEqual(["known"]);
  });

  it("starts a generation from its pre-checked rows", () => {
    const rows = buildRows(gen, ctx());
    const sel = reconcileSelection(EMPTY_SELECTION, "g", rows, true);
    expect([...sel.ids]).toEqual(["known"]);
    expect([...sel.seen].sort()).toEqual(["closed", "known", "unknown-restorability"]);
  });

  it("returns the SAME object when nothing changed (safe to set during render)", () => {
    const rows = buildRows(gen, ctx());
    const sel = reconcileSelection(EMPTY_SELECTION, "g", rows, true);
    expect(reconcileSelection(sel, "g", rows, true)).toBe(sel);
  });

  it("pre-checks a row when its page settles, keeping the operator's un-check", () => {
    const first = reconcileSelection(EMPTY_SELECTION, "g", buildRows(gen, ctx()), true);
    const unchecked = { ...first, ids: new Set<string>() }; // operator un-checked "known"
    const settled = buildRows(gen, ctx({ complete: ["p1", "p2"] }));
    const next = reconcileSelection(unchecked, "g", settled, true);
    expect([...next.ids]).toEqual(["waiting"]);
  });

  it("a new generation starts over", () => {
    const first = reconcileSelection(EMPTY_SELECTION, "g", buildRows(gen, ctx()), true);
    const other = generation("h", { missing: [outcome("z")] });
    expect([...reconcileSelection(first, "h", buildRows(other, ctx()), true).ids]).toEqual(["z"]);
  });

  it("acts only on selected rows that are still selectable", () => {
    const rows = buildRows(gen, ctx());
    const picked = selectedResumable(rows, new Set(["known", "needs", "b"]));
    expect(picked.map((r) => r.outcome.claudeSessionId)).toEqual(["known"]);
  });
});

describe("which generation the panel opens on", () => {
  const a = generation("newest", { missing: [], returned: [back("x")] });
  const b = generation("older", { missing: [outcome("m")] });
  const c = generation("oldest", { missing: [outcome("n")] });

  it("is the NEWEST generation when it has an unfinished miss not yet reviewed", () => {
    expect(defaultGenerationFile(report([b, c]), new Set())).toBe("older");
  });
  it("never falls back to an OLDER generation — the current roster (null) instead", () => {
    expect(defaultGenerationFile(report([a, b, c]), new Set())).toBeNull();
    expect(defaultGenerationFile(report([b, c]), new Set(["older"]))).toBeNull();
    expect(defaultGenerationFile(null, new Set())).toBeNull();
  });
  it("is the generation marked this boot's, never the first by stamp when this boot retired none", () => {
    // This boot retained nothing: generations[0] is an older boot's roster.
    expect(defaultGenerationFile(report([b, c], [], false), new Set())).toBeNull();
    expect(stripModel(report([b, c], [], false), ctx())).toBeNull();
    expect(thisBootGeneration(report([b, c]))?.file).toBe("older");
  });
});

describe("strip copy", () => {
  it("K > 0: N of M are back · K didn't come back", () => {
    const r = report([
      generation("g", { returned: [back("a"), back("b")], missing: [outcome("m")] }),
    ]);
    expect(stripModel(r, ctx())).toMatchObject({
      kind: "some-missing",
      back: 2,
      total: 3,
      missing: 1,
      text: "2 of 3 sessions from before the restart are back · 1 didn't come back",
    });
  });

  it("K = 0: all M are back", () => {
    const r = report([generation("g", { returned: [back("a"), back("b")] })]);
    expect(stripModel(r, ctx())?.text).toBe("All 2 sessions from before the restart are back");
    const one = report([generation("g", { returned: [back("a")] })]);
    expect(stripModel(one, ctx())?.text).toBe("The session from before the restart is back");
  });

  it("finished sessions are neither back nor missing", () => {
    const r = report([generation("g", { returned: [back("a")], finished: [finished("f")] })]);
    expect(stripModel(r, ctx())?.kind).toBe("all-back");
  });

  it("an `unknown` verdict makes K an upper bound", () => {
    const r = report([
      generation("g", { returned: [back("a")], missing: [outcome("m")] }, "unknown"),
    ]);
    expect(stripModel(r, ctx())?.text).toBe(
      "1 of 2 sessions from before the restart are back · up to 1 didn't come back",
    );
  });

  it("deferred by drain: says how many are waiting", () => {
    const r = report([
      generation("g", { returned: [back("a")], missing: [outcome("m"), outcome("n")] }),
    ]);
    expect(stripModel(r, ctx({ complete: [], deferred: ["p1"] }))).toMatchObject({
      kind: "deferred",
      waiting: 2,
      text: "Restore deferred by drain — 2 sessions waiting",
    });
  });

  it("deferred by drain: a miss on a page that already restored is not waiting", () => {
    const r = report([
      generation("g", {
        missing: [outcome("settled"), outcome("held", { pageId: "p2" })],
      }),
    ]);
    expect(stripModel(r, ctx({ complete: ["p1"], deferred: ["p2"] }))).toMatchObject({
      kind: "deferred",
      waiting: 1,
    });
  });

  it("closed-by-user sessions are neither back nor missing", () => {
    const r = report([
      generation("g", { returned: [back("a")], closedByUser: [closedByUser("c")] }),
    ]);
    expect(stripModel(r, ctx())).toMatchObject({ kind: "all-back", total: 1 });
  });

  it("stays hidden until the first page has restored — never on the census latch", () => {
    const r = report([generation("g", { returned: [back("a")], missing: [outcome("m")] })]);
    expect(stripModel(r, ctx({ complete: [] }))).toBeNull();
    expect(stripModel(r, ctx({ complete: ["p1"] }))?.kind).toBe("some-missing");
  });

  it("an unopened page does not hide the strip: its rows are counted as waiting, never missing", () => {
    const r = report([
      generation("g", { returned: [back("a")], missing: [outcome("m", { pageId: "p2" })] }),
    ]);
    expect(stripModel(r, ctx({ complete: ["p1"] }))).toMatchObject({
      kind: "some-missing",
      missing: 0,
      waiting: 1,
      text: "1 of 2 sessions from before the restart are back · 1 waiting on a page not opened yet",
    });
    expect(stripModel(r, ctx({ complete: ["p1", "p2"] }))).toMatchObject({
      missing: 1,
      waiting: 0,
      text: "1 of 2 sessions from before the restart are back · 1 didn't come back",
    });
  });

  it("speaks about the NEWEST generation, and nothing without one", () => {
    const r = report([
      generation("new", { returned: [back("a")] }),
      generation("old", { missing: [outcome("m")] }),
    ]);
    expect(stripModel(r, ctx())).toMatchObject({ kind: "all-back", generationFile: "new" });
    expect(stripModel(report([]), ctx())).toBeNull();
  });
});

describe("the sequential resume queue", () => {
  it("runs one at a time and continues after a failure and a throw", async () => {
    const events: string[] = [];
    let inFlight = 0;
    let maxInFlight = 0;
    const resumeOne = async (id: string) => {
      inFlight += 1;
      maxInFlight = Math.max(maxInFlight, inFlight);
      events.push(`start ${id}`);
      await new Promise((r) => setTimeout(r, 5));
      inFlight -= 1;
      events.push(`end ${id}`);
      if (id === "b") return { ok: false as const, reason: "the resume did not verify" };
      if (id === "c") throw new Error("boom");
      return { ok: true as const };
    };
    const progress = new Map<string, ResumeProgress[]>();
    await runResumeQueue(["a", "b", "c", "d"], resumeOne, (id, p) =>
      progress.set(id, [...(progress.get(id) ?? []), p]),
    );
    expect(maxInFlight).toBe(1);
    expect(events).toEqual([
      "start a",
      "end a",
      "start b",
      "end b",
      "start c",
      "end c",
      "start d",
      "end d",
    ]);
    expect(progress.get("a")?.map((p) => p.state)).toEqual(["resuming", "back"]);
    expect(progress.get("b")?.at(-1)).toEqual({
      state: "failed",
      reason: "the resume did not verify",
    });
    expect(progress.get("c")?.at(-1)).toEqual({ state: "failed", reason: "boom" });
    expect(progress.get("d")?.at(-1)).toEqual({ state: "back" });
  });

  it("marks every row queued SYNCHRONOUSLY at enqueue, before the first starts", async () => {
    const seen: string[] = [];
    const queue = new ResumeQueue();
    const { done } = queue.enqueue(
      ["a", "b"],
      async () => ({ ok: true }),
      (id, p) => seen.push(`${id}:${p.state}`),
    );
    expect(seen).toEqual(["a:queued", "b:queued"]);
    await done;
    expect(seen.slice(2)).toEqual(["a:resuming", "a:back", "b:resuming", "b:back"]);
  });

  it("never queues an id that is already queued or in flight — no double resume", async () => {
    const started: string[] = [];
    let release: () => void = () => {};
    const gate = new Promise<void>((r) => (release = r));
    const resumeOne = async (id: string) => {
      started.push(id);
      await gate;
      return { ok: true as const };
    };
    const queue = new ResumeQueue();
    const first = queue.enqueue(["a"], resumeOne, () => {});
    const second = queue.enqueue(["a", "b"], resumeOne, () => {});
    expect(first.admitted).toEqual(["a"]);
    expect(second.admitted).toEqual(["b"]);
    release();
    await Promise.all([first.done, second.done]);
    expect(started).toEqual(["a", "b"]);
    // Once settled, the same id may be queued again (a Retry).
    expect(queue.enqueue(["a"], resumeOne, () => {}).admitted).toEqual(["a"]);
  });

  it("a batch whose onProgress throws neither wedges the queue nor rejects", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    const queue = new ResumeQueue();
    const broken = queue.enqueue(
      ["a"],
      async () => ({ ok: true }),
      (_id, p) => {
        if (p.state === "resuming") throw new Error("render blew up");
      },
    );
    await expect(broken.done).resolves.toBeUndefined();
    // The same id is admitted again, and it actually runs.
    const started: string[] = [];
    const again = queue.enqueue(
      ["a"],
      async (id) => {
        started.push(id);
        return { ok: true };
      },
      () => {},
    );
    expect(again.admitted).toEqual(["a"]);
    await again.done;
    expect(started).toEqual(["a"]);
    warn.mockRestore();
  });

  it("cancel stops before the next spawn and marks the rest failed: cancelled", async () => {
    const started: string[] = [];
    const queue = new ResumeQueue();
    const progress = new Map<string, ResumeProgress>();
    const { done } = queue.enqueue(
      ["a", "b", "c"],
      async (id) => {
        started.push(id);
        if (id === "a") queue.cancel(); // the panel unmounts mid-resume
        return { ok: true };
      },
      (id, p) => progress.set(id, p),
    );
    await done;
    expect(started).toEqual(["a"]);
    expect(progress.get("a")).toEqual({ state: "back" });
    expect(progress.get("b")).toEqual({ state: "failed", reason: RESUME_CANCELLED });
    expect(progress.get("c")).toEqual({ state: "failed", reason: RESUME_CANCELLED });
    expect(
      queue.enqueue(
        ["d"],
        async () => ({ ok: true }),
        () => {},
      ).admitted,
    ).toEqual([]);
  });

  it("renders progress as queued → resuming → back / failed: <reason>", () => {
    expect(describeProgress({ state: "queued" })).toBe("queued");
    expect(describeProgress({ state: "resuming" })).toBe("resuming");
    expect(describeProgress({ state: "back" })).toBe("back");
    expect(describeProgress({ state: "failed", reason: "x" })).toBe("failed: x");
  });
});

describe("Finish / Unfinish row state", () => {
  it("offers Finish on an unfinished row and Unfinish on a finished one", () => {
    expect(finishButtonModel(false, undefined)).toMatchObject({
      label: "Finish",
      target: true,
      disabled: false,
    });
    expect(finishButtonModel(true, undefined)).toMatchObject({
      label: "Unfinish",
      target: false,
      disabled: false,
    });
  });

  it("shows the transition and disables the button while pending", () => {
    expect(finishButtonModel(false, { phase: "pending", to: true })).toMatchObject({
      label: "Finishing…",
      disabled: true,
    });
    expect(finishButtonModel(true, { phase: "pending", to: false })).toMatchObject({
      label: "Unfinishing…",
      disabled: true,
    });
  });

  it("after a failure keeps the roster's truth and says why", () => {
    const m = finishButtonModel(false, { phase: "failed", to: true, message: "store down" });
    expect(m.label).toBe("Finish");
    expect(m.disabled).toBe(false);
    expect(m.title).toMatch(/Last attempt failed: store down/);
  });
});

describe("Review from any page", () => {
  it("asks the app to bring the Terminal view up", () => {
    const target = new EventTarget();
    const pages: unknown[] = [];
    target.addEventListener(APP_NAVIGATE_EVENT, (e) =>
      pages.push((e as CustomEvent<{ page: string }>).detail.page),
    );
    requestTerminalView(target);
    expect(pages).toEqual(["terminal"]);
  });
});

describe("the resume target", () => {
  it("passes the DEFAULT home's explicit path so the verified resume records the account", () => {
    const row = buildRow(outcome("d", { resumeAccount: { known: true, configDir: null } }), ctx());
    expect(resumeTargetFor(row, "/home/u/.claude")).toMatchObject({
      claudeSessionId: "d",
      workingDir: "D:/repo/sub",
      configDir: "/home/u/.claude",
    });
    // An explicit account is passed as is; an unknown home leaves it unset.
    expect(resumeTargetFor(buildRow(outcome("x"), ctx()), "/home/u/.claude")?.configDir).toBe(
      "C:/claude/.claude-gmail",
    );
    expect(resumeTargetFor(row, null)?.configDir).toBeUndefined();
  });

  it("a chosen default account is recorded explicitly too", () => {
    const row = buildRow(
      outcome("n"),
      ctx({
        needsAccount: new Map([["n", "no-transcript-holder"]]),
        accountChoices: new Map([["n", { known: true, configDir: null }]]),
      }),
    );
    expect(resumeTargetFor(row, "/home/u/.claude")?.configDir).toBe("/home/u/.claude");
  });

  it("never targets a row whose account is unknown", () => {
    const row = buildRow(outcome("u", { resumeAccount: { known: false, configDir: null } }), ctx());
    expect(resumeTargetFor(row, "/home/u/.claude")).toBeNull();
  });
});

describe("the pre-rebuild preview", () => {
  it("counts the unfinished sessions a restart now would bring back", () => {
    const r = report([], [entry("a"), entry("b"), entry("f", { finished: true })]);
    const p = previewModel(r, 10_000);
    expect(p.text).toBe("If the runner restarts now: 2 unfinished sessions will come back");
    expect(p.savedText).toBe("last captured just now");
    expect(p.savedStale).toBe(false);
  });

  it("says when nothing was captured this boot, and when the saved roster lags", () => {
    const none = { ...report([], [entry("a")]), savedAtMs: null };
    expect(previewModel(none, 10_000).savedText).toBe("not captured yet this boot");
    const lagging = { ...report([], [entry("a")]), savedAtMs: 1_000, savedMatchesCurrent: false };
    const p = previewModel(lagging, 121_000);
    expect(p.savedText).toBe("last captured 2m ago");
    expect(p.savedStale).toBe(true);
  });
});

describe("stable UI-Bridge ids", () => {
  const SECTION = readFileSync(
    fileURLToPath(new URL("./SinceRestartSection.tsx", import.meta.url)),
    "utf8",
  );
  const STRIP = readFileSync(
    fileURLToPath(new URL("./SinceRestartStrip.tsx", import.meta.url)),
    "utf8",
  );
  const PAST = readFileSync(
    fileURLToPath(new URL("./PastSessionsView.tsx", import.meta.url)),
    "utf8",
  );

  it("keys per-row ids on the session id", () => {
    expect(sinceRestartRowId("abc")).toBe("terminal.since-restart-row-abc");
    expect(sinceRestartCheckId("abc")).toBe("terminal.since-restart-check-abc");
    expect(pastSessionFinishId("abc")).toBe("terminal.past-session-finish-abc");
  });

  it("stamps every control of the strip and the section", () => {
    for (const src of [SECTION, STRIP]) {
      const opening = src.match(/<(button|select|input)\b[\s\S]*?\n\s*\/?>/g) ?? [];
      expect(opening.length).toBeGreaterThan(0);
      for (const tag of opening) expect(tag).toContain("data-ui-bridge-id=");
    }
    expect(STRIP).toContain("data-ui-bridge-id={SINCE_RESTART_STRIP_ID}");
    expect(SECTION).toContain("data-ui-bridge-id={SINCE_RESTART_RESUME_SELECTED_ID}");
    expect(SECTION).toContain("data-ui-bridge-id={sinceRestartRowId(id)}");
    expect(SECTION).toContain("data-ui-bridge-id={sinceRestartCheckId(id)}");
  });

  it("puts Finish/Unfinish on every Previous Sessions cohort card", () => {
    expect(PAST).toContain("data-ui-bridge-id={pastSessionFinishId(session.claudeSessionId)}");
  });
});
