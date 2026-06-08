/**
 * Tests for the Phase 5 session-durability classifier
 * (`2026-06-06-runner-dev-loop-and-restart-resilience` honesty label).
 *
 * Same constraint as the other terminal-hook tests: the runner's vitest env
 * is `node` (no jsdom), so we exercise the exported pure helpers directly —
 * no React render.
 */

import { describe, it, expect } from "vitest";
import {
  classifyTabDurability,
  isMarkableSession,
  isNonDurablePty,
  NON_DURABLE_TOOLTIP,
  NON_DURABLE_LABEL,
  DURABLE_TOOLTIP,
} from "./sessionDurability";
import type { TerminalTab } from "./useTerminalManager";

/** Build a minimal TerminalTab fixture. */
function tab(overrides: Partial<TerminalTab> = {}): TerminalTab {
  return {
    id: "t1",
    title: "shell",
    pid: 123,
    isAlive: true,
    exitCode: null,
    type: "terminal",
    ...overrides,
  };
}

describe("classifyTabDurability", () => {
  it("marks a plain interactive PTY shell (no claude session / task run) as non-durable", () => {
    expect(classifyTabDurability(tab())).toBe("non-durable");
  });

  it("marks a tab with a captured claudeSessionId as durable", () => {
    expect(classifyTabDurability(tab({ claudeSessionId: "sess-abc" }))).toBe("durable");
  });

  it("marks a worker-backed tab (taskRunId, no claude id yet) as durable", () => {
    expect(classifyTabDurability(tab({ taskRunId: "run-xyz" }))).toBe("durable");
  });

  it("treats empty-string ids as absent → non-durable", () => {
    expect(classifyTabDurability(tab({ claudeSessionId: "", taskRunId: "" }))).toBe("non-durable");
  });
});

describe("isMarkableSession", () => {
  it("is true for a real terminal tab", () => {
    expect(isMarkableSession(tab())).toBe(true);
  });

  it("is false for a plan (markdown viewer) tab — not a session", () => {
    expect(isMarkableSession(tab({ type: "plan" }))).toBe(false);
  });

  it("treats an undefined type as a terminal (markable)", () => {
    expect(isMarkableSession(tab({ type: undefined }))).toBe(true);
  });
});

describe("isNonDurablePty", () => {
  it("is true ONLY for a markable, non-durable interactive PTY", () => {
    expect(isNonDurablePty(tab())).toBe(true);
  });

  it("is false for a durable AI session", () => {
    expect(isNonDurablePty(tab({ claudeSessionId: "sess-abc" }))).toBe(false);
  });

  it("is false for a plan tab even though it has no claude session id", () => {
    expect(isNonDurablePty(tab({ type: "plan" }))).toBe(false);
  });
});

describe("copy", () => {
  it("non-durable tooltip is honest and ties to the recovery summary", () => {
    expect(NON_DURABLE_TOOLTIP).toMatch(/not saved/i);
    expect(NON_DURABLE_TOOLTIP).toMatch(/restart/i);
    expect(NON_DURABLE_TOOLTIP).toMatch(/recovery summary/i);
    expect(NON_DURABLE_LABEL).toBe("ephemeral");
  });

  it("durable tooltip explains automatic resume", () => {
    expect(DURABLE_TOOLTIP).toMatch(/resumes automatically/i);
  });
});
