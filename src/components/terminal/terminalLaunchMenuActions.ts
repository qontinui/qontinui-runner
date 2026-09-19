/**
 * terminalLaunchMenuActions — the four `terminal-launch-menu` UI Bridge
 * actions, as a pure function of the effects they invoke.
 *
 * ## Why they left `TerminalPage.tsx`
 *
 * `TerminalPage.tsx` transitively pulls xterm, Tauri IPC and a dozen contexts,
 * so nothing declared inside it can be unit-tested under the runner's
 * `environment: "node"` vitest config. That is not a stylistic complaint: a
 * manual-test round found the most costly unvalidated-bag defects in this app
 * INSIDE these handlers — `create-best-account(5)` starting a paid AI session
 * from a bag that was never an object — and there was no test file for
 * `TerminalPage.tsx`'s handlers at all. That absence was the defect behind the
 * defect.
 *
 * The handlers are the part with the semantics; the page is the part with the
 * closures. Splitting them puts the semantics somewhere a test can reach with
 * SPIES on `callRegistry` and `launchAiSession`, which is what makes "refused
 * with a completely empty effect wire" an assertion rather than an on-page
 * observation repeated by hand each round.
 *
 * ## Why every handler is guarded, and every action is still a plain literal
 *
 * `const {count = 1} = (params ?? {}) as {count?: number}` is not validation.
 * `Object.entries(5)` is `[]`, so the destructure yielded the DEFAULTS and the
 * action ran bare — measured, one batch, identical input `5`:
 *
 *     create-ai-session(5)   → throws, wire [], 0 created
 *     create-plain(5)        → success: true, wire ['terminal_create'], live 0 → 1
 *     create-best-account(5) → success: true, wire ['terminal_create',
 *                              'build_ai_launch_command', 'terminal_write']
 *
 * So each `handler` is a {@link guardedHandler}. The action DEF stays an
 * object literal carrying its `effect` safety class, because the component
 * `effect` walk (`src/lib/ui-bridge/action-effect-coverage.test.ts`) and the
 * action-surface enumeration (`src/lib/ui-bridge/actionSurfaces.ts`) both read
 * the literal out of the AST — the walk follows `actions:
 * buildTerminalLaunchMenuActions(…)` into this function's returned array.
 *
 * Originally written for qontinui-runner#1301; ported onto main's `effect`
 * annotations and `aiSessionSpawnEnvelope` (qontinui-runner e910610f2).
 */

import { guardedHandler } from "@/lib/ui-bridge/guardedHandler";
import { textArg } from "./commands/parse";
import { buildAiSessionSpawnEnvelope, type AiSessionSpawnEnvelope } from "./aiSessionSpawnEnvelope";

/**
 * `paramSchema`s hoisted so the registration and the guard read ONE
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
   * at `context.replace(…)`, AFTER a PTY existed.
   */
  launchAiSession: (count: number, configDir: string, context?: string) => Promise<string[]>;
}

/** The wire envelope the spawn actions answer with. */
type SpawnResult = AiSessionSpawnEnvelope;

/** The action shape `useUIComponent({ actions })` takes, with `effect` required. */
export interface LaunchMenuActionDef {
  id: string;
  label: string;
  description: string;
  paramSchema: Readonly<Record<string, string>>;
  effect: "read" | "write" | "destructive";
  handler: (params?: unknown) => Promise<SpawnResult>;
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

export function buildTerminalLaunchMenuActions(effects: LaunchMenuEffects): LaunchMenuActionDef[] {
  return [
    {
      id: "create-plain",
      label: "Create Plain Terminal",
      description: "Spawn N blank terminals using the user's default shell.",
      paramSchema: CREATE_PLAIN_SCHEMA,
      // `write` — N blank shells, no command typed. Same reasoning as
      // `terminal-page.create-terminal`.
      effect: "write",
      handler: guardedHandler("create-plain", CREATE_PLAIN_SCHEMA, async (args) => {
        const count = readCount(args);
        if (count === null) {
          throw new Error("create-plain requires { count: number } where count >= 1");
        }
        const tabIds = await effects.callRegistry<string[]>("terminal.spawn", { count });
        return { success: true as const, tab_ids: tabIds, task_run_ids: [] };
      }),
    },
    {
      id: "create-ai-session",
      label: "Create AI Session",
      description:
        "Spawn N terminals pre-configured to launch `claude` under the given CLAUDE_CONFIG_DIR, optionally pre-typing a context prompt.",
      paramSchema: CREATE_AI_SESSION_SCHEMA,
      // `destructive` — launches N autonomous Claude agents under a caller-supplied
      // config dir, optionally auto-typing a prompt. Dim 2: an agent writes to the
      // operator's repositories and spends account budget; dim 1: those edits and that
      // spend are not undone by closing the tab.
      effect: "destructive",
      handler: guardedHandler("create-ai-session", CREATE_AI_SESSION_SCHEMA, async (args) => {
        const count = readCount(args);
        // `textArg` for the two text fields, exactly as the registry's
        // `terminal.spawn-ai` handler reads them: binding coerces a clean
        // numeric token to a number, so `context: "5"` is `5` by the time it
        // gets here and only `textArg` turns it back into the text the caller
        // supplied.
        const configDir = textArg(args, "configDir");
        const context = textArg(args, "context") || undefined;
        if (!configDir) {
          throw new Error(
            "create-ai-session requires { count?: number, configDir: string, context?: string }",
          );
        }
        if (count === null) throw new Error("create-ai-session: count must be a positive number");
        // Through the same `spawnVerdict` every OTHER spawn surface reaches via
        // `callRegistry` — see `aiSessionSpawnEnvelope` for why this one action
        // did not, and what #1169 widened.
        return buildAiSessionSpawnEnvelope(
          await effects.launchAiSession(count, configDir, context),
          count,
        );
      }),
    },
    {
      id: "create-best-account",
      label: "Create AI Session with Best Account",
      description:
        "Like create-ai-session, but picks the AI account with the lowest current utilization. Fails if no accounts are configured.",
      paramSchema: CREATE_BEST_ACCOUNT_SCHEMA,
      // `destructive` — `create-ai-session` with account selection done for you. Same
      // score, and it additionally consumes the least-utilized account without asking.
      effect: "destructive",
      handler: guardedHandler("create-best-account", CREATE_BEST_ACCOUNT_SCHEMA, async (args) => {
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
        return { success: true as const, tab_ids: tabIds, task_run_ids: tabIds.map(() => null) };
      }),
    },
    {
      id: "create-with-command",
      label: "Create Terminal with Command",
      description:
        "Spawn N terminals and auto-type the given shell command into each after the prompt renders.",
      paramSchema: CREATE_WITH_COMMAND_SCHEMA,
      // `destructive` — spawns N shells and auto-types an arbitrary `command` into each.
      // Nothing about the action bounds what that command does, so its blast radius is
      // the parameter's, not the action's: unclassifiable at the call site, and the
      // rubric's fail-closed rule makes unclassifiable destructive.
      effect: "destructive",
      handler: guardedHandler("create-with-command", CREATE_WITH_COMMAND_SCHEMA, async (args) => {
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
        return { success: true as const, tab_ids: tabIds, task_run_ids: [] };
      }),
    },
  ];
}
