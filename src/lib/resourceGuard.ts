/**
 * Frontend half of the spawn-time resource gate (plan
 * `2026-08-07-runner-resource-guard-and-session-protection.md` §Part D).
 *
 * The gate itself lives in Rust (`src-tauri/src/resource_guard.rs`) on the two
 * spawn seams — `TerminalSession::spawn` and
 * `InstanceManager::launch_instance_with_app` — because those are reachable from
 * gate continuations, looping agents and the HTTP/relay doors, not only from a
 * button in this app. What lives here is the *attended* half: recognising the
 * typed CRITICAL refusal, asking the operator, and replaying the spawn with the
 * override set.
 *
 * ## Why a module store rather than context or props
 *
 * The refusal surfaces wherever a spawn is invoked — `useTerminalManager`, the
 * coordinator/worker launch buttons, the Instances panel — but the dialog is
 * rendered once, at the App root. Threading a callback from the root down to
 * every one of those call sites would be a prop chain whose only payload is
 * "here is how to ask a question". Same shape, same reason as
 * `src/lib/authOverlayStore.ts`.
 */

import { useSyncExternalStore } from "react";

/**
 * Prefix the Rust side stamps on an overridable CRITICAL refusal. Must match
 * `resource_guard::CRITICAL_REFUSAL_PREFIX` byte for byte — it is the only thing
 * that distinguishes "the guard said no, and you may overrule it" from a real
 * spawn failure (no PTY, bad cwd, dead account) which must NEVER be offered an
 * override.
 */
export const CRITICAL_REFUSAL_PREFIX = "resource_guard:critical:";

/**
 * The operator-facing text of a CRITICAL refusal, or `null` when `err` is
 * anything else.
 *
 * Tauri hands a Rust `Err(String)` to the frontend as a plain string, but the
 * invoke layer can also reject with an `Error` (transport failure) or an
 * arbitrary value, so this normalises before matching rather than assuming.
 */
export function parseResourceGuardRefusal(err: unknown): string | null {
  const text =
    typeof err === "string" ? err : err instanceof Error ? err.message : String(err ?? "");
  if (!text.startsWith(CRITICAL_REFUSAL_PREFIX)) return null;
  return text.slice(CRITICAL_REFUSAL_PREFIX.length).trim();
}

/** A refusal awaiting the operator's decision. */
export interface PendingResourceBlock {
  /** Message from Rust: names the lane, the live headroom and the floor. */
  message: string;
}

interface QueuedBlock extends PendingResourceBlock {
  decide: (startAnyway: boolean) => void;
}

/**
 * FIFO of refusals waiting on the operator.
 *
 * A queue rather than a single slot because two spawns can be in flight at once
 * (the operator opens a terminal while a worker launch is mid-invoke, or two
 * pop-out windows both spawn). Replacing a pending refusal would strand the
 * first promise forever — the caller would hang, not fail — which is the one
 * outcome a guard whose entire contract is "never a silent refusal" cannot
 * produce.
 */
const queue: QueuedBlock[] = [];
const listeners = new Set<() => void>();

function notify(): void {
  for (const l of [...listeners]) l();
}

/**
 * Store half of the `useSyncExternalStore` contract. Exported so it can be unit
 * tested directly — vitest runs `environment: "node"` here with no React
 * Testing Library, the same constraint `authOverlayStore.ts` documents.
 */
export function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

/** The refusal currently on screen, or `null`. See {@link subscribe}. */
export function getSnapshot(): PendingResourceBlock | null {
  return queue[0] ?? null;
}

/** The refusal the dialog should be showing, or `null` when there is none. */
export function usePendingResourceBlock(): PendingResourceBlock | null {
  return useSyncExternalStore(subscribe, getSnapshot);
}

/**
 * Answer the refusal at the head of the queue. Called by the dialog's confirm /
 * cancel handlers; a no-op when the queue is empty (a double-click on Confirm
 * must not answer the *next* operator's question).
 */
export function resolvePendingResourceBlock(startAnyway: boolean): void {
  const head = queue.shift();
  notify();
  head?.decide(startAnyway);
}

/** Enqueue a refusal and resolve once the operator answers it. */
function askOperator(message: string): Promise<boolean> {
  return new Promise<boolean>((resolve) => {
    queue.push({ message, decide: resolve });
    notify();
  });
}

/**
 * Run a spawn through the resource gate's attended path.
 *
 * `attempt` is invoked with `false` first. If it rejects with anything other
 * than a typed CRITICAL refusal the error propagates untouched — this wrapper
 * must never convert a genuine spawn failure into a "start anyway?" prompt.
 * On a typed refusal the operator is asked; "Start anyway" re-invokes with
 * `true`, and declining re-throws the ORIGINAL refusal so the caller's existing
 * error handling still runs and the spawn is not silently reported as a success.
 *
 * Every attended spawn surface should call through this rather than invoking
 * directly, so the override handling exists in exactly one place.
 */
export async function spawnWithResourceGuard<T>(
  attempt: (resourceOverride: boolean) => Promise<T>,
): Promise<T> {
  try {
    return await attempt(false);
  } catch (err) {
    const message = parseResourceGuardRefusal(err);
    if (message === null) throw err;
    const startAnyway = await askOperator(message);
    if (!startAnyway) throw err;
    return attempt(true);
  }
}
