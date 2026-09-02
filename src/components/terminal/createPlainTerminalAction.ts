/**
 * `create-plain-terminal` — a discoverable UI Bridge action on the
 * `terminal-page` component that spawns a plain (non-AI) PTY terminal into
 * the active page/zone and mounts its `<TerminalInstance>` (xterm).
 *
 * Why this exists (real friction): when external automation/verification
 * needs to drive the now-primary terminal surface headlessly, there was no
 * discoverable on-page action that *creates and mounts* a plain terminal.
 * The auto-discovered `button-add-terminal-page-1` only adds an empty
 * page/zone (no session), `tab-switch-to-terminal` only navigates, and the
 * `terminal-launch-menu.create-plain` action lives on the launch-menu
 * component (and reshapes to a registry `{success, tab_ids}` envelope). This
 * action surfaces the plain-terminal create directly on the `terminal-page`
 * component so a single invocation reliably yields a mounted xterm.
 *
 * It deliberately routes through the SAME frontend create path the launch
 * menu uses — `createAndAssignTerminal()` (TerminalPage → useZoneActions) —
 * which calls `createTerminal()` and then auto-adjusts the zone layout so the
 * new tab is assigned to a zone and an `<TerminalInstance>` mounts. It is NOT
 * a raw backend `POST /terminals` (that would not mount the xterm here).
 *
 * Robust-from-fresh-page: `useTerminalPages` always seeds a `"default"` page
 * and `createAndAssignTerminal` grows/selects the zone layout as needed, so a
 * single invocation on a cold page still produces a mounted xterm.
 *
 * Extracted as a pure factory (no React, no Tauri) so the action wiring is
 * unit-testable under the runner's `environment: "node"` vitest config —
 * same precedent as `LaunchMenu`'s exported pure helpers.
 */

import { guardedAction, type GuardedComponentAction } from "@/lib/ui-bridge/guardedAction";

/** The action id agents invoke on the `terminal-page` component. */
export const CREATE_PLAIN_TERMINAL_ACTION_ID = "create-plain-terminal";

/**
 * The shape this factory produces.
 *
 * It is the guard's own `GuardedComponentAction`, aliased rather than
 * re-declared: this action USED to declare its own structural copy, and a
 * hand-written copy of a shape is exactly how a surface drifts out of the one
 * mechanism that governs it. `label` is narrowed to required because this
 * factory always supplies one.
 */
export type PlainTerminalActionDef = GuardedComponentAction & { label: string };

/**
 * Build the `create-plain-terminal` action def.
 *
 * ## Why this is a `guardedAction` and not a bare handler
 *
 * It was a bare arity-0 handler, and an arity-0 handler cannot be INFLUENCED
 * by a bag — which is a true statement about the wrong question. Measured on
 * the page, `create-plain-terminal({zzz: "x"})` answered `success: true` over
 * a key the action does not declare AND SPAWNED A PTY. A process started for
 * an argument nobody checked, reported as success, is a wrong answer whether
 * or not the argument reached the effect.
 *
 * `paramSchema: {}` is what makes "takes no arguments" enforced rather than
 * merely documented: `bindSchemaBag` refuses a non-object bag and every
 * undeclared key BEFORE `run` is entered — before the PTY.
 *
 * @param createAndAssignTerminal the TerminalPage create path that spawns a
 *   plain PTY tab, assigns it to a zone, and mounts its xterm. Returns the new
 *   tab id (or `null`/`undefined` if the backend declined to create one).
 */
export function buildCreatePlainTerminalAction(
  createAndAssignTerminal: (title?: string) => Promise<string | null>,
): PlainTerminalActionDef {
  return guardedAction({
    id: CREATE_PLAIN_TERMINAL_ACTION_ID,
    label: "Create Plain Terminal",
    description:
      "Spawn one plain (non-AI) PTY terminal in the user's default shell, assign it to " +
      "the active page's next free zone, and mount its xterm. Robust from a fresh page. " +
      "Returns { success, tab_id }. Takes no arguments — any supplied key is refused " +
      "before the PTY is spawned. Use this to drive the terminal surface headlessly.",
    paramSchema: {},
    run: async () => {
      const tabId = await createAndAssignTerminal();
      return { success: Boolean(tabId), tab_id: tabId ?? null };
    },
  }) as PlainTerminalActionDef;
}
