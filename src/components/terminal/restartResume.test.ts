import { describe, expect, it } from "vitest";
import { planRestart } from "./restartResume";

const claudeTab = {
  title: "claude abcd1234",
  workingDir: "D:/qontinui-root",
  claudeSessionId: "abcd1234-0000-4000-8000-000000000000",
  claudeConfigDir: "C:/claude/.claude-erik",
};

describe("planRestart", () => {
  it("spawns a shell for a pane with no Claude session", () => {
    expect(planRestart({ title: "pwsh", workingDir: "D:/x" }, new Set())).toEqual({
      kind: "shell",
    });
    expect(planRestart(undefined, new Set())).toEqual({ kind: "shell" });
  });

  it("resumes a pane that hosted a Claude session, keeping id, config dir and cwd", () => {
    const plan = planRestart(claudeTab, new Set(["other"]));
    expect(plan.kind).toBe("resume");
    if (plan.kind !== "resume") return;
    expect(plan.session.session_id).toBe(claudeTab.claudeSessionId);
    expect(plan.session.config_dir).toBe(claudeTab.claudeConfigDir);
    expect(plan.session.project_path).toBe(claudeTab.workingDir);
  });

  it("blocks when a live process already hosts the id", () => {
    const plan = planRestart(claudeTab, new Set([claudeTab.claudeSessionId]));
    expect(plan.kind).toBe("blocked");
  });

  it("fails closed when liveness is indeterminate", () => {
    expect(planRestart(claudeTab, null).kind).toBe("blocked");
  });

  it("does not consult liveness for a shell pane", () => {
    expect(planRestart({ title: "pwsh" }, null)).toEqual({ kind: "shell" });
  });
});
