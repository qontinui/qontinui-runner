/**
 * React hook that registers a {@link CommandAction} with the command
 * registry on mount and unregisters on unmount.
 *
 * Mirrors the `useUIComponent` pattern from `@qontinui/ui-bridge` — the
 * caller passes a fresh action object on every render but the registry
 * sees a stable handler ref. This is essential because action handlers
 * almost always close over React state (e.g. `zoneLayout.focusedZone`),
 * and a stale closure captured at registration time would silently
 * operate on the wrong session.
 *
 * The pattern is identical to `useUIComponent`'s `actionsRef` trick in
 * `ui-bridge/packages/ui-bridge/src/react/useUIComponent.ts:147,192-205`:
 * we snapshot the latest action into a ref on every render, then the
 * registered handler wrapper looks up the current ref each invocation.
 *
 * Strict-mode / fast-refresh safety: the registry's `register()` is
 * idempotent on id collision (replaces in place), so a double-mount in
 * dev does the right thing.
 */

import { useEffect, useRef } from "react";
import { register, unregister } from "./registry";
import type { CommandAction, CommandResult, ResolverContext } from "./types";

export function useCommandAction<TArgs = Record<string, unknown>, TResult = unknown>(
  action: CommandAction<TArgs, TResult>,
): void {
  const actionRef = useRef(action);
  actionRef.current = action;

  useEffect(() => {
    const stable: CommandAction<TArgs, TResult> = {
      ...action,
      handler: (args: TArgs, ctx: ResolverContext): Promise<CommandResult<TResult>> =>
        actionRef.current.handler(args, ctx),
    };
    register(stable as unknown as CommandAction);
    return () => {
      unregister(action.id);
    };
    // The effect intentionally re-runs ONLY on id change. Handler / label /
    // slash / paramSchema updates flow through `actionRef.current` without
    // re-registering, matching `useUIComponent`'s in-place update semantics.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [action.id]);
}
