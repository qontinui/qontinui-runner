/**
 * partitionSpecs
 *
 * Partitions a set of discovered specs across N runners for parallel
 * multi-loop execution. Supports multiple strategies:
 *
 * - "round-robin": Distributes specs one at a time across runners.
 * - "by-app": Groups specs by appName, one app per runner (or spread
 *   if fewer apps than runners).
 * - "by-tag": Groups specs by their first metadata tag.
 * - "balanced": Distributes by total assertion count so each runner
 *   gets roughly equal verification work.
 */

import type { DiscoveredSpec } from "../spec-prompt-builder";

export interface SpecPartition {
  /** Human label for this partition (e.g., "Qontinui Web" or "Group 1"). */
  label: string;
  /** Specs assigned to this partition. */
  specs: DiscoveredSpec[];
}

export type PartitionStrategy = "round-robin" | "by-app" | "by-tag" | "balanced";

/** Count total enabled assertions across all groups in a spec. */
export function countAssertions(spec: DiscoveredSpec): number {
  return spec.config.groups.reduce((sum, group) => {
    return sum + group.assertions.filter((a) => a.enabled !== false).length;
  }, 0);
}

function partitionRoundRobin(specs: DiscoveredSpec[], n: number): SpecPartition[] {
  const partitions: SpecPartition[] = Array.from({ length: n }, (_, i) => ({
    label: `Group ${i + 1}`,
    specs: [],
  }));
  specs.forEach((spec, i) => partitions[i % n].specs.push(spec));
  return partitions.filter((p) => p.specs.length > 0);
}

function partitionByKey(
  specs: DiscoveredSpec[],
  n: number,
  keyFn: (spec: DiscoveredSpec) => string,
): SpecPartition[] {
  const groups = new Map<string, DiscoveredSpec[]>();
  for (const spec of specs) {
    const key = keyFn(spec);
    const list = groups.get(key) ?? [];
    list.push(spec);
    groups.set(key, list);
  }

  const keys = [...groups.keys()].sort();

  // If we have more keys than runners, merge smaller groups via round-robin
  if (keys.length <= n) {
    return keys.map((key) => ({ label: key, specs: groups.get(key)! }));
  }

  const partitions: SpecPartition[] = Array.from({ length: n }, (_, i) => ({
    label: `Group ${i + 1}`,
    specs: [],
  }));
  keys.forEach((key, i) => {
    partitions[i % n].specs.push(...groups.get(key)!);
  });
  // Update labels to reflect merged groups
  for (const p of partitions) {
    const appNames = [...new Set(p.specs.map(keyFn))];
    if (appNames.length <= 3) {
      p.label = appNames.join(", ");
    }
  }
  return partitions.filter((p) => p.specs.length > 0);
}

function partitionBalanced(specs: DiscoveredSpec[], n: number): SpecPartition[] {
  // Sort specs by assertion count descending for greedy bin-packing
  const sorted = [...specs].sort((a, b) => countAssertions(b) - countAssertions(a));

  const partitions: SpecPartition[] = Array.from({ length: n }, (_, i) => ({
    label: `Group ${i + 1}`,
    specs: [],
  }));
  const weights = new Array(n).fill(0);

  for (const spec of sorted) {
    // Find the partition with the least total work
    let minIdx = 0;
    for (let i = 1; i < n; i++) {
      if (weights[i] < weights[minIdx]) minIdx = i;
    }
    partitions[minIdx].specs.push(spec);
    weights[minIdx] += countAssertions(spec);
  }

  return partitions.filter((p) => p.specs.length > 0);
}

/**
 * Partition specs across N runners.
 *
 * @param specs All discovered specs to distribute.
 * @param runnerCount Number of target runners available.
 * @param strategy How to split the specs.
 * @returns Array of partitions (length <= runnerCount).
 */
export function partitionSpecs(
  specs: DiscoveredSpec[],
  runnerCount: number,
  strategy: PartitionStrategy = "balanced",
): SpecPartition[] {
  if (specs.length === 0) return [];
  const n = Math.max(1, Math.min(runnerCount, specs.length));

  switch (strategy) {
    case "round-robin":
      return partitionRoundRobin(specs, n);
    case "by-app":
      return partitionByKey(specs, n, (s) => s.appName ?? "Unknown");
    case "by-tag": {
      return partitionByKey(specs, n, (s) => {
        const tags = (s.config.metadata?.tags as string[] | undefined) ?? [];
        return tags[0] ?? "untagged";
      });
    }
    case "balanced":
      return partitionBalanced(specs, n);
  }
}
