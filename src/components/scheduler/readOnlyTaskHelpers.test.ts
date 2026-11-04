import { readFileSync } from "node:fs";
import { resolve } from "node:path";

import { describe, it, expect } from "vitest";

import { readOnlyTaskType } from "./readOnlyTaskHelpers";

describe("readOnlyTaskType", () => {
  it("names a task type the form has no editor for", () => {
    expect(readOnlyTaskType({ task_type: "Watcher", watcher_id: "w" })).toBe("Watcher");
    expect(readOnlyTaskType({ task_type: "BackgroundCapture", capture_interval_secs: 60 })).toBe(
      "BackgroundCapture",
    );
  });

  it("leaves the types the form handles, and no task, to the form", () => {
    for (const t of ["Workflow", "Prompt", "AutoFix", "RemoteAgent", "Script"]) {
      expect(readOnlyTaskType({ task_type: t })).toBeUndefined();
    }
    expect(readOnlyTaskType(undefined)).toBeUndefined();
    expect(readOnlyTaskType(null)).toBeUndefined();
    expect(readOnlyTaskType({})).toBeUndefined();
  });

  it("is what the form opens, submits and validates a read-only task by", () => {
    const form = readFileSync(resolve(__dirname, "SchedulerTaskForm.tsx"), "utf8");
    expect(form).toContain('if (readOnlyTaskType(initTask)) return "readonly";');
    expect(form).toContain(
      "if (readOnlyTaskType(initTask)) return initTask as unknown as ScheduledTaskType;",
    );
    expect(form).toContain(
      'case "readonly":\n        return readOnlyTaskType(initTask) !== undefined;',
    );
  });
});
