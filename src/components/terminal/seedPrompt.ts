/**
 * Consuming an activation's `seedPrompt` — the terminal half of [Fix this]
 * (projects-dashboard plan §8.4).
 *
 * The Projects page composes the prompt (it owns the snapshot) and publishes it
 * on the activation hint; this module decides whether a given hint should
 * actually spawn an agent. That decision is separated from the spawning because
 * the interesting part is the GUARD, and the guard is worth testing without a
 * PTY.
 *
 * ## Why an id, and not just "is seedPrompt present?"
 *
 * The hint is a cache as much as an event. `activeProject.ts` mirrors every
 * hint into `instanceStorage` so a reload starts from it, and
 * `subscribeActiveProject` re-delivers it on cross-window `storage` events so a
 * popped-out terminal follows the main window. Both are correct for the fields
 * the hint was designed for — a path and a page id are idempotent.
 *
 * A seed prompt is not. Acting on it SPAWNS an AI session, which costs the
 * user's own quota and starts an agent editing their project. Keying on
 * presence alone would turn one [Fix this] click into a fresh agent on every
 * webview reload and every cross-window replay, unbounded. So each click mints
 * a `seedPromptId` and this module remembers the ids it has already acted on.
 *
 * The record lives in `instanceStorage` rather than in memory precisely because
 * the replay that matters most — a reload — destroys memory.
 */

import { instanceStorage } from "@/lib/instance-storage";
import type { ActiveProjectHint } from "./activeProject";

const CONSUMED_KEY = "qontinui-consumed-seed-prompts";

/**
 * How many consumed ids to retain. Small because the only thing that must be
 * remembered is "recently acted on"; an id evicted after this many further
 * fixes cannot be replayed anyway, since a replay only ever carries the LAST
 * hint.
 */
const MAX_REMEMBERED = 50;

export interface SeedPromptRequest {
  /** The prompt text to hand the agent. */
  prompt: string;
  /** The request's identity — record it once acted on. */
  id: string;
  /** Project root, so the session starts in the right directory. */
  projectRoot?: string;
}

function readConsumed(): string[] {
  const v = instanceStorage.getJSON<string[] | null>(CONSUMED_KEY, null);
  return Array.isArray(v) ? v.filter((x) => typeof x === "string") : [];
}

/** Whether this seed request has already been acted on in this instance. */
export function isSeedConsumed(id: string): boolean {
  return readConsumed().includes(id);
}

/**
 * Record a seed id as acted-on.
 *
 * Call this BEFORE the spawn, not after. A spawn that throws halfway has still
 * potentially started a session, and re-running it on the next replay would be
 * worse than skipping a fix the user can retry with another click.
 */
export function markSeedConsumed(id: string): void {
  const next = [...readConsumed().filter((x) => x !== id), id];
  instanceStorage.setJSON(CONSUMED_KEY, next.slice(-MAX_REMEMBERED));
}

/**
 * Read a hint into a seed request, or `null` when there is nothing to do.
 *
 * Pure apart from the consumed-set read, so the whole decision is testable.
 * Returns `null` for: no hint, no prompt, a blank prompt, a prompt with no id
 * (which we refuse rather than guess an identity for — an un-ided seed cannot
 * be replay-guarded, and spawning it would be the exact unbounded case this
 * module exists to prevent), and an id already consumed.
 */
export function planSeedPrompt(hint: ActiveProjectHint | null): SeedPromptRequest | null {
  if (!hint) return null;
  const prompt = hint.seedPrompt?.trim();
  const id = hint.seedPromptId?.trim();
  if (!prompt || !id) return null;
  if (isSeedConsumed(id)) return null;
  return { prompt, id, projectRoot: hint.path };
}
