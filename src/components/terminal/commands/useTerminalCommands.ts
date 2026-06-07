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

import {
  useTerminalSession,
  useTransitionEffects,
  useUIStateCx,
  useZoneMetadata,
} from "../contexts";
import type { AccountUsageInfo } from "../useSessionManager";
import { compareByUsageHeadroom } from "../../settings/types";
import type { ResultCardSpec } from "../result-card";
import { buildMetricsCardSpec, buildHistoryCardSpec } from "../result-card";
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
  spawnAi: (count: number, configDir: string, context?: string) => Promise<string[] | void>;
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
  /**
   * Open the DocFinder modal. Mirrors the closure ZoneStatusBar's
   * "Doc" button used (now lifted into `TerminalPage` so the modal
   * mount and the `/doc-finder` registry action share a single
   * `showDocFinder` state).
   */
  openDocFinder: () => void;
  /**
   * Pop a result card. Threaded in from `TerminalPageInner` (which holds
   * the `ResultCardProvider`'s `showCard`). Used by `/metrics` and
   * `/history` to present the ZoneStatusBar popover bodies as a card.
   */
  showCard: (spec: ResultCardSpec) => void;
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
    context: "string (optional initial prompt auto-typed after `claude` starts)",
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
function readZoneArg(args: Record<string, unknown>, field: string = "zone"): number | null {
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
 * string) resolves to the account with the most headroom relative to its
 * projected usage (most-negative `usage_delta`; see
 * `compareByUsageHeadroom`). Returns `null` when no matching account
 * exists — handlers surface that as `code: "no-account"`.
 */
function resolveAccountConfigDir(
  account: unknown,
  accounts: readonly AccountUsageInfo[],
): string | null {
  if (accounts.length === 0) return null;
  const raw = typeof account === "string" ? account.trim().toLowerCase() : "";
  if (raw === "" || raw === "best" || raw === "@best") {
    const sorted = [...accounts].sort(compareByUsageHeadroom);
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
    stateDurations,
    lastOutputLines,
    stateTimeAccum: stateTimeAccumRef,
    zoneLayout,
    workflowGen,
    findingsActions,
    analysis,
  } = session;
  const transitionEffects = useTransitionEffects();
  const { dispatch: uiDispatch, toggleFocusMode } = useUIStateCx();
  const { labelsAndTags, metrics: metricsRef, eventHistory } = useZoneMetadata();

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
      'Spawn N Claude CLI sessions in a specific account. account="best" picks the ' +
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
    patterns: [/^maximize(?:\s+(?<zone>\d+))?$/i, /^fullscreen(?:\s+(?<zone>\d+))?$/i],
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
      const normalized = preset.replace(/sixpack/, "six-pack").replace(/fullgrid/, "full-grid");
      if (!normalized) return fail("invalid-preset");
      // `/layout <preset>` is an explicit operator choice — pin it so auto-grow
      // stops overriding it.
      zoneLayout.setLayoutId(normalized, { pinned: true });
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
      const state = tabId ? (sessionStates[tabId] ?? "idle") : "idle";
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

  // ── Phase 9d — ZoneStatusBar migration slice 2 ──────────────────────
  // 7 more actions for the simplest workflow-toggle / preference-toggle
  // controls. Same pattern as Phase 9b: each handler calls the same
  // closure the existing ZSB button does, so both surfaces stay in
  // lockstep until the buttons come out (in the same commit /
  // subsequent commit). Pairs with the ZSB button deletions below.

  // 18. /sessions — toggle the Claude Code sessions sidebar
  useCommandAction({
    id: "terminal.toggle-sessions-sidebar",
    slash: "/sessions",
    aliases: ["/toggle-sessions"],
    label: "Toggle sessions sidebar",
    description:
      "Open/close the SessionManagerPanel — browses Claude Code sessions for resume / inspection.",
    paramSchema: SCHEMA.empty,
    patterns: [/^sessions$/i, /^toggle\s+sessions$/i],
    handler: async (): Promise<CommandResult> => {
      workflowGen.setShowSidebar((v: boolean) => !v);
      return ok();
    },
  });

  // 19. /resume — open sessions sidebar (always opens, never closes)
  // Mirrors ZSB's Resume button: opens the sidebar so operator can pick
  // a session to resume. Force-open semantics so the operator never
  // closes the sidebar by accident when intending to resume.
  useCommandAction({
    id: "terminal.resume",
    slash: "/resume",
    label: "Resume a previous Claude Code session",
    description: "Open the sessions sidebar (always opens, never closes).",
    paramSchema: SCHEMA.empty,
    patterns: [/^resume$/i],
    handler: async (): Promise<CommandResult> => {
      workflowGen.setShowSidebar(true);
      return ok();
    },
  });

  // 20. /findings — toggle findings decisions panel
  useCommandAction({
    id: "terminal.toggle-findings",
    slash: "/findings",
    aliases: ["/toggle-findings"],
    label: "Toggle findings panel",
    description:
      "Show/hide the findings decisions panel in the right sidebar (drift / quality / regression findings from the active session).",
    paramSchema: SCHEMA.empty,
    patterns: [/^findings$/i, /^toggle\s+findings$/i],
    handler: async (): Promise<CommandResult> => {
      findingsActions.handleToggleFindings();
      return ok();
    },
  });

  // 21. /file-ownership — toggle file-ownership heatmap panel
  useCommandAction({
    id: "terminal.toggle-file-ownership",
    slash: "/file-ownership",
    aliases: ["/files", "/toggle-file-ownership"],
    label: "Toggle file-ownership heatmap",
    description:
      "Show/hide the file-ownership heatmap (recent session-touched files) in the right sidebar.",
    paramSchema: SCHEMA.empty,
    patterns: [/^file[- ]ownership$/i, /^files$/i],
    handler: async (): Promise<CommandResult> => {
      workflowGen.setRightPanelMode((prev) =>
        prev === "file-ownership" ? null : "file-ownership",
      );
      return ok();
    },
  });

  // 22. /desktop-notify — toggle native desktop notifications
  // Mirrors ZSB's onToggleDesktopNotify: flips the boolean AND persists
  // to instanceStorage so the choice survives reload (same key the
  // existing button uses).
  useCommandAction({
    id: "terminal.toggle-desktop-notify",
    slash: "/desktop-notify",
    aliases: ["/notifications", "/toggle-notifications"],
    label: "Toggle desktop notifications",
    description:
      "Show/hide native desktop notifications when sessions enter needs-input or error states.",
    paramSchema: SCHEMA.empty,
    patterns: [/^desktop[- ]notify$/i, /^notifications$/i],
    handler: async (): Promise<CommandResult> => {
      transitionEffects.setDesktopNotify((prev: boolean) => {
        const next = !prev;
        instanceStorage.setItem("zone-desktop-notify", String(next));
        return next;
      });
      return ok();
    },
  });

  // 23. /plan-refresh — reload the workspace's plan file from disk
  useCommandAction({
    id: "terminal.plan-refresh",
    slash: "/plan-refresh",
    aliases: ["/refresh-plan"],
    label: "Refresh plan file",
    description:
      "Reload the workspace's PLAN*.md / TODO*.md file from disk. Useful after editing the plan in another editor while the runner is open.",
    paramSchema: SCHEMA.empty,
    patterns: [/^plan[- ]refresh$/i, /^refresh\s+plan$/i],
    handler: async (): Promise<CommandResult> => {
      await workflowGen.loadPlanContent();
      return ok();
    },
  });

  // 24. /auto-approve — manage the auto-approve regex pattern list
  // Patterns are regex-matched against the last output of a session
  // entering needs-input; matching patterns auto-send "y\r" without
  // requiring operator confirmation. Useful for headless polling
  // workflows where "Do you want to proceed?" prompts are routine.
  // Subcommands: add <pattern>, list, clear, remove <pattern>.
  useCommandAction({
    id: "terminal.auto-approve",
    slash: "/auto-approve",
    label: "Manage auto-approve patterns",
    description:
      "Sub-commands: /auto-approve add <regex>, /auto-approve list, " +
      "/auto-approve clear, /auto-approve remove <regex>. Patterns are " +
      "regex-matched against last-output on needs-input; matches auto-send 'y'.",
    paramSchema: {
      action: 'string — "add" | "list" | "clear" | "remove"',
      pattern: "string (regex; required for add and remove)",
    },
    patterns: [
      /^auto-approve\s+(?<action>add|remove)\s+(?<pattern>.+)$/i,
      /^auto-approve\s+(?<action>list|clear)$/i,
    ],
    handler: async (
      args: Record<string, unknown>,
    ): Promise<CommandResult<{ patterns: string[] }>> => {
      const action = typeof args.action === "string" ? args.action.toLowerCase() : "";
      const pattern = typeof args.pattern === "string" ? args.pattern : "";
      const current = transitionEffects.autoApprovePatterns ?? [];
      if (action === "list") return ok({ patterns: current });
      if (action === "clear") {
        transitionEffects.setAutoApprovePatterns([]);
        return ok({ patterns: [] });
      }
      if (action === "add") {
        if (!pattern) return fail("invalid-args", "pattern required for add");
        const next = current.includes(pattern) ? current : [...current, pattern];
        transitionEffects.setAutoApprovePatterns(next);
        return ok({ patterns: next });
      }
      if (action === "remove") {
        if (!pattern) return fail("invalid-args", "pattern required for remove");
        const next = current.filter((p) => p !== pattern);
        transitionEffects.setAutoApprovePatterns(next);
        return ok({ patterns: next });
      }
      return fail("invalid-args", `unknown action "${action}"`);
    },
  });

  // ── Phase 9f — ZSB middle-group control migration ───────────────────
  // Three actions covering the state-count buttons and tag-filter pills.
  // (Next Action button = already covered by /focus needs-input.)

  // 25. /select-by-state <state> — select all zones in a given session state
  useCommandAction({
    id: "terminal.select-by-state",
    slash: "/select-by-state",
    aliases: ["/select-state"],
    label: "Select zones by state",
    description:
      "Select all zones whose session is in the given state " +
      "(idle, working, needs-input, completed, error). Same as clicking " +
      "a state-count pill in ZoneStatusBar.",
    paramSchema: {
      state: 'string — one of "idle", "working", "needs-input", "completed", "error"',
    },
    patterns: [/^select(?:-by-state)?\s+(?<state>idle|working|needs[-_ ]?input|completed|error)$/i],
    handler: async (args: Record<string, unknown>): Promise<CommandResult> => {
      const raw = typeof args.state === "string" ? args.state.toLowerCase() : "";
      const state = /^needs[-_ ]?input$/.test(raw) ? "needs-input" : raw;
      const valid = ["idle", "working", "needs-input", "completed", "error"];
      if (!valid.includes(state)) {
        return fail("invalid-args", `state must be one of: ${valid.join(", ")}`);
      }
      const zones = new Set<number>();
      for (const [zoneStr, tabId] of Object.entries(zoneLayout.assignments)) {
        if ((sessionStates[tabId] ?? "idle") === state) {
          zones.add(Number(zoneStr));
        }
      }
      uiDispatch({ type: "SET_SELECTED_ZONES", payload: zones });
      return ok({ count: zones.size });
    },
  });

  // 26. /tag <name> — toggle a tag filter on/off
  useCommandAction({
    id: "terminal.tag-toggle",
    slash: "/tag",
    aliases: ["/tag-toggle", "/filter-tag"],
    label: "Toggle tag filter",
    description:
      "Toggle a tag in the active tag-filter set. Same as clicking a tag " +
      "pill in ZoneStatusBar — narrows the visible zones to those matching " +
      "the selected tags.",
    paramSchema: { tag: "string (tag name; case-sensitive match against zone labels)" },
    patterns: [/^tag\s+(?<tag>\S+)$/i, /^filter-tag\s+(?<tag>\S+)$/i],
    handler: async (args: Record<string, unknown>): Promise<CommandResult> => {
      const tag = typeof args.tag === "string" ? args.tag.trim() : "";
      if (!tag) return fail("invalid-args", "tag required");
      labelsAndTags.setActiveTagFilters((prev) => {
        const next = new Set(prev);
        if (next.has(tag)) next.delete(tag);
        else next.add(tag);
        return next;
      });
      return ok();
    },
  });

  // 27. /tag-clear — clear all tag filters
  useCommandAction({
    id: "terminal.tag-clear",
    slash: "/tag-clear",
    aliases: ["/tags-clear", "/clear-tags"],
    label: "Clear tag filters",
    description:
      "Clear all active tag filters. Same as clicking the 'All' pill in " +
      "ZoneStatusBar when one or more tag filters are active.",
    paramSchema: SCHEMA.empty,
    patterns: [/^tag-clear$/i, /^tags?-clear$/i, /^clear-tags?$/i],
    handler: async (): Promise<CommandResult> => {
      labelsAndTags.setActiveTagFilters(new Set());
      return ok();
    },
  });

  // ── Phase 9e — workflow + analysis + plan-build actions ─────────────
  // Five actions covering the ZSB workflow-generation, Analyze dropdown,
  // and plan-build button cluster. All wrap existing onClick closures —
  // no semantic change, just registry surface.

  // 28. /generate — generate workflow from latest Claude Code session
  useCommandAction({
    id: "terminal.generate-workflow",
    slash: "/generate",
    aliases: ["/generate-workflow"],
    label: "Generate workflow",
    description:
      "Generate a workflow from the latest Claude Code session in the active " +
      "terminal. Same as the Generate button in ZoneStatusBar.",
    paramSchema: SCHEMA.empty,
    patterns: [/^generate(?:\s+workflow)?$/i],
    handler: async (): Promise<CommandResult> => {
      await workflowGen.handleGenerateFromLatestSession();
      return ok();
    },
  });

  // 29. /save-workflow — save the most recently generated workflow
  useCommandAction({
    id: "terminal.save-workflow",
    slash: "/save-workflow",
    aliases: ["/save"],
    label: "Save generated workflow",
    description:
      "Save the most recently generated workflow to the library. Requires " +
      "a workflow to have been generated first (via /generate). Same as the " +
      "Save button in ZoneStatusBar.",
    paramSchema: SCHEMA.empty,
    patterns: [/^save(?:\s+workflow)?$/i],
    handler: async (): Promise<CommandResult> => {
      if (!workflowGen.generatedWorkflow) {
        return fail("no-workflow", "generate a workflow first via /generate");
      }
      await workflowGen.handleSaveWorkflow();
      return ok();
    },
  });

  // 30. /analyze <type> — run a Claude analysis on terminal output
  useCommandAction({
    id: "terminal.analyze",
    slash: "/analyze",
    label: "Analyze terminal output",
    description:
      "Run a Claude analysis: session-summary, architecture, change-impact, " +
      "progress, cross-tab, page-architecture. Same as picking an option in " +
      "ZoneStatusBar's Analyze dropdown.",
    paramSchema: {
      type: 'string — one of "session-summary", "architecture", "change-impact", "progress", "cross-tab", "page-architecture"',
    },
    patterns: [
      /^analyze\s+(?<type>session-summary|architecture|change-impact|progress|cross-tab|page-architecture)$/i,
    ],
    handler: async (args: Record<string, unknown>): Promise<CommandResult> => {
      const raw = typeof args.type === "string" ? args.type.toLowerCase() : "";
      const valid = [
        "session-summary",
        "architecture",
        "change-impact",
        "progress",
        "cross-tab",
        "page-architecture",
      ] as const;
      if (!(valid as readonly string[]).includes(raw)) {
        return fail("invalid-args", `type must be one of: ${valid.join(", ")}`);
      }
      analysis.handleAnalyze(raw as (typeof valid)[number]);
      return ok();
    },
  });

  // 31. /plan-implement — build the plan-implementation workflow
  useCommandAction({
    id: "terminal.plan-implement",
    slash: "/plan-implement",
    aliases: ["/implement"],
    label: "Build plan implementation workflow",
    description:
      "Build a plan-implementation workflow from the loaded plan file " +
      "(implement + review + next-steps per phase). Requires a PLAN*.md / " +
      "TODO*.md file in the workspace. Same as the Implement button in " +
      "ZoneStatusBar.",
    paramSchema: SCHEMA.empty,
    patterns: [/^plan-implement$/i, /^implement$/i],
    handler: async (): Promise<CommandResult> => {
      if (!workflowGen.planFileName) {
        return fail("no-plan", "no PLAN*.md / TODO*.md file detected in workspace");
      }
      await workflowGen.handleBuildPlanImplementationFromFile();
      return ok();
    },
  });

  // 32. /plan-verify — build the plan-verification workflow
  useCommandAction({
    id: "terminal.plan-verify",
    slash: "/plan-verify",
    aliases: ["/verify"],
    label: "Build plan verification workflow",
    description:
      "Build a plan workflow with the verification-only loop (lighter, no " +
      "review/next-steps). Requires a PLAN*.md / TODO*.md file in the " +
      "workspace. Same as the Verify button in ZoneStatusBar.",
    paramSchema: SCHEMA.empty,
    patterns: [/^plan-verify$/i, /^verify$/i],
    handler: async (): Promise<CommandResult> => {
      if (!workflowGen.planFileName) {
        return fail("no-plan", "no PLAN*.md / TODO*.md file detected in workspace");
      }
      await workflowGen.handleBuildPlanFromFile();
      return ok();
    },
  });

  // 33. /doc-finder — open the doc-picker modal
  useCommandAction({
    id: "terminal.doc-finder",
    slash: "/doc-finder",
    aliases: ["/doc", "/docs"],
    label: "Open doc-finder",
    description:
      "Open the doc-picker modal to load a documentation file into a zone. " +
      "Same as the Doc button in ZoneStatusBar.",
    paramSchema: SCHEMA.empty,
    patterns: [/^docs?$/i, /^doc-finder$/i],
    handler: async (): Promise<CommandResult> => {
      ctx.openDocFinder();
      return ok();
    },
  });

  // 34. /metrics — pop the session-metrics result card (same data the
  // ZoneStatusBar chart-icon MetricsPopover shows).
  useCommandAction({
    id: "terminal.metrics",
    slash: "/metrics",
    aliases: ["/stats"],
    label: "Show session metrics",
    description:
      "Show session metrics (approvals, rejections, broadcasts, state breakdown, " +
      "time-in-state, top keywords). Same as the chart icon in ZoneStatusBar.",
    paramSchema: SCHEMA.empty,
    patterns: [/^(?:metrics|stats)$/i],
    handler: async (): Promise<CommandResult> => {
      ctx.showCard(
        buildMetricsCardSpec({
          metrics: metricsRef.current,
          sessionStates,
          tabs,
          autoApproveCount: transitionEffects.autoApproveCount ?? 0,
          autoRestartCount: transitionEffects.autoRestartCount ?? 0,
          stateTimeAccum: stateTimeAccumRef.current,
          lastOutputLines,
          assignments: zoneLayout.assignments,
          zoneLabels: labelsAndTags.zoneLabels,
          stateDurations,
        }),
      );
      return ok();
    },
  });

  // 35. /history — pop the event-history result card (same data the
  // ZoneStatusBar clock-icon HistoryPopover shows).
  useCommandAction({
    id: "terminal.history",
    slash: "/history",
    aliases: ["/events"],
    label: "Show event history",
    description:
      "Show recent event history (state transitions, etc.). " +
      "Same as the clock icon in ZoneStatusBar.",
    paramSchema: SCHEMA.empty,
    patterns: [/^(?:history|events)$/i],
    handler: async (): Promise<CommandResult> => {
      ctx.showCard(buildHistoryCardSpec(eventHistory ?? []));
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
