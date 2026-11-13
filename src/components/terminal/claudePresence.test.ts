/**
 * `probeClaudeInPanes` (the "Keep AI" button's batch process-table read) and
 * the known-Claude store it feeds. vitest runs `environment: "node"` here.
 */

import { describe, it, expect, vi, beforeEach } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

import {
  getKnownClaudePanes,
  probeClaudeInPanes,
  rememberClaudePanes,
  resetKnownClaudePanes,
} from "./claudePresence";

describe("probeClaudeInPanes", () => {
  it("asks once for every id and keeps only the four known states", async () => {
    const invoker = vi.fn(async () => ({
      data: { states: { a: "live", b: "absent", c: "remote", d: "unknown", e: "bogus", f: 3 } },
    }));
    const out = await probeClaudeInPanes(["a", "b", "c", "d", "e", "f"], invoker);
    expect(invoker).toHaveBeenCalledTimes(1);
    expect(invoker).toHaveBeenCalledWith("terminal_probe_claude_many", {
      terminalIds: ["a", "b", "c", "d", "e", "f"],
    });
    expect(out).toEqual({ a: "live", b: "absent", c: "remote", d: "unknown" });
  });

  it("an IPC failure or malformed answer is {} — every id UNKNOWN, never absent", async () => {
    expect(
      await probeClaudeInPanes(["a"], async () => {
        throw new Error("boom");
      }),
    ).toEqual({});
    expect(await probeClaudeInPanes(["a"], async () => ({ data: {} }))).toEqual({});
    expect(await probeClaudeInPanes(["a"], async () => null)).toEqual({});
  });

  it("does not call the backend for an empty list", async () => {
    const invoker = vi.fn();
    expect(await probeClaudeInPanes([], invoker)).toEqual({});
    expect(invoker).not.toHaveBeenCalled();
  });
});

describe("rememberClaudePanes", () => {
  beforeEach(() => resetKnownClaudePanes());

  it("remembers live panes only, and accumulates across probes", () => {
    rememberClaudePanes({ a: "live", b: "absent", c: "unknown", d: "remote" });
    expect(getKnownClaudePanes()).toEqual(new Set(["a"]));
    rememberClaudePanes({ e: "live" });
    expect(getKnownClaudePanes()).toEqual(new Set(["a", "e"]));
  });

  it("forgets a remembered pane a later probe reads absent, and only then", () => {
    rememberClaudePanes({ a: "live", b: "live" });
    rememberClaudePanes({ a: "absent", b: "unknown" });
    expect(getKnownClaudePanes()).toEqual(new Set(["b"]));
  });

  it("keeps the same set identity when nothing new is live", () => {
    rememberClaudePanes({ a: "live" });
    const before = getKnownClaudePanes();
    rememberClaudePanes({ a: "live", b: "absent" });
    expect(getKnownClaudePanes()).toBe(before);
  });
});
