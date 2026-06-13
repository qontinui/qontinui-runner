/**
 * Tests for the `ResumeFailedBanner` pure tab filters: which tabs surface in
 * the "resume failed — retry" list vs the item-5 quarantine ("binding
 * unverified — confirm resume") list. vitest runs `environment: "node"` with
 * no React Testing Library, so we exercise the exported filters directly
 * (same precedent as `useTerminalInitialization.test.ts`).
 */

import { describe, it, expect } from "vitest";
import { failedResumeTabs, quarantinedResumeTabs } from "./ResumeFailedBanner";
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
  });
});
