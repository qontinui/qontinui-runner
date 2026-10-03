/**
 * Tests for the `ResumeFailedBanner` pure tab filters: which tabs surface in
 * the "resume failed — retry" list vs the "terminal-only — fresh
 * conversation" informational list. vitest runs `environment: "node"` with
 * no React Testing Library, so we exercise the exported filters directly
 * (same precedent as `useTerminalInitialization.test.ts`).
 */

import { describe, it, expect } from "vitest";
import {
  failedResumeTabs,
  resumeFailedDetail,
  terminalOnlyRestoreTabs,
} from "./ResumeFailedBanner";
import type { TerminalTab } from "./useTerminalManager";

const tab = (id: string, overrides: Partial<TerminalTab> = {}): TerminalTab => ({
  id,
  title: id,
  pid: 1234,
  isAlive: true,
  exitCode: null,
  ...overrides,
});

describe("failedResumeTabs", () => {
  it("a failed tab surfaces in the failed list only", () => {
    const tabs = [tab("t-1", { resumeFailed: true })];
    expect(failedResumeTabs(tabs).map((t) => t.id)).toEqual(["t-1"]);
  });

  it("dead tabs are dropped", () => {
    const tabs = [tab("t-1", { resumeFailed: true, isAlive: false })];
    expect(failedResumeTabs(tabs)).toEqual([]);
  });

  it("ordinary tabs (auto-resumed pinned restores, plain shells) surface in neither list", () => {
    const tabs = [tab("t-1"), tab("t-2", { claudeSessionId: "sess-pinned" })];
    expect(failedResumeTabs(tabs)).toEqual([]);
    expect(terminalOnlyRestoreTabs(tabs)).toEqual([]);
  });
});

// Phase 5 (honest capability tiers): a terminal-only restore (terminal + cwd
// back, conversation NOT resumed) surfaces in its own informational list — the
// user must SEE the conversation is fresh, never have it silently posed as
// resumed. This also covers a reconciled/backstop-guessed id: too weak to act
// on, so it lands here rather than behind a best-effort confirm. It is the
// lowest-priority section: an actionable failed state on the SAME tab wins so
// a tab appears in exactly one section.
describe("terminalOnlyRestoreTabs (Phase 5 honest tiers)", () => {
  it("a terminal-only tab surfaces in the terminal-only list only", () => {
    const tabs = [tab("t-1", { restoreTerminalOnly: true, claudeSessionId: "sess-1" })];
    expect(terminalOnlyRestoreTabs(tabs).map((t) => t.id)).toEqual(["t-1"]);
    expect(failedResumeTabs(tabs)).toEqual([]);
  });

  it("an actionable failed state on the same tab takes precedence over the note", () => {
    const tabs = [tab("t-1", { restoreTerminalOnly: true, resumeFailed: true })];
    expect(failedResumeTabs(tabs).map((t) => t.id)).toEqual(["t-1"]);
    expect(terminalOnlyRestoreTabs(tabs)).toEqual([]);
  });

  it("dead terminal-only tabs are dropped", () => {
    const tabs = [tab("t-1", { restoreTerminalOnly: true, isAlive: false })];
    expect(terminalOnlyRestoreTabs(tabs)).toEqual([]);
  });
});

// A `failed` resume where nothing was typed (qontinui-runner#1867's probe)
// must not read as "the --resume was typed and never hand-shook".
describe("resumeFailedDetail", () => {
  it("explains a not-typed failure, and adds nothing for a typed one", () => {
    expect(resumeFailedDetail("pane-occupied")).toMatch(/already running.*nothing was typed/i);
    expect(resumeFailedDetail("pane-unreadable")).toMatch(
      /could not be checked.*nothing was typed/i,
    );
    expect(resumeFailedDetail(undefined)).toBeNull();
  });
});
