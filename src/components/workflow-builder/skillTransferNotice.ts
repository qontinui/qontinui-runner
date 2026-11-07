/**
 * The user-facing notice for a skill import or export: what the runner refused
 * or left out, so a partial transfer never reads as a complete one.
 *
 * Returns `null` when nothing needs saying (everything transferred).
 */

/** A skill the runner refused on import (`/skills/import` → `refused`). */
export interface SkillIntakeRefusal {
  code: string;
  skill: string;
}

/** A stored skill the runner left out of an export (`/skills/export` → `withheld`). */
export interface WithheldSkill {
  skill_id: string;
  code: string;
  error: string;
}

export interface SkillImportOutcome {
  imported?: number;
  overwritten?: number;
  errors?: string[];
  refused?: SkillIntakeRefusal[];
}

const PLAYBOOK_TEMPLATE_RETIRED = "PLAYBOOK_TEMPLATE_RETIRED";

function quoted(names: string[]): string {
  return names.map((n) => `"${n}"`).join(", ");
}

/** The notice for a completed `/skills/import` call. */
export function skillImportNotice(outcome: SkillImportOutcome): string | null {
  const parts: string[] = [];
  const refused = outcome.refused ?? [];
  const retired = refused.filter((r) => r.code === PLAYBOOK_TEMPLATE_RETIRED).map((r) => r.skill);
  const otherRefused = refused.filter((r) => r.code !== PLAYBOOK_TEMPLATE_RETIRED);
  if (retired.length > 0) {
    parts.push(
      `Not imported: ${quoted(retired)} ${retired.length === 1 ? "is a playbook skill" : "are playbook skills"}, ` +
        "which this runner no longer runs. Put that knowledge in a .qontinui/contexts/*.md file instead.",
    );
  }
  if (otherRefused.length > 0) {
    parts.push(`Not imported: ${otherRefused.map((r) => `"${r.skill}" (${r.code})`).join(", ")}.`);
  }
  const errors = outcome.errors ?? [];
  if (errors.length > 0) {
    parts.push(`Import errors: ${errors.join("; ")}`);
  }
  return parts.length > 0 ? `Skill import: ${parts.join(" ")}` : null;
}

/** The notice for a completed `/skills/export` call. */
export function skillExportNotice(withheld: WithheldSkill[] | undefined): string | null {
  if (!withheld || withheld.length === 0) return null;
  const ids = withheld.map((w) => w.skill_id);
  return (
    `Skill export: left out ${quoted(ids)} because ${ids.length === 1 ? "its" : "their"} stored ` +
    "template no longer parses. Delete and re-create " +
    `${ids.length === 1 ? "that skill" : "those skills"} to export ${ids.length === 1 ? "it" : "them"}.`
  );
}
