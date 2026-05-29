/**
 * Module-level command-action registry.
 *
 * Lifecycle mirrors `@qontinui/ui-bridge`'s `useUIComponent` registry: a
 * single module-scoped store that `useCommandAction` writes into on
 * mount and clears on unmount, plus a subscribe pattern so React
 * consumers re-render when actions register/unregister.
 *
 * Why a module singleton (rather than a context): the future CommandBar,
 * `Ctrl+Shift+K` palette, and Tier-3 subprocess executor all sit at
 * different points in the tree (and the subprocess executor runs in
 * `src-tauri` Rust over IPC), so a flat module store is the only surface
 * every consumer can reach without prop drilling. Same pattern used by
 * `lib/terminal-sessions-registry.ts` for the UI Bridge IPC handlers.
 *
 * Reactivity contract: every {@link register}, {@link unregister}, and
 * {@link replace} call notifies subscribers synchronously. React
 * consumers should subscribe via `useSyncExternalStore` so concurrent
 * mode handles the tearing edge cases — see Phase 2's CommandBar where
 * the autocomplete list reads via `getSnapshot`.
 */

import type { CommandAction, RegistryListener } from "./types";

const actions = new Map<string, CommandAction>();
const listeners = new Set<RegistryListener>();

/** Cached snapshot for `useSyncExternalStore`. Recomputed on mutation —
 *  reused between calls so React's identity-equality check skips renders
 *  when nothing changed (matches the `terminal-sessions-registry`
 *  invariant). */
let snapshot: readonly CommandAction[] = [];

function rebuildSnapshot(): void {
  snapshot = Array.from(actions.values());
}

function notify(): void {
  for (const listener of listeners) {
    listener();
  }
}

/**
 * Add an action to the registry, replacing any prior entry with the
 * same id. Returns an unregister function so callers can manage the
 * lifetime without having to remember the id (the hook uses this).
 *
 * Replacing-on-collision rather than rejecting is the same posture as
 * `useUIComponent` — a React fast-refresh or strict-mode double-mount
 * must not crash; the new registration wins and the prior handler is
 * shed cleanly.
 */
export function register(action: CommandAction): () => void {
  actions.set(action.id, action);
  rebuildSnapshot();
  notify();
  return () => unregister(action.id);
}

/** Remove an action by id. No-op when the id is unknown. */
export function unregister(id: string): void {
  if (!actions.delete(id)) return;
  rebuildSnapshot();
  notify();
}

/**
 * Look up an action by id. Returns `undefined` when no match exists —
 * the resolver paths (slash, palette, AI) turn that into a "no such
 * command" error rendered to the user, not a thrown exception.
 */
export function getById(id: string): CommandAction | undefined {
  return actions.get(id);
}

/**
 * Look up an action by its primary slash form or any alias.
 * Linear scan — the registry is small (~25 entries target in Phase 1,
 * ~50 long-term) and lookups happen on user input cadence, so the
 * constant-factor cost of a Map<slash, id> index isn't worth the
 * coordination overhead with `aliases?` reshuffling on replace.
 */
export function getBySlash(slash: string): CommandAction | undefined {
  for (const action of actions.values()) {
    if (action.slash === slash) return action;
    if (action.aliases?.includes(slash)) return action;
  }
  return undefined;
}

/**
 * Snapshot of every registered action. Returns the same array reference
 * between mutations so `useSyncExternalStore`'s identity check is cheap.
 */
export function getAll(): readonly CommandAction[] {
  return snapshot;
}

/**
 * Subscribe to registry changes. Returns an unsubscribe function. Pair
 * with `getAll` in a `useSyncExternalStore` consumer.
 */
export function subscribe(listener: RegistryListener): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

/**
 * Reset for tests. Not exported through the package barrel — vitest
 * specs reach into this module directly. Mirrors the
 * `__resetTerminalSessionsForTest` convention from
 * `lib/terminal-sessions-registry.ts`.
 */
export function __resetForTest(): void {
  actions.clear();
  listeners.clear();
  rebuildSnapshot();
}
