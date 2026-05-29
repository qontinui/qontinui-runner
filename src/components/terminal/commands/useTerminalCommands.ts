/**
 * Registers the Phase 1 terminal-page action set with the command
 * registry. One call per Terminal page mount; unregisters on unmount.
 *
 * Action coverage matches `./audit.md`'s top-10 collapsed to 10 registry
 * entries:
 *
 *   - `terminal.focus`       — `/focus <n|next|prev|needs-input>`
 *   - `terminal.spawn`       — `/spawn`
 *   - `terminal.spawn-ai`    — `/spawn-ai` (alias `/spawn-best`; `account`
 *                              accepts `"best"` per the Phase 1b decision
 *                              to collapse `create-best-account` into a
 *                              single registry entry — clean code over
 *                              two parallel handlers)
 *   - `terminal.spawn-with`  — `/spawn-with`
 *   - `terminal.approve-all` — `/approve-all`
 *   - `terminal.maximize`    — `/maximize [<n>]`
 *   - `terminal.close`       — `/close [<n>]`
 *   - `terminal.layout`      — `/layout <preset>`
 *   - `terminal.restart`     — `/restart [<n>]`
 *   - `terminal.swap`        — `/swap <a> <b>`
 *
 * The existing `useUIComponent` registrations in `TerminalPage.tsx`,
 * `TerminalTabBar.tsx`, `ZoneLayoutPicker.tsx`, and
 * `ZoneProfilePicker.tsx` remain UNCHANGED in this commit. Phase 1c will
 * introduce the UI Bridge adapter that projects registry entries into
 * those component-level action ids; until then both surfaces coexist
 * and source from the same underlying functions (`onQuickLaunch`,
 * `onLaunchAiSession`, `zoneLayout.*`, `closeTerminal`,
 * `transitionEffects.handleRestartInZone`) so drift is structurally
 * prevented.
 *
 * See the design rationale in `plans/2026-05-28-terminal-page-redesign-plan.md`
 * §1 and the per-action keystroke budget in `./audit.md`.
 */

import { instanceStorage } from "@/lib/instance-storage";

import { useTerminalSession, useTransitionEffects, useUIStateCx } from "../contexts";
import type { AccountUsageInfo } from "../useSessionManager";
import type { CommandAction, CommandResult, ResolverContext } from "./types";
import { useCommandAction } from "./useCommandAction";

/**
 * Inputs that can't be read from the existing React contexts — handed in
 * by `TerminalPageInner` because the spawn closures it constructs hold
 * page-local state (layout picker, write-when-ready cadence) that we
 * don't want to duplicate here. Reading everything else (`tabs`,
 * `activeId`, `zoneLayout`, `closeTerminal`, `sessionStates`,
 * `terminalRefs`, `transitionEffects.handleRestartInZone`) directly from
 * the contexts keeps this hook trivially update-safe.
 */
export interface TerminalCommandsContext {
  /**
   * Spawn N plain PTY tabs and zone-assign them. Mirrors the closure at
   * `TerminalPage.tsx:539` (passed down as `onQuickLaunch` to
   * `TerminalTabBar`). Returns the new tab ids in creation order.
   */
  spawnPlain: (count: number, autoCommand?: string) => Promise<string[] | void>;
  /**
   * Spawn N AI sessions in a single account's `configDir`. Mirrors the
   * closure at `TerminalPage.tsx:559` (passed down as `onLaunchAiSession`
   * to `TerminalTabBar`).
   */
  spawnAi: (
    count: number,
    configDir: string,
    context?: string,
  ) => Promise<string[] | void>;
  /**
   * Snapshot of the available Claude Code accounts, in the original
   * `useSessionManager` shape (NOT sorted). Handlers sort locally when
   * resolving `account: "best"`.
   */
  accounts: AccountUsageInfo[];
  /**
   * Sort the zone-grid by session state — `needs-input` first, then
   * `error`, then the rest. Mirrors `useZoneActions.handleSortZones`,
   * which is the closure ZoneStatusBar's "Sort Zones" button used.
   * Threaded through here so the registry can fire it without
   * re-importing `useZoneActions`.
   */
  sortZones: () => void;
  /**
   * Export every session's transcript to file. Mirrors
   * `useZoneActions.handleExportOutput`, the closure ZoneStatusBar's
   * "Export" button used.
   */
  exportAll: () => void;
}

/**
 * `paramSchema` objects below follow the UI Bridge contract — loose
 * `Record<string, unknown>` with hint strings, no runtime validation.
 * The Phase 8 Tier-3 subprocess normalizes these into Anthropic-
 * compatible `input_schema` at spawn time. Keeping the shapes here as
 * data (not as TS literals) gives the future adapter exactly what it
 * needs to project them back into `useUIComponent` action defs.
 */
const SCHEMA = {
  focus: { target: 'number | "next" | "prev" | "needs-input"' },
  spawn: { count: "number (>= 1, defaults to 1)" },
  spawnAi: {
    count: "number (>= 1, defaults to 1)",
    account:
      'string — either a Claude account label (e.g. "gmail", "hotmail") or the literal "best" to pick the lowest-utilization account',
    context: 'string (optional initial prompt auto-typed after `claude` starts)',
  },
  spawnWith: {
    count: "number (>= 1, defaults to 1)",
    command: "string (shell command typed into each new terminal after spawn)",
  },
  empty: {},
  maximize: { zone: "number (1-based; defaults to the currently focused zone)" },
  close: {
    zone: "number (1-based zone index)",
    tabId: "string (explicit tab id; takes precedence over zone)",
  },
  layout: { preset: 'string — one of "single", "split", "quad", "six-pack", "full-grid"' },
  restart: { zone: "number (1-based; defaults to the currently focused zone)" },
  swap: { a: "number (1-based zone index)", b: "number (1-based zone index)" },
} as const;

/** Helper: build a successful result. */
function ok<T>(value?: T): CommandResult<T> {
  return { ok: true, value };
}

/** Helper: build a failure result with a stable machine code. */
function fail(code: string, message?: string): CommandResult<never> {
  return { ok: false, code, message };
}

/**
 * Resolve a 1-based zone index from an args bag. Accepts either a literal
 * number or the words `next` / `prev` / `needs-input`. Returns `null` when
 * the args don't carry a usable target.
 */
function readZoneArg(
  args: Record<string, unknown>,
  field: string = "zone",
): number | null {
  const v = args[field];
  if (typeof v === "number" && Number.isFinite(v)) return Math.floor(v);
  if (typeof v === "string") {
    const n = Number(v);
    if (Number.isFinite(n)) return Math.floor(n);
  }
  return null;
}

/**
 * Look up a `configDir` from an `account` arg. `"best"` (or empty
 * string) resolves to the lowest-utilization account. Returns `null`
 * when no matching account exists — handlers surface that as
 * `code: "no-account"`.
 */
function resolveAccountConfigDir(
  account: unknown,
  accounts: readonly AccountUsageInfo[],
): string | null {
  if (accounts.length === 0) return null;
  const raw = typeof account === "string" ? account.trim().toLowerCase() : "";
  if (raw === "" || raw === "best" || raw === "@best") {
    const sorted = [...accounts].sort(
      (a, b) => (a.utilization ?? 0) - (b.utilization ?? 0),
    );
    return sorted[0]?.config_dir ?? null;
  }
  const match = accounts.find(
    (a) => a.label.toLowerCase() === raw || a.config_dir.toLowerCase() === raw,
  );
  return match?.config_dir ?? null;
}

export function useTerminalCommands(ctx: TerminalCommandsContext): void {
  const session = useTerminalSession();
  const {
    tabs,
    activeId,
    setActiveId,
    closeTerminal,
    terminalRefs,
    sessionStates,
    zoneLayout,
  } = session;
  const transitionEffects = useTransitionEffects();
  const { dispatch: uiDispatch, toggleFocusMode } = useUIStateCx();

  /**
   * Helper that converts 1-based zone arg to 0-based, defaulting to
   * `zoneLayout.focusedZone` when no arg given. Returns null when the
   * resolved index is out of range (handler returns `out-of-range`).
   */
  const resolveZoneIdx = (args: Record<string, unknown>): number | null => {
    const arg = readZoneArg(args);
    const idx = arg !== null ? arg - 1 : zoneLayout.focusedZone;
    if (idx < 0 || idx >= zoneLayout.layout.zones.length) return null;
    return idx;
  };

  // ── 1. /focus ────────────────────────────────────────────────────────
  useCommandAction({
    id: "terminal.focus",
    slash: "/focus",
    label: "Focus zone",
    description:
      "Focus a session by 1-based zone index, or jump to next/prev/needs-input. " +
      "When a zone is maximized this also swaps the maximized view.",
    paramSchema: SCHEMA.focus,
    // Tier-2 patterns: "focus 3", "focus next", "focus prev",
    // "focus previous", "focus needs input", "focus needs-input",
    // "focus needsinput", "next session", "previous session".
    patterns: [
      /^focus\s+(?<target>next|prev|previous|needs[-_ ]?input|\d+)$/i,
      /^(?<target>next|previous|prev)\s+session$/i,
    ],
    handler: async (args: Record<string, unknown>): Promise<CommandResult> => {
      // Normalize the variants the patterns produce so the handler's
      // dispatch stays tight.
      const raw = typeof args.target === "string" ? args.target.toLowerCase() : args.target;
      const target =
        raw === "previous"
          ? "prev"
          : typeof raw === "string" && /^needs[-_ ]?input$/.test(raw)
            ? "needs-input"
            : raw;
      if (target === "next") {
        zoneLayout.focusNextZone();
        return ok();
      }
      if (target === "prev") {
        zoneLayout.focusPrevZone();
        return ok();
      }
      if (target === "needs-input") {
        const found = zoneLayout.focusNextNeedsInput(sessionStates);
        return found ? ok() : fail("none-needs-input");
      }
      const idx = resolveZoneIdx({ zone: target ?? args.zone });
      if (idx === null) return fail("out-of-range");
      zoneLayout.setFocusedZone(idx);
      return ok();
    },
  });

  // ── 2. /spawn ────────────────────────────────────────────────────────
  useCommandAction({
    id: "terminal.spawn",
    slash: "/spawn",
    label: "Spawn plain terminal",
    description: "Spawn N plain PTY tabs in the user's default shell and zone-assign them.",
    paramSchema: SCHEMA.spawn,
    // Tier-2 patterns: "spawn 3", "spawn 3 plain".
    // Ordered BEFORE /spawn-ai's pattern in registration order so
    // `spawn 3 plain` routes to the plain handler rather than being
    // captured by spawn-ai's wider account regex.
    patterns: [/^spawn\s+(?<count>\d+)(?:\s+plain)?$/i],
    handler: async (args: Record<string, unknown>): Promise<CommandResult<string[]>> => {
      const count = typeof args.count === "number" ? args.count : 1;
      if (count < 1) return fail("invalid-count", "count must be >= 1");
      const result = await ctx.spawnPlain(count);
      return ok(Array.isArray(result) ? result : []);
    },
  });

  // ── 3. /spawn-ai (alias /spawn-best) ──────────────────────────────────
  // Single handler covers both wire ids (`create-ai-session` and
  // `create-best-account`) via the `account: "best"` collapse decision.
  useCommandAction({
    id: "terminal.spawn-ai",
    slash: "/spawn-ai",
    aliases: ["/spawn-best"],
    label: "Spawn AI session",
    description:
      "Spawn N Claude CLI sessions in a specific account. account=\"best\" picks the " +
      "lowest-utilization account. context (optional) is typed after `claude` starts.",
    paramSchema: SCHEMA.spawnAi,
    // Tier-2 patterns:
    //   "spawn-ai 3 best"            → count=3, account=best
    //   "spawn-ai 3 best do the X"   → count=3, account=best, context="do the X"
    //   "spawn 3 best"               → count=3, account=best   (natural form)
    //   "spawn 3 claude"             → count=3, account="claude" (handler
    //                                   maps via resolveAccountConfigDir;
    //                                   "claude" is not a real label, falls
    //                                   through to fail("no-account") which
    //                                   surfaces a clear error rather than
    //                                   silently launching the wrong account)
    patterns: [
      /^spawn-ai\s+(?<count>\d+)\s+(?<account>[\w-]+|best)(?:\s+(?<context>.+))?$/i,
      /^spawn\s+(?<count>\d+)\s+(?<account>best|claude|[\w-]+)$/i,
    ],
    handler: async (args: Record<string, unknown>): Promise<CommandResult<string[]>> => {
      const count = typeof args.count === "number" ? args.count : 1;
      if (count < 1) return fail("invalid-count", "count must be >= 1");
      const configDir = resolveAccountConfigDir(args.account, ctx.accounts);
      if (!configDir) return fail("no-account", "no matching Claude account");
      const context = typeof args.context === "string" ? args.context : undefined;
      const result = await ctx.spawnAi(count, configDir, context);
      return ok(Array.isArray(result) ? result : []);
    },
  });

  // ── 4. /spawn-with ────────────────────────────────────────────────────
  useCommandAction({
    id: "terminal.spawn-with",
    slash: "/spawn-with",
    label: "Spawn terminal with command",
    description: "Spawn N plain PTY tabs and auto-type the given shell command into each.",
    paramSchema: SCHEMA.spawnWith,
    handler: async (args: Record<string, unknown>): Promise<CommandResult<string[]>> => {
      const count = typeof args.count === "number" ? args.count : 1;
      if (count < 1) return fail("invalid-count", "count must be >= 1");
      const command = typeof args.command === "string" ? args.command : "";
      if (!command) return fail("invalid-command", "command is required");
      const result = await ctx.spawnPlain(count, command);
      return ok(Array.isArray(result) ? result : []);
    },
  });

  // ── 5. /approve-all ───────────────────────────────────────────────────
  // Sends `y\r` to every PTY whose session-state is `needs-input`. Mirrors
  // the inline writer at `useKeyboardShortcuts.ts:142-151` (Ctrl+Shift+
  // Enter); the keyboard handler stays untouched in Phase 1b. Phase 9
  // will collapse the two paths through this registry handler.
  useCommandAction({
    id: "terminal.approve-all",
    slash: "/approve-all",
    label: "Approve all needs-input sessions",
    description:
      "Sends 'y' followed by Enter to every session currently waiting on input. " +
      "Same behavior as Ctrl+Shift+Enter.",
    paramSchema: SCHEMA.empty,
    patterns: [/^approve(?:\s+all)?$/i, /^yes\s+all$/i],
    handler: async (): Promise<CommandResult<{ approved: number }>> => {
      const waiting = tabs.filter((t) => sessionStates[t.id] === "needs-input");
      for (const tab of waiting) {
        terminalRefs.current.get(tab.id)?.current?.writeToTerminal("y\r");
      }
      return ok({ approved: waiting.length });
    },
  });

  // ── 6. /maximize ──────────────────────────────────────────────────────
  useCommandAction({
    id: "terminal.maximize",
    slash: "/maximize",
    label: "Maximize / restore zone",
    description:
      "Toggle maximize for the given zone (1-based), or the currently focused zone " +
      "when no argument is provided. Calling on the already-maximized zone restores.",
    paramSchema: SCHEMA.maximize,
    patterns: [
      /^maximize(?:\s+(?<zone>\d+))?$/i,
      /^fullscreen(?:\s+(?<zone>\d+))?$/i,
    ],
    handler: async (args: Record<string, unknown>): Promise<CommandResult> => {
      const idx = resolveZoneIdx(args);
      if (idx === null) return fail("out-of-range");
      zoneLayout.toggleMaximize(idx);
      return ok();
    },
  });

  // ── 7. /close ─────────────────────────────────────────────────────────
  // `destructive: true` would trigger a confirm modal once Phase 4
  // executor is in place; per plan §5(3) single-target close uses Undo
  // instead of confirm. Mark `undoable: true` so the future executor
  // captures pre-state. Today neither flag changes runtime behavior.
  useCommandAction({
    id: "terminal.close",
    slash: "/close",
    label: "Close session",
    description:
      "Close a session by tab id (preferred when known) or 1-based zone index. " +
      "Without args, closes the currently focused session.",
    paramSchema: SCHEMA.close,
    undoable: true,
    patterns: [/^close(?:\s+(?<zone>\d+))?$/i],
    handler: async (args: Record<string, unknown>): Promise<CommandResult> => {
      const explicitTabId = typeof args.tabId === "string" ? args.tabId : null;
      let tabId: string | null = explicitTabId;
      if (!tabId) {
        const idx = resolveZoneIdx(args);
        if (idx !== null) {
          tabId = zoneLayout.assignments[idx] ?? null;
        }
      }
      if (!tabId) tabId = activeId;
      if (!tabId) return fail("no-target", "no session to close");
      closeTerminal(tabId);
      return ok();
    },
  });

  // ── 8. /layout ────────────────────────────────────────────────────────
  useCommandAction({
    id: "terminal.layout",
    slash: "/layout",
    label: "Change layout preset",
    description:
      'Set the zone-grid layout preset. Accepts "single", "split", "quad", "six-pack", ' +
      'or "full-grid".',
    paramSchema: SCHEMA.layout,
    // Named-group form so the preset is bound to `args.preset` for both
    // pattern and slash-form invocations.
    patterns: [
      /^layout\s+(?<preset>single|split|quad|six-?pack|full-?grid)$/i,
      /^(?<preset>single|split|quad|six-?pack|full-?grid)\s+layout$/i,
    ],
    handler: async (args: Record<string, unknown>): Promise<CommandResult> => {
      const preset = typeof args.preset === "string" ? args.preset.toLowerCase() : "";
      // Normalize "six-pack" / "sixpack" → "six-pack"; same for full-grid.
      const normalized = preset
        .replace(/sixpack/, "six-pack")
        .replace(/fullgrid/, "full-grid");
      if (!normalized) return fail("invalid-preset");
      zoneLayout.setLayoutId(normalized);
      return ok();
    },
  });

  // ── 9. /restart ───────────────────────────────────────────────────────
  // State-gated handler — surfaces `not-restartable` per the audit's
  // §"Per-action success criterion checks" so the CommandBar can show
  // a friendly error instead of a silent no-op. Today
  // `transitionEffects.handleRestartInZone` itself returns silently
  // (`TransitionEffectsContext.tsx:44`) so we replicate the same gate
  // here to give the user feedback.
  useCommandAction({
    id: "terminal.restart",
    slash: "/restart",
    label: "Restart session in zone",
    description:
      "Restart the session in the given zone (1-based) or the focused zone. " +
      "Only available when the session is in 'completed' or 'error' state.",
    paramSchema: SCHEMA.restart,
    patterns: [/^restart(?:\s+(?<zone>\d+))?$/i],
    handler: async (args: Record<string, unknown>): Promise<CommandResult> => {
      const idx = resolveZoneIdx(args);
      if (idx === null) return fail("out-of-range");
      const tabId = zoneLayout.assignments[idx];
      const state = tabId ? sessionStates[tabId] ?? "idle" : "idle";
      if (state !== "completed" && state !== "error") {
        return fail("not-restartable", `session state is ${state}`);
      }
      transitionEffects.handleRestartInZone(idx);
      return ok();
    },
  });

  // ── 10. /swap ────────────────────────────────────────────────────────
  // Replaces today's two-chord Ctrl+Shift+X workflow (mark source, focus
  // dest, swap). Single invocation; no mode tracking. Per the audit
  // table this is the second marquee CommandBar win after /spawn-ai.
  useCommandAction({
    id: "terminal.swap",
    slash: "/swap",
    label: "Swap two zones",
    description: "Swap the tab assignments of two zones (1-based indices).",
    paramSchema: SCHEMA.swap,
    patterns: [/^swap\s+(?<a>\d+)\s+(?<b>\d+)$/i],
    handler: async (args: Record<string, unknown>): Promise<CommandResult> => {
      const a = readZoneArg(args, "a");
      const b = readZoneArg(args, "b");
      if (a === null || b === null) return fail("invalid-args", "a and b required");
      const aIdx = a - 1;
      const bIdx = b - 1;
      const maxIdx = zoneLayout.layout.zones.length;
      if (aIdx < 0 || aIdx >= maxIdx || bIdx < 0 || bIdx >= maxIdx) {
        return fail("out-of-range");
      }
      if (aIdx === bIdx) return ok();
      const aTabId = zoneLayout.assignments[aIdx];
      const bTabId = zoneLayout.assignments[bIdx];
      if (aTabId) zoneLayout.assignTabToZone(bIdx, aTabId);
      if (bTabId) zoneLayout.assignTabToZone(aIdx, bTabId);
      return ok();
    },
  });

  // ── Phase 9b — ZoneStatusBar migration slice ────────────────────────
  // Seven preference-toggle + zone-op actions that previously only had
  // ZoneStatusBar button homes (and partial keyboard-shortcut coverage).
  // Adding them to the registry makes them invokable from CommandBar /
  // palette / Tier-3, paving the way to delete the corresponding
  // ZoneStatusBar buttons in a follow-up commit. Each handler calls the
  // SAME underlying context function the existing button click does, so
  // both surfaces stay in lockstep until the button comes out.

  // 11. /focus-mode — toggle the "fade unfocused zones" overlay
  useCommandAction({
    id: "terminal.toggle-focus-mode",
    slash: "/focus-mode",
    aliases: ["/toggle-focus-mode"],
    label: "Toggle focus mode",
    description:
      "Dim every zone except the focused one (and those in needs-input/error). " +
      "Same as Ctrl+Shift+D and the Eye icon in ZoneStatusBar.",
    paramSchema: SCHEMA.empty,
    patterns: [/^focus[ -]mode$/i, /^toggle\s+focus[ -]mode$/i],
    handler: async (): Promise<CommandResult> => {
      toggleFocusMode();
      return ok();
    },
  });

  // 12. /auto-focus — toggle auto-focus-on-needs-input
  useCommandAction({
    id: "terminal.toggle-auto-focus",
    slash: "/auto-focus",
    aliases: ["/toggle-auto-focus"],
    label: "Toggle auto-focus on needs-input",
    description:
      "When ON, the grid automatically focuses the next session that enters " +
      "needs-input state. Same as Ctrl+Shift+A and the Focus icon in ZoneStatusBar.",
    paramSchema: SCHEMA.empty,
    patterns: [/^auto[ -]focus$/i, /^toggle\s+auto[ -]focus$/i],
    handler: async (): Promise<CommandResult> => {
      transitionEffects.toggleAutoFocus();
      return ok();
    },
  });

  // 13. /sound — toggle audible needs-input/error notifications
  useCommandAction({
    id: "terminal.toggle-sound",
    slash: "/sound",
    aliases: ["/toggle-sound", "/mute", "/unmute"],
    label: "Toggle sound notifications",
    description:
      "Audible chimes when a session enters needs-input or error. Same as " +
      "Ctrl+Shift+S and the speaker icon in ZoneStatusBar.",
    paramSchema: SCHEMA.empty,
    patterns: [/^(?:un)?mute$/i, /^toggle\s+sound$/i, /^sound$/i],
    handler: async (): Promise<CommandResult> => {
      transitionEffects.toggleSound();
      return ok();
    },
  });

  // 14. /auto-restart — toggle auto-restart-completed-sessions
  // Mirrors ZoneStatusBar's onToggleAutoRestart: flips the boolean AND
  // persists the new value to instanceStorage under the same key the
  // existing button uses (`zone-auto-restart`) so both paths stay in
  // sync across reloads.
  useCommandAction({
    id: "terminal.toggle-auto-restart",
    slash: "/auto-restart",
    aliases: ["/toggle-auto-restart"],
    label: "Toggle auto-restart of completed sessions",
    description:
      "When ON, sessions that exit cleanly are immediately respawned with the " +
      "same launch command. Useful for headless polling agents. Same as the " +
      "RefreshCw icon in ZoneStatusBar.",
    paramSchema: SCHEMA.empty,
    patterns: [/^auto[ -]restart$/i, /^toggle\s+auto[ -]restart$/i],
    handler: async (): Promise<CommandResult> => {
      transitionEffects.setAutoRestart((prev: boolean) => {
        const next = !prev;
        instanceStorage.setItem("zone-auto-restart", String(next));
        return next;
      });
      return ok();
    },
  });

  // 15. /sort-zones — reorder zones by state
  useCommandAction({
    id: "terminal.sort-zones",
    slash: "/sort-zones",
    aliases: ["/sort"],
    label: "Sort zones by state",
    description:
      "Reorder zones so needs-input lands first, then error, working, idle, " +
      "completed. Same as the ArrowUpDown icon in ZoneStatusBar.",
    paramSchema: SCHEMA.empty,
    patterns: [/^sort(?:\s+zones)?$/i],
    handler: async (): Promise<CommandResult> => {
      ctx.sortZones();
      return ok();
    },
  });

  // 16. /export-all — export every session's transcript
  useCommandAction({
    id: "terminal.export-all",
    slash: "/export-all",
    aliases: ["/export"],
    label: "Export all session output",
    description:
      "Save every visible session's terminal output to a file. Same as the " +
      "Download icon in ZoneStatusBar.",
    paramSchema: SCHEMA.empty,
    patterns: [/^export(?:\s+all)?$/i],
    handler: async (): Promise<CommandResult> => {
      ctx.exportAll();
      return ok();
    },
  });

  // 17. /shortcuts — open the keyboard-shortcuts overlay
  useCommandAction({
    id: "terminal.show-shortcuts",
    slash: "/shortcuts",
    aliases: ["/keys", "/help"],
    label: "Show keyboard shortcuts",
    description:
      "Open the keyboard-shortcuts cheat sheet overlay. Same as Ctrl+Shift+? " +
      "and the Keyboard icon in ZoneStatusBar.",
    paramSchema: SCHEMA.empty,
    patterns: [/^shortcuts$/i, /^keys$/i, /^help$/i],
    handler: async (): Promise<CommandResult> => {
      uiDispatch({ type: "SET_SHOW_SHORTCUTS", payload: true });
      return ok();
    },
  });

  // Mark unused vars so linters don't trip on the resolver-context
  // signature placeholder (handlers below don't consult `ctx` directly
  // today — Phase 2's CommandBar wires it in for cancellation).
  void setActiveId;
}

/** Re-export so tests + the index barrel can reach a single source. */
export type { ResolverContext } from "./types";
