import { afterEach, describe, expect, it, vi } from "vitest";
import type { SkillDefinition } from "@qontinui/shared-types/workflow";

const { registerUserSkills } = vi.hoisted(() => ({ registerUserSkills: vi.fn() }));
vi.mock("@qontinui/workflow-utils", () => ({ registerUserSkills }));

import { hasRunnableTemplate, registerRunnableUserSkills } from "./runnableUserSkills";

function skill(id: string, template: unknown): SkillDefinition {
  return {
    id,
    name: id,
    slug: id,
    description: "",
    category: "custom",
    tags: [],
    icon: "puzzle",
    color: "gray",
    allowed_phases: ["setup"],
    parameters: [],
    template: template as SkillDefinition["template"],
    source: "user",
  } as SkillDefinition;
}

/** What the runner serves for a stored template it cannot parse. */
const FALLBACK = { kind: "single_step", step: {} };

afterEach(() => {
  registerUserSkills.mockReset();
  vi.restoreAllMocks();
});

describe("hasRunnableTemplate", () => {
  it("passes a template whose every step has a type, and a composition", () => {
    expect(
      hasRunnableTemplate(skill("a", { kind: "single_step", step: { type: "command" } })),
    ).toBe(true);
    expect(
      hasRunnableTemplate(
        skill("b", { kind: "multi_step", steps: [{ type: "command" }, { type: "prompt" }] }),
      ),
    ).toBe(true);
    expect(hasRunnableTemplate(skill("c", { kind: "composition", skill_refs: [] }))).toBe(true);
  });

  it("refuses the served fallback and any step without a string type", () => {
    expect(hasRunnableTemplate(skill("fallback", FALLBACK))).toBe(false);
    expect(
      hasRunnableTemplate(
        skill("half", { kind: "multi_step", steps: [{ type: "command" }, { type: null }] }),
      ),
    ).toBe(false);
    expect(hasRunnableTemplate(skill("no-steps", { kind: "multi_step" }))).toBe(false);
  });

  it("refuses a kind it does not know and a missing template", () => {
    expect(hasRunnableTemplate(skill("old", { kind: "playbook", content: "x" }))).toBe(false);
    expect(hasRunnableTemplate(skill("none", undefined))).toBe(false);
  });
});

describe("registerRunnableUserSkills", () => {
  it("registers only the runnable skills and names the ones left out", () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    const lint = skill("user:lint", { kind: "single_step", step: { type: "command" } });
    const broken = skill("user:sf-login", FALLBACK);

    const leftOut = registerRunnableUserSkills([lint, broken]);

    expect(registerUserSkills).toHaveBeenCalledTimes(1);
    expect(registerUserSkills).toHaveBeenCalledWith([lint]);
    expect(leftOut).toEqual([broken]);
    expect(warn).toHaveBeenCalledTimes(1);
    expect(String(warn.mock.calls[0][0])).toContain('"user:sf-login"');
  });

  it("registers everything, and warns about nothing, when every skill is runnable", () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    const lint = skill("user:lint", { kind: "single_step", step: { type: "command" } });

    expect(registerRunnableUserSkills([lint])).toEqual([]);
    expect(registerUserSkills).toHaveBeenCalledWith([lint]);
    expect(warn).not.toHaveBeenCalled();
  });
});
