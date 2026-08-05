/**
 * `Fix this` — the amber/red affordance on a project card (plan §8.4).
 *
 * §8.4 asks for a traffic light that answers *"is it broken?"*, and says that
 * amber/red exposes **[Fix this]**, "which launches an agent with the error
 * text already attached". The attaching is the whole point: Stefan can see
 * that his pizzeria is red, but he cannot write the bug report — he does not
 * know which process died or what it printed. Handing him a text box would be
 * asking the question he opened the dashboard to avoid.
 *
 * This module is the PURE half: deciding whether the button appears, and
 * composing the prompt. It is separated from the wiring so both are testable
 * without React, a snapshot, or a terminal — the same split `openProject.ts`
 * uses for the `Open` state machine.
 */

import { isLiveProcessState, type HealthLevel, type ProjectSnapshot } from "./types";

/**
 * Health levels that expose [Fix this].
 *
 * `unknown` is excluded on purpose and it is NOT an oversight: a project that
 * declares no processes is grey, not amber — there is simply nothing to be
 * healthy about (the same distinction `HEALTH_DOT` draws on the card). Offering
 * "Fix this" there would invite an agent to hunt a failure that was never
 * observed, which is the "assert a cause nobody established" shape.
 */
const FIXABLE_LEVELS: readonly HealthLevel[] = ["amber", "red"];

/** Longest health reason we paste verbatim before truncating. */
const MAX_REASON_CHARS = 1200;

export function isFixable(level: HealthLevel | undefined): boolean {
  return level !== undefined && FIXABLE_LEVELS.includes(level);
}

/**
 * Whether this project should show [Fix this] right now.
 *
 * Requires a LOADED snapshot: while `snapshot` is undefined the card is
 * pending, not healthy, and an action that appears mid-load then vanishes is
 * worse than one that arrives a beat late.
 */
export function shouldOfferFix(snapshot: ProjectSnapshot | undefined): boolean {
  return isFixable(snapshot?.health?.level);
}

/** Collapse whitespace and clamp, so a huge stderr dump cannot swamp the prompt. */
function clamp(text: string, max: number): string {
  const t = text.trim();
  if (t.length <= max) return t;
  return `${t.slice(0, max).trimEnd()}\n…(truncated)`;
}

/**
 * Compose the prompt an agent starts from.
 *
 * Written as instructions to an agent rather than as a description of a
 * button: it names the project, states what was observed, and asks for a
 * diagnosis BEFORE a change. That last part matters — the health reason is a
 * symptom (a process is down, a port is unreachable), and an agent told only
 * "fix it" tends to restart the thing rather than find out why it stopped.
 *
 * Returns `null` when there is nothing to attach. A "Fix this" that opens an
 * agent with an empty prompt is exactly the blank text box §8.4 is trying to
 * avoid, so the caller should not offer the button at all in that case.
 */
export function buildFixPrompt(params: {
  projectName: string;
  projectPath?: string;
  snapshot: ProjectSnapshot | undefined;
}): string | null {
  const { projectName, projectPath, snapshot } = params;
  if (!shouldOfferFix(snapshot)) return null;

  const level = snapshot?.health?.level;
  const reason = snapshot?.health?.reason?.trim();

  // The processes that are NOT live are the concrete handle on the failure —
  // far more actionable than the level alone, and already in the snapshot.
  const down = (snapshot?.processes ?? [])
    .filter((p) => !isLiveProcessState(p.state))
    .map((p) => `  - ${p.name} (${p.state})`)
    .join("\n");

  const lines: string[] = [
    `The project "${projectName}" is reporting ${level === "red" ? "a failure" : "a problem"}.`,
  ];
  if (projectPath) lines.push(`Project root: ${projectPath}`);
  lines.push("");
  lines.push("What the dashboard observed:");
  lines.push(reason ? clamp(reason, MAX_REASON_CHARS) : "  (no reason text was recorded)");
  if (down) {
    lines.push("");
    lines.push("Processes not running:");
    lines.push(down);
  }
  lines.push("");
  lines.push(
    "Please find out WHY this is failing before changing anything — check the " +
      "process output and the recent logs first, then explain what you found and " +
      "fix it. If restarting would just hide the cause, say so instead.",
  );
  return lines.join("\n");
}
