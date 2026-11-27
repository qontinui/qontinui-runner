/**
 * The history's message line. The runner's vitest config uses
 * `environment: "node"`, so the component is checked through its source.
 */

import { readFileSync } from "node:fs";
import { resolve } from "node:path";

import { describe, it, expect } from "vitest";

import { historyMessage } from "./historyMessageHelpers";

describe("historyMessage", () => {
  it("shows a successful run's message as a neutral note", () => {
    expect(
      historyMessage({ success: true, errorMessage: "launched (detached; exit not observed)" }),
    ).toEqual({ label: "Note", tone: "muted", text: "launched (detached; exit not observed)" });
  });

  it("shows a failed run's message as an error", () => {
    expect(historyMessage({ success: false, errorMessage: "exit 3\nboom" })).toEqual({
      label: "Error",
      tone: "error",
      text: "exit 3\nboom",
    });
  });

  it("shows nothing without a message", () => {
    expect(historyMessage({ success: false, errorMessage: null })).toBeUndefined();
    expect(historyMessage({ success: true })).toBeUndefined();
  });

  it("is what the history renders the message by", () => {
    const src = readFileSync(resolve(__dirname, "SchedulerHistory.tsx"), "utf8");
    expect(src).toContain("historyMessage(record)");
    expect(src).not.toContain("Error: {record.errorMessage}");
  });
});
