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

import { getById } from "./registry";

/**
 * Invoke a registry action by id. Unwraps `CommandResult` — returns
 * `value` on success, throws on failure with the action's reported
 * `code` / `message`.
 *
 * `source: "uibridge"` so handler-side telemetry (or future Tier-3
 * routing logs) can attribute the call to the UI Bridge surface.
 */
export async function callRegistry<T = unknown>(
  actionId: string,
  args: Record<string, unknown>,
): Promise<T> {
  const action = getById(actionId);
  if (!action) {
    throw new Error(
      `Registry action "${actionId}" not found. ` +
        "If this fires from a UI Bridge handler, the terminal-page registry " +
        "may not have mounted yet — useTerminalCommands runs in TerminalPageInner.",
    );
  }
  const result = await action.handler(args, { source: "uibridge" });
  if (!result.ok) {
    throw new Error(result.message ?? result.code);
  }
  return result.value as T;
}
