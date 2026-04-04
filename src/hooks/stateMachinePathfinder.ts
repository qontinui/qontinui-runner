/**
 * Lightweight Dijkstra pathfinder for state machine navigation.
 * Operates directly on DB types (StateMachineTransition) — no ui-bridge-auto dependency.
 */

import type { StateMachineTransition } from "@qontinui/shared-types";

export interface PathResult {
  transitions: StateMachineTransition[];
  totalCost: number;
}

/** Serialize a set of state IDs into a deterministic string key for visited tracking. */
function stateKey(states: Set<string>): string {
  return [...states].sort().join("|");
}

/**
 * Compute the next active-state set after executing a transition.
 * - Adds `activate_states`
 * - Removes `exit_states`
 */
function applyTransition(active: Set<string>, t: StateMachineTransition): Set<string> {
  const next = new Set(active);
  for (const s of t.exit_states) next.delete(s);
  for (const s of t.activate_states) next.add(s);
  return next;
}

/** Check whether a transition's preconditions are met (all from_states must be active). */
function canFire(active: Set<string>, t: StateMachineTransition): boolean {
  return t.from_states.length === 0 || t.from_states.every((s) => active.has(s));
}

interface QueueEntry {
  states: Set<string>;
  cost: number;
  path: StateMachineTransition[];
}

/**
 * Find the cheapest sequence of transitions from `activeStates` to a state set
 * that includes `targetStateId`.
 *
 * Returns `null` if no path exists.
 */
export function findPath(
  activeStates: Set<string>,
  targetStateId: string,
  transitions: StateMachineTransition[],
): PathResult | null {
  if (activeStates.has(targetStateId)) {
    return { transitions: [], totalCost: 0 };
  }

  const visited = new Set<string>();
  const queue: QueueEntry[] = [{ states: new Set(activeStates), cost: 0, path: [] }];

  while (queue.length > 0) {
    // Poor-man's priority queue: sort by cost (fine for small state graphs)
    queue.sort((a, b) => a.cost - b.cost);
    const current = queue.shift()!;

    const key = stateKey(current.states);
    if (visited.has(key)) continue;
    visited.add(key);

    for (const t of transitions) {
      if (!canFire(current.states, t)) continue;

      const nextStates = applyTransition(current.states, t);
      const nextKey = stateKey(nextStates);
      if (visited.has(nextKey)) continue;

      const nextCost = current.cost + Math.max(t.path_cost, 1);
      const nextPath = [...current.path, t];

      if (nextStates.has(targetStateId)) {
        return { transitions: nextPath, totalCost: nextCost };
      }

      queue.push({ states: nextStates, cost: nextCost, path: nextPath });
    }
  }

  return null;
}
