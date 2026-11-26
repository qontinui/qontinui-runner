/**
 * Pure-helper tests for the scheduler form's `RemoteAgent` task. The runner's
 * vitest config uses `environment: "node"`, so the form itself is not rendered
 * here (see `LockYieldPolicySettings.test.tsx` for the precedent).
 */

import { readFileSync } from "node:fs";
import { resolve } from "node:path";

import { describe, it, expect } from "vitest";

import { buildRemoteAgentTask, type RemoteAgentTask } from "./remoteAgentTaskHelpers";

const edited = {
  prompt: "edited",
  working_directory: null,
  allowed_tools: ["Bash"],
  model: null,
  mcp_connections: [],
  max_turns: null,
  timeout_seconds: null,
};

// Typed through `unknown`: the published @qontinui/shared-types predates these
// keys, and the point is what happens to them at runtime.
const stored = (extra: Record<string, unknown>) =>
  ({
    task_type: "RemoteAgent",
    ...edited,
    prompt: "stored",
    ...extra,
  }) as unknown as RemoteAgentTask;

describe("buildRemoteAgentTask", () => {
  it("keeps the spawn-effort keys the form has no control for", () => {
    const task = buildRemoteAgentTask(
      stored({ effort: "max", ultracode: false, effort_profile: "research" }),
      edited,
    );
    expect(task).toMatchObject({ effort: "max", ultracode: false, effort_profile: "research" });
  });

  it("keeps a key the runner's schemas pin knows and the npm types do not", () => {
    expect(buildRemoteAgentTask(stored({ newer_than_the_npm_types: 1 }), edited)).toMatchObject({
      newer_than_the_npm_types: 1,
    });
  });

  it("lets the form's value win over the stored one, including a cleared field", () => {
    const task = buildRemoteAgentTask(stored({ model: "opus", max_turns: 9 }), edited);
    expect(task.prompt).toBe("edited");
    expect(task.model).toBeNull();
    expect(task.max_turns).toBeNull();
  });

  it("adds nothing to a new task", () => {
    expect(buildRemoteAgentTask(undefined, edited)).toEqual({
      task_type: "RemoteAgent",
      ...edited,
    });
  });

  it("does not let an opened task change the task type", () => {
    const opened = { task_type: "Prompt", prompt_id: "p" } as unknown as RemoteAgentTask;
    expect(buildRemoteAgentTask(opened, edited).task_type).toBe("RemoteAgent");
  });
});

// The form is not rendered here, so this pins its one line of wiring: passing
// `undefined` would keep every test above green and bring the silent drop back.
describe("SchedulerTaskForm", () => {
  it("builds its RemoteAgent task over the task it opened with", () => {
    const form = readFileSync(resolve(__dirname, "SchedulerTaskForm.tsx"), "utf8");
    expect(form).toContain("buildRemoteAgentTask(initRemoteAgent, {");
  });
});
