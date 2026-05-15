/**
 * Tests for `useTerminalManager`'s worker-marker decision logic (Phase 2
 * of the worker-tab title suppression plan; Phase 1 backend gate ships in
 * `terminal/manager.rs::set_title_unless_worker`).
 *
 * The runner's vitest config is `environment: "node"` and React Testing
 * Library is not in scope (see `useCommitState.test.ts` /
 * `useFileLockTracking.test.ts` for the precedent). Rather than render
 * the hook, this file targets the extracted pure helper `applyWorkerMark`
 * — the only place where race-safety + idempotency live. The hook's
 * `markAsWorker` callback is a thin `setTabs(prev => applyWorkerMark(prev, …))`
 * wrapper whose side effect (the `pendingWorkerMarks` ref write) is the
 * literal `buffered === true` branch covered below.
 */

import { describe, it, expect } from "vitest";
import { applyWorkerMark, type TerminalTab } from "./useTerminalManager";

const tab = (id: string, overrides: Partial<TerminalTab> = {}): TerminalTab => ({
  id,
  title: id,
  pid: 1,
  isAlive: true,
  exitCode: null,
  ...overrides,
});

describe("applyWorkerMark", () => {
  it("buffers the mark when the tab record hasn't arrived yet", () => {
    const tabs = [tab("a"), tab("b")];
    const result = applyWorkerMark(tabs, "ghost", "task-1");
    expect(result.buffered).toBe(true);
    expect(result.tabs).toBe(tabs);
  });

  it("stamps taskRunId onto the matching tab and returns a fresh array", () => {
    const tabs = [tab("a"), tab("worker-tab")];
    const result = applyWorkerMark(tabs, "worker-tab", "task-1");
    expect(result.buffered).toBe(false);
    expect(result.tabs).not.toBe(tabs);
    expect(result.tabs[1].taskRunId).toBe("task-1");
    expect(result.tabs[0].taskRunId).toBeUndefined();
  });

  it("is idempotent: re-marking with the same taskRunId returns the same array identity", () => {
    const tabs = [tab("worker-tab", { taskRunId: "task-1" })];
    const result = applyWorkerMark(tabs, "worker-tab", "task-1");
    expect(result.buffered).toBe(false);
    expect(result.tabs).toBe(tabs);
  });

  it("does not mutate the input array", () => {
    const tabs = [tab("worker-tab")];
    applyWorkerMark(tabs, "worker-tab", "task-1");
    expect(tabs[0].taskRunId).toBeUndefined();
  });

  it("overwrites a stale taskRunId when a different one arrives", () => {
    const tabs = [tab("worker-tab", { taskRunId: "task-old" })];
    const result = applyWorkerMark(tabs, "worker-tab", "task-new");
    expect(result.buffered).toBe(false);
    expect(result.tabs[0].taskRunId).toBe("task-new");
  });
});
