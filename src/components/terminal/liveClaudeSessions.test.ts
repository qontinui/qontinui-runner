import { describe, it, expect } from "vitest";

import {
  DERIVED_NAME_SOURCE,
  extractLiveSessions,
  groupByAccount,
  isOperatorNamed,
  registryNamesBySessionId,
  sharedSessionIds,
  type LiveClaudeSession,
} from "./liveClaudeSessions";

function mk(over: Partial<LiveClaudeSession> = {}): LiveClaudeSession {
  return {
    sessionId: "b770ae37-1ffa-4888-a5d1-89d058307adf",
    name: "per-agent coord-mcp proxy",
    pid: 2804,
    account: { label: "paktis", wrapper: "clp" },
    workingDir: "D:/qontinui-root",
    status: "idle",
    kind: "interactive",
    startedAt: 1784712055852,
    updatedAt: 1784770342016,
    resumeCommand: "cd 'D:/qontinui-root' && clp --resume b770ae37-1ffa-4888-a5d1-89d058307adf",
    ...over,
  };
}

describe("extractLiveSessions", () => {
  it("unwraps the {sessions:[…]} envelope the command returns", () => {
    const got = extractLiveSessions({ sessions: [mk()] });
    expect(got).toHaveLength(1);
    expect(got[0].name).toBe("per-agent coord-mcp proxy");
  });

  it("accepts a bare array", () => {
    expect(extractLiveSessions([mk()])).toHaveLength(1);
  });

  it("degrades to [] rather than throwing on any other shape", () => {
    // Callers iterate the result during render; a throw here takes the panel
    // (or the whole command card) down with it.
    expect(extractLiveSessions(null)).toEqual([]);
    expect(extractLiveSessions(undefined)).toEqual([]);
    expect(extractLiveSessions("nope")).toEqual([]);
    expect(extractLiveSessions({})).toEqual([]);
    expect(extractLiveSessions({ sessions: "not-an-array" })).toEqual([]);
  });
});

describe("isOperatorNamed (the nameSource gate)", () => {
  it("rejects Claude Code's own derived cwd slug", () => {
    // The regression this gate exists to prevent: `qontinui-root-ec` is WORSE
    // than the transcript/plan-title name the card already shows, so letting it
    // through would make the feature a net downgrade.
    expect(isOperatorNamed(mk({ name: "qontinui-root-ec", nameSource: "derived" }))).toBe(false);
    expect(
      isOperatorNamed(mk({ name: "qontinui-coord-88", nameSource: DERIVED_NAME_SOURCE })),
    ).toBe(false);
  });

  it("accepts a row with NO nameSource key — the absent-key majority", () => {
    // 12 of the 17 named rows on the operator's box (2026-07-28) omit the key
    // entirely and carry real `/rename` names. Absent must read as trustworthy,
    // or the gate silently disables the whole feature.
    const s = mk({ name: "worktree prune" });
    delete (s as { nameSource?: unknown }).nameSource;
    expect(isOperatorNamed(s)).toBe(true);
    expect(isOperatorNamed(mk({ name: "worktree prune", nameSource: undefined }))).toBe(true);
    // A runner binary predating the field can also send an explicit null.
    expect(isOperatorNamed(mk({ name: "worktree prune", nameSource: null }))).toBe(true);
  });

  it("accepts an unrecognized future nameSource", () => {
    // One-sided by design: only `"derived"` is disqualifying. Guessing wrong
    // about a future variant costs us nothing worse than today's behaviour.
    expect(isOperatorNamed(mk({ name: "n", nameSource: "launch-flag" }))).toBe(true);
  });

  it("rejects a blank name whatever its provenance", () => {
    expect(isOperatorNamed(mk({ name: "" }))).toBe(false);
    expect(isOperatorNamed(mk({ name: "   " }))).toBe(false);
  });

  it("degrades rather than throwing when `name` is missing entirely", () => {
    // Same defensiveness `groupByAccount` applies to `account`: the payload
    // reaches us through an unchecked cast and this runs during render.
    const s = mk();
    delete (s as { name?: unknown }).name;
    expect(isOperatorNamed(s)).toBe(false);
  });
});

describe("registryNamesBySessionId", () => {
  it("indexes operator-named rows and omits derived ones", () => {
    const got = registryNamesBySessionId([
      mk({ sessionId: "good", name: "worktree prune" }),
      mk({ sessionId: "slug", name: "qontinui-root-ec", nameSource: "derived" }),
    ]);
    expect(got.get("good")).toBe("worktree prune");
    // A MISS is the contract: the caller keeps whatever it shows today.
    expect(got.has("slug")).toBe(false);
  });

  it("lets an operator-named process win over a derived sibling on the same id", () => {
    // 22 of 80 ids had several live processes on 2026-07-23; only one of them
    // may carry the operator's name.
    const got = registryNamesBySessionId([
      mk({ sessionId: "dup", pid: 10, name: "qontinui-web-0c", nameSource: "derived" }),
      mk({ sessionId: "dup", pid: 11, name: "P3 window name" }),
    ]);
    expect(got.get("dup")).toBe("P3 window name");
  });

  it("picks the most recently updated process when several share an id", () => {
    const got = registryNamesBySessionId([
      mk({ sessionId: "dup", pid: 10, name: "older window", updatedAt: 1_000 }),
      mk({ sessionId: "dup", pid: 11, name: "newer window", updatedAt: 2_000 }),
    ]);
    expect(got.get("dup")).toBe("newer window");
  });

  it("does not let a rename hand the card a SIBLING process's name", () => {
    // The trap a name-ordered tiebreak falls into. `read_live_sessions` sorts
    // account → name → pid, so renaming the selected process can re-sort the
    // pair and silently switch which window the card describes.
    const before = registryNamesBySessionId([
      mk({ sessionId: "dup", pid: 10, name: "aaa", updatedAt: 5_000 }),
      mk({ sessionId: "dup", pid: 11, name: "zzz", updatedAt: 1_000 }),
    ]);
    expect(before.get("dup")).toBe("aaa");

    // Operator renames pid 10 (the active one) to "zzz2". Sorted by name it is
    // now SECOND — a first-wins tiebreak would show pid 11's "zzz" and the
    // rename would look like it did nothing.
    const after = registryNamesBySessionId([
      mk({ sessionId: "dup", pid: 11, name: "zzz", updatedAt: 1_000 }),
      mk({ sessionId: "dup", pid: 10, name: "zzz2", updatedAt: 6_000 }),
    ]);
    expect(after.get("dup")).toBe("zzz2");
  });

  it("keeps the first row when updatedAt ties, so the result is deterministic", () => {
    // Includes the 0 the Rust reader substitutes when the registry omits the
    // field — a whole-fleet tie must not flicker between polls.
    const got = registryNamesBySessionId([
      mk({ sessionId: "dup", pid: 10, name: "first", updatedAt: 0 }),
      mk({ sessionId: "dup", pid: 11, name: "second", updatedAt: 0 }),
    ]);
    expect(got.get("dup")).toBe("first");
  });

  it("trims the stored name", () => {
    // The gate tolerates padding; the card renders into a truncating span
    // where it would be visible.
    const got = registryNamesBySessionId([mk({ sessionId: "s", name: "  worktree prune  " })]);
    expect(got.get("s")).toBe("worktree prune");
  });

  it("ignores rows with no session id, and is empty for no input", () => {
    expect(registryNamesBySessionId([mk({ sessionId: "" })]).size).toBe(0);
    expect(registryNamesBySessionId([]).size).toBe(0);
  });
});

describe("sharedSessionIds", () => {
  it("reports ids held by more than one live process", () => {
    // The restore-duplication symptom: one session id, several live processes,
    // each with its own auto-generated name.
    const sessions = [
      mk({ sessionId: "dup", pid: 10, name: "qontinui-web-0c" }),
      mk({ sessionId: "dup", pid: 11, name: "qontinui-web-07" }),
      mk({ sessionId: "solo", pid: 12, name: "qontinui-root-11" }),
    ];
    const shared = sharedSessionIds(sessions);
    expect([...shared.keys()]).toEqual(["dup"]);
    expect(shared.get("dup")!.map((s) => s.name)).toEqual(["qontinui-web-0c", "qontinui-web-07"]);
  });

  it("is empty when every id is unique", () => {
    expect(sharedSessionIds([mk({ sessionId: "a" }), mk({ sessionId: "b" })]).size).toBe(0);
  });

  it("is empty for no sessions", () => {
    expect(sharedSessionIds([]).size).toBe(0);
  });
});

describe("groupByAccount", () => {
  it("groups by label and preserves input order within a group", () => {
    const sessions = [
      mk({ account: { label: "gmail", wrapper: "clg" }, name: "a" }),
      mk({ account: { label: "paktis", wrapper: "clp" }, name: "b" }),
      mk({ account: { label: "gmail", wrapper: "clg" }, name: "c" }),
    ];
    const g = groupByAccount(sessions);
    expect([...g.keys()]).toEqual(["gmail", "paktis"]);
    expect(g.get("gmail")!.map((s) => s.name)).toEqual(["a", "c"]);
  });

  it("buckets a missing account under 'unknown' instead of throwing", () => {
    const s = mk();
    // Force the defensive path: the Rust side always sends `account`, but a
    // version skew must not crash the grouping.
    delete (s as { account?: unknown }).account;
    expect([...groupByAccount([s]).keys()]).toEqual(["unknown"]);
  });
});
