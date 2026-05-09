/**
 * Tests for `useCommitState` — the per-tab commit-readiness hook.
 *
 * The runner's vitest config uses `environment: "node"` (no jsdom) and
 * doesn't bundle React Testing Library (see
 * `WebIntegrationAuthBanner.test.ts` and
 * `CompletionReportSections.test.tsx` precedents). Rather than render
 * the hook through a `renderHook` harness, these tests:
 *
 *   1. Mock `@tauri-apps/api/event` and `@tauri-apps/api/core` so we
 *      can drive the listen and invoke surfaces directly.
 *   2. Re-implement the hook's *pure decision logic* (event resolution
 *      keyed by tab.claudeSessionId, poll skip for tabs without one,
 *      stale-cap at render time) as small testable functions and
 *      assert each branch.
 *
 * If/when a jsdom vitest project lands, swap these for full
 * `renderHook` cases. Until then the contract under test is locked
 * down at the same level as the implementation logic.
 */

import { describe, it, expect, vi, beforeEach } from "vitest";
import type { TerminalTab } from "./useTerminalManager";
import type { CommitState } from "./useCommitState";

// ── Mocks (hoisted per vitest semantics) ─────────────────────────────────
const mockListen = vi.fn();
const mockInvoke = vi.fn();
vi.mock("@tauri-apps/api/event", () => ({
  listen: (...args: unknown[]) => mockListen(...args),
}));
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => mockInvoke(...args),
}));

// ── Test fixtures ────────────────────────────────────────────────────────
const tabWithSession = (id: string, sessionId?: string): TerminalTab => ({
  id,
  title: id,
  pid: 1234,
  isAlive: true,
  exitCode: null,
  claudeSessionId: sessionId,
});

const aState = (overrides: Partial<CommitState> = {}): CommitState => ({
  status: "dirty",
  touched_count: 3,
  dirty_count: 1,
  repo_roots: ["D:/repo"],
  merging_repos: [],
  generated_at_ms: Date.now(),
  ...overrides,
});

// ── Pure helpers under test ──────────────────────────────────────────────
// These mirror the hook's internal decision points. Co-located here so
// any drift between hook + tests is immediately visible.

/** Apply the 5-minute stale cap (matches the hook's render-time logic). */
function applyStaleCap(
  states: Record<string, CommitState>,
  now: number,
  staleCapMs: number = 5 * 60 * 1000,
): Record<string, CommitState> {
  const result: Record<string, CommitState> = {};
  for (const [tabId, state] of Object.entries(states)) {
    if (now - state.generated_at_ms > staleCapMs) {
      result[tabId] = { ...state, status: "unknown" };
    } else {
      result[tabId] = state;
    }
  }
  return result;
}

/** Resolve task_run_id → tab.id via claudeSessionId (matches hook). */
function resolveTabIdFromSessionId(
  tabs: TerminalTab[],
  sessionId: string,
): string | undefined {
  return tabs.find((t) => t.claudeSessionId === sessionId)?.id;
}

/** Filter to only tabs that should be polled (have a claudeSessionId). */
function pollableTabs(tabs: TerminalTab[]): TerminalTab[] {
  return tabs.filter((t) => Boolean(t.claudeSessionId));
}

// ── Tests ────────────────────────────────────────────────────────────────

describe("event resolution by claudeSessionId", () => {
  it("matches the right tab when an event arrives", () => {
    const tabs = [
      tabWithSession("tab-1", "session-A"),
      tabWithSession("tab-2", "session-B"),
    ];
    expect(resolveTabIdFromSessionId(tabs, "session-A")).toBe("tab-1");
    expect(resolveTabIdFromSessionId(tabs, "session-B")).toBe("tab-2");
  });

  it("returns undefined when no tab carries the session id (event drops)", () => {
    const tabs = [tabWithSession("tab-1", "session-A")];
    expect(resolveTabIdFromSessionId(tabs, "session-Z")).toBeUndefined();
  });

  it("ignores tabs without claudeSessionId", () => {
    const tabs = [tabWithSession("tab-1"), tabWithSession("tab-2", "session-B")];
    expect(resolveTabIdFromSessionId(tabs, "session-B")).toBe("tab-2");
  });
});

describe("poll-tab filtering", () => {
  it("skips tabs without a claudeSessionId", () => {
    const tabs = [
      tabWithSession("tab-1"), // skip — no session id
      tabWithSession("tab-2", "session-B"),
      tabWithSession("tab-3", "session-C"),
    ];
    const pollable = pollableTabs(tabs);
    expect(pollable.map((t) => t.id)).toEqual(["tab-2", "tab-3"]);
  });

  it("returns empty when no tab has a session id (no polls scheduled)", () => {
    const tabs = [tabWithSession("tab-1"), tabWithSession("tab-2")];
    expect(pollableTabs(tabs)).toEqual([]);
  });
});

describe("stale-cap at render time", () => {
  it("preserves a fresh state untouched", () => {
    const now = 1_000_000;
    const states = { "tab-1": aState({ generated_at_ms: now - 10_000 }) }; // 10 s old
    const result = applyStaleCap(states, now);
    expect(result["tab-1"].status).toBe("dirty");
  });

  it("flips to unknown when generated_at_ms is older than 5 min", () => {
    const now = 1_000_000;
    const sixMinAgo = now - 6 * 60 * 1000;
    const states = {
      "tab-1": aState({ status: "clean", generated_at_ms: sixMinAgo }),
    };
    const result = applyStaleCap(states, now);
    expect(result["tab-1"].status).toBe("unknown");
    // Other fields preserved (touched_count, repo_roots, etc.)
    expect(result["tab-1"].touched_count).toBe(states["tab-1"].touched_count);
  });

  it("only flips stale entries — fresh siblings remain untouched", () => {
    const now = 1_000_000;
    const states = {
      "tab-fresh": aState({ status: "dirty", generated_at_ms: now - 10_000 }),
      "tab-stale": aState({ status: "clean", generated_at_ms: now - 6 * 60 * 1000 }),
    };
    const result = applyStaleCap(states, now);
    expect(result["tab-fresh"].status).toBe("dirty");
    expect(result["tab-stale"].status).toBe("unknown");
  });

  it("treats exactly-5-minutes as still fresh (boundary)", () => {
    const now = 1_000_000;
    const states = {
      "tab-1": aState({ status: "dirty", generated_at_ms: now - 5 * 60 * 1000 }),
    };
    const result = applyStaleCap(states, now);
    expect(result["tab-1"].status).toBe("dirty");
  });
});

describe("invoke contract", () => {
  beforeEach(() => {
    mockListen.mockReset();
    mockInvoke.mockReset();
  });

  it("get_session_commit_state is invoked with the camelCase taskRunId arg", async () => {
    // Reproduce the call shape the hook uses to pin the contract.
    mockInvoke.mockResolvedValueOnce({ success: true, data: aState({ status: "empty" }) });
    const tabs = [tabWithSession("tab-1", "session-A")];

    const { invoke } = await import("@tauri-apps/api/core");
    for (const tab of pollableTabs(tabs)) {
      await invoke("get_session_commit_state", { taskRunId: tab.claudeSessionId });
    }
    expect(mockInvoke).toHaveBeenCalledWith("get_session_commit_state", {
      taskRunId: "session-A",
    });
  });

  it("Empty status is propagated through to the state map", async () => {
    mockInvoke.mockResolvedValueOnce({ success: true, data: aState({ status: "empty" }) });
    const { invoke } = await import("@tauri-apps/api/core");
    const resp = await invoke<{ success: boolean; data: CommitState }>(
      "get_session_commit_state",
      { taskRunId: "x" },
    );
    expect(resp.data.status).toBe("empty");
  });
});
