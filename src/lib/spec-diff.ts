/**
 * spec-diff.ts
 *
 * Pure TypeScript spec diff computation. Compares two SpecConfig objects
 * by matching groups and assertions by ID, producing a structured diff
 * that can drive targeted implementation plans.
 */

import type { SpecConfig, SpecAssertion } from "./workflow-builder/buildSpecWorkflow";

export interface GroupSummary {
  id: string;
  name: string;
  assertionCount: number;
}

export interface AssertionSummary {
  id: string;
  description: string;
  groupId: string;
}

export interface AssertionModification {
  id: string;
  field: string;
  from: string;
  to: string;
}

export interface GroupModification {
  id: string;
  name: string;
  addedAssertions: string[];
  removedAssertions: string[];
  modifiedAssertions: AssertionModification[];
}

export interface SpecDiff {
  addedGroups: GroupSummary[];
  removedGroups: GroupSummary[];
  modifiedGroups: GroupModification[];
  unchangedGroups: string[];
  addedAssertions: AssertionSummary[];
  removedAssertions: AssertionSummary[];
  modifiedAssertions: AssertionModification[];
  hasChanges: boolean;
  summary: string;
}

/**
 * Compute a structured diff between two spec configs.
 * Groups and assertions are matched by `id`.
 */
export function diffSpecs(oldSpec: SpecConfig, newSpec: SpecConfig): SpecDiff {
  const oldGroups = new Map(oldSpec.groups.map((g) => [g.id, g]));
  const newGroups = new Map(newSpec.groups.map((g) => [g.id, g]));

  const addedGroups: GroupSummary[] = [];
  const removedGroups: GroupSummary[] = [];
  const modifiedGroups: GroupModification[] = [];
  const unchangedGroups: string[] = [];
  const addedAssertions: AssertionSummary[] = [];
  const removedAssertions: AssertionSummary[] = [];
  const modifiedAssertions: AssertionModification[] = [];

  // Process new groups
  for (const [gid, newGroup] of newGroups) {
    const oldGroup = oldGroups.get(gid);
    if (!oldGroup) {
      // Entirely new group
      addedGroups.push({
        id: gid,
        name: newGroup.name,
        assertionCount: newGroup.assertions.length,
      });
      for (const a of newGroup.assertions) {
        addedAssertions.push({ id: a.id, description: a.description, groupId: gid });
      }
      continue;
    }

    // Group exists in both — diff assertions
    const oldAssertions = new Map(oldGroup.assertions.map((a) => [a.id, a]));
    const newAssertions = new Map(newGroup.assertions.map((a) => [a.id, a]));

    const gAdded: string[] = [];
    const gRemoved: string[] = [];
    const gModified: AssertionModification[] = [];

    for (const [aid, newA] of newAssertions) {
      const oldA = oldAssertions.get(aid);
      if (!oldA) {
        gAdded.push(aid);
        addedAssertions.push({ id: aid, description: newA.description, groupId: gid });
      } else {
        const mods = diffAssertion(oldA, newA);
        if (mods.length > 0) {
          gModified.push(...mods);
          modifiedAssertions.push(...mods);
        }
      }
    }

    for (const [aid, oldA] of oldAssertions) {
      if (!newAssertions.has(aid)) {
        gRemoved.push(aid);
        removedAssertions.push({ id: aid, description: oldA.description, groupId: gid });
      }
    }

    if (gAdded.length > 0 || gRemoved.length > 0 || gModified.length > 0) {
      modifiedGroups.push({
        id: gid,
        name: newGroup.name,
        addedAssertions: gAdded,
        removedAssertions: gRemoved,
        modifiedAssertions: gModified,
      });
    } else {
      unchangedGroups.push(gid);
    }
  }

  // Find removed groups
  for (const [gid, oldGroup] of oldGroups) {
    if (!newGroups.has(gid)) {
      removedGroups.push({
        id: gid,
        name: oldGroup.name,
        assertionCount: oldGroup.assertions.length,
      });
      for (const a of oldGroup.assertions) {
        removedAssertions.push({ id: a.id, description: a.description, groupId: gid });
      }
    }
  }

  const hasChanges =
    addedGroups.length > 0 ||
    removedGroups.length > 0 ||
    modifiedGroups.length > 0 ||
    addedAssertions.length > 0 ||
    removedAssertions.length > 0 ||
    modifiedAssertions.length > 0;

  const parts: string[] = [];
  if (addedGroups.length > 0) parts.push(`Added ${addedGroups.length} group(s)`);
  if (removedGroups.length > 0) parts.push(`Removed ${removedGroups.length} group(s)`);
  if (modifiedGroups.length > 0) parts.push(`Modified ${modifiedGroups.length} group(s)`);
  if (addedAssertions.length > 0) parts.push(`${addedAssertions.length} assertion(s) added`);
  if (removedAssertions.length > 0) parts.push(`${removedAssertions.length} assertion(s) removed`);
  const summary = parts.length > 0 ? parts.join(", ") : "No changes";

  return {
    addedGroups,
    removedGroups,
    modifiedGroups,
    unchangedGroups,
    addedAssertions,
    removedAssertions,
    modifiedAssertions,
    hasChanges,
    summary,
  };
}

function diffAssertion(oldA: SpecAssertion, newA: SpecAssertion): AssertionModification[] {
  const mods: AssertionModification[] = [];
  const fields = ["description", "assertionType", "severity", "enabled"] as const;

  for (const field of fields) {
    const oldVal = String((oldA as unknown as Record<string, unknown>)[field] ?? "");
    const newVal = String((newA as unknown as Record<string, unknown>)[field] ?? "");
    if (oldVal !== newVal) {
      mods.push({ id: newA.id, field, from: oldVal, to: newVal });
    }
  }

  // Check target criteria changes — only the "search" target variant has criteria
  const oldCriteria = JSON.stringify(
    oldA.target.type === "search" ? oldA.target.criteria : {},
  );
  const newCriteria = JSON.stringify(
    newA.target.type === "search" ? newA.target.criteria : {},
  );
  if (oldCriteria !== newCriteria) {
    mods.push({ id: newA.id, field: "target.criteria", from: oldCriteria, to: newCriteria });
  }

  return mods;
}
