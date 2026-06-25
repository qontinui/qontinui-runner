/**
 * Tests for the `ResumeFailedBanner` pure tab filters: which tabs surface in
 * the "resume failed — retry" list vs the item-5 quarantine ("binding
 * unverified — confirm resume") list. vitest runs `environment: "node"` with
 * no React Testing Library, so we exercise the exported filters directly
 * (same precedent as `useTerminalInitialization.test.ts`).
 */

import { describe, it, expect } from "vitest";
import {
  failedResumeTabs,
  quarantinedResumeTabs,
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

describe("failedResumeTabs / quarantinedResumeTabs", () => {
  it("a quarantined (guessed-binding) tab surfaces in the confirm list, not the failed list", () => {
    const tabs = [tab("t-1", { resumeQuarantined: true, claudeSessionId: "sess-guessed" })];
    expect(quarantinedResumeTabs(tabs).map((t) => t.id)).toEqual(["t-1"]);
    expect(failedResumeTabs(tabs)).toEqual([]);
  });

  it("a failed tab surfaces in the failed list only", () => {
    const tabs = [tab("t-1", { resumeFailed: true })];
    expect(failedResumeTabs(tabs).map((t) => t.id)).toEqual(["t-1"]);
    expect(quarantinedResumeTabs(tabs)).toEqual([]);
  });

  it("a confirmed-then-failed tab moves to the failed list — never listed twice", () => {
    // Operator confirmed the quarantined resume; verification then failed.
    // (`handleRetryResume` clears the quarantine flag, but even if a stale
    // flag survived, failed wins.)
    const tabs = [tab("t-1", { resumeQuarantined: true, resumeFailed: true })];
    expect(failedResumeTabs(tabs).map((t) => t.id)).toEqual(["t-1"]);
    expect(quarantinedResumeTabs(tabs)).toEqual([]);
  });

  it("dead tabs are dropped from both lists", () => {
    const tabs = [
      tab("t-1", { resumeQuarantined: true, isAlive: false }),
      tab("t-2", { resumeFailed: true, isAlive: false }),
    ];
    expect(quarantinedResumeTabs(tabs)).toEqual([]);
    expect(failedResumeTabs(tabs)).toEqual([]);
  });

  it("ordinary tabs (auto-resumed pinned restores, plain shells) surface in neither", () => {
    const tabs = [tab("t-1"), tab("t-2", { claudeSessionId: "sess-pinned" })];
    expect(quarantinedResumeTabs(tabs)).toEqual([]);
    expect(failedResumeTabs(tabs)).toEqual([]);
    expect(terminalOnlyRestoreTabs(tabs)).toEqual([]);
  });
});

// Phase 5 (honest capability tiers): a terminal-only restore (terminal + cwd
// back, conversation NOT resumed) surfaces in its own informational list — the
// user must SEE the conversation is fresh, never have it silently posed as
// resumed. It is the lowest-priority section: an actionable failed/quarantine
// state on the SAME tab wins so a tab appears in exactly one section.
describe("terminalOnlyRestoreTabs (Phase 5 honest tiers)", () => {
  it("a terminal-only tab surfaces in the terminal-only list only", () => {
    const tabs = [tab("t-1", { restoreTerminalOnly: true, claudeSessionId: "sess-1" })];
    expect(terminalOnlyRestoreTabs(tabs).map((t) => t.id)).toEqual(["t-1"]);
    expect(failedResumeTabs(tabs)).toEqual([]);
    expect(quarantinedResumeTabs(tabs)).toEqual([]);
  });

  it("an actionable failed state on the same tab takes precedence over the note", () => {
    const tabs = [tab("t-1", { restoreTerminalOnly: true, resumeFailed: true })];
    expect(failedResumeTabs(tabs).map((t) => t.id)).toEqual(["t-1"]);
    expect(terminalOnlyRestoreTabs(tabs)).toEqual([]);
  });

  it("an actionable quarantine state on the same tab takes precedence over the note", () => {
    const tabs = [tab("t-1", { restoreTerminalOnly: true, resumeQuarantined: true })];
    expect(quarantinedResumeTabs(tabs).map((t) => t.id)).toEqual(["t-1"]);
    expect(terminalOnlyRestoreTabs(tabs)).toEqual([]);
  });

  it("dead terminal-only tabs are dropped", () => {
    const tabs = [tab("t-1", { restoreTerminalOnly: true, isAlive: false })];
    expect(terminalOnlyRestoreTabs(tabs)).toEqual([]);
  });
});
