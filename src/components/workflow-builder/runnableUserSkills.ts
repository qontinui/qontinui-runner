/**
 * Which user and community skills the skill catalog may offer.
 *
 * The runner serves a stored skill template it cannot parse (a retired
 * `playbook` template, or any malformed one) as an empty `single_step`,
 * `{"kind": "single_step", "step": {}}`, so one bad row never fails the whole
 * list. The catalog instantiates skills on the client, and instantiating that
 * fallback adds a step with no `type` to the workflow. The runner's own
 * instantiate door refuses such a step (`skills::instantiate_skill`); this is
 * the same check for the catalog.
 */

import { registerUserSkills } from "@qontinui/workflow-utils";
import type { SkillDefinition } from "@qontinui/shared-types/workflow";

function isTypedStep(step: unknown): boolean {
  return (
    typeof step === "object" &&
    step !== null &&
    typeof (step as Record<string, unknown>).type === "string"
  );
}

/**
 * True when every step the skill's template instantiates carries a string
 * `type`. A composition instantiates through the skills it references, so it
 * passes; a template of any other kind does not.
 */
export function hasRunnableTemplate(skill: SkillDefinition): boolean {
  // Read defensively: the list is runner data, and the static type is only
  // what the runner is meant to send.
  const template = skill.template as unknown as Record<string, unknown> | undefined;
  switch (template?.kind) {
    case "single_step":
      return isTypedStep(template.step);
    case "multi_step":
      return Array.isArray(template.steps) && template.steps.every(isTypedStep);
    case "composition":
      return true;
    default:
      return false;
  }
}

/**
 * Register the user and community skills the catalog may offer, leaving out
 * any whose template has a step with no `type` ({@link hasRunnableTemplate}).
 * Returns the skills left out, and names them in the console.
 */
export function registerRunnableUserSkills(skills: SkillDefinition[]): SkillDefinition[] {
  const runnable: SkillDefinition[] = [];
  const leftOut: SkillDefinition[] = [];
  for (const skill of skills) {
    (hasRunnableTemplate(skill) ? runnable : leftOut).push(skill);
  }
  if (leftOut.length > 0) {
    console.warn(
      `Skill catalog: left out ${leftOut.map((s) => `"${s.id}"`).join(", ")}: a template ` +
        "step has no `type`. If the stored template does not parse, the runner serves it " +
        "that way: export the skill to recover the stored template. Otherwise, update the " +
        "template so every step has a string `type`.",
    );
  }
  registerUserSkills(runnable);
  return leftOut;
}
