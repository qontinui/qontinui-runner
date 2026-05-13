/**
 * Tests for the terminal-sessions IPC handler.
 *
 * The vitest config uses `environment: "node"` (no jsdom, no
 * `@testing-library/react`), so we exercise the load-bearing pieces
 * by:
 *   1. Driving the module-level registry directly and asserting on
 *      `getTerminalSessions` / `getTerminalSession`.
 *   2. Calling the pure `projectTerminalSessionEntry` projector and
 *      asserting on the snake_case wire shape (the Rust side reads
 *      `task_run_id`, not `taskRunId`).
 *
 * This matches the precedent in `LaunchMenu.test.tsx` — test the
 * pure helpers, not the JSX wiring.
 */

import { describe, it, expect, beforeEach } from "vitest";
import {
  setTerminalSessions,
  getTerminalSessions,
  getTerminalSession,
  __resetTerminalSessionsForTest,
  type TerminalSessionEntry,
} from "@/lib/terminal-sessions-registry";
import { projectTerminalSessionEntry } from "./useTerminalsEvents";

function makeEntry(overrides: Partial<TerminalSessionEntry> = {}): TerminalSessionEntry {
  return {
    id: "tab-1",
    title: "Terminal 1",
    taskRunId: null,
    claudeSessionId: null,
    workingDir: "C:/work",
    state: "idle",
    isAlive: true,
    exitCode: null,
    type: "terminal",
    createdAt: 1700000000000,
    ...overrides,
  };
}

describe("terminal-sessions-registry", () => {
  beforeEach(() => {
    __resetTerminalSessionsForTest();
  });

  it("starts empty so a poll during runner startup returns []", () => {
    expect(getTerminalSessions()).toEqual([]);
    expect(getTerminalSession("nope")).toBeUndefined();
  });

  it("setTerminalSessions stores the snapshot in order", () => {
    const entries = [makeEntry({ id: "a" }), makeEntry({ id: "b" })];
    setTerminalSessions(entries);
    expect(getTerminalSessions().map((e) => e.id)).toEqual(["a", "b"]);
  });

  it("getTerminalSession returns the entry by id or undefined", () => {
    setTerminalSessions([
      makeEntry({ id: "tab-1" }),
      makeEntry({ id: "tab-2", title: "Terminal 2" }),
    ]);
    expect(getTerminalSession("tab-2")?.title).toBe("Terminal 2");
    expect(getTerminalSession("missing")).toBeUndefined();
  });

  it("publishing a fresh snapshot replaces the prior one", () => {
    setTerminalSessions([makeEntry({ id: "a" })]);
    setTerminalSessions([makeEntry({ id: "b" })]);
    const ids = getTerminalSessions().map((e) => e.id);
    expect(ids).toEqual(["b"]);
  });
});

describe("projectTerminalSessionEntry", () => {
  it("renames every camelCase field to snake_case for the wire", () => {
    const entry = makeEntry({
      id: "tab-x",
      title: "claude",
      taskRunId: "task-abc",
      claudeSessionId: "task-abc",
      workingDir: "D:/repo",
      state: "working",
      isAlive: true,
      exitCode: null,
      createdAt: 1234567890,
    });
    expect(projectTerminalSessionEntry(entry)).toEqual({
      id: "tab-x",
      title: "claude",
      task_run_id: "task-abc",
      claude_session_id: "task-abc",
      working_dir: "D:/repo",
      state: "working",
      is_alive: true,
      exit_code: null,
      type: "terminal",
      created_at: 1234567890,
    });
  });

  it("preserves null task_run_id / claude_session_id for fresh AI tabs", () => {
    // Pre-JSONL-capture window: the tab exists but its Claude session id
    // hasn't been observed yet. Callers detect this and poll until it
    // flips non-null.
    const projected = projectTerminalSessionEntry(makeEntry());
    expect(projected.task_run_id).toBeNull();
    expect(projected.claude_session_id).toBeNull();
  });

  it("passes through exit_code for dead tabs so callers can see the failure", () => {
    const projected = projectTerminalSessionEntry(
      makeEntry({ isAlive: false, exitCode: 137 }),
    );
    expect(projected.is_alive).toBe(false);
    expect(projected.exit_code).toBe(137);
  });

  it("returns the plan tab type literally so callers can filter", () => {
    expect(
      projectTerminalSessionEntry(
        makeEntry({ type: "plan", state: "idle", isAlive: true }),
      ).type,
    ).toBe("plan");
  });
});
