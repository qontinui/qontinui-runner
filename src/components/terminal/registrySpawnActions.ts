/**
 * Registry-backed terminal spawn actions — the AI-spawn affordances that the
 * NL planner relies on ("open an ai session in the terminal page" →
 * `create-best-account`).
 *
 * Why this factory exists (root-cause fix): these spawn actions historically
 * lived ONLY on the `terminal-launch-menu` UI Bridge component. That component
 * is mounted with `TerminalPage`, but the launch-menu surface is only the
 * *initial / empty-state* affordance — once a session has been created and
 * then closed the terminal page renders a different view, and headless
 * automation reproduced `component "terminal-launch-menu" never registered`
 * because the spawn affordance was not reachable in that state. The fix is to
 * make these registry-only spawn actions reachable WHENEVER the terminal page
 * is active by ALSO registering them on the always-mounted `terminal-page`
 * component (which registers even on the page's `!initialized` early-return
 * path). Extracting them here keeps the two registration sites in lockstep —
 * one source of truth for the action ids, descriptions, param schemas, and
 * handlers.
 *
 * Every handler here is a pure delegation to `callRegistry(...)` — it does NOT
 * close over any React state or any "menu open" UI state, so it works
 * identically whether or not the launch menu is visually open and regardless
 * of prior page state. (The `create-ai-session` action is intentionally NOT
 * here: it closes over the page-local `handleLaunchAiSession` closure and so
 * cannot be hoisted to a pure factory. It remains registered on
 * `terminal-launch-menu` only — the planner's spawn flow uses
 * `create-best-account`, which IS here.)
 *
 * Extracted as a pure factory (no React, no Tauri at module scope) so the
 * action wiring is unit-testable under the runner's `environment: "node"`
 * vitest config — same precedent as `createPlainTerminalAction`.
 */

/** Minimal shape of a UI Bridge component action (matches the SDK's ComponentActionDef). */
export interface RegistrySpawnActionDef {
  id: string;
  label: string;
  description: string;
  paramSchema?: Record<string, unknown>;
  handler: (params?: unknown) => Promise<unknown>;
}

/**
 * The registry envelope every spawn action returns. `task_run_ids` is parallel
 * to `tab_ids` (one entry per spawned tab); entries are `null` for spawn paths
 * that don't yet thread a task-run id.
 */
export interface SpawnEnvelope {
  success: true;
  tab_ids: string[];
  task_run_ids: Array<string | null>;
}

/** The `callRegistry` shape this factory needs (matches `commands/callRegistry`). */
export type CallRegistry = <T>(action: string, args: Record<string, unknown>) => Promise<T>;

/**
 * Build the registry-backed spawn action defs shared by `terminal-launch-menu`
 * and `terminal-page`.
 *
 * @param callRegistry the terminal command-registry dispatcher
 *   (`commands/callRegistry`). Injected so this stays a pure, testable factory.
 */
export function buildRegistrySpawnActions(callRegistry: CallRegistry): RegistrySpawnActionDef[] {
  return [
    {
      id: "create-plain",
      label: "Create Plain Terminal",
      description: "Spawn N blank terminals using the user's default shell.",
      paramSchema: { count: "number (>= 1, defaults to 1)" },
      handler: async (params?: unknown) => {
        const { count = 1 } = (params ?? {}) as { count?: number };
        if (typeof count !== "number" || count < 1) {
          throw new Error("create-plain requires { count: number } where count >= 1");
        }
        const tabIds = await callRegistry<string[]>("terminal.spawn", { count });
        return {
          success: true,
          tab_ids: tabIds,
          task_run_ids: [] as Array<string | null>,
        } satisfies SpawnEnvelope;
      },
    },
    {
      id: "create-best-account",
      label: "Create AI Session with Best Account",
      description:
        "Like create-ai-session, but picks the AI account with the lowest current utilization. " +
        "Fails if no accounts are configured. Spawns a `claude` session and mounts it in a " +
        "terminal pane. Reachable whenever the terminal page is active — no menu need be open.",
      paramSchema: {
        count: "number (>= 1, defaults to 1)",
        context: "string (optional initial prompt auto-typed after claude starts)",
      },
      handler: async (params?: unknown) => {
        const { count = 1, context } = (params ?? {}) as {
          count?: number;
          context?: string;
        };
        if (typeof count !== "number" || count < 1) {
          throw new Error("create-best-account: count must be a positive number");
        }
        // Delegate to registry `terminal.spawn-ai` with the literal
        // `account: "best"`. The registry handler does the lowest-utilization
        // lookup; we rethrow `no-account` as the original "No AI accounts
        // available" wording so existing automation regexes keep matching.
        let tabIds: string[];
        try {
          tabIds = await callRegistry<string[]>("terminal.spawn-ai", {
            count,
            account: "best",
            context,
          });
        } catch (err) {
          const msg = err instanceof Error ? err.message : String(err);
          if (msg.includes("no-account") || msg.toLowerCase().includes("no matching")) {
            throw new Error("No AI accounts available", { cause: err });
          }
          throw err;
        }
        return {
          success: true,
          tab_ids: tabIds,
          task_run_ids: tabIds.map(() => null) as Array<string | null>,
        } satisfies SpawnEnvelope;
      },
    },
    {
      id: "create-with-command",
      label: "Create Terminal with Command",
      description:
        "Spawn N terminals and auto-type the given shell command into each after the prompt renders.",
      paramSchema: {
        count: "number (>= 1, defaults to 1)",
        command: "string (the shell command to type + Enter, required)",
      },
      handler: async (params?: unknown) => {
        const { count = 1, command } = (params ?? {}) as {
          count?: number;
          command?: string;
        };
        if (!command)
          throw new Error("create-with-command requires { count?: number, command: string }");
        if (typeof count !== "number" || count < 1) {
          throw new Error("create-with-command: count must be a positive number");
        }
        const tabIds = await callRegistry<string[]>("terminal.spawn-with", {
          count,
          command,
        });
        return {
          success: true,
          tab_ids: tabIds,
          task_run_ids: [] as Array<string | null>,
        } satisfies SpawnEnvelope;
      },
    },
  ];
}
