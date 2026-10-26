/**
 * Process-table answers to "is a `claude` running in this pane?" for the
 * "Keep AI" button (`PruneTerminalsButton`).
 *
 * Two halves:
 * - {@link probeClaudeInPanes} — one `terminal_probe_claude_many` call (one
 *   process snapshot) for every close candidate, plus the remembered panes.
 * - The known-Claude store — panes a probe has read `live`. `usePrunePlan`
 *   keeps them without re-probing, so a pane the probe rescued stops being
 *   counted as a close candidate and the button stops offering to close it.
 *   Only `live` adds a pane; a later probe reading it `absent` (its Claude
 *   exited) drops it again, so it can be pruned once more. Ids are backend
 *   uuids or `plan-` ids, so entries for closed tabs never collide.
 */

import { invoke } from "@tauri-apps/api/core";
import { useSyncExternalStore } from "react";

import type { PaneClaudeProbe } from "./resumeVerification";

export type PaneClaudeState = PaneClaudeProbe["state"];

const STATES: ReadonlySet<string> = new Set(["live", "absent", "remote", "unknown"]);

/**
 * Probe many panes at once. Never throws: an IPC failure or an unparseable
 * answer yields `{}`, and an id missing from the result is UNKNOWN — callers
 * must keep it.
 */
export async function probeClaudeInPanes(
  terminalIds: readonly string[],
  invoker: (cmd: string, args: Record<string, unknown>) => Promise<unknown> = invoke,
): Promise<Record<string, PaneClaudeState>> {
  if (terminalIds.length === 0) return {};
  try {
    const resp = await invoker("terminal_probe_claude_many", { terminalIds: [...terminalIds] });
    const states = (resp as { data?: { states?: unknown } } | null)?.data?.states;
    if (!states || typeof states !== "object") return {};
    const out: Record<string, PaneClaudeState> = {};
    for (const [id, state] of Object.entries(states as Record<string, unknown>)) {
      if (typeof state === "string" && STATES.has(state)) out[id] = state as PaneClaudeState;
    }
    return out;
  } catch {
    return {};
  }
}

let known: ReadonlySet<string> = new Set();
const listeners = new Set<() => void>();

/**
 * Fold a probe into the store: a pane read `live` is remembered, a remembered
 * pane read `absent` is forgotten, and every other answer changes nothing.
 * No-op (same set identity) when nothing changed.
 */
export function rememberClaudePanes(states: Readonly<Record<string, PaneClaudeState>>): void {
  const next = new Set(known);
  for (const [id, state] of Object.entries(states)) {
    if (state === "live") next.add(id);
    else if (state === "absent") next.delete(id);
  }
  if (next.size === known.size && [...next].every((id) => known.has(id))) return;
  known = next;
  for (const l of listeners) l();
}

/** Panes a probe has read `live`. Stable identity until it changes. */
export function getKnownClaudePanes(): ReadonlySet<string> {
  return known;
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

/** {@link getKnownClaudePanes}, re-rendering when it changes. */
export function useKnownClaudePanes(): ReadonlySet<string> {
  return useSyncExternalStore(subscribe, getKnownClaudePanes);
}

/** Test hook: forget every remembered pane. */
export function resetKnownClaudePanes(): void {
  known = new Set();
  for (const l of listeners) l();
}
