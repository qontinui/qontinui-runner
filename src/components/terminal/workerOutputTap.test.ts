/**
 * Tests for the Conductor-worker tracking feed (`workerOutputTap.ts`).
 *
 * What these lock is a HONESTY contract, not a rendering detail: the grid's
 * state chip, the CompactZoneCard, the StatusStrip pills and the needs-input /
 * error cyclers all read `useSessionStateTracking`, and before this feed a
 * worker was invisible to every one of them. Two failure modes are each worse
 * than the silence they replace, so both get cases here:
 *
 *   - routing a FOREIGN AI session's events onto a worker tab (every page's
 *     scope hears every session), and
 *   - manufacturing a state the worker is not in — `needs-input` above all,
 *     because that one arms quick-approve affordances that write into a PTY a
 *     worker does not have.
 *
 * vitest runs `environment: "node"` here, so the pure mapping half is
 * exercised directly with no React and no runner.
 */

import { describe, it, expect } from "vitest";

import {
  DROPPED_AI_OUTPUT_SOURCES,
  WorkerOutputCoalescer,
  workerSessionStateFor,
  workerTabsByTaskRun,
  workerTextFromAiOutput,
} from "./workerOutputTap";

describe("workerTextFromAiOutput", () => {
  it("passes assistant text through", () => {
    expect(workerTextFromAiOutput({ line: "Editing foo.rs", source: "claude" })).toBe(
      "Editing foo.rs",
    );
  });

  it("passes tool activity through — it is the most useful compact-card line", () => {
    expect(workerTextFromAiOutput({ line: "Reading src/lib.rs", source: "tool_activity" })).toBe(
      "Reading src/lib.rs",
    );
  });

  it("passes a system note through", () => {
    expect(workerTextFromAiOutput({ line: "resumed", source: "system_note" })).toBe("resumed");
  });

  it("drops the echo of steering we sent", () => {
    // Otherwise the operator's own keystroke refreshes `lastOutputTime` and a
    // wedged worker looks alive the moment someone types at it.
    expect(workerTextFromAiOutput({ line: "please retry", source: "user_message" })).toBeNull();
  });

  it("drops the runner's own heartbeat line", () => {
    // `status` is emitted on a timer, not by the worker. Feeding it would keep
    // `lastOutputTime` fresh forever and the 60s staleness sweep could never
    // mark a stuck worker stale.
    expect(
      workerTextFromAiOutput({ line: "⏳ AI is working... (30s)", source: "status" }),
    ).toBeNull();
  });

  it("names both dropped sources explicitly", () => {
    expect([...DROPPED_AI_OUTPUT_SOURCES].sort()).toEqual(["status", "user_message"]);
  });

  it("contributes nothing for an empty, absent or non-string line", () => {
    expect(workerTextFromAiOutput({ line: "", source: "claude" })).toBeNull();
    expect(workerTextFromAiOutput({ source: "claude" })).toBeNull();
    expect(workerTextFromAiOutput({ line: null, source: "claude" })).toBeNull();
    expect(workerTextFromAiOutput(null)).toBeNull();
  });

  it("keeps a line whose source is absent or unrecognised", () => {
    // An unknown source is a source we have no reason to drop; silence would
    // be the lie this feed exists to remove.
    expect(workerTextFromAiOutput({ line: "hello" })).toBe("hello");
    expect(workerTextFromAiOutput({ line: "hello", source: "something_new" })).toBe("hello");
  });
});

describe("workerSessionStateFor", () => {
  it("maps a working worker to `working`", () => {
    expect(workerSessionStateFor("processing")).toBe("working");
    expect(workerSessionStateFor("interrupting")).toBe("working");
  });

  it("maps a worker waiting for input to `idle`", () => {
    expect(workerSessionStateFor("ready")).toBe("idle");
  });

  it("maps the start-up states to `idle` rather than to nothing", () => {
    expect(workerSessionStateFor("connecting")).toBe("idle");
    expect(workerSessionStateFor("initializing")).toBe("idle");
    expect(workerSessionStateFor("restoring")).toBe("idle");
  });

  it("maps a finished worker to `completed` and a failed one to `error`", () => {
    expect(workerSessionStateFor("closed")).toBe("completed");
    expect(workerSessionStateFor("error")).toBe("error");
  });

  it("renders UNKNOWN (null) rather than a default for the states that are not an answer", () => {
    // `not_found` is the SessionManager not knowing the id, and `disconnected`
    // is the frontend not knowing either. A `completed` here would light up the
    // compact card's Restart affordance for a worker nobody can account for.
    expect(workerSessionStateFor("not_found")).toBeNull();
    expect(workerSessionStateFor("disconnected")).toBeNull();
    expect(workerSessionStateFor(undefined)).toBeNull();
    expect(workerSessionStateFor(null)).toBeNull();
    expect(workerSessionStateFor("some-state-we-have-never-seen")).toBeNull();
  });

  it("never yields `needs-input`", () => {
    // A stream-json worker has no interactive prompt. `needs-input` arms
    // quick-approve, which writes `y` into a PTY the worker does not have —
    // so no input may ever produce it.
    const everyState = [
      "connecting",
      "initializing",
      "ready",
      "processing",
      "interrupting",
      "closed",
      "disconnected",
      "error",
      "not_found",
      "restoring",
      "",
      "needs-input",
    ];
    for (const s of everyState) {
      expect(workerSessionStateFor(s)).not.toBe("needs-input");
    }
  });
});

describe("workerTabsByTaskRun", () => {
  it("indexes worker tabs by their task run id", () => {
    const byRun = workerTabsByTaskRun([
      { id: "run-a", taskRunId: "run-a", sessionBacked: true },
      { id: "run-b", taskRunId: "run-b", sessionBacked: true },
    ]);
    expect(byRun.get("run-a")).toBe("run-a");
    expect(byRun.get("run-b")).toBe("run-b");
    expect(byRun.size).toBe(2);
  });

  it("ignores a PTY tab that merely carries a taskRunId", () => {
    // The old Productivity workers ran the CLI inside a real terminal and are
    // tagged with a task run id too. Routing `ai-output` onto one would feed a
    // tab whose tracking the terminal tap already owns.
    const byRun = workerTabsByTaskRun([{ id: "term-1", taskRunId: "run-a" }]);
    expect(byRun.size).toBe(0);
  });

  it("ignores a sessionBacked tab with no task run id", () => {
    const byRun = workerTabsByTaskRun([{ id: "odd", sessionBacked: true }]);
    expect(byRun.size).toBe(0);
  });

  it("does not resolve a run id this page holds no tab for", () => {
    // Every page's scope hears every AI session's events; the roster IS the
    // ownership filter, so a foreign session must miss.
    const byRun = workerTabsByTaskRun([{ id: "run-a", taskRunId: "run-a", sessionBacked: true }]);
    expect(byRun.get("some-process-manager-chat")).toBeUndefined();
  });
});

describe("WorkerOutputCoalescer", () => {
  it("joins a tab's lines with the separator the downstream reader splits on", () => {
    const c = new WorkerOutputCoalescer();
    c.push("run-a", "first");
    c.push("run-a", "second");
    expect(c.drain()).toEqual([["run-a", "first\nsecond"]]);
  });

  it("keeps tabs apart", () => {
    const c = new WorkerOutputCoalescer();
    c.push("run-a", "a1");
    c.push("run-b", "b1");
    c.push("run-a", "a2");
    expect(new Map(c.drain())).toEqual(
      new Map([
        ["run-a", "a1\na2"],
        ["run-b", "b1"],
      ]),
    );
  });

  it("empties on drain, so text is delivered exactly once", () => {
    const c = new WorkerOutputCoalescer();
    c.push("run-a", "only");
    expect(c.drain()).toEqual([["run-a", "only"]]);
    expect(c.drain()).toEqual([]);
    expect(c.size).toBe(0);
  });

  it("drops buffered text for tabs that left the page", () => {
    const c = new WorkerOutputCoalescer();
    c.push("run-a", "a");
    c.push("run-gone", "g");
    c.retain(new Set(["run-a"]));
    expect(c.drain()).toEqual([["run-a", "a"]]);
  });
});
