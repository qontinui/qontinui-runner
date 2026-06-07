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
import type { TerminalInfo } from "@qontinui/shared-types/tauri-events";
import {
  applyWorkerMark,
  reduceCreatedTerminal,
  shouldIngestCreatedTerminal,
  type TerminalTab,
} from "./useTerminalManager";

const tab = (id: string, overrides: Partial<TerminalTab> = {}): TerminalTab => ({
  id,
  title: id,
  pid: 1,
  isAlive: true,
  exitCode: null,
  ...overrides,
});

// Minimal `terminal-created` payload. Only the fields the ingest path reads
// matter; the rest satisfy the generated `TerminalInfo` shape.
const createdEvent = (
  id: string,
  pageId: string,
  overrides: Partial<TerminalInfo> = {},
): TerminalInfo => ({
  id,
  title: id,
  pid: 100,
  isAlive: true,
  exitCode: null,
  workingDir: "/repo",
  createdAt: 1_700_000_000_000,
  pageId,
  cols: 80,
  rows: 24,
  totalBytesProduced: 0,
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

// Phase 3 (mount-hydration lift): the session provider is lifted above the
// page and every page's `useTerminalManager` runs simultaneously. Each page's
// `terminal-created` listener must claim ONLY the terminals tagged with its own
// pageId, and dedup by id — so a `terminal-created` event arriving while the
// operator is on another page lands in the right page's slice and is never
// dropped.
describe("shouldIngestCreatedTerminal (page routing)", () => {
  it("claims an event whose pageId matches this manager's page", () => {
    expect(shouldIngestCreatedTerminal("page-a", "page-a")).toBe(true);
  });

  it("ignores an event destined for a different page", () => {
    expect(shouldIngestCreatedTerminal("page-b", "page-a")).toBe(false);
  });

  it("treats a missing/empty pageId as the default page", () => {
    expect(shouldIngestCreatedTerminal(undefined, "default")).toBe(true);
    expect(shouldIngestCreatedTerminal(null, "default")).toBe(true);
    expect(shouldIngestCreatedTerminal("", "default")).toBe(true);
    expect(shouldIngestCreatedTerminal(undefined, "page-a")).toBe(false);
  });
});

describe("reduceCreatedTerminal (ingest + dedup)", () => {
  it("captures a terminal-created payload into the page slice", () => {
    const next = reduceCreatedTerminal([], createdEvent("t1", "page-a"), undefined);
    expect(next).toHaveLength(1);
    expect(next[0]).toMatchObject({ id: "t1", title: "t1", isAlive: true, workingDir: "/repo" });
  });

  it("appends without clearing the existing page state", () => {
    const existing = [tab("t1")];
    const next = reduceCreatedTerminal(existing, createdEvent("t2", "page-a"), undefined);
    expect(next).toHaveLength(2);
    expect(next.map((t) => t.id)).toEqual(["t1", "t2"]);
    // Original array is not mutated (the t1 tab object survives untouched).
    expect(next[0]).toBe(existing[0]);
  });

  it("dedups by id: a re-delivered event returns the same array identity", () => {
    const existing = [tab("t1")];
    const next = reduceCreatedTerminal(existing, createdEvent("t1", "page-a"), undefined);
    expect(next).toBe(existing);
  });

  it("stamps the drained worker mark onto the new tab", () => {
    const next = reduceCreatedTerminal([], createdEvent("w1", "page-a"), "task-42");
    expect(next[0].taskRunId).toBe("task-42");
  });

  it("end-to-end: an event for page B while page A is active lands in B's slice only", () => {
    // Two independent page slices (provider lifted ⇒ both mounted at once).
    let pageA: TerminalTab[] = [tab("a1")];
    let pageB: TerminalTab[] = [];

    const deliver = (info: TerminalInfo) => {
      // page A's listener
      if (shouldIngestCreatedTerminal(info.pageId, "page-a")) {
        pageA = reduceCreatedTerminal(pageA, info, undefined);
      }
      // page B's listener
      if (shouldIngestCreatedTerminal(info.pageId, "page-b")) {
        pageB = reduceCreatedTerminal(pageB, info, undefined);
      }
    };

    // An externally-created terminal for page B arrives while page A is active.
    deliver(createdEvent("b1", "page-b"));

    expect(pageB.map((t) => t.id)).toEqual(["b1"]); // captured in B
    expect(pageA.map((t) => t.id)).toEqual(["a1"]); // A unchanged, not cleared
  });
});
