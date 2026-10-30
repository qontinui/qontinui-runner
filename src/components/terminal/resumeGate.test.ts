/**
 * The click-resume liveness gate (plan
 * `2026-10-27-click-resume-forks-a-live-conversation-and-pane-titles-hide-its-identity`,
 * Phase 3): every arm of `decideClickResume`, its polarity against
 * `decideColdResume`, the fork argument builder, the click funnel (nothing is
 * started before a dialog choice), the gated Retry (nothing is retyped before
 * a choice), the bulk tally and summary, and "go to the existing pane".
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => ({})) }));
vi.mock("@/lib/instance-storage", () => ({
  instanceStorage: {
    getItem: () => null,
    setItem: () => {},
    removeItem: () => {},
    getJSON: <T>(_k: string, fallback: T) => fallback,
    setJSON: () => {},
  },
}));

import { liveSessionIdSet, type LiveClaudeSession } from "./liveClaudeSessions";
import {
  bulkResumeSummary,
  canForkTarget,
  claimResume,
  clearResumesInFlight,
  conversationNameOf,
  decideClickResume,
  describeHolder,
  forkTabTitle,
  GATE_LIVENESS_READ_TIMEOUT_MS,
  goToHolderPane,
  isResumeInFlight,
  outcomeOfFailedAttempt,
  paneHolderOf,
  readLiveWithin,
  RESUME_IN_FLIGHT_GRACE_MS,
  runBulkResume,
  runClaimedRestoreResume,
  runClaimedResume,
  runClickResume,
  runGatedRetry,
  withForkSession,
  type ClickResumeDeps,
  type ResumeGateChoice,
  type ResumeGatePrompt,
  type ResumeOutcome,
  type RestoreDecision,
} from "./resumeGate";
import type { ResumeAttempt, ResumeTarget } from "./resumeInNewTab";
import type { RevealableZoneLayout } from "./tabReveal";
import { decideColdResume } from "./useTerminalInitialization";

const ID = "9b40e22d-deec-415c-9379-5f85ae994ad8";

function row(over: Partial<LiveClaudeSession> = {}): LiveClaudeSession {
  return {
    sessionId: ID,
    name: "maxim",
    pid: 49448,
    account: { label: "hotmail", wrapper: "clh" },
    workingDir: "D:/qontinui-root",
    status: "busy",
    kind: "interactive",
    startedAt: 1,
    updatedAt: 2,
    resumeCommand: null,
    ...over,
  };
}

const target: ResumeTarget = {
  claudeSessionId: ID,
  displayName: "maxim",
  workingDir: "D:/qontinui-root",
  configDir: "C:/claude/.claude-hotmail",
};

/** A promise the test resolves by hand — the dialog the operator has not answered yet. */
function deferred<T>() {
  let resolve!: (v: T) => void;
  const promise = new Promise<T>((r) => (resolve = r));
  return { promise, resolve };
}

const flush = () => new Promise((r) => setTimeout(r, 0));

// The in-flight registry is module state: every test starts with none held.
beforeEach(() => clearResumesInFlight());

// ---------------------------------------------------------------------------

describe("decideClickResume — every arm", () => {
  it("an unreadable registry (null) is unknown, never free", () => {
    expect(decideClickResume(null, ID)).toEqual({ kind: "unknown" });
  });

  it("no rows, or rows for other ids only, is free", () => {
    expect(decideClickResume([], ID)).toEqual({ kind: "free" });
    expect(decideClickResume([row({ sessionId: "other" })], ID)).toEqual({ kind: "free" });
  });

  it("one holder is live with that holder", () => {
    const h = row({ terminalId: "pane-a", pageId: "page-1" });
    expect(decideClickResume([h, row({ sessionId: "other" })], ID)).toEqual({
      kind: "live",
      holders: [h],
    });
  });

  it("many holders: all are returned, runner-pane holders first", () => {
    const outside = row({ pid: 1 });
    const inPaneA = row({ pid: 2, terminalId: "pane-a", pageId: "p" });
    const inPaneB = row({ pid: 3, terminalId: "pane-b" });
    const d = decideClickResume([outside, inPaneA, inPaneB], ID);
    expect(d.kind).toBe("live");
    if (d.kind !== "live") return;
    expect(d.holders.map((h) => h.pid)).toEqual([2, 3, 1]);
    expect(paneHolderOf(d.holders)?.pid).toBe(2);
  });

  it("a holder outside every runner pane is still live (pane unknown, not dead)", () => {
    const d = decideClickResume([row()], ID);
    expect(d).toEqual({ kind: "live", holders: [row()] });
    expect(paneHolderOf(d.kind === "live" ? d.holders : [])).toBeNull();
  });

  it("ignores rows without a usable session id", () => {
    const junk = [{ ...row(), sessionId: "" }, null, { pid: 3 }] as unknown as LiveClaudeSession[];
    expect(decideClickResume(junk, ID)).toEqual({ kind: "free" });
    expect(decideClickResume(junk, "")).toEqual({ kind: "free" });
  });
});

describe("decideClickResume agrees with decideColdResume (one oracle, one polarity)", () => {
  const cases: Array<[string, LiveClaudeSession[] | null, string]> = [
    ["unreadable", null, ID],
    ["empty", [], ID],
    ["other id only", [row({ sessionId: "other" })], ID],
    ["one holder in a pane", [row({ terminalId: "t" })], ID],
    ["one holder outside", [row()], ID],
    ["two holders", [row({ pid: 1 }), row({ pid: 2, terminalId: "t" })], ID],
    ["case differs", [row({ sessionId: ID.toUpperCase() })], ID],
    ["blank id rows", [{ ...row(), sessionId: "" }], ""],
    ["mixed", [row({ sessionId: "a" }), row({ sessionId: ID }), row({ sessionId: "b" })], ID],
  ];
  it.each(cases)("%s: free iff respawn", (_name, rows, id) => {
    const click = decideClickResume(rows, id).kind;
    const cold = decideColdResume(rows === null ? null : liveSessionIdSet(rows), id);
    expect(click === "free").toBe(cold === "respawn");
    expect(click === "live").toBe(cold === "skip-alive");
    expect(click === "unknown").toBe(cold === "skip-unknown");
  });
});

// ---------------------------------------------------------------------------

describe("withForkSession (the fork argument builder)", () => {
  it("appends --fork-session after a bare --resume <id>, keeping the trailing CR", () => {
    expect(withForkSession(`claude --dangerously-skip-permissions --resume ${ID}\r`, ID)).toBe(
      `claude --dangerously-skip-permissions --resume ${ID} --fork-session\r`,
    );
  });

  it("works through env prefixes in both shell syntaxes and with a quoted id", () => {
    expect(
      withForkSession(
        `$env:CLAUDE_CODE_RESUME_TOKEN_THRESHOLD="999999999"; $env:CLAUDE_CONFIG_DIR="C:/c"; claude --resume '${ID}'\r`,
        ID,
      ),
    ).toBe(
      `$env:CLAUDE_CODE_RESUME_TOKEN_THRESHOLD="999999999"; $env:CLAUDE_CONFIG_DIR="C:/c"; claude --resume '${ID}' --fork-session\r`,
    );
    expect(withForkSession(`CLAUDE_CONFIG_DIR="/c" claude --resume "${ID}"`, ID)).toBe(
      `CLAUDE_CONFIG_DIR="/c" claude --resume "${ID}" --fork-session`,
    );
  });

  it("is idempotent on a command that already forks", () => {
    const forked = `claude --resume ${ID} --fork-session\r`;
    expect(withForkSession(forked, ID)).toBe(forked);
  });

  it("refuses (null) when the command does not END in --resume <that id>", () => {
    expect(withForkSession("my-alias\r", ID)).toBeNull();
    expect(withForkSession(`claude --resume ${ID} -- "hi"\r`, ID)).toBeNull();
    expect(withForkSession(`claude --resume other-id\r`, ID)).toBeNull();
    expect(withForkSession(`claude --resume ${ID}x\r`, ID)).toBeNull();
    expect(withForkSession(`claude --resume ${ID}\r`, "")).toBeNull();
  });
});

describe("names and labels", () => {
  it("the fork tab title is `fork of <name> · <id8>`", () => {
    expect(forkTabTitle("maxim", ID)).toBe("fork of maxim · 9b40e22d");
    expect(forkTabTitle(null, ID)).toBe("fork of (unnamed) · 9b40e22d");
  });

  it("an operator-named live holder's name wins; a derived one does not; else the fallback", () => {
    expect(conversationNameOf([row({ name: "  maxim " })], "transcript-name")).toBe("maxim");
    expect(
      conversationNameOf([row({ name: "qontinui-root-ec", nameSource: "derived" })], "tn"),
    ).toBe("tn");
    expect(conversationNameOf([], "  ")).toBeNull();
    expect(
      conversationNameOf(
        [row({ name: "older", updatedAt: 1 }), row({ name: "newer", updatedAt: 9 })],
        null,
      ),
    ).toBe("newer");
  });

  it("describes a pane holder by pane, and an outside holder by pid, account and directory", () => {
    expect(describeHolder(row({ terminalId: "09ce90d9-xxxx", pageId: "e7810bcf-yyyy" }))).toBe(
      "runner pane 09ce90d9 on page e7810bcf (pid 49448)",
    );
    expect(describeHolder(row())).toBe(
      "pid 49448 · account hotmail · D:/qontinui-root (outside any runner pane)",
    );
  });

  it("only a Claude session can fork", () => {
    expect(canForkTarget({})).toBe(true);
    expect(canForkTarget({ provider: "claude" })).toBe(true);
    expect(canForkTarget({ provider: "gemini" })).toBe(false);
  });
});

// ---------------------------------------------------------------------------

function clickDeps(
  rows: LiveClaudeSession[] | null,
  over: Partial<ClickResumeDeps> = {},
): ClickResumeDeps & {
  start: ReturnType<typeof vi.fn>;
  ask: ReturnType<typeof vi.fn>;
} {
  const start = vi.fn(async (): Promise<ResumeAttempt> => ({ kind: "verified", tabId: "t" }));
  const ask = vi.fn(async (): Promise<ResumeGateChoice> => "cancel");
  return { readLive: async () => rows, start, ask, ...over } as never;
}

describe("runClickResume — the funnel", () => {
  it("free: resumes as before, with the pre-type liveness re-read left on", async () => {
    const d = clickDeps([]);
    await expect(runClickResume(d, target)).resolves.toBe("resumed");
    expect(d.start).toHaveBeenCalledWith(target, { fork: false, allowLive: false });
    expect(d.ask).not.toHaveBeenCalled();
  });

  it("free but the attempt fails or throws: reports refused / failed and says so", async () => {
    const onFailed = vi.fn();
    const failing = clickDeps([], {
      start: vi.fn(async () => ({
        kind: "failed" as const,
        failure: "not-created" as const,
        tabId: null,
      })),
      onFailed,
    });
    // No tab was opened: a refusal, not a failed resume.
    await expect(runClickResume(failing, target)).resolves.toBe("refused");
    expect(onFailed.mock.calls[0][0]).toMatchObject({ failure: "not-created" });

    const unverified = clickDeps([], {
      start: vi.fn(async () => ({
        kind: "failed" as const,
        failure: "not-verified" as const,
        tabId: "t-left",
      })),
    });
    await expect(runClickResume(unverified, target)).resolves.toBe("failed");
    // That attempt left a tab, so its id keeps the in-flight grace; the next
    // case is a fresh click.
    clearResumesInFlight();

    const onError = vi.fn();
    const throwing = clickDeps([], {
      start: vi.fn(async () => {
        throw new Error("ipc down");
      }),
      onError,
    });
    await expect(runClickResume(throwing, target)).resolves.toBe("failed");
    expect(onError).toHaveBeenCalledTimes(1);
  });

  it("live: never starts anything before the dialog is answered", async () => {
    const choice = deferred<ResumeGateChoice>();
    const d = clickDeps([row({ terminalId: "pane-a", pageId: "page-1" })], {
      ask: vi.fn(() => choice.promise),
    });
    const outcome = runClickResume(d, target, { conversationName: "ignored" });
    await flush();
    expect(d.ask).toHaveBeenCalledTimes(1);
    expect(d.start).not.toHaveBeenCalled();
    const prompt = d.ask.mock.calls[0][0] as ResumeGatePrompt;
    expect(prompt).toMatchObject({
      kind: "live",
      claudeSessionId: ID,
      conversationName: "maxim",
      canFork: true,
    });
    expect(prompt.holders).toHaveLength(1);
    choice.resolve("cancel");
    await expect(outcome).resolves.toBe("cancelled");
    expect(d.start).not.toHaveBeenCalled();
  });

  it.each<[ResumeGateChoice, ResumeOutcome, unknown]>([
    ["go-to-pane", "skipped-live", null],
    ["fork", "resumed", { fork: true, allowLive: true, forkTitle: "fork of maxim · 9b40e22d" }],
    ["resume-anyway", "resumed", { fork: false, allowLive: true }],
    ["cancel", "cancelled", null],
  ])("live + %s → %s", async (choice, outcome, mode) => {
    const d = clickDeps([row({ terminalId: "pane-a" })], { ask: vi.fn(async () => choice) });
    await expect(runClickResume(d, target)).resolves.toBe(outcome);
    if (mode === null) expect(d.start).not.toHaveBeenCalled();
    else expect(d.start).toHaveBeenCalledWith(target, mode);
  });

  it("live outside every pane: 'go to pane' cannot apply, so it reads as cancelled", async () => {
    const d = clickDeps([row()], { ask: vi.fn(async () => "go-to-pane" as const) });
    await expect(runClickResume(d, target)).resolves.toBe("cancelled");
    expect(d.start).not.toHaveBeenCalled();
  });

  it("unknown (D2): asks in the UNKNOWN form and never falls through", async () => {
    const choice = deferred<ResumeGateChoice>();
    const d = clickDeps(null, { ask: vi.fn(() => choice.promise) });
    const outcome = runClickResume(d, target, { conversationName: "maxim" });
    await flush();
    expect(d.start).not.toHaveBeenCalled();
    expect(d.ask.mock.calls[0][0]).toMatchObject({
      kind: "unknown",
      holders: [],
      conversationName: "maxim",
    });
    choice.resolve("resume-anyway");
    await expect(outcome).resolves.toBe("resumed");
    expect(d.start).toHaveBeenCalledWith(target, { fork: false, allowLive: true });
  });

  it("non-interactive: live → skipped-live and unknown → skipped-unknown, with no dialog and no start", async () => {
    const live = clickDeps([row()]);
    await expect(runClickResume(live, target, { interactive: false })).resolves.toBe(
      "skipped-live",
    );
    const unknown = clickDeps(null);
    await expect(runClickResume(unknown, target, { interactive: false })).resolves.toBe(
      "skipped-unknown",
    );
    for (const d of [live, unknown]) {
      expect(d.ask).not.toHaveBeenCalled();
      expect(d.start).not.toHaveBeenCalled();
    }
  });

  it("a non-Claude session is offered no fork, and a fork answer is not acted on", async () => {
    const d = clickDeps([row()], { ask: vi.fn(async () => "fork" as const) });
    await expect(runClickResume(d, { ...target, provider: "gemini" })).resolves.toBe("cancelled");
    expect((d.ask.mock.calls[0][0] as ResumeGatePrompt).canFork).toBe(false);
    expect(d.start).not.toHaveBeenCalled();
  });
});

// ---------------------------------------------------------------------------

describe("runGatedRetry — the restore-failed Retry", () => {
  const args = { tabId: "tab-retry", claudeSessionId: ID, conversationName: "maxim" };

  it("a free id retries at once", async () => {
    const retry = vi.fn(async () => "verified" as const);
    const ask = vi.fn();
    await expect(runGatedRetry({ readLive: async () => [], ask, retry }, args)).resolves.toBe(
      "resumed",
    );
    expect(retry).toHaveBeenCalledTimes(1);
    expect(ask).not.toHaveBeenCalled();
  });

  it("a row held in THIS tab is not another writer — the retry verifies it in place", async () => {
    const retry = vi.fn(async () => "verified" as const);
    const ask = vi.fn();
    await runGatedRetry(
      { readLive: async () => [row({ terminalId: "tab-retry" })], ask, retry },
      args,
    );
    expect(ask).not.toHaveBeenCalled();
    expect(retry).toHaveBeenCalledTimes(1);
  });

  it("a live id elsewhere never calls runVerifiedResume before a dialog choice", async () => {
    const choice = deferred<ResumeGateChoice>();
    const retry = vi.fn(async () => "verified" as const);
    const ask = vi.fn(() => choice.promise);
    const outcome = runGatedRetry(
      { readLive: async () => [row({ terminalId: "other-pane" })], ask, retry },
      args,
    );
    await flush();
    expect(ask).toHaveBeenCalledTimes(1);
    expect(retry).not.toHaveBeenCalled();
    expect(ask.mock.calls[0][0]).toMatchObject({ kind: "live", canFork: false });
    choice.resolve("resume-anyway");
    await expect(outcome).resolves.toBe("resumed");
    expect(retry).toHaveBeenCalledTimes(1);
  });

  it("cancel and go-to-pane leave the tab untyped", async () => {
    for (const [choice, expected] of [
      ["cancel", "cancelled"],
      ["go-to-pane", "skipped-live"],
    ] as const) {
      const retry = vi.fn(async () => "verified" as const);
      await expect(
        runGatedRetry(
          {
            readLive: async () => [row({ terminalId: "other-pane" })],
            ask: async () => choice,
            retry,
          },
          args,
        ),
      ).resolves.toBe(expected);
      expect(retry).not.toHaveBeenCalled();
    }
  });

  it("an unreadable registry opens the UNKNOWN form and retries only on 'Resume anyway'", async () => {
    const retry = vi.fn(async () => "failed" as const);
    const ask = vi.fn(async (p: ResumeGatePrompt) => {
      expect(p.kind).toBe("unknown");
      return "resume-anyway" as const;
    });
    await expect(runGatedRetry({ readLive: async () => null, ask, retry }, args)).resolves.toBe(
      "failed",
    );
    expect(retry).toHaveBeenCalledTimes(1);
  });
});

// ---------------------------------------------------------------------------

describe("bulk 'resume selected'", () => {
  const ids = ["a", "b", "c", "d"];
  const items = ids.map((id) => ({ id, label: `name-${id}` }));

  /** The real funnel, non-interactively, over a fixed registry. */
  function bulk(rows: LiveClaudeSession[] | null, list = items) {
    const start = vi.fn(async (): Promise<ResumeAttempt> => ({ kind: "verified", tabId: "t" }));
    const ask = vi.fn();
    const sleep = vi.fn(async () => {});
    const run = runBulkResume(
      list,
      (it) => it.label,
      (it) =>
        runClickResume(
          { readLive: async () => rows, start, ask: ask as never },
          { ...target, claudeSessionId: it.id },
          { interactive: false },
        ),
      { sleep },
    );
    return { run, start, ask, sleep };
  }

  it("nothing selected: no summary", async () => {
    const { run } = bulk([], []);
    expect(bulkResumeSummary(await run)).toBeNull();
  });

  it("0 live ids: every item resumes, spaced between spawns", async () => {
    const { run, start, ask, sleep } = bulk([]);
    const tally = await run;
    expect(tally.resumed).toEqual(["name-a", "name-b", "name-c", "name-d"]);
    expect(start).toHaveBeenCalledTimes(4);
    expect(ask).not.toHaveBeenCalled();
    expect(sleep).toHaveBeenCalledTimes(3);
    expect(bulkResumeSummary(tally)).toEqual({
      message: "Resume selected — 4 resumed",
      type: "success",
    });
  });

  it("1 live id: it is skipped (never forked) and named", async () => {
    const { run, start, ask } = bulk([row({ sessionId: "b" })]);
    const tally = await run;
    expect(tally.live).toEqual(["name-b"]);
    expect(start.mock.calls.map(([t]) => (t as ResumeTarget).claudeSessionId)).toEqual([
      "a",
      "c",
      "d",
    ]);
    expect(ask).not.toHaveBeenCalled();
    expect(bulkResumeSummary(tally)).toEqual({
      message: "Resume selected — 3 resumed, 1 already live: name-b",
      type: "error",
    });
  });

  it("many live ids: all skipped and named", async () => {
    const { run, start } = bulk([
      row({ sessionId: "a" }),
      row({ sessionId: "c", terminalId: "p" }),
      row({ sessionId: "d" }),
    ]);
    const tally = await run;
    expect(tally.live).toEqual(["name-a", "name-c", "name-d"]);
    expect(start).toHaveBeenCalledTimes(1);
    expect(bulkResumeSummary(tally)?.message).toBe(
      "Resume selected — 1 resumed, 3 already live: name-a, name-c, name-d",
    );
  });

  it("an unreadable registry: nothing is resumed, every item is liveness-unknown", async () => {
    const { run, start, ask, sleep } = bulk(null);
    const tally = await run;
    expect(tally.unknown).toEqual(["name-a", "name-b", "name-c", "name-d"]);
    expect(start).not.toHaveBeenCalled();
    expect(ask).not.toHaveBeenCalled();
    expect(sleep).not.toHaveBeenCalled();
    expect(bulkResumeSummary(tally)).toEqual({
      message: "Resume selected — 0 resumed, 4 liveness unknown: name-a, name-b, name-c, name-d",
      type: "error",
    });
  });

  it("a failed or throwing item is tallied as failed, never as resumed", async () => {
    const outcomes: Array<ResumeOutcome | Error> = ["failed", new Error("boom"), "resumed"];
    const tally = await runBulkResume(
      outcomes,
      (_o) => `item-${outcomes.indexOf(_o)}`,
      async (o) => {
        if (o instanceof Error) throw o;
        return o;
      },
      { sleep: async () => {} },
    );
    expect(tally.failed).toEqual(["item-0", "item-1"]);
    expect(tally.resumed).toEqual(["item-2"]);
    expect(bulkResumeSummary(tally)?.message).toBe(
      "Resume selected — 1 resumed, 2 failed: item-0, item-1",
    );
  });
});

// ---------------------------------------------------------------------------

describe("refusals at the pre-type re-read (review W1)", () => {
  it.each<[string, string | null, ResumeOutcome]>([
    ["already-running", null, "skipped-live"],
    ["liveness-unknown", null, "skipped-unknown"],
    ["in-flight", null, "skipped-live"],
    ["invalid-session-id", null, "refused"],
    ["needs-account", null, "refused"],
    ["not-created", null, "refused"],
    ["not-verified", "t", "failed"],
    ["fork-unavailable", "t", "failed"],
    ["fork-not-verified", "t", "failed"],
  ])("%s (tab %s) → %s", (failure, tabId, outcome) => {
    expect(
      outcomeOfFailedAttempt({ kind: "failed", failure, tabId } as Extract<
        ResumeAttempt,
        { kind: "failed" }
      >),
    ).toBe(outcome);
  });

  it("the funnel reports a late live/unknown read as the gate would, and still says why", async () => {
    for (const [failure, expected] of [
      ["already-running", "skipped-live"],
      ["liveness-unknown", "skipped-unknown"],
    ] as const) {
      const onFailed = vi.fn();
      const d = clickDeps([], {
        start: vi.fn(async () => ({ kind: "failed" as const, failure, tabId: null })),
        onFailed,
      });
      await expect(runClickResume(d, target)).resolves.toBe(expected);
      expect(onFailed).toHaveBeenCalledTimes(1);
    }
  });

  it("bulk waits its spacing only after an item that opened a tab", async () => {
    // a: refused at the pre-type re-read (live by then); b: refused before a
    // tab; c: resumed; d: failed in its tab; e: resumed.
    const attempts: Record<string, ResumeAttempt> = {
      a: { kind: "failed", failure: "already-running", tabId: null },
      b: { kind: "failed", failure: "not-created", tabId: null },
      c: { kind: "verified", tabId: "t-c" },
      d: { kind: "failed", failure: "not-verified", tabId: "t-d" },
      e: { kind: "verified", tabId: "t-e" },
    };
    const order: string[] = [];
    const sleep = vi.fn(async () => {
      order.push("sleep");
    });
    const tally = await runBulkResume(
      Object.keys(attempts),
      (id) => id,
      (id) =>
        runClickResume(
          {
            readLive: async () => [],
            start: async () => {
              order.push(id);
              return attempts[id];
            },
            ask: vi.fn() as never,
          },
          { ...target, claudeSessionId: id },
          { interactive: false },
        ),
      { sleep },
    );
    // No sleep after a or b (no tab); one after c and one after d.
    expect(order).toEqual(["a", "b", "c", "sleep", "d", "sleep", "e"]);
    expect(tally).toEqual({
      resumed: ["c", "e"],
      live: ["a"],
      unknown: [],
      failed: ["b", "d"],
      cancelled: [],
    });
  });
});

describe("resumes in flight (review M3)", () => {
  afterEach(() => {
    vi.useRealTimers();
  });

  it("a claim is exclusive until released, and a grace keeps it for that long", () => {
    vi.useFakeTimers();
    const first = claimResume(ID);
    expect(first).not.toBeNull();
    expect(claimResume(ID)).toBeNull();
    expect(claimResume("another-id")).not.toBeNull();
    first!.release(RESUME_IN_FLIGHT_GRACE_MS);
    expect(isResumeInFlight(ID)).toBe(true);
    vi.advanceTimersByTime(RESUME_IN_FLIGHT_GRACE_MS - 1);
    expect(claimResume(ID)).toBeNull();
    vi.advanceTimersByTime(1);
    expect(isResumeInFlight(ID)).toBe(false);
    const next = claimResume(ID);
    expect(next).not.toBeNull();
    // A release with no grace frees at once, and a second release is a no-op.
    next!.release();
    next!.release(RESUME_IN_FLIGHT_GRACE_MS);
    expect(isResumeInFlight(ID)).toBe(false);
  });

  it("a stale grace timer never frees a NEWER claim", () => {
    vi.useFakeTimers();
    claimResume(ID)!.release(RESUME_IN_FLIGHT_GRACE_MS);
    clearResumesInFlight(); // e.g. a test reset while the old timer is pending
    const newer = claimResume(ID);
    expect(newer).not.toBeNull();
    vi.advanceTimersByTime(RESUME_IN_FLIGHT_GRACE_MS);
    expect(isResumeInFlight(ID)).toBe(true);
  });

  it("two concurrent clicks on one free id start exactly one resume", async () => {
    const live = deferred<LiveClaudeSession[] | null>();
    const onInFlight = vi.fn();
    const d = clickDeps([], { readLive: vi.fn(() => live.promise), onInFlight });
    const first = runClickResume(d, target);
    const second = runClickResume(d, target);
    await expect(second).resolves.toBe("skipped-live");
    expect(onInFlight).toHaveBeenCalledWith(target);
    // The claim was taken before the FIRST liveness read: only one read.
    expect(d.readLive).toHaveBeenCalledTimes(1);
    live.resolve([]);
    await expect(first).resolves.toBe("resumed");
    expect(d.start).toHaveBeenCalledTimes(1);
  });

  it("the claim is held across the dialog, then kept for the grace only when a tab was opened", async () => {
    const choice = deferred<ResumeGateChoice>();
    const d = clickDeps([row()], { ask: vi.fn(() => choice.promise) });
    const outcome = runClickResume(d, target);
    await flush();
    expect(isResumeInFlight(ID)).toBe(true);
    await expect(runClickResume(clickDeps([]), target)).resolves.toBe("skipped-live");
    choice.resolve("cancel");
    await expect(outcome).resolves.toBe("cancelled");
    expect(isResumeInFlight(ID), "a cancel opened nothing: no grace").toBe(false);

    await expect(runClickResume(clickDeps([]), target)).resolves.toBe("resumed");
    expect(isResumeInFlight(ID), "a resume that opened a tab keeps its grace").toBe(true);
  });

  it("an interactive click acknowledges itself before its read; a bulk item does not", async () => {
    const order: string[] = [];
    const deps = (): ClickResumeDeps => ({
      ...clickDeps([]),
      readLive: async () => {
        order.push("read");
        return [];
      },
      onChecking: () => order.push("checking"),
    });
    await runClickResume(deps(), target);
    expect(order).toEqual(["checking", "read"]);
    order.length = 0;
    clearResumesInFlight();
    await runClickResume(deps(), target, { interactive: false });
    expect(order).toEqual(["read"]);
  });

  it("runClaimedResume refuses an id in flight as `in-flight` and starts nothing", async () => {
    const held = claimResume(ID)!;
    const start = vi.fn(async (): Promise<ResumeAttempt> => ({ kind: "verified", tabId: "t" }));
    await expect(runClaimedResume(target, start)).resolves.toEqual({
      kind: "failed",
      failure: "in-flight",
      tabId: null,
    });
    expect(start).not.toHaveBeenCalled();
    held.release();
    await expect(runClaimedResume(target, start)).resolves.toEqual({
      kind: "verified",
      tabId: "t",
    });
    expect(isResumeInFlight(ID), "a tab was opened: grace").toBe(true);
    clearResumesInFlight();
    const refused = vi.fn(
      async (): Promise<ResumeAttempt> => ({
        kind: "failed",
        failure: "needs-account",
        tabId: null,
      }),
    );
    await runClaimedResume(target, refused);
    expect(isResumeInFlight(ID), "no tab: released at once").toBe(false);
  });
});

// ---------------------------------------------------------------------------

/** A liveness read that never answers. */
const hungRead = (): Promise<LiveClaudeSession[] | null> => new Promise(() => {});

describe("a liveness read that never answers (review r2 finding 2)", () => {
  afterEach(() => {
    vi.useRealTimers();
  });

  it("readLiveWithin: an answer passes through; a throw, and no answer in time, are UNKNOWN (null)", async () => {
    vi.useFakeTimers();
    await expect(readLiveWithin(async () => [row()])).resolves.toEqual([row()]);
    await expect(readLiveWithin(async () => null)).resolves.toBeNull();
    await expect(
      readLiveWithin(async () => {
        throw new Error("ipc");
      }),
    ).resolves.toBeNull();

    let settled: LiveClaudeSession[] | null | "pending" = "pending";
    void readLiveWithin(hungRead).then((v) => (settled = v));
    await vi.advanceTimersByTimeAsync(GATE_LIVENESS_READ_TIMEOUT_MS - 1);
    expect(settled).toBe("pending");
    await vi.advanceTimersByTimeAsync(1);
    expect(settled).toBeNull();
  });

  it("a bulk item whose read hangs is skipped-unknown, and its claim released", async () => {
    vi.useFakeTimers();
    const d = clickDeps([], { readLive: hungRead });
    const outcome = runClickResume(d, target, { interactive: false });
    await vi.advanceTimersByTimeAsync(0);
    expect(isResumeInFlight(ID)).toBe(true);
    await vi.advanceTimersByTimeAsync(GATE_LIVENESS_READ_TIMEOUT_MS);
    await expect(outcome).resolves.toBe("skipped-unknown");
    expect(d.start).not.toHaveBeenCalled();
    expect(isResumeInFlight(ID)).toBe(false);
  });

  it("a click whose read hangs opens the UNKNOWN dialog (D2), and its answer releases the claim", async () => {
    vi.useFakeTimers();
    const d = clickDeps([], { readLive: hungRead });
    const outcome = runClickResume(d, target);
    await vi.advanceTimersByTimeAsync(GATE_LIVENESS_READ_TIMEOUT_MS);
    expect(d.ask).toHaveBeenCalledTimes(1);
    expect(d.ask.mock.calls[0][0]).toMatchObject({ kind: "unknown", holders: [] });
    await expect(outcome).resolves.toBe("cancelled");
    expect(d.start).not.toHaveBeenCalled();
    expect(isResumeInFlight(ID)).toBe(false);
  });

  it("the restore-failed Retry's read is bounded the same way", async () => {
    vi.useFakeTimers();
    const ask = vi.fn(async (): Promise<ResumeGateChoice> => "cancel");
    const retry = vi.fn(async (): Promise<"verified" | "failed"> => "verified");
    const outcome = runGatedRetry(
      { readLive: hungRead, ask, retry },
      { tabId: "t", claudeSessionId: ID },
    );
    await vi.advanceTimersByTimeAsync(GATE_LIVENESS_READ_TIMEOUT_MS);
    await expect(outcome).resolves.toBe("cancelled");
    expect(ask.mock.calls[0][0]).toMatchObject({ kind: "unknown" });
    expect(retry).not.toHaveBeenCalled();
    expect(isResumeInFlight(ID)).toBe(false);
  });
});

describe("the click's 'checking…' acknowledgement ends with the check (review r2 finding 4)", () => {
  it("onChecked runs after the read and before the resume or the dialog — never for a bulk item", async () => {
    const order: string[] = [];
    const deps = (rows: LiveClaudeSession[] | null): ClickResumeDeps => ({
      readLive: async () => {
        order.push("read");
        return rows;
      },
      start: async (): Promise<ResumeAttempt> => {
        order.push("start");
        return { kind: "verified", tabId: "t" };
      },
      ask: async (): Promise<ResumeGateChoice> => {
        order.push("ask");
        return "cancel";
      },
      onChecking: () => order.push("checking"),
      onChecked: () => order.push("checked"),
    });
    await runClickResume(deps([]), target);
    expect(order).toEqual(["checking", "read", "checked", "start"]);

    clearResumesInFlight();
    order.length = 0;
    await runClickResume(deps([row()]), target);
    expect(order).toEqual(["checking", "read", "checked", "ask"]);

    clearResumesInFlight();
    order.length = 0;
    await runClickResume(deps([]), target, { interactive: false });
    expect(order).toEqual(["read", "start"]);
  });
});

describe("runClaimedRestoreResume — the automatic restores take the claim (review r2 finding 3)", () => {
  afterEach(() => {
    vi.useRealTimers();
  });

  it("a held claim skips the record as alive (in flight): nothing is read or typed", async () => {
    const held = claimResume(ID)!;
    const decide = vi.fn(async (): Promise<RestoreDecision> => "respawn");
    const type = vi.fn(async () => {});
    await expect(runClaimedRestoreResume(ID, decide, type)).resolves.toEqual({
      kind: "skip-alive",
      inFlight: true,
    });
    expect(decide).not.toHaveBeenCalled();
    expect(type).not.toHaveBeenCalled();
    held.release();
  });

  it("respawn: the claim is held while typing and for the grace after, then released", async () => {
    vi.useFakeTimers();
    const typed = deferred<void>();
    const result = await runClaimedRestoreResume(
      ID,
      async () => "respawn",
      () => typed.promise,
    );
    expect(result.kind).toBe("typing");
    // A click now finds the id in flight instead of reading it free.
    expect(claimResume(ID)).toBeNull();
    typed.resolve();
    if (result.kind === "typing") await result.settled;
    expect(isResumeInFlight(ID), "the grace").toBe(true);
    await vi.advanceTimersByTimeAsync(RESUME_IN_FLIGHT_GRACE_MS);
    expect(isResumeInFlight(ID)).toBe(false);
  });

  it("a skip verdict releases at once; a type that throws keeps the grace and never rejects", async () => {
    vi.useFakeTimers();
    for (const verdict of ["skip-alive", "skip-unknown"] as const) {
      const type = vi.fn(async () => {});
      const r = await runClaimedRestoreResume(ID, async () => verdict, type);
      expect(r.kind).toBe(verdict);
      expect(type).not.toHaveBeenCalled();
      expect(isResumeInFlight(ID)).toBe(false);
    }
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    const r = await runClaimedRestoreResume(
      ID,
      async () => "respawn",
      async () => {
        throw new Error("ipc");
      },
    );
    expect(r.kind).toBe("typing");
    if (r.kind === "typing") await expect(r.settled).resolves.toBeUndefined();
    expect(isResumeInFlight(ID)).toBe(true);
    await vi.advanceTimersByTimeAsync(RESUME_IN_FLIGHT_GRACE_MS);
    expect(isResumeInFlight(ID)).toBe(false);
    warn.mockRestore();
  });

  it("a liveness read that throws releases the claim and rethrows", async () => {
    await expect(
      runClaimedRestoreResume(
        ID,
        async () => {
          throw new Error("read");
        },
        async () => {},
      ),
    ).rejects.toThrow("read");
    expect(isResumeInFlight(ID)).toBe(false);
  });
});

// ---------------------------------------------------------------------------

describe("goToHolderPane — 'Go to the existing pane'", () => {
  function layout(assignments: Record<number, string>): RevealableZoneLayout & {
    calls: string[];
  } {
    const calls: string[] = [];
    return {
      calls,
      assignments,
      layout: { zones: [0, 1, 2, 3] },
      focusedZone: 0,
      maximizedZone: null,
      getReservedZones: () => new Set<number>(),
      assignTabToZone: (z, t) => calls.push(`assign ${z} ${t}`),
      setMaximizedZone: (z) => calls.push(`maximize ${z}`),
      setFocusedZone: (z) => calls.push(`focus ${z}`),
      requestReveal: (t) => calls.push(`reveal ${t}`),
    };
  }

  it("switches to the holder's page, focuses its pane and flashes it", () => {
    const zl = layout({ 0: "x", 2: "pane-a" });
    const openPage = vi.fn();
    const flash = vi.fn();
    const result = goToHolderPane(
      {
        currentPageId: "page-here",
        getPage: (id) => (id === "page-1" ? { tabs: [{ id: "pane-a" }], zoneLayout: zl } : null),
        openPage,
        flash,
      },
      { terminalId: "pane-a", pageId: "page-1" },
    );
    expect(result).toBe("revealed");
    expect(openPage).toHaveBeenCalledWith("page-1");
    expect(zl.calls).toEqual(["focus 2", "reveal pane-a"]);
    expect(flash).toHaveBeenCalledWith("pane-a");
  });

  it("on the current page (or with no pageId) it switches nothing, and places a hidden tab", () => {
    const zl = layout({ 0: "x" });
    const openPage = vi.fn();
    const flash = vi.fn();
    expect(
      goToHolderPane(
        {
          currentPageId: "page-here",
          getPage: (id) =>
            id === "page-here" ? { tabs: [{ id: "pane-a" }], zoneLayout: zl } : null,
          openPage,
          flash,
        },
        { terminalId: "pane-a" },
      ),
    ).toBe("revealed");
    expect(openPage).not.toHaveBeenCalled();
    expect(zl.calls).toEqual(["assign 1 pane-a", "focus 1", "reveal pane-a"]);
    expect(flash).toHaveBeenCalledWith("pane-a");
  });

  it("a pane this window does not mount is not-found: no switch, no flash", () => {
    const openPage = vi.fn();
    const flash = vi.fn();
    const deps = { currentPageId: "here", getPage: () => null, openPage, flash };
    expect(goToHolderPane(deps, { terminalId: "pane-a", pageId: "popped-out" })).toBe("not-found");
    expect(goToHolderPane(deps, {})).toBe("not-found");
    const zl = layout({});
    expect(
      goToHolderPane(
        { ...deps, getPage: () => ({ tabs: [{ id: "someone-else" }], zoneLayout: zl }) },
        { terminalId: "pane-a", pageId: "here" },
      ),
    ).toBe("not-found");
    expect(openPage).not.toHaveBeenCalled();
    expect(flash).not.toHaveBeenCalled();
    expect(zl.calls).toEqual([]);
  });
});
