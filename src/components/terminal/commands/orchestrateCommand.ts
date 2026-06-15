/**
 * Approach-D Conductor — Phase 5 `/orchestrate` command.
 *
 * Registers `/orchestrate <goal>` (or `/orchestrate <recipe-id> <args…>`) as
 * a SINGLE {@link CommandAction} through the existing
 * {@link useCommandAction} path. It is pattern-agnostic — there is NO change
 * to the command resolver/parser; the registry already projects every
 * registered action into the `Ctrl+Shift+K` palette
 * (`palette-projection.ts`) and the CommandBar, so `/orchestrate` surfaces
 * automatically the moment this hook mounts.
 *
 * Behavior:
 *  1. Parse the trailing argument. If the FIRST token names a known recipe
 *     ({@link resolveRecipe}), fill its `goal_template` with the remaining
 *     tokens → seeded `goal` + `default_phases`, and POST
 *     `{goal, recipe, phases}`. Otherwise treat the WHOLE argument as a
 *     free-form `goal` and POST `{goal}` with no recipe.
 *  2. On success, navigate/focus the run's page (`page_id = run_id`) so the
 *     operator sees the org-chart zone grid fill in (the workers share the
 *     run page — see `ai_session_executor::dispatch_subtask`).
 *  3. Surface failures through the normal command-feedback channel
 *     (`CommandResult { ok:false, code, message }`).
 *
 * The backend control surface is the `start_orchestration_run` Tauri command
 * (registered in `main.rs`), whose `args` mirror the HTTP
 * `POST /orchestration/runs` body — `{ goal, recipe?, phases? }`
 * (camelCase wire via `StartOrchestrationRunArgs`). The returned `Run` row
 * carries `run_id`, which is the page id workers land on.
 */

import { invoke } from "@tauri-apps/api/core";

import { resolveRecipe } from "./recipes";
import type { CommandAction, CommandResult, ResolverContext } from "./types";
import { useCommandAction } from "./useCommandAction";

/** The `Run` row returned by `start_orchestration_run` (serde snake_case). */
interface OrchestrationRun {
  run_id: string;
  goal: string;
  recipe: string | null;
  phases: string[];
  status: string;
  created_at: string;
  updated_at: string;
}

/**
 * Window CustomEvent name the `/orchestrate` handler dispatches to ask the
 * Terminal page-tab manager (`useTerminalPages`) to materialize + activate
 * the run's page. Mirrors the existing event-based navigation idiom
 * (`ui-bridge-navigate`, `navigate-to-active`, `productivity-set-view`):
 * keeping navigation event-driven means `/orchestrate` stays a
 * self-contained registry action and doesn't have to prop-drill a page
 * setter through App → providers → command ctx.
 */
export const TERMINAL_OPEN_PAGE_EVENT = "terminal-open-page";

/** Detail payload for {@link TERMINAL_OPEN_PAGE_EVENT}. */
export interface TerminalOpenPageDetail {
  /** The page id to add (if missing) and activate. For a run this is the run_id. */
  pageId: string;
  /** Human-readable tab label. */
  name: string;
}

/** Build a successful result. */
function ok<T>(value?: T): CommandResult<T> {
  return { ok: true, value };
}

/** Build a failure result with a stable machine code. */
function fail(code: string, message?: string): CommandResult<never> {
  return { ok: false, code, message };
}

/**
 * Split the raw `/orchestrate` argument into `{ firstToken, rest }`. The
 * resolver hands the full trailing string as `args.goal` (the registry is
 * pattern-agnostic — see the hook below for how the slash form maps the
 * trailing text into `goal`). We tokenize HERE so recipe resolution stays a
 * data lookup rather than a resolver concern.
 */
export function splitOrchestrateArg(raw: string): { firstToken: string; rest: string[] } {
  const trimmed = raw.trim();
  if (trimmed === "") return { firstToken: "", rest: [] };
  const tokens = trimmed.split(/\s+/);
  return { firstToken: tokens[0], rest: tokens.slice(1) };
}

/**
 * Pure planner: turn the raw argument into the `{goal, recipe?, phases?}`
 * body POSTed to the conductor. Exported for unit testing.
 *
 *  - `<recipe-id> <args…>` where the id is known → seeded goal from the
 *    template + the recipe's `default_phases` + `recipe` provenance.
 *  - anything else → the whole argument verbatim as `goal`, no recipe, no
 *    phases (the conductor's DESIGN bootstrap composes phases from the goal).
 */
export function planOrchestration(raw: string): {
  goal: string;
  recipe?: string;
  phases?: string[];
} | null {
  const trimmed = raw.trim();
  if (trimmed === "") return null;

  const { firstToken, rest } = splitOrchestrateArg(trimmed);
  const resolved = resolveRecipe(firstToken, rest);
  if (resolved) {
    return {
      goal: resolved.goal,
      recipe: resolved.recipe.id,
      phases: resolved.phases,
    };
  }
  // Free-form: the entire argument is the goal.
  return { goal: trimmed };
}

/**
 * Ask the Terminal page-tab manager to open + activate a page. Fire-and-
 * forget window event; `useTerminalPages` owns the persisted page list and
 * the active-page selection.
 */
function openRunPage(runId: string, goal: string): void {
  const name = goal.length > 24 ? `Run ${runId.slice(0, 8)}` : `Run: ${goal}`;
  const detail: TerminalOpenPageDetail = { pageId: runId, name };
  window.dispatchEvent(new CustomEvent(TERMINAL_OPEN_PAGE_EVENT, { detail }));
}

/**
 * Register the `/orchestrate` action. One `useCommandAction` call — exactly
 * the same registration path every other Terminal command uses.
 */
export function useOrchestrateCommand(): void {
  useCommandAction({
    id: "terminal.orchestrate",
    slash: "/orchestrate",
    aliases: ["/conduct", "/run-goal"],
    label: "Orchestrate a goal",
    description:
      "Start an Approach-D conductor run. `/orchestrate <goal>` runs a free-form goal; " +
      "`/orchestrate <recipe-id> <args…>` seeds the goal from a named recipe (e.g. " +
      "`/orchestrate website-to-mobile https://example.com`). Workers spawn into the " +
      "run's zone grid (page_id = run_id) so you watch the org-chart fill in live.",
    paramSchema: {
      goal: "string — a free-form goal, OR `<recipe-id> <args…>` to seed from a recipe",
    },
    // Tier-2 natural-language pattern: "orchestrate <anything>". The capture
    // group binds the trailing text to `args.goal` exactly like the slash
    // form, so no resolver change is needed — the pattern is declarative
    // data on the action, consistent with every other registry entry.
    patterns: [/^orchestrate\s+(?<goal>.+)$/i],
    handler: async (
      args: Record<string, unknown>,
      _ctx: ResolverContext,
    ): Promise<CommandResult<{ runId: string }>> => {
      const raw = typeof args.goal === "string" ? args.goal : "";
      const plan = planOrchestration(raw);
      if (!plan) return fail("invalid-args", "a goal (or recipe-id + args) is required");

      try {
        const run = await invoke<OrchestrationRun>("start_orchestration_run", {
          args: {
            goal: plan.goal,
            recipe: plan.recipe ?? null,
            phases: plan.phases ?? [],
          },
        });
        const runId = run?.run_id;
        if (!runId) {
          return fail("no-run-id", "conductor did not return a run id");
        }
        // Navigate to the run's page so workers (page_id = run_id) are visible.
        openRunPage(runId, plan.goal);
        return ok({ runId });
      } catch (e) {
        const msg = e instanceof Error ? e.message : String(e);
        return fail("start-failed", `failed to start orchestration run: ${msg}`);
      }
    },
  });
}
