/**
 * `/ui-bridge/control/health` must agree with its own body.
 *
 * The handler called `getConsoleRecent(50)` — which returns console entries of
 * EVERY level — and reported the whole list's length as `errorCount`, with
 * `healthy` driven off the same length. Live repro: `errorCount: 2` for a body
 * of one `warn` plus one `error`, and `healthy: false` driven partly by the
 * warning.
 */

import { describe, it, expect } from "vitest";

import { summarizeConsoleHealth } from "./useDebugInspectEvents";

const err = (message = "boom") => ({ level: "error", message });
const warn = (message = "careful") => ({ level: "warn", message });
const info = (message = "fyi") => ({ level: "info", message });

describe("summarizeConsoleHealth", () => {
  it("counts only error-level entries as errors (the reported repro)", () => {
    // One warn + one error came back as errorCount: 2.
    const s = summarizeConsoleHealth([warn(), err()]);
    expect(s.errorCount).toBe(1);
    expect(s.warnCount).toBe(1);
    expect(s.consoleEntryCount).toBe(2);
  });

  it("does not let a warning drive healthy:false", () => {
    const s = summarizeConsoleHealth([warn(), warn(), info()]);
    expect(s.errorCount).toBe(0);
    expect(s.healthy).toBe(true);
  });

  it("still reports unhealthy when a real error is present", () => {
    const s = summarizeConsoleHealth([warn(), err()]);
    expect(s.healthy).toBe(false);
  });

  it("keeps the counter and the predicate in agreement", () => {
    for (const entries of [[], [warn()], [err()], [info(), warn(), err(), err()]]) {
      const s = summarizeConsoleHealth(entries);
      expect(s.healthy).toBe(s.errorCount === 0);
      // The three counts must describe the same list.
      expect(s.consoleEntryCount).toBe(entries.length);
      expect(s.errorCount + s.warnCount).toBeLessThanOrEqual(s.consoleEntryCount);
    }
  });

  it("treats fatal/exception as errors and warning as a warn", () => {
    expect(summarizeConsoleHealth([{ level: "fatal" }]).errorCount).toBe(1);
    expect(summarizeConsoleHealth([{ level: "EXCEPTION" }]).errorCount).toBe(1);
    expect(summarizeConsoleHealth([{ level: "warning" }]).warnCount).toBe(1);
  });

  it("counts an entry with no recognisable level as neither", () => {
    // Guessing a level would be the same class of error as the bug: reporting
    // something the body does not say.
    const s = summarizeConsoleHealth([{ message: "no level" }, null, "string"]);
    expect(s.errorCount).toBe(0);
    expect(s.warnCount).toBe(0);
    expect(s.consoleEntryCount).toBe(3);
    expect(s.healthy).toBe(true);
  });
});
