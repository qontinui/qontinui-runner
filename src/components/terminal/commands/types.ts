/**
 * Action registry — type surface.
 *
 * One source of truth for slash commands, the Ctrl+Shift+K palette, the
 * future CommandBar, suggestion-chip apply buttons, and (Phase 8) AI
 * tool-call routing via the `claude` CLI subprocess. Every projection
 * reads from the same {@link CommandAction} list — adding a new layout,
 * for instance, becomes one `useCommandAction` call instead of edits in
 * `ZoneLayoutPicker`, `useKeyboardShortcuts`, the UI Bridge action list,
 * and the launch menu.
 *
 * See `plans/2026-05-28-terminal-page-redesign-plan.md` §1 + the audit
 * at `./audit.md` for the design context and the initial 12-entry target.
 */

/**
 * Result returned by a {@link CommandAction.handler}. The shape mirrors
 * the resolver tiers in the plan: handlers signal success/failure with a
 * machine-readable code so the CommandBar can render appropriate UI
 * (Undo affordance, confirm modal, "did you mean?" row).
 *
 * Handlers may throw instead of returning `ok: false` — the executor
 * normalizes thrown errors into a `CommandResult` with `ok: false` and
 * `code: "handler-threw"`. Use the typed return only when the failure is
 * a *predictable* outcome the caller should branch on (e.g. restart
 * gating on session state, action requires confirmation).
 */
export type CommandResult<T = unknown> =
  | { ok: true; value?: T }
  | { ok: false; code: string; message?: string };

/**
 * Context handed to every action handler. Phase 1 plumbing keeps this
 * minimal — `signal` for cancellable invocations (the CommandBar's
 * "Interpreting…" cancel button in Phase 8, the suggestion-chip dismiss
 * in Phase 4) and `source` so handlers can branch on invocation surface
 * if needed.
 *
 * Phase-2+ will widen this to include refs to the page's
 * `TerminalSession` context shape so handlers don't need to be
 * re-registered on every render. For now handlers close over their
 * registration-site context directly — the ref pattern in
 * {@link useCommandAction} keeps that closure current.
 */
export interface ResolverContext {
  /** Aborted when the user dismisses an in-flight Tier-3 interpretation. */
  signal?: AbortSignal;
  /**
   * Where the invocation originated; useful for telemetry + result UI.
   *
   * - `slash` / `pattern` / `ai` / `palette` / `chip` / `hotkey` —
   *   operator-facing surfaces.
   * - `uibridge` — external-agent invocation routed through the
   *   `useUIComponent` adapter (`commands/uibridge.ts`). Phase 1c added
   *   this so handler-side telemetry can attribute calls coming from
   *   the existing UI Bridge wire ids (`create-plain`, `create-best-
   *   account`, `create-with-command`, `select-layout`).
   * - `test` — synthetic invocation from unit tests.
   */
  source:
    | "slash"
    | "pattern"
    | "ai"
    | "palette"
    | "chip"
    | "hotkey"
    | "uibridge"
    | "test";
}

/**
 * The unit of an action in the registry. Self-registered via
 * {@link useCommandAction} on mount, unregistered on unmount.
 *
 * Field invariants worth keeping in mind:
 *
 * - `id` is the wire contract. External agents (Claude subprocesses
 *   spawned via `agent_runtime.rs`, UI Bridge consumers) discover by
 *   id. Renaming an id is a breaking change.
 * - `slash` is the primary alias surfaced to operators. Aliases
 *   live in {@link CommandAction.aliases}.
 * - `paramSchema` follows the UI Bridge contract at
 *   `ui-bridge/packages/ui-bridge/src/react/useUIComponent.ts:27` —
 *   `Record<string, unknown>`. There is **no** exported `JsonSchema` in
 *   `@qontinui/ui-bridge` today; the plan §1.1 calls for introducing
 *   one in Phase 1b alongside the UI Bridge migration. Until then the
 *   shape stays loose and Tier-3 (Phase 8) is responsible for
 *   normalising it into an Anthropic-compatible `input_schema` at
 *   subprocess-spawn time.
 * - `patterns` are declarative regex rules for Tier 2; capture groups
 *   map positionally into `paramSchema` field order. Live alongside
 *   the action def so adding a phrasing doesn't require touching the
 *   resolver. Empty/undefined = the action only matches by slash + AI.
 * - `destructive` gates the executor's confirm modal. Today only used
 *   by multi-target close/kill actions per plan §5(3); single-target
 *   destructive actions rely on `undoable` + a 3s Undo affordance.
 */
export interface CommandAction<TArgs = Record<string, unknown>, TResult = unknown> {
  /** Stable wire contract, e.g. `"terminal.spawn-ai"`. */
  id: string;
  /** Primary slash form, e.g. `"/spawn-ai"`. */
  slash: string;
  /** Additional slash forms accepted by the Tier-1 resolver. */
  aliases?: string[];
  /** Display label for the palette + chip + autocomplete row. */
  label: string;
  /** Longer help, also fed verbatim into the Tier-3 tool definition. */
  description: string;
  /** UI Bridge-compatible loose schema; see invariants above. */
  paramSchema?: Record<string, unknown>;
  /** Regex patterns for Tier-2 matching. */
  patterns?: RegExp[];
  /** Requires a confirm modal before execution. See plan §5(3). */
  destructive?: boolean;
  /** Pre-state snapshot captured by the executor for Undo. */
  undoable?: boolean;
  /** The action body. */
  handler: (args: TArgs, ctx: ResolverContext) => Promise<CommandResult<TResult>>;
}

/** Subscriber callback signature for the registry's change broadcast. */
export type RegistryListener = () => void;
