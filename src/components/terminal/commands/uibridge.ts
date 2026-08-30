/**
 * UI Bridge ↔ command-registry delegation helper.
 *
 * The plan's §1.2 / §5(4) decision: registry replaces `useUIComponent`
 * for terminal-page actions, with a thin adapter that mirrors entries
 * back to UI Bridge so external agents see no change. Phase 1c
 * implements that as **inline delegation, not a wrapping abstraction**
 * — the existing `useUIComponent({...})` callsites in
 * `TerminalTabBar.tsx` (`terminal-launch-menu`) and
 * `ZoneLayoutPicker.tsx` (`zone-layout-picker`) keep their wire ids,
 * paramSchemas, and response shapes; their handlers just call
 * {@link callRegistry} instead of duplicating the underlying logic.
 *
 * Why inline rather than a wrapping `useRegistryToUIBridge` hook:
 *
 *  - The UI Bridge wire contract for the launch-menu actions includes a
 *    reshape from registry's raw `string[]` → `{success, tab_ids,
 *    task_run_ids}` per existing convention, plus UI side effects
 *    (close the launch menu popover). A generic adapter has to expose
 *    mapParams + mapResult + onSuccess hooks for each entry — by the
 *    time you spell that out, the existing inline handler IS clearer.
 *  - Only 4 actions across 2 sites are eligible. The "abstract once,
 *    reuse N times" math doesn't favor a wrapper at N=4.
 *  - External agents discover by `useUIComponent`'s component id; the
 *    less we change about the registration shape, the lower the risk of
 *    breaking those consumers in subtle ways.
 *
 * The drift question this resolves: today the UI Bridge handlers call
 * `onQuickLaunch` / `onLaunchAiSession` directly. The registry handlers
 * (Phase 1b) call the same closures. If someone later modifies the
 * spawn logic in only one path, the two surfaces diverge. After
 * Phase 1c the UI Bridge handlers route through the registry handler,
 * making the registry the single source of business logic. The reshape
 * + UI side effects stay at the UI Bridge boundary where they belong.
 */

import { bindDirect } from "./bind";
import { getById } from "./registry";
import type { CommandResult, ResolverContext } from "./types";

const NOT_FOUND = (actionId: string): string =>
  `Registry action "${actionId}" not found. ` +
  "If this fires from a UI Bridge handler, the terminal-page registry " +
  "may not have mounted yet — useTerminalCommands runs in TerminalPageInner.";

/**
 * Run a registry action by id, from a surface that is NOT the CommandBar.
 *
 * The three routes this replaces — `callRegistry`'s callers, the palette
 * projection, and `useKeyboardShortcuts`' bare
 * `getById(id)?.handler({}, {source: "hotkey"})` — reached handlers with
 * neither argument binding nor the arity gate. `SuggestionChip.tsx` is the
 * sharp end: it passes `chip.args`, authored by a rule, verbatim into a
 * handler. Nothing checked that those keys are ones the action declares or
 * that their values are values a command can take.
 *
 * `bindDirect` applies the same coercion and the same gate the CommandBar
 * applies, minus the two steps that need typed text. What is NOT applied is
 * positional parsing: a direct caller names its arguments, so there is no
 * ordering to infer and inventing one would be a second binding rule.
 *
 * Returns a `CommandResult` — every failure, including "no such action" and a
 * thrown handler, comes back as a value. {@link callRegistry} is the arm that
 * converts that to a throw for its wire contract; this is the arm that does
 * not, so a caller who wants to branch on the failure need not catch to do it.
 */
export async function runRegistryAction<T = unknown>(
  actionId: string,
  args: Record<string, unknown>,
  source: ResolverContext["source"] = "uibridge",
): Promise<CommandResult<T>> {
  const action = getById(actionId);
  if (!action) return { ok: false, code: "unknown-action", message: NOT_FOUND(actionId) };
  const bound = bindDirect(action, args);
  if (bound.refusal !== null) return { ok: false, code: "invalid-args", message: bound.refusal };
  try {
    return (await action.handler(bound.args, { source })) as CommandResult<T>;
  } catch (err) {
    return {
      ok: false,
      code: "handler-threw",
      message: err instanceof Error ? err.message : String(err),
    };
  }
}

/**
 * Invoke a registry action by id. Unwraps `CommandResult` — returns
 * `value` on success, throws on failure with the action's reported
 * `code` / `message`.
 *
 * The unwrap-by-throw INVERTS the `CommandResult` contract, and it stays,
 * because it is a WIRE contract rather than an internal convenience: the UI
 * Bridge handlers in `TerminalTabBar.tsx` / `ZoneLayoutPicker.tsx`,
 * `SuggestionChip.tsx`'s error copy and `TerminalPage.tsx`'s `no-account`
 * rewrite all catch it, and `/spawn`'s `string[]` is reshaped by its callers
 * into `{success, tab_ids, task_run_ids}`. Changing either shape breaks
 * external agents that discover these by wire id. {@link runRegistryAction} is
 * where the un-inverted contract lives for callers who want it.
 *
 * `source: "uibridge"` so handler-side telemetry (or future Tier-3
 * routing logs) can attribute the call to the UI Bridge surface.
 */
export async function callRegistry<T = unknown>(
  actionId: string,
  args: Record<string, unknown>,
): Promise<T> {
  const result = await runRegistryAction<T>(actionId, args);
  if (!result.ok) {
    throw new Error(result.message ?? result.code);
  }
  return result.value as T;
}
