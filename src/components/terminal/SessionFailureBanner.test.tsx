/**
 * Tests for the session-failure surface (plan
 * `2026-09-20-ai-session-handling-is-claude-shaped`, Phase 7): the notice
 * reducer, the banner's rows and actions, the resume verifier's reporting
 * rule, and the terminal-only restore note split out of the former
 * `ResumeFailedBanner`. vitest runs `environment: "node"` with no React
 * Testing Library, so the exported pure pieces are exercised directly (same
 * precedent as `HoldingLockBanner.test.tsx`).
 */

import { describe, it, expect } from "vitest";
import { actionIsClickable, suggestedActions } from "./SessionFailureBanner";
import { terminalOnlyRestoreTabs } from "./RestoreTerminalOnlyNote";
import {
  applyFailureNotice,
  failureBannerEntries,
  forgetClosedTabs,
  isHint,
  resumeReports,
  retainLiveTabFailures,
  setTerminalFailures,
  type FailuresByTerminal,
  type ServedSessionFailure,
} from "./sessionFailures";
import type { TerminalTab } from "./useTerminalManager";

const tab = (id: string, overrides: Partial<TerminalTab> = {}): TerminalTab => ({
  id,
  title: id,
  pid: 1234,
  isAlive: true,
  exitCode: null,
  ...overrides,
});

/** A payload shaped as the runner serializes `SessionFailure` (camelCase). */
const failure = (overrides: Partial<ServedSessionFailure> = {}): ServedSessionFailure => ({
  id: "f-1",
  kind: "resume_failed",
  category: "session",
  severity: "error",
  title: "Session resume may have failed",
  details: "The resumed conversation's UI never appeared. The runner takes no automatic action.",
  reason: "resume handshake never appeared",
  provider: "claude",
  evidence: { source: "handshake_timeout", confidence: "hint" },
  actions: ["resume", "new_session"],
  recoveryPolicy: "never",
  ...overrides,
});

describe("applyFailureNotice", () => {
  it("adds an active failure, replaces the same kind in place, and removes on clear", () => {
    let state: FailuresByTerminal = {};
    state = applyFailureNotice(state, { terminalId: "t-1", failure: failure(), active: true });
    expect(state["t-1"]).toHaveLength(1);

    // A repeat of the same kind (the runner keeps its id) replaces, never stacks.
    const confirmed = failure({
      title: "Session resume failed",
      evidence: { source: "handshake_timeout", confidence: "confirmed" },
    });
    state = applyFailureNotice(state, { terminalId: "t-1", failure: confirmed, active: true });
    expect(state["t-1"]).toEqual([confirmed]);

    // A different kind stacks beside it.
    const quota = failure({ id: "f-2", kind: "quota_exhausted", severity: "error" });
    state = applyFailureNotice(state, { terminalId: "t-1", failure: quota, active: true });
    expect(state["t-1"]?.map((f) => f.kind)).toEqual(["resume_failed", "quota_exhausted"]);

    state = applyFailureNotice(state, { terminalId: "t-1", failure: confirmed, active: false });
    expect(state["t-1"]?.map((f) => f.kind)).toEqual(["quota_exhausted"]);
    state = applyFailureNotice(state, { terminalId: "t-1", failure: quota, active: false });
    expect(state["t-1"]).toBeUndefined();
  });

  it("a clear for a failure that is not there changes nothing", () => {
    const state: FailuresByTerminal = {};
    expect(
      applyFailureNotice(state, { terminalId: "t-1", failure: failure(), active: false }),
    ).toBe(state);
  });

  it("the initial read replaces a terminal's list, and an empty read drops it", () => {
    let state = setTerminalFailures({}, "t-1", [failure()]);
    expect(state["t-1"]).toHaveLength(1);
    state = setTerminalFailures(state, "t-1", []);
    expect(state["t-1"]).toBeUndefined();
  });
});

describe("failureBannerEntries", () => {
  it("shows every tab's failures, errors before warnings, and nothing for tabs not on this page", () => {
    const tabs = [tab("t-1"), tab("t-2")];
    const warning = failure({ id: "w", kind: "rate_limited", severity: "warning" });
    const error = failure({ id: "e", kind: "process_exited", severity: "error" });
    const rows = failureBannerEntries(tabs, {
      "t-1": [warning],
      "t-2": [error],
      "t-elsewhere": [failure({ id: "x" })],
    });
    expect(rows.map((r) => `${r.tab.id}:${r.failure.id}`)).toEqual(["t-2:e", "t-1:w"]);
  });

  it("an exited tab still shows its failure — exiting is what it is about", () => {
    const rows = failureBannerEntries([tab("t-1", { isAlive: false })], {
      "t-1": [failure({ kind: "process_exited" })],
    });
    expect(rows).toHaveLength(1);
  });

  it("no failures, no rows (the banner renders nothing)", () => {
    expect(failureBannerEntries([tab("t-1")], {})).toEqual([]);
  });
});

describe("hint wording and actions", () => {
  it("a hint is flagged, and its runner-worded title says 'may'", () => {
    const f = failure();
    expect(isHint(f)).toBe(true);
    expect(f.title.toLowerCase()).toContain("may");
    expect(isHint(failure({ evidence: { source: "hook", confidence: "confirmed" } }))).toBe(false);
  });

  it("resume is one click only on a live tab with a session id and no retry in flight", () => {
    const f = failure();
    const live = { tab: tab("t-1", { claudeSessionId: "s-1" }), failure: f };
    expect(actionIsClickable("resume", live)).toBe(true);
    expect(suggestedActions(live)).toEqual(["new_session"]);

    const dead = { tab: tab("t-1", { claudeSessionId: "s-1", isAlive: false }), failure: f };
    expect(actionIsClickable("resume", dead)).toBe(false);
    expect(suggestedActions(dead)).toEqual(["resume", "new_session"]);

    const retrying = {
      tab: tab("t-1", { claudeSessionId: "s-1", isReconnecting: true }),
      failure: f,
    };
    expect(actionIsClickable("resume", retrying)).toBe(false);

    const noSession = { tab: tab("t-1"), failure: f };
    expect(actionIsClickable("resume", noSession)).toBe(false);
  });

  it("actions this page cannot perform are named, and `none` is never shown", () => {
    const row = {
      tab: tab("t-1"),
      failure: failure({ kind: "auth_required", actions: ["login"] }),
    };
    expect(suggestedActions(row)).toEqual(["login"]);
    const unknown = { tab: tab("t-1"), failure: failure({ kind: "unknown", actions: ["none"] }) };
    expect(suggestedActions(unknown)).toEqual([]);
  });
});

// The resume verifier stays the webview's (it watches the pane), but its
// outcome goes to the runner so a failed resume is one `SessionFailure` among
// the rest — resume_failed is one consumer of the shared surface.
describe("resumeReports", () => {
  it("a newly failed resume is reported once", () => {
    const tabs = [tab("t-1", { resumeFailed: true })];
    expect(resumeReports(tabs, new Set())).toEqual({ failed: ["t-1"], verified: [] });
    expect(resumeReports(tabs, new Set(["t-1"]))).toEqual({ failed: [], verified: [] });
  });

  it("a retry in flight is not evidence; the verified retry is", () => {
    const reported = new Set(["t-1"]);
    const retrying = [tab("t-1", { resumeFailed: false, isReconnecting: true })];
    expect(resumeReports(retrying, reported)).toEqual({ failed: [], verified: [] });
    const verified = [tab("t-1", { resumeFailed: false, isReconnecting: false })];
    expect(resumeReports(verified, reported)).toEqual({ failed: [], verified: ["t-1"] });
  });

  it("tabs that never failed report nothing", () => {
    expect(resumeReports([tab("t-1"), tab("t-2", { isReconnecting: true })], new Set())).toEqual({
      failed: [],
      verified: [],
    });
  });
});

describe("closed-tab pruning", () => {
  it("forgets a closed tab's bookkeeping and keeps a live one's", () => {
    const tracked = new Set(["t-1", "t-gone"]);
    expect(forgetClosedTabs(tracked, [tab("t-1"), tab("t-2")])).toEqual(["t-gone"]);
    expect([...tracked]).toEqual(["t-1"]);
  });

  it("drops a closed tab's failures and returns the same object when nothing closed", () => {
    const byTerminal = { "t-1": [failure()], "t-gone": [failure({ id: "f-2" })] };
    expect(retainLiveTabFailures(byTerminal, [tab("t-1")])).toEqual({ "t-1": [failure()] });
    const live = { "t-1": [failure()] };
    expect(retainLiveTabFailures(live, [tab("t-1")])).toBe(live);
  });
});

// Phase 5 (honest capability tiers), carried over unchanged from the former
// `ResumeFailedBanner`: a terminal-only restore (terminal + cwd back,
// conversation NOT resumed) surfaces in its own informational note. A tab
// whose resume failed is left to the failure banner, so one tab appears in one
// place.
describe("terminalOnlyRestoreTabs (Phase 5 honest tiers)", () => {
  it("a terminal-only tab surfaces in the note", () => {
    const tabs = [tab("t-1", { restoreTerminalOnly: true, claudeSessionId: "sess-1" })];
    expect(terminalOnlyRestoreTabs(tabs).map((t) => t.id)).toEqual(["t-1"]);
  });

  it("an actionable failed resume on the same tab takes precedence over the note", () => {
    const tabs = [tab("t-1", { restoreTerminalOnly: true, resumeFailed: true })];
    expect(terminalOnlyRestoreTabs(tabs)).toEqual([]);
  });

  it("dead terminal-only tabs are dropped", () => {
    const tabs = [tab("t-1", { restoreTerminalOnly: true, isAlive: false })];
    expect(terminalOnlyRestoreTabs(tabs)).toEqual([]);
  });

  it("ordinary tabs (auto-resumed pinned restores, plain shells) are not in the note", () => {
    const tabs = [tab("t-1"), tab("t-2", { claudeSessionId: "sess-pinned" })];
    expect(terminalOnlyRestoreTabs(tabs)).toEqual([]);
  });
});
