/**
 * The "Since restart" roster model (plan
 * `2026-10-04-runner-session-roster-restore-picker`, Phases 4-5; the counts
 * per plan `2026-11-11-since-restart-roster-counts-closed-sessions-as-missing`,
 * Phase 5): pre-check selection, the sequential resume queue, the strip's
 * copy, Finish/Unfinish row state, and the pre-rebuild preview. Pure — the
 * runner's vitest env is `node` — plus a source read pinning the stable
 * UI-Bridge ids.
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
  clockTime,
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
  rowSummary,
  runResumeQueue,
  selectedResumable,
  sinceRestartCheckId,
  sinceRestartRowId,
  stripModel,
  thisBootGeneration,
  type ResumeProgress,
  type RowContext,
  type RowState,
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
    laterClosed: null,
    // A miss this runner's restore still owes, unless a test says otherwise.
    restoreOwed: true,
    resumeDir: "D:/repo/sub",
    resumeCommand: `cd "D:/repo/sub" && CLAUDE_CONFIG_DIR="C:/claude/.claude-gmail" claude --resume ${id}`,
    resumeAccount: { known: true, configDir: "C:/claude/.claude-gmail" },
    ...over,
  };
}

const back = (id: string, over: Partial<LedgerOutcome> = {}) =>
  outcome(id, { outcome: "back", reason: null, restoreOwed: null, ...over });
const finished = (id: string) =>
  outcome(id, { outcome: "finished", reason: null, finished: true, restoreOwed: false });
const closedSince = (id: string) =>
  outcome(id, { outcome: "closed-since", reason: "closed:explicit", restoreOwed: false });
/** A miss no restore in this runner will bring back (it closed since the boot). */
const lost = (id: string, over: Partial<LedgerOutcome> = {}) =>
  outcome(id, { reason: "closed:pty-exit", restoreOwed: false, ...over });
/** A session that came back and was then closed. */
const backLaterClosed = (
  id: string,
  closed: Partial<NonNullable<LedgerOutcome["laterClosed"]>> = {},
) =>
  back(id, {
    laterClosed: { atMs: 1_700_000_090_000, reason: "explicit", deliberate: true, ...closed },
  });

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
    closedSince?: LedgerOutcome[];
  },
  verdict: LedgerGeneration["verdict"] = "partial",
): LedgerGeneration {
  const returned = parts.returned ?? [];
  const missing = parts.missing ?? [];
  const fin = parts.finished ?? [];
  const closed = parts.closedSince ?? [];
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
    unknownReasons: [],
    returned,
    missing,
    finished: fin,
    closedSince: closed,
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
    closedSince: [],
    unknownReasons: [],
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
  it("does not settle an orphan page on a page the drain deferred — nothing was adopted", () => {
    const deferredOnly = ctx({ complete: ["p1"], deferred: ["p1"] }).progress;
    expect(pageRestoreSettled("gone", deferredOnly)).toBe(false);
    const oneRan = ctx({ complete: ["p1", "p2"], deferred: ["p1"] }).progress;
    expect(pageRestoreSettled("gone", oneRan)).toBe(true);
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

  it("labels a session closed on purpose since as `closed since the restart` — tickable, never missing", () => {
    const row = buildRow(closedSince("c"), ctx());
    expect(row.state).toBe("closed-since");
    expect(row.label).toBe("closed since the restart");
    expect(row.detail).toBe(
      "closed (explicit) after this roster was captured, without coming back — not expected back",
    );
    expect(row.selectable).toBe(true);
    const unknown = buildRow(
      outcome("u", {
        outcome: "closed-since",
        reason: "closed:explicit",
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
    closedSince: [closedSince("closed")],
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
  it("K > 0: N of M came back · K didn't come back", () => {
    const r = report([
      generation("g", { returned: [back("a"), back("b")], missing: [outcome("m")] }),
    ]);
    expect(stripModel(r, ctx())).toMatchObject({
      kind: "some-missing",
      back: 2,
      total: 3,
      missing: 1,
      text: "2 of 3 sessions from before the restart came back · 1 didn't come back",
    });
  });

  it("K = 0: all M came back", () => {
    const r = report([generation("g", { returned: [back("a"), back("b")] })]);
    expect(stripModel(r, ctx())?.text).toBe("All 2 sessions from before the restart came back");
    const one = report([generation("g", { returned: [back("a")] })]);
    expect(stripModel(one, ctx())?.text).toBe("The session from before the restart came back");
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
      "1 of 2 sessions from before the restart came back · up to 1 didn't come back",
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

  it("closed-since sessions are neither back nor missing", () => {
    const r = report([generation("g", { returned: [back("a")], closedSince: [closedSince("c")] })]);
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
      text: "1 of 2 sessions from before the restart came back · 1 waiting on a page not opened yet",
    });
    expect(stripModel(r, ctx({ complete: ["p1", "p2"] }))).toMatchObject({
      missing: 1,
      waiting: 0,
      text: "1 of 2 sessions from before the restart came back · 1 didn't come back",
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

/**
 * A `missing` outcome carrying the backend's restore-owed verdict (plan
 * `2026-11-11-since-restart-roster-counts-closed-sessions-as-missing`, D3):
 * `true` while a restore in this process may still bring it back, `false` when
 * none will (it closed since the boot, or is in no restore set), `null` when
 * that cannot be read.
 */
function owed(
  id: string,
  restoreOwed: boolean | null,
  over: Partial<LedgerOutcome> = {},
): LedgerOutcome {
  return outcome(id, { ...over, restoreOwed });
}

/** How many rows the strip says are waiting, whichever form it takes. */
function waitingIn(model: ReturnType<typeof stripModel>): number {
  return model?.kind === "deferred" || model?.kind === "some-missing" ? model.waiting : 0;
}

// Plan 2026-11-11-since-restart-roster-counts-closed-sessions-as-missing,
// Phase 0: "waiting" must mean a restore is still owed (D3). Red until Phase 5
// made them green.
describe("a row no restore is owed is never waiting", () => {
  it("the drain-deferred strip counts as waiting only the rows still owed a restore", () => {
    // Two misses on the drain-deferred page: a restore in this process will
    // still bring one back; the other closed `explicit` since the boot, and no
    // restore will.
    const r = report([
      generation("g", {
        returned: [back("a")],
        missing: [owed("owed", true), owed("closed", false)],
      }),
    ]);
    expect(waitingIn(stripModel(r, ctx({ complete: [], deferred: ["p1"] })))).toBe(1);
  });

  it("the drain-deferred strip says nothing is waiting when no miss is owed a restore", () => {
    const r = report([
      generation("g", { returned: [back("a")], missing: [owed("closed", false)] }),
    ]);
    expect(waitingIn(stripModel(r, ctx({ complete: [], deferred: ["p1"] })))).toBe(0);
  });

  it("buildRow does not label a row no restore is owed waiting-drain", () => {
    // Control: a row a restore still owes IS waiting on the drain.
    expect(buildRow(owed("o", true), ctx({ deferred: ["p1"] })).state).toBe("waiting-drain");

    const row = buildRow(owed("d", false), ctx({ deferred: ["p1"] }));
    expect(row.state).not.toMatch(/^waiting/);
    expect(row.label).not.toMatch(/^waiting/);
    expect(row.state).toBe("missing");
  });

  it("buildRow does not label a row no restore is owed waiting-page", () => {
    // Control: a row a restore still owes IS waiting on its unopened page.
    expect(buildRow(owed("o", true, { pageId: "p2" }), ctx()).state).toBe("waiting-page");

    const row = buildRow(owed("w", false, { pageId: "p2" }), ctx());
    expect(row.state).not.toMatch(/^waiting/);
    expect(row.label).not.toMatch(/^waiting/);
    expect(row.state).toBe("missing");
  });
});

// Plan 2026-11-11-since-restart-roster-counts-closed-sessions-as-missing,
// Phase 5: the strip and the panel read `restoreOwed` and `laterClosed`.
describe("owed, lost and closed-since rows", () => {
  it("labels every RowState", () => {
    const owedOnDrain = owed("drain", true, { pageId: "p2" });
    const cases: [LedgerOutcome, RowContext, RowState, string][] = [
      [back("b"), ctx(), "back", "back"],
      [
        backLaterClosed("lc", { atMs: null }),
        ctx(),
        "back-later-closed",
        "came back · later closed (explicit)",
      ],
      [outcome("m"), ctx(), "missing", "missing: no-attempt"],
      [lost("l", { pageId: "p2" }), ctx(), "missing", "missing: closed:pty-exit"],
      [owed("u", null, { pageId: "p2" }), ctx(), "unknown", "unknown: may still be restored"],
      [
        outcome("n", { resumeAccount: { known: false, configDir: null } }),
        ctx(),
        "needs-account",
        "needs-account",
      ],
      [finished("f"), ctx(), "finished", "finished"],
      [closedSince("c"), ctx(), "closed-since", "closed since the restart"],
      [owed("w", true, { pageId: "p2" }), ctx(), "waiting-page", "waiting: page not opened"],
      [
        owedOnDrain,
        ctx({ deferred: ["p2"] }),
        "waiting-drain",
        "waiting: restore deferred by drain",
      ],
    ];
    for (const [o, c, state, label] of cases) {
      const row = buildRow(o, c);
      expect([o.claudeSessionId, row.state, row.label]).toEqual([o.claudeSessionId, state, label]);
    }
    const covered = new Set(cases.map(([o, c]) => buildRow(o, c).state));
    const every: RowState[] = [
      "back",
      "back-later-closed",
      "missing",
      "unknown",
      "needs-account",
      "finished",
      "closed-since",
      "waiting-page",
      "waiting-drain",
    ];
    expect([...covered].sort()).toEqual([...every].sort());
  });

  it("a lost miss on an unopened page says no restore will bring it back", () => {
    const row = buildRow(lost("l", { pageId: "p2" }), ctx());
    expect(row.state).toBe("missing");
    expect(row.detail).toBe("no restore in this runner will bring it back");
    expect(row.selectable).toBe(true);
  });

  it("a session that came back and later closed names when and why — tickable, never pre-checked", () => {
    const at = 1_700_000_090_000;
    const row = buildRow(backLaterClosed("lc", { atMs: at }), ctx());
    expect(row.state).toBe("back-later-closed");
    expect(row.label).toBe(`came back · later closed ${clockTime(at)} (explicit)`);
    expect(row.detail).toBe("it came back after the restart, then was closed on purpose");
    expect(row.selectable).toBe(true);
    const pty = buildRow(backLaterClosed("p", { reason: "pty-exit", deliberate: null }), ctx());
    expect(pty.detail).toBe(
      "it came back after the restart, then its row closed (pty-exit); that reason does not say whether on purpose",
    );
    expect([...preCheckedIds([row, pty])]).toEqual([]);
    // A back row that is still open has nothing to resume.
    expect(buildRow(back("b"), ctx()).blockedReason).toBe("nothing to resume: back");
  });

  it("pre-checks the same rows: missing and known-resumable — never unknown, waiting, closed-since or later-closed", () => {
    const gen = generation("g", {
      returned: [back("b"), backLaterClosed("lc")],
      missing: [
        outcome("missed"),
        lost("lost", { pageId: "p2" }),
        owed("unknown", null, { pageId: "p2" }),
        owed("waiting", true, { pageId: "p2" }),
      ],
      closedSince: [closedSince("closed")],
      finished: [finished("f")],
    });
    const rows = buildRows(gen, ctx());
    expect([...preCheckedIds(rows)].sort()).toEqual(["lost", "missed"]);
    expect(rows.filter((r) => r.selectable).map((r) => r.outcome.claudeSessionId)).toEqual([
      "lost",
      "missed",
      "unknown",
      "lc",
      "closed",
    ]);
  });

  it("the strip counts 0, 1 and many owed rows as waiting and lost rows as didn't come back", () => {
    const strip = (missing: LedgerOutcome[]) =>
      stripModel(report([generation("g", { returned: [back("a")], missing })]), ctx());
    // 0 owed, 0 lost: all back.
    expect(strip([])?.text).toBe("The session from before the restart came back");
    // 1 owed (waiting), 0 lost.
    expect(strip([owed("w", true, { pageId: "p2" })])?.text).toBe(
      "1 of 2 sessions from before the restart came back · 1 waiting on a page not opened yet",
    );
    // 0 owed, 1 lost on the SAME unopened page: never waiting.
    expect(strip([lost("l", { pageId: "p2" })])?.text).toBe(
      "1 of 2 sessions from before the restart came back · 1 didn't come back",
    );
    // many of each.
    expect(
      strip([
        owed("w1", true, { pageId: "p2" }),
        owed("w2", true, { pageId: "p2" }),
        lost("l1", { pageId: "p2" }),
        lost("l2"),
        lost("l3"),
      ]),
    ).toMatchObject({
      kind: "some-missing",
      back: 1,
      total: 6,
      missing: 3,
      waiting: 2,
      upperBound: false,
      text: "1 of 6 sessions from before the restart came back · 3 didn't come back · 2 waiting on pages not opened yet",
    });
  });

  it("closed-since rows are neither waiting nor missing; finished ones neither", () => {
    const r = report([
      generation("g", {
        returned: [back("a")],
        closedSince: [closedSince("c1"), closedSince("c2")],
        finished: [finished("f")],
      }),
    ]);
    expect(stripModel(r, ctx())).toMatchObject({
      kind: "all-back",
      total: 1,
      text: "The session from before the restart came back",
    });
  });

  it("adds `· C later closed` to either form", () => {
    const all = report([
      generation("g", { returned: [back("a"), backLaterClosed("b"), backLaterClosed("c")] }),
    ]);
    expect(stripModel(all, ctx())).toMatchObject({
      kind: "all-back",
      laterClosed: 2,
      text: "All 3 sessions from before the restart came back · 2 later closed",
    });
    const some = report([
      generation("g", { returned: [back("a"), backLaterClosed("b")], missing: [lost("l")] }),
    ]);
    expect(stripModel(some, ctx())?.text).toBe(
      "2 of 3 sessions from before the restart came back · 1 later closed · 1 didn't come back",
    );
    const one = report([generation("g", { returned: [backLaterClosed("b")] })]);
    expect(stripModel(one, ctx())?.text).toBe(
      "The session from before the restart came back · 1 later closed",
    );
  });

  it("an unknown restoreOwed is never waiting and makes K an upper bound", () => {
    const r = report([
      generation("g", {
        returned: [back("a")],
        missing: [owed("u", null, { pageId: "p2" }), lost("l")],
      }),
    ]);
    expect(stripModel(r, ctx())).toMatchObject({
      kind: "some-missing",
      missing: 2,
      waiting: 0,
      upperBound: true,
      text: "1 of 3 sessions from before the restart came back · up to 2 didn't come back",
    });
    // Unknown on a page that already restored is still a miss whose fate was not read.
    const settled = report([
      generation("g", { returned: [back("a")], missing: [owed("u", null)] }),
    ]);
    expect(stripModel(settled, ctx())?.text).toBe(
      "1 of 2 sessions from before the restart came back · up to 1 didn't come back",
    );
    expect(buildRow(owed("u", null), ctx()).detail).toBe(
      "it was resumable and nothing brought it back; whether this runner's restore still owes it could not be read",
    );
  });

  it("a record-gone miss is an upper bound too", () => {
    const r = report([
      generation("g", { returned: [back("a")], missing: [lost("g", { reason: "record-gone" })] }),
    ]);
    expect(stripModel(r, ctx())?.text).toBe(
      "1 of 2 sessions from before the restart came back · up to 1 didn't come back",
    );
  });

  it("the deferred form with 0 owed rows falls through to the ordinary forms", () => {
    // p1 restored; p2 is deferred by the drain and holds only a lost session.
    const r = report([
      generation("g", { returned: [back("a")], missing: [lost("l", { pageId: "p2" })] }),
    ]);
    expect(stripModel(r, ctx({ complete: ["p1"], deferred: ["p2"] }))).toMatchObject({
      kind: "some-missing",
      missing: 1,
      waiting: 0,
      text: "1 of 2 sessions from before the restart came back · 1 didn't come back",
    });
    // ...and to nothing while no page has restored yet.
    expect(stripModel(r, ctx({ complete: [], deferred: ["p1", "p2"] }))).toBeNull();
    // The threshold: one owed row on the deferred page brings the deferred form back.
    const withOwed = report([
      generation("g", {
        returned: [back("a")],
        missing: [lost("l", { pageId: "p2" }), owed("w", true, { pageId: "p2" })],
      }),
    ]);
    expect(stripModel(withOwed, ctx({ complete: ["p1"], deferred: ["p2"] }))).toMatchObject({
      kind: "deferred",
      waiting: 1,
      text: "Restore deferred by drain — 1 session waiting",
    });
  });

  it("the panel summary counts came back, later closed, not back, waiting, closed since and finished", () => {
    const gen = generation("g", {
      returned: [back("b1"), back("b2"), backLaterClosed("lc")],
      missing: [outcome("m"), lost("l", { pageId: "p2" }), owed("w", true, { pageId: "p2" })],
      closedSince: [closedSince("c")],
      finished: [finished("f")],
    });
    expect(rowSummary(buildRows(gen, ctx()))).toBe(
      "3 came back (1 later closed) · 2 did not come back · 1 waiting · 1 closed since the restart · 1 finished",
    );
    const unknown = generation("g", {
      returned: [back("b")],
      missing: [owed("u", null, { pageId: "p2" })],
    });
    expect(rowSummary(buildRows(unknown, ctx()))).toBe(
      "1 came back · up to 1 did not come back · 0 finished",
    );
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

// Found by the 2026-11-13 since-restart E2E on a temp runner: with one page
// drain-deferred, the strip said "Restore deferred by drain" while every row
// read "waiting: page not opened". The strip and the rows must name the same
// cause, per row: deferred-by-drain when that row's page is in the deferred
// set, "page not opened" only when its page is neither deferred nor restored.
describe("the strip and the rows name the same waiting cause", () => {
  /** p1 restored; p2 and p3 are in the layout. */
  const ctx3 = (deferred: string[]) => {
    const c = ctx({ complete: ["p1"], deferred });
    return { ...c, progress: { ...c.progress, knownPageIds: ["p1", "p2", "p3"] } };
  };

  it("a row on the deferred page reads deferred by drain, and so does the strip", () => {
    const o = owed("held", true, { pageId: "p2" });
    const c = ctx3(["p2"]);
    expect(buildRow(o, c)).toMatchObject({
      state: "waiting-drain",
      label: "waiting: restore deferred by drain",
    });
    const r = report([generation("g", { returned: [back("a")], missing: [o] })]);
    expect(stripModel(r, c)).toMatchObject({
      kind: "deferred",
      waiting: 1,
      text: "Restore deferred by drain — 1 session waiting",
    });
  });

  it("a deferred page OTHER than the row's does not make the strip say drain", () => {
    const o = owed("unopened", true, { pageId: "p3" });
    const c = ctx3(["p2"]);
    expect(buildRow(o, c)).toMatchObject({
      state: "waiting-page",
      label: "waiting: page not opened",
    });
    const r = report([generation("g", { returned: [back("a")], missing: [o] })]);
    expect(stripModel(r, c)).toMatchObject({
      kind: "some-missing",
      waiting: 1,
      text: "1 of 2 sessions from before the restart came back · 1 waiting on a page not opened yet",
    });
  });

  it("rows on a deferred page and on an unopened page: the strip names both causes", () => {
    const held = owed("held", true, { pageId: "p2" });
    const unopened = owed("unopened", true, { pageId: "p3" });
    const c = ctx3(["p2"]);
    expect(buildRow(held, c).state).toBe("waiting-drain");
    expect(buildRow(unopened, c).state).toBe("waiting-page");
    const r = report([generation("g", { missing: [held, unopened] })]);
    expect(stripModel(r, c)).toMatchObject({
      kind: "deferred",
      waiting: 2,
      text: "Restore deferred by drain — 1 session waiting · 1 waiting on a page not opened yet",
    });
  });

  it("a row on a page neither deferred nor restored reads page not opened", () => {
    const o = owed("unopened", true, { pageId: "p3" });
    expect(buildRow(o, ctx3([])).state).toBe("waiting-page");
  });

  it("a row on a restored page that is not deferred is not waiting", () => {
    const o = owed("settled", true, { pageId: "p1" });
    expect(buildRow(o, ctx3([])).state).not.toMatch(/^waiting/);
  });
});
