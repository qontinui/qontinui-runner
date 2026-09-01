/**
 * The `terminal-launch-menu.create-ai-session` UI Bridge envelope — the one
 * spawn surface that does not reach `spawnVerdict` through the registry.
 *
 * ## Why this exists
 *
 * Every other launch-menu action (`create-plain`, `create-with-command`,
 * `create-best-account`) invokes its registry handler through `callRegistry`,
 * whose contract is "unwrap the `CommandResult` or throw". Those handlers all
 * end in {@link spawnVerdict}, so a spawn that produced fewer tabs than asked
 * for — or none — reaches the caller as a failure.
 *
 * `create-ai-session` deliberately bypasses the registry: it takes a raw
 * `configDir` rather than the registry's account label, so it calls
 * `handleLaunchAiSession` directly. That left it the only spawn surface that
 * answered `success: true` for a short delivery.
 *
 * `qontinui-runner#1169` widened the hole rather than opening it. Before it, a
 * launch-spec build that threw rejected out of `handleLaunchAiSession` and the
 * action rejected with it. After it, the throw disposes its own tab and the
 * launch is SKIPPED — correct for the UI, where a toast reports it — so a
 * TOTAL failure now RESOLVES with `[]`, and a caller driving the runner
 * headlessly sees `success: true`, zero tabs, and no reason anywhere it can
 * read. That is the HTTP-200-for-a-failure shape.
 *
 * Extracted as a pure function (no React, no Tauri) so the wiring is testable
 * under the runner's `environment: "node"` vitest config — the same precedent
 * as `buildCreatePlainTerminalAction` and `buildAiLaunchCommandForTab`.
 */

import { spawnVerdict } from "./commands";

/** The UI Bridge wire shape the launch-menu spawn actions answer with. */
export interface AiSessionSpawnEnvelope {
  success: true;
  tab_ids: string[];
  task_run_ids: Array<string | null>;
}

/**
 * Turn a `handleLaunchAiSession` result into the action's envelope, or throw
 * with the registry's own verdict message.
 *
 * The verdict is {@link spawnVerdict}'s, not a second opinion: the two wire ids
 * are documented to collapse onto one handler, so they must not disagree about
 * what a partial spawn means. A `void` result (the launch path returning
 * nothing) is treated as zero produced, exactly as `spawnVerdict` already does.
 *
 * @param result the tab ids that actually received a launch command
 * @param count how many sessions the caller asked for
 */
export function buildAiSessionSpawnEnvelope(
  result: string[] | void,
  count: number,
): AiSessionSpawnEnvelope {
  const verdict = spawnVerdict(result, count, "AI sessions");
  if (!verdict.ok) {
    throw new Error(verdict.message ?? verdict.code);
  }
  const tabIds = verdict.value ?? [];
  return {
    success: true,
    tab_ids: tabIds,
    task_run_ids: tabIds.map(() => null) as Array<string | null>,
  };
}
