import { describe, it, expect, vi } from "vitest";
import {
  evaluateTransitions,
  matchesApprovalPattern,
  isRestartable,
  APPROVAL_MATCH_LINES,
  type EvaluateTransitionsInput,
  type TransitionTab,
} from "./transitionAutomation";
import type { SessionState } from "./useZoneLayout";

const TAB = (id: string, over: Partial<TransitionTab> = {}): TransitionTab => ({
  id,
  title: `title-${id}`,
  ...over,
});

function input(over: Partial<EvaluateTransitionsInput> = {}): EvaluateTransitionsInput {
  return {
    prev: {},
    next: {},
    tabs: [],
    assignments: {},
    autoApprovePatterns: [],
    autoRestart: false,
    getLastOutputLines: () => [],
    ...over,
  };
}

describe("matchesApprovalPattern", () => {
  it("matches case-insensitively", () => {
    expect(matchesApprovalPattern(["Do you want to PROCEED?"], ["proceed"])).toBe(true);
  });

  it("returns false with no patterns", () => {
    expect(matchesApprovalPattern(["proceed?"], [])).toBe(false);
  });

  it("returns false when nothing matches", () => {
    expect(matchesApprovalPattern(["all done"], ["proceed", "continue"])).toBe(false);
  });

  it("swallows an invalid regex instead of throwing, and keeps checking the others", () => {
    // "(" is a syntax error. It must not throw, and must not mask the sibling.
    expect(() => matchesApprovalPattern(["proceed?"], ["("])).not.toThrow();
    expect(matchesApprovalPattern(["proceed?"], ["("])).toBe(false);
    expect(matchesApprovalPattern(["proceed?"], ["(", "proceed"])).toBe(true);
  });

  // The window is pinned to a LITERAL, deliberately. Deriving the fixture from
  // APPROVAL_MATCH_LINES makes the test self-adjusting and therefore blind to
  // the constant's value — a mutation from 5 to 50 survived exactly that way.
  it("pins the match window at 5 lines", () => {
    expect(APPROVAL_MATCH_LINES).toBe(5);
  });

  it("ignores a match that has scrolled out of the 5-line window", () => {
    const lines = ["proceed?", "f1", "f2", "f3", "f4", "f5"]; // 6 lines
    expect(matchesApprovalPattern(lines, ["proceed"])).toBe(false);
  });

  it("matches on the oldest line still INSIDE the 5-line window", () => {
    const lines = ["f0", "proceed?", "f1", "f2", "f3", "f4"]; // "proceed?" is 5th-from-last
    expect(matchesApprovalPattern(lines, ["proceed"])).toBe(true);
  });

  it("joins lines with newline so a pattern can span the window", () => {
    expect(matchesApprovalPattern(["yes", "no"], ["yes\\nno"])).toBe(true);
  });
});

describe("isRestartable", () => {
  it("accepts a clean exit", () => {
    expect(isRestartable(TAB("a", { exitCode: 0 }))).toBe(true);
  });

  it("accepts an unreported exit (null / undefined)", () => {
    expect(isRestartable(TAB("a", { exitCode: null }))).toBe(true);
    expect(isRestartable(TAB("a", { exitCode: undefined }))).toBe(true);
    expect(isRestartable(TAB("a"))).toBe(true);
  });

  it("refuses a non-zero exit — a failure the operator should see", () => {
    expect(isRestartable(TAB("a", { exitCode: 1 }))).toBe(false);
    expect(isRestartable(TAB("a", { exitCode: 130 }))).toBe(false);
  });

  it("refuses a missing tab", () => {
    expect(isRestartable(undefined)).toBe(false);
  });
});

describe("evaluateTransitions — purity", () => {
  it("does NOT advance prev — the single-advance invariant depends on it", () => {
    const prev: Record<string, SessionState> = { a: "working" };
    const next: Record<string, SessionState> = { a: "needs-input" };
    const prevSnapshot = { ...prev };

    evaluateTransitions(input({ prev, next }));

    expect(prev).toEqual(prevSnapshot);
    expect(prev.a).toBe("working");
  });

  it("does not mutate next, tabs or assignments", () => {
    const next: Record<string, SessionState> = { a: "completed" };
    const tabs = [TAB("a", { exitCode: 0 })];
    const assignments = { 0: "a" };
    const nextCopy = { ...next };
    const assignmentsCopy = { ...assignments };

    evaluateTransitions(input({ prev: {}, next, tabs, assignments, autoRestart: true }));

    expect(next).toEqual(nextCopy);
    expect(assignments).toEqual(assignmentsCopy);
    expect(tabs).toEqual([TAB("a", { exitCode: 0 })]);
  });

  it("is idempotent across the stateful-looking branches (regex + restart)", () => {
    // Deliberately exercises the approval regex path and the restart path —
    // a default-args fixture would make this a restatement of the purity test
    // above rather than an independent check.
    const args = input({
      prev: { a: "working", b: "working" },
      next: { a: "needs-input", b: "completed" },
      tabs: [TAB("a"), TAB("b", { exitCode: 0 })],
      assignments: { 0: "a", 1: "b" },
      autoRestart: true,
      autoApprovePatterns: ["proceed", "("],
      getLastOutputLines: () => ["proceed?"],
    });
    const first = evaluateTransitions(args);
    expect(first.approvals).toEqual(["a"]);
    expect(first.restarts).toHaveLength(1);
    expect(evaluateTransitions(args)).toEqual(first);
  });
});

describe("evaluateTransitions — edge detection", () => {
  it("detects a needs-input edge", () => {
    const out = evaluateTransitions(
      input({ prev: { a: "working" }, next: { a: "needs-input" } }),
    );
    expect(out.newNeedsInput).toEqual(["a"]);
    expect(out.newErrors).toEqual([]);
    expect(out.newCompleted).toEqual([]);
  });

  it("does NOT re-fire an edge while the state is unchanged", () => {
    const out = evaluateTransitions(
      input({ prev: { a: "needs-input" }, next: { a: "needs-input" } }),
    );
    expect(out.newNeedsInput).toEqual([]);
    expect(out.stateChanges).toEqual([]);
  });

  it("treats a first observation (no prior state) as an edge", () => {
    const out = evaluateTransitions(input({ prev: {}, next: { a: "needs-input" } }));
    expect(out.newNeedsInput).toEqual(["a"]);
    // toStrictEqual, not toEqual: toEqual treats `{from: undefined}` and `{}`
    // as equal, and "the key is present and undefined" is the point here.
    expect(out.stateChanges).toStrictEqual([
      { tabId: "a", from: undefined, to: "needs-input", zoneIdx: undefined, title: "a" },
    ]);
  });

  it("detects error and completed edges independently", () => {
    const out = evaluateTransitions(
      input({
        prev: { a: "working", b: "working" },
        next: { a: "error", b: "completed" },
      }),
    );
    expect(out.newErrors).toEqual(["a"]);
    expect(out.newCompleted).toEqual(["b"]);
    expect(out.newNeedsInput).toEqual([]);
  });

  it("records the zone and title on a state change", () => {
    const out = evaluateTransitions(
      input({
        prev: { a: "working" },
        next: { a: "error" },
        tabs: [TAB("a", { title: "my session" })],
        assignments: { 3: "a" },
      }),
    );
    expect(out.stateChanges).toEqual([
      { tabId: "a", from: "working", to: "error", zoneIdx: 3, title: "my session" },
    ]);
  });

  it("falls back to the tab id when the tab is not in the roster", () => {
    const out = evaluateTransitions(input({ prev: {}, next: { ghost: "error" } }));
    expect(out.stateChanges[0].title).toBe("ghost");
  });

  it("ignores a tab that left `next` entirely", () => {
    const out = evaluateTransitions(input({ prev: { gone: "working" }, next: {} }));
    expect(out.stateChanges).toEqual([]);
    expect(out.newErrors).toEqual([]);
  });
});

describe("evaluateTransitions — auto-approve", () => {
  const needsInput = { prev: { a: "working" as SessionState }, next: { a: "needs-input" as SessionState } };

  it("approves a tab whose trailing output matches", () => {
    const out = evaluateTransitions(
      input({
        ...needsInput,
        autoApprovePatterns: ["proceed\\?"],
        getLastOutputLines: () => ["Do you want to proceed?"],
      }),
    );
    expect(out.approvals).toEqual(["a"]);
  });

  it("does not approve when no pattern matches", () => {
    const out = evaluateTransitions(
      input({
        ...needsInput,
        autoApprovePatterns: ["proceed\\?"],
        getLastOutputLines: () => ["something else entirely"],
      }),
    );
    expect(out.approvals).toEqual([]);
  });

  it("never reads output when no patterns are configured", () => {
    const reader = vi.fn(() => ["proceed?"]);
    const out = evaluateTransitions({
      ...input({ ...needsInput, autoApprovePatterns: [] }),
      getLastOutputLines: reader,
    });
    expect(out.approvals).toEqual([]);
    expect(reader).not.toHaveBeenCalled();
  });

  it("reads output ONLY for tabs on the needs-input edge", () => {
    const reader = vi.fn(() => ["proceed?"]);
    evaluateTransitions({
      ...input({
        prev: { a: "working", b: "working", c: "needs-input" },
        next: { a: "needs-input", b: "error", c: "needs-input" },
        autoApprovePatterns: ["proceed"],
      }),
      getLastOutputLines: reader,
    });
    expect(reader.mock.calls.map((c) => c[0])).toEqual(["a"]);
  });

  it("approves only on the EDGE — a tab already needs-input is not re-approved", () => {
    const out = evaluateTransitions(
      input({
        prev: { a: "needs-input" },
        next: { a: "needs-input" },
        autoApprovePatterns: ["proceed"],
        getLastOutputLines: () => ["proceed?"],
      }),
    );
    expect(out.approvals).toEqual([]);
  });

  it("an invalid pattern does not throw and does not approve", () => {
    const args = input({
      ...needsInput,
      autoApprovePatterns: ["("],
      getLastOutputLines: () => ["proceed?"],
    });
    expect(() => evaluateTransitions(args)).not.toThrow();
    // The name promises this second half; assert it rather than implying it.
    expect(evaluateTransitions(args).approvals).toEqual([]);
  });

  it("an empty output read cannot match — the digest/tap feed supplies these lines", () => {
    const out = evaluateTransitions(
      input({
        ...needsInput,
        autoApprovePatterns: ["proceed"],
        getLastOutputLines: () => [],
      }),
    );
    expect(out.approvals).toEqual([]);
  });
});

describe("evaluateTransitions — auto-restart", () => {
  const completed = {
    prev: { a: "working" as SessionState },
    next: { a: "completed" as SessionState },
    assignments: { 2: "a" },
  };

  it("restarts a cleanly-exited tab in its zone", () => {
    const out = evaluateTransitions(
      input({ ...completed, tabs: [TAB("a", { exitCode: 0 })], autoRestart: true }),
    );
    expect(out.restarts).toEqual([{ zoneIdx: 2, tabId: "a", title: "title-a" }]);
  });

  it("does nothing when auto-restart is disarmed", () => {
    const out = evaluateTransitions(
      input({ ...completed, tabs: [TAB("a", { exitCode: 0 })], autoRestart: false }),
    );
    expect(out.restarts).toEqual([]);
    // …but the completed EDGE is still reported.
    expect(out.newCompleted).toEqual(["a"]);
  });

  it("refuses a non-zero exit", () => {
    const out = evaluateTransitions(
      input({ ...completed, tabs: [TAB("a", { exitCode: 1 })], autoRestart: true }),
    );
    expect(out.restarts).toEqual([]);
  });

  it("refuses an unassigned tab — a restart is a zone operation", () => {
    const out = evaluateTransitions(
      input({
        prev: { a: "working" },
        next: { a: "completed" },
        assignments: {},
        tabs: [TAB("a", { exitCode: 0 })],
        autoRestart: true,
      }),
    );
    expect(out.restarts).toEqual([]);
  });

  it("does not restart on an error edge", () => {
    const out = evaluateTransitions(
      input({
        prev: { a: "working" },
        next: { a: "error" },
        assignments: { 2: "a" },
        tabs: [TAB("a", { exitCode: 0 })],
        autoRestart: true,
      }),
    );
    expect(out.restarts).toEqual([]);
  });

  it("restarts on an error -> completed transition", () => {
    // The transition through `error` is the case the original nested `else if`
    // chain made non-obvious, so it gets its own test rather than relying on
    // the working -> completed case above.
    const out = evaluateTransitions(
      input({
        prev: { a: "error" },
        next: { a: "completed" },
        assignments: { 2: "a" },
        tabs: [TAB("a", { exitCode: 0 })],
        autoRestart: true,
      }),
    );
    expect(out.restarts).toEqual([{ zoneIdx: 2, tabId: "a", title: "title-a" }]);
    expect(out.newCompleted).toEqual(["a"]);
  });

  it("does not re-restart a tab that was already completed", () => {
    const out = evaluateTransitions(
      input({
        prev: { a: "completed" },
        next: { a: "completed" },
        assignments: { 2: "a" },
        tabs: [TAB("a", { exitCode: 0 })],
        autoRestart: true,
      }),
    );
    expect(out.restarts).toEqual([]);
  });
});

describe("evaluateTransitions — multi-tab", () => {
  it("handles approvals and restarts together in one diff", () => {
    const out = evaluateTransitions(
      input({
        prev: { a: "working", b: "working", c: "working" },
        next: { a: "needs-input", b: "completed", c: "error" },
        tabs: [TAB("a"), TAB("b", { exitCode: 0 }), TAB("c")],
        assignments: { 0: "a", 1: "b", 2: "c" },
        autoRestart: true,
        autoApprovePatterns: ["y/n"],
        getLastOutputLines: (id) => (id === "a" ? ["continue? y/n"] : []),
      }),
    );
    expect(out.newNeedsInput).toEqual(["a"]);
    expect(out.newCompleted).toEqual(["b"]);
    expect(out.newErrors).toEqual(["c"]);
    expect(out.approvals).toEqual(["a"]);
    expect(out.restarts).toEqual([{ zoneIdx: 1, tabId: "b", title: "title-b" }]);
    expect(out.stateChanges).toHaveLength(3);
  });

  it("preserves observation order in stateChanges — the hook replays it for history", () => {
    const out = evaluateTransitions(
      input({
        prev: { a: "working", b: "working", c: "working" },
        next: { a: "needs-input", b: "error", c: "completed" },
        tabs: [TAB("a"), TAB("b"), TAB("c")],
        assignments: { 0: "a", 1: "b", 2: "c" },
      }),
    );
    expect(out.stateChanges.map((s) => s.tabId)).toEqual(["a", "b", "c"]);
    expect(out.stateChanges.map((s) => s.to)).toEqual([
      "needs-input",
      "error",
      "completed",
    ]);
  });

  it("reports an empty outcome for an empty diff", () => {
    const out = evaluateTransitions(input({ prev: { a: "idle" }, next: { a: "idle" } }));
    expect(out).toEqual({
      newNeedsInput: [],
      newErrors: [],
      newCompleted: [],
      approvals: [],
      restarts: [],
      stateChanges: [],
    });
  });
});
