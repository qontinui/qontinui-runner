/**
 * The webview's mirror of the runner's `agent_truth` verdict (plan
 * `2026-09-20-terminal-session-state-comes-from-events-not-screen-scraping`,
 * Phases 4-5): THE keystroke selector, the verdict → chip projection, and the
 * "State source" text the session-info dropdown renders.
 */

import { describe, it, expect, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(() => Promise.reject(new Error("no runner"))),
}));

import {
  describeSkippedInferred,
  fetchTerminalAgentStates,
  isAuthoritativePermissionAsk,
  isEventSourced,
  isInferredVerdict,
  isTerminalAgentStateEvent,
  offerAgentObservation,
  partitionPermissionAsks,
  sessionStateToObservation,
  stateSourceText,
  verdictToSessionState,
  type AgentState,
  type AgentTruthEntry,
  type Verdict,
} from "./agentTruth";

function verdict(over: Partial<Verdict> = {}): Verdict {
  return {
    state: { name: "needs_you", reason: "permission" },
    source: "hook",
    sinceMs: 1,
    confidence: "authoritative",
    disagreement: null,
    ...over,
  };
}

function entry(v: Partial<Verdict> = {}, hook: AgentTruthEntry["hookDelivery"] = { status: "installed" }): AgentTruthEntry {
  return { verdict: verdict(v), hookDelivery: hook };
}

describe("isAuthoritativePermissionAsk — the one keystroke gate", () => {
  it("is true only for an authoritative needs_you/permission", () => {
    expect(isAuthoritativePermissionAsk(verdict())).toBe(true);
  });

  it("is false with no verdict at all (runner predates the reducer)", () => {
    expect(isAuthoritativePermissionAsk(undefined)).toBe(false);
    expect(isAuthoritativePermissionAsk(null)).toBe(false);
  });

  it("is false for an inferred or fallback permission ask", () => {
    expect(isAuthoritativePermissionAsk(verdict({ confidence: "inferred", source: "regex" }))).toBe(false);
    expect(isAuthoritativePermissionAsk(verdict({ confidence: "fallback" }))).toBe(false);
    expect(isAuthoritativePermissionAsk(verdict({ confidence: null }))).toBe(false);
  });

  it("is false for every other needs_you reason, even authoritative", () => {
    for (const reason of ["question", "elicitation", "idle_prompt", "unspecified"] as const) {
      expect(isAuthoritativePermissionAsk(verdict({ state: { name: "needs_you", reason } }))).toBe(false);
    }
  });

  it("is false for every non-needs_you state", () => {
    const states: AgentState[] = [
      { name: "unknown" },
      { name: "starting" },
      { name: "working" },
      { name: "turn_ended" },
      { name: "failed", kind: "rate_limited" },
      { name: "ended", why: "exit" },
    ];
    for (const state of states) {
      expect(isAuthoritativePermissionAsk(verdict({ state }))).toBe(false);
    }
  });
});

describe("partitionPermissionAsks", () => {
  const tabs = [{ id: "hook" }, { id: "regex" }, { id: "working" }, { id: "none" }];
  const states = { hook: "needs-input", regex: "needs-input", working: "working", none: "needs-input" };
  const verdicts = {
    hook: entry(),
    regex: entry({ source: "regex", confidence: "inferred" }),
    working: entry({ state: { name: "working" } }),
  };

  it("types only into hook-reported asks and lists every inferred needs-input as skipped", () => {
    const { actionable, skippedInferred } = partitionPermissionAsks(tabs, states, verdicts);
    expect(actionable.map((t) => t.id)).toEqual(["hook"]);
    expect(skippedInferred.map((t) => t.id)).toEqual(["regex", "none"]);
  });

  it("names skipped panes with the reason in a bulk result", () => {
    expect(describeSkippedInferred([])).toBe("");
    const text = describeSkippedInferred([{ id: "t1", title: "api" }, { id: "t2" }]);
    expect(text).toContain("skipped 2 inferred (api, t2)");
    expect(text).toContain("not reported by a hook");
  });
});

describe("verdictToSessionState", () => {
  it("keeps unknown as unknown — never idle", () => {
    expect(verdictToSessionState(verdict({ state: { name: "unknown" }, source: null }))).toBe("unknown");
  });

  it("maps every state onto the chip vocabulary", () => {
    const cases: Array<[AgentState, string]> = [
      [{ name: "starting" }, "working"],
      [{ name: "working" }, "working"],
      [{ name: "needs_you", reason: "question" }, "needs-input"],
      [{ name: "turn_ended" }, "idle"],
      [{ name: "failed", kind: "quota_exhausted" }, "error"],
      [{ name: "ended", why: "exit" }, "completed"],
    ];
    for (const [state, chip] of cases) {
      expect(verdictToSessionState(verdict({ state }))).toBe(chip);
    }
  });
});

describe("source / confidence predicates", () => {
  it("treats hook and sideband as event-sourced, everything else not", () => {
    expect(isEventSourced(verdict({ source: "hook" }))).toBe(true);
    expect(isEventSourced(verdict({ source: "sideband" }))).toBe(true);
    for (const source of ["statusline", "transcript", "screen_stability", "regex"] as const) {
      expect(isEventSourced(verdict({ source }))).toBe(false);
    }
    expect(isEventSourced(undefined)).toBe(false);
  });

  it("is inferred unless authoritative", () => {
    expect(isInferredVerdict(undefined)).toBe(true);
    expect(isInferredVerdict(verdict({ confidence: "inferred" }))).toBe(true);
    expect(isInferredVerdict(verdict())).toBe(false);
  });
});

describe("observation offers", () => {
  it("maps inferred chip states onto observation states", () => {
    expect(sessionStateToObservation("working")).toBe("working");
    expect(sessionStateToObservation("needs-input", "approval_shaped")).toBe("approval_shaped");
    expect(sessionStateToObservation("needs-input")).toBe("question_shaped");
    expect(sessionStateToObservation("unknown")).toBeNull();
  });

  it("never throws when the runner has no such command", async () => {
    expect(() => offerAgentObservation({ terminalId: "t", source: "regex", state: "working" })).not.toThrow();
    await expect(fetchTerminalAgentStates()).resolves.toEqual([]);
  });

  it("guards the event payload shape", () => {
    expect(isTerminalAgentStateEvent({ terminalId: "t", verdict: verdict(), hookDelivery: { status: "installed" } })).toBe(true);
    expect(isTerminalAgentStateEvent({ terminalId: "t" })).toBe(false);
    expect(isTerminalAgentStateEvent(null)).toBe(false);
  });
});

describe("stateSourceText (Phase 4, SessionInfoDropdown)", () => {
  it("says hooks when a hook reported the state", () => {
    expect(stateSourceText(entry())).toBe("hooks");
  });

  it("says inferred from screen with the hook-delivery reason otherwise", () => {
    expect(
      stateSourceText(
        entry({ source: "regex", confidence: "inferred" }, { status: "shadowed", detail: "disableAllHooks" }),
      ),
    ).toBe("inferred from screen (hooks not firing: shadowed by settings: disableAllHooks)");
    expect(stateSourceText(entry({ source: "screen_stability" }, { status: "absent" }))).toBe(
      "inferred from screen (hooks not firing: hooks absent)",
    );
  });

  it("is null (rendered unknown) when the runner has reported nothing", () => {
    expect(stateSourceText(undefined)).toBeNull();
  });
});
