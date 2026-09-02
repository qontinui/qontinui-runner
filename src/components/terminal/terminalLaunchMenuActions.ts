/**
 * terminalLaunchMenuActions — the four `terminal-launch-menu` UI Bridge
 * actions, as a pure function of the effects they invoke.
 *
 * ## Why they left `TerminalPage.tsx`
 *
 * `TerminalPage.tsx` is 1700 lines and transitively pulls xterm, Tauri IPC and
 * a dozen contexts, so nothing declared inside it can be unit-tested under the
 * runner's `environment: "node"` vitest config. That is not a stylistic
 * complaint: iteration 12 of the manual-test loop found the four most costly
 * unvalidated-bag defects in this app INSIDE those handlers —
 * `create-best-account(5)` starting a paid AI session from a bag that was
 * never an object — and recorded that "there is currently no test file for
 * `TerminalPage.tsx`'s handlers at all; that absence is the defect behind the
 * defect".
 *
 * The handlers are the part with the semantics; the page is the part with the
 * closures. Splitting them puts the semantics somewhere a test can reach with
 * SPIES on `callRegistry` and `launchAiSession`, which is what makes
 * "refused with a completely empty effect wire" an assertion rather than an
 * on-page observation that has to be repeated by hand each round.
 *
 * ## Why every one is a `guardedAction`
 *
 * See `lib/ui-bridge/guardedAction.ts`. In short: `const {count = 1} =
 * (params ?? {}) as {count?: number}` is not validation. `Object.entries(5)`
 * is `[]`, so the destructure yielded the DEFAULTS and the action ran bare —
 * measured, one batch, identical input `5`:
 *
 *     create-ai-session(5)   → throws, wire [], 0 created
 *     create-plain(5)        → success: true, wire ['terminal_create'], live 0 → 1
 *     create-best-account(5) → success: true, wire ['terminal_create',
 *                              'build_ai_launch_command', 'terminal_write'],
 *                              the write being `claude --session-id … --config-dir …`
 *
 * Three of the four siblings in one `actions: [...]` array answered the same
 * malformed input three different ways, and the one that refused was the one
 * a previous round had been pointed at.
 */

import { guardedAction, type GuardedComponentAction } from "@/lib/ui-bridge/guardedAction";
import { textArg } from "./commands/parse";

/**
 * `paramSchema`s hoisted so the registration and the binder read ONE
 * declaration. Inlining a schema and then re-typing its field names in a cast
 * inside the handler is how the two drifted: `create-ai-session` declared
 * `context: "string (optional …)"` on the wire and accepted `{}` at runtime.
 */
export const CREATE_PLAIN_SCHEMA = {
  count: "number (>= 1, defaults to 1)",
} as const;

export const CREATE_AI_SESSION_SCHEMA = {
  count: "number (>= 1, defaults to 1)",
  configDir: "string (absolute path to a Claude Code config dir, required)",
  context: "string (optional initial prompt auto-typed after claude starts)",
} as const;

export const CREATE_BEST_ACCOUNT_SCHEMA = {
  count: "number (>= 1, defaults to 1)",
  context: "string (optional initial prompt auto-typed after claude starts)",
} as const;

export const CREATE_WITH_COMMAND_SCHEMA = {
  count: "number (>= 1, defaults to 1)",
  command: "string (the shell command to type + Enter, required)",
} as const;

/** What the page supplies. Exactly the two effects these four reach for. */
export interface LaunchMenuEffects {
  /** The command registry funnel — itself binding, via `bindDirect`. */
  callRegistry: <T>(actionId: string, args: Record<string, unknown>) => Promise<T>;
  /**
   * The page's own AI-session launcher.
   *
   * `create-ai-session` is the one launch-menu action that does NOT route
   * through `callRegistry`: `configDir` and the operator's `account` label are
   * different abstractions and the wire contract takes the raw `configDir` for
   * historical reasons. That is precisely why it was the one that used to
   * reach a spawn closure with the caller's raw JSON and die 750 lines later
   * at `context.replace(…)`, AFTER a PTY existed, showing the operator
   * `od.replace is not a function`.
   */
  launchAiSession: (
    count: number,
    configDir: string,
    context?: string,
  ) => Promise<string[] | null | undefined> | string[] | null | undefined;
}

/** The wire envelope all four return. */
interface SpawnResult {
  success: true;
  tab_ids: string[];
  task_run_ids: Array<string | null>;
}

/**
 * Read the `count` a bound bag carries.
 *
 * Binding has already refused a non-scalar, so the only thing left to reject
 * is a supplied value that is not a usable count — `{count: "abc"}` survives
 * coercion as the string `"abc"`. Each caller keeps its own sentence, because
 * automation regexes match on them.
 */
function readCount(args: Record<string, unknown>): number | null {
  const { count = 1 } = args as { count?: unknown };
  return typeof count === "number" && Number.isFinite(count) && count >= 1 ? count : null;
}

export function buildTerminalLaunchMenuActions(
  effects: LaunchMenuEffects,
): GuardedComponentAction[] {
  return [
    guardedAction({
      id: "create-plain",
      label: "Create Plain Terminal",
      description: "Spawn N blank terminals using the user's default shell.",
      paramSchema: CREATE_PLAIN_SCHEMA,
      run: async (args): Promise<SpawnResult> => {
        const count = readCount(args);
        if (count === null) {
          throw new Error("create-plain requires { count: number } where count >= 1");
        }
        const tabIds = await effects.callRegistry<string[]>("terminal.spawn", { count });
        return { success: true, tab_ids: tabIds, task_run_ids: [] };
      },
    }),
    guardedAction({
      id: "create-ai-session",
      label: "Create AI Session",
      description:
        "Spawn N terminals pre-configured to launch `claude` under the given CLAUDE_CONFIG_DIR, optionally pre-typing a context prompt.",
      paramSchema: CREATE_AI_SESSION_SCHEMA,
      run: async (args): Promise<SpawnResult> => {
        const count = readCount(args);
        // `textArg` for the two text fields, exactly as the registry's
        // `terminal.spawn-ai` handler reads them: binding coerces a clean
        // numeric token to a number, so `context: "5"` is `5` by the time it
        // gets here and only `textArg` turns it back into the text the caller
        // supplied. Skipping that is how `/spawn-with 2 5` once reported
        // "command is required" for a command that was supplied.
        const configDir = textArg(args, "configDir");
        const context = textArg(args, "context") || undefined;
        if (!configDir) {
          throw new Error(
            "create-ai-session requires { count?: number, configDir: string, context?: string }",
          );
        }
        if (count === null) throw new Error("create-ai-session: count must be a positive number");
        const tabIds = ((await effects.launchAiSession(count, configDir, context)) ??
          []) as string[];
        return { success: true, tab_ids: tabIds, task_run_ids: tabIds.map(() => null) };
      },
    }),
    guardedAction({
      id: "create-best-account",
      label: "Create AI Session with Best Account",
      description:
        "Like create-ai-session, but picks the AI account with the lowest current utilization. Fails if no accounts are configured.",
      paramSchema: CREATE_BEST_ACCOUNT_SCHEMA,
      run: async (args): Promise<SpawnResult> => {
        const count = readCount(args);
        const context = textArg(args, "context") || undefined;
        if (count === null) throw new Error("create-best-account: count must be a positive number");
        // Delegate to registry `terminal.spawn-ai` with the literal
        // `account: "best"`. The registry handler does the lowest-utilization
        // lookup; `no-account` is rethrown as the original "No AI accounts
        // available" wording so existing automation regexes keep matching.
        let tabIds: string[];
        try {
          tabIds = await effects.callRegistry<string[]>("terminal.spawn-ai", {
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
        return { success: true, tab_ids: tabIds, task_run_ids: tabIds.map(() => null) };
      },
    }),
    guardedAction({
      id: "create-with-command",
      label: "Create Terminal with Command",
      description:
        "Spawn N terminals and auto-type the given shell command into each after the prompt renders.",
      paramSchema: CREATE_WITH_COMMAND_SCHEMA,
      run: async (args): Promise<SpawnResult> => {
        const count = readCount(args);
        const command = textArg(args, "command");
        if (!command) {
          throw new Error("create-with-command requires { count?: number, command: string }");
        }
        if (count === null) throw new Error("create-with-command: count must be a positive number");
        const tabIds = await effects.callRegistry<string[]>("terminal.spawn-with", {
          count,
          command,
        });
        return { success: true, tab_ids: tabIds, task_run_ids: [] };
      },
    }),
  ];
}
