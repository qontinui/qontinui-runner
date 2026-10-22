/**
 * Effect-calculus signatures for the `terminal-page` and
 * `terminal-launch-menu` UI Bridge components.
 *
 * Plan `2026-10-22-runner-ui-bridge-makes-an-agent-guess-its-own-vocabulary`,
 * Phase 2. Before this file, `predict` on any of these actions answered
 * `UNCLASSIFIED: no effect signature resolved`, because no runner component
 * action declared a `ComponentAction.signature`.
 *
 * ## What a signature here claims, and what it does not
 *
 * Each `predicts` states the element changes its handler causes IN THIS
 * WEBVIEW'S REGISTRY, read from the handler as written:
 *
 * - A read action predicts an explicit EMPTY delta with `errorsAppear: 'none'`
 *   and `reversibility: 'idempotent'`. That is a checkable claim that nothing
 *   changes — different from "unknown".
 * - A spawn predicts a `terminal-input-*` element appearing. Every non-plan tab
 *   of every page owns exactly one `terminal-input-<tabId>` element whether or
 *   not its pane is mounted (`TerminalBridgeProxies`, rendered by the
 *   always-mounted `PageSessionScope`), so a new PTY tab always surfaces one.
 *   The new tab's id is unknowable before the call, so the criterion is the
 *   glob, never a guessed id.
 * - Where the handler also touches zone placement but the exact element cannot
 *   be stated (which zone, whether its header renders under flow-grid
 *   virtualization), the SCOPE is widened to the zone headers
 *   (`terminal-zone-header-*`) rather than an element invented.
 * - An OS-window action whose only effect is another webview predicts the
 *   change it causes HERE (often none) — the other window is a different
 *   registry this signature cannot observe.
 *
 * Every signature names itself `<componentId>.<actionId>` — the same id the
 * SDK resolver would stamp (`componentSignatureId`) — so a predict caveat and a
 * verification record can be traced back to this file by grep.
 *
 * Declaring a signature opts the action into post-execution verification
 * (`ComponentAction.signature`, ui-bridge `core/types.ts`). A prediction that
 * holds but sees extra unpredicted changes reports `Surprise`, not
 * `Contradiction`; a wrong signature is fixed here, never in the handler.
 */

import { getCurrentWindow } from "@tauri-apps/api/window";
import type {
  ActionParams,
  EffectSignature,
  PredictedDelta,
  SemanticSnapshot,
} from "@qontinui/ui-bridge";

/** Every terminal tab's UI Bridge element (`TerminalBridgeProxies`). */
export const TERMINAL_INPUT_GLOB = "terminal-input-*";
/** Every rendered zone header (`zoneHeaderElementSpec` in `zone-grid/ZoneLabel.tsx`). */
export const ZONE_HEADER_GLOB = "terminal-zone-header-*";

/** The terminal surface: tab elements plus the zone headers they render under. */
const TERMINAL_ZONE_SCOPE = { elementIds: [TERMINAL_INPUT_GLOB, ZONE_HEADER_GLOB] };

/** Does the pre-snapshot contain an element whose id starts with `prefix`? */
export function preHasPrefix(pre: SemanticSnapshot, prefix: string): boolean {
  return pre.elements.some((el) => el.id.startsWith(prefix));
}

/** Count pre-snapshot elements whose id starts with `prefix`. */
export function preCountPrefix(pre: SemanticSnapshot, prefix: string): number {
  return pre.elements.filter((el) => el.id.startsWith(prefix)).length;
}

const TERMINAL_INPUT_PREFIX = "terminal-input-";
const ZONE_HEADER_PREFIX = "terminal-zone-header-";

/** A text param, or `undefined` when absent / not a non-empty string. */
function textParam(params: ActionParams, key: string): string | undefined {
  const value = params.params?.[key];
  return typeof value === "string" && value.length > 0 ? value : undefined;
}

/** A finite number param, or `undefined`. */
function numberParam(params: ActionParams, key: string): number | undefined {
  const value = params.params?.[key];
  return typeof value === "number" && Number.isFinite(value) ? value : undefined;
}

/**
 * The read-action signature: nothing changes, nothing errors, and calling it
 * twice is calling it once.
 */
export function readSignature(id: string): EffectSignature {
  return {
    id,
    predicts: (): PredictedDelta => ({ errorsAppear: "none" }),
    scope: {},
    reversibility: "idempotent",
    provenance: "declared",
  };
}

/** A spawn of one or more PTY tabs: a `terminal-input-*` element appears. */
function spawnSignature(
  id: string,
  reversibility: EffectSignature["reversibility"],
  scope: EffectSignature["scope"],
): EffectSignature {
  return {
    id,
    predicts: (): PredictedDelta => ({ elementsAppear: [{ id: TERMINAL_INPUT_GLOB }] }),
    scope,
    reversibility,
    provenance: "declared",
  };
}

// ---------------------------------------------------------------------------
// terminal-page
// ---------------------------------------------------------------------------

/**
 * `create-terminal` — raw `createTerminal`: a PTY tab with no zone assignment.
 * Closing the tab undoes it.
 */
export const CREATE_TERMINAL_SIGNATURE = spawnSignature(
  "terminal-page.create-terminal",
  "reversible",
  { elementIds: [TERMINAL_INPUT_GLOB] },
);

/**
 * `create-plain-terminal` — `createAndAssignTerminal`: a PTY tab assigned to
 * the next free zone. The tab's element is certain; whether a zone header
 * renders for it is not (flow-grid virtualization, single-zone layouts), so
 * the scope widens to the headers instead of predicting one.
 */
export const CREATE_PLAIN_TERMINAL_SIGNATURE = spawnSignature(
  "terminal-page.create-plain-terminal",
  "reversible",
  TERMINAL_ZONE_SCOPE,
);

export const LIST_TERMINALS_SIGNATURE = readSignature("terminal-page.list-terminals");
export const GET_ZONE_VIEW_STATE_SIGNATURE = readSignature("terminal-page.get-zone-view-state");
export const LIST_RUNNER_WINDOWS_SIGNATURE = readSignature("terminal-page.list-runner-windows");

/**
 * `open-terminal-window` — opens an EMPTY pop-out OS window. The new window is
 * its own webview with its own registry; nothing in this one changes. Closing
 * the window undoes it.
 */
export const OPEN_TERMINAL_WINDOW_SIGNATURE: EffectSignature = {
  id: "terminal-page.open-terminal-window",
  predicts: (): PredictedDelta => ({}),
  scope: {},
  reversibility: "reversible",
  provenance: "declared",
};

/**
 * `pop-out-active-terminal` — moves the active tab into a new window. This
 * window stops owning it (`ownedTabs` in `WindowAssignmentsContext`), so its
 * `terminal-input-<id>` leaves this registry. `move-terminal-to-window` moves
 * it back.
 */
export const POP_OUT_ACTIVE_TERMINAL_SIGNATURE: EffectSignature = {
  id: "terminal-page.pop-out-active-terminal",
  predicts: (): PredictedDelta => ({ elementsDisappear: [{ id: TERMINAL_INPUT_GLOB }] }),
  scope: TERMINAL_ZONE_SCOPE,
  reversibility: "reversible",
  provenance: "declared",
};

/**
 * `pop-out-page` — binds a whole page to a new window; a page binding beats
 * the per-terminal owner map, so all of that page's tab elements leave this
 * registry. Which `terminal-input-*` ids belong to the page is not visible in
 * the snapshot, so the criterion is the glob (scope widened to every tab),
 * and it is only predicted when the pre-snapshot holds any tab at all.
 */
export const POP_OUT_PAGE_SIGNATURE: EffectSignature = {
  id: "terminal-page.pop-out-page",
  predicts: (_params, pre): PredictedDelta =>
    preHasPrefix(pre, TERMINAL_INPUT_PREFIX)
      ? { elementsDisappear: [{ id: TERMINAL_INPUT_GLOB }] }
      : {},
  scope: TERMINAL_ZONE_SCOPE,
  reversibility: "reversible",
  provenance: "declared",
};

/** This webview's window label; `"main"` outside Tauri (tests). */
function currentWindowLabel(): string {
  try {
    return getCurrentWindow().label || "main";
  } catch {
    return "main";
  }
}

/**
 * Pure core of `move-terminal-to-window`'s prediction, with the current
 * window label injected so it is testable without Tauri.
 *
 * The handler reassigns ONE tab's owner window. From this window's point of
 * view: a tab it renders that is sent elsewhere disappears; a tab it does not
 * render that is sent here appears; anything else is a no-op here. Missing
 * params make the handler throw before any effect, so nothing is predicted.
 */
export function predictMoveTerminal(
  params: ActionParams,
  pre: SemanticSnapshot,
  thisWindow: string,
): PredictedDelta {
  const terminalId = textParam(params, "terminalId");
  const target = textParam(params, "windowLabel");
  if (!terminalId || !target) return {};
  const elementId = `${TERMINAL_INPUT_PREFIX}${terminalId}`;
  const renderedHere = pre.elements.some((el) => el.id === elementId);
  if (renderedHere && target !== thisWindow) return { elementsDisappear: [{ id: elementId }] };
  if (!renderedHere && target === thisWindow) return { elementsAppear: [{ id: elementId }] };
  return {};
}

export const MOVE_TERMINAL_TO_WINDOW_SIGNATURE: EffectSignature = {
  id: "terminal-page.move-terminal-to-window",
  predicts: (params, pre) => predictMoveTerminal(params, pre, currentWindowLabel()),
  scope: { elementIds: [TERMINAL_INPUT_GLOB] },
  reversibility: "reversible",
  provenance: "declared",
};

/** The zone-view state the maximize/restore handlers read at invocation time. */
export interface ZoneViewFacts {
  maximizedZone: number | null;
  zoneCount: number;
}

/**
 * Zone headers render only in the tiled multi-zone view (`ZoneGrid`:
 * `showLabels = isMultiZone`, and the maximized branch renders its own
 * unregistered header instead of `ZoneLabel`). So entering the maximized view
 * removes every rendered header, and leaving it brings them back.
 */
function enterMaximized(pre: SemanticSnapshot, facts: ZoneViewFacts): PredictedDelta {
  if (facts.maximizedZone !== null || !preHasPrefix(pre, ZONE_HEADER_PREFIX)) return {};
  return { elementsDisappear: [{ id: ZONE_HEADER_GLOB }] };
}

function leaveMaximized(facts: ZoneViewFacts): PredictedDelta {
  if (facts.maximizedZone === null || facts.zoneCount <= 1) return {};
  return { elementsAppear: [{ id: ZONE_HEADER_GLOB }] };
}

/**
 * Pure core of the three zone-view predictions. `maximize-zone` and
 * `toggle-maximize-zone` with an out-of-range index throw before any effect,
 * so nothing is predicted for them.
 */
export function predictZoneView(
  kind: "maximize" | "restore" | "toggle",
  params: ActionParams,
  pre: SemanticSnapshot,
  facts: ZoneViewFacts,
): PredictedDelta {
  if (kind === "restore") return leaveMaximized(facts);
  const zoneIndex = numberParam(params, "zoneIndex");
  if (zoneIndex === undefined || zoneIndex < 0 || zoneIndex >= facts.zoneCount) return {};
  if (kind === "toggle" && facts.maximizedZone === zoneIndex) return leaveMaximized(facts);
  return enterMaximized(pre, facts);
}

/**
 * `maximize-zone` / `restore-zone` / `toggle-maximize-zone` — display state
 * only (`effect: 'read'`: never persisted, undone by its inverse). They change
 * which elements are rendered but modify none, which is what a read effect may
 * predict. `readFacts` is called at PREDICT time, so the prediction is made
 * against the zone view as it is then, not as it was at registration.
 */
export function zoneViewSignature(
  kind: "maximize" | "restore" | "toggle",
  readFacts: () => ZoneViewFacts,
): EffectSignature {
  const actionId = kind === "toggle" ? "toggle-maximize-zone" : `${kind}-zone`;
  return {
    id: `terminal-page.${actionId}`,
    predicts: (params, pre) => predictZoneView(kind, params, pre, readFacts()),
    scope: { elementIds: [ZONE_HEADER_GLOB] },
    reversibility: "reversible",
    provenance: "declared",
  };
}

/**
 * `close-empty-terminal-windows` — closes pop-out OS windows with no live tab
 * and prunes their records. Those windows are other webviews; this registry
 * loses nothing it renders. `one-way`: a reopened window does not restore the
 * pruned bindings (and the `destructive` effect requires it). The recovery
 * path where a pruned stale binding lets a hidden page render here again is
 * not predicted — it would surface as `Surprise`, which is the honest report.
 */
export const CLOSE_EMPTY_TERMINAL_WINDOWS_SIGNATURE: EffectSignature = {
  id: "terminal-page.close-empty-terminal-windows",
  predicts: (): PredictedDelta => ({}),
  scope: {},
  reversibility: "one-way",
  provenance: "declared",
};

// ---------------------------------------------------------------------------
// terminal-launch-menu
// ---------------------------------------------------------------------------

/**
 * `create-plain` — `terminal.spawn` of N blank shells into zones. Same claim
 * and scope as `terminal-page.create-plain-terminal`.
 */
export const LAUNCH_CREATE_PLAIN_SIGNATURE = spawnSignature(
  "terminal-launch-menu.create-plain",
  "reversible",
  TERMINAL_ZONE_SCOPE,
);

/**
 * The three agent/command launchers. Each spawns PTY tabs (an element
 * appears) and is `destructive`: a launched agent or an auto-typed command
 * does work and spends budget that closing the tab does not undo — `one-way`.
 */
export const LAUNCH_CREATE_AI_SESSION_SIGNATURE = spawnSignature(
  "terminal-launch-menu.create-ai-session",
  "one-way",
  TERMINAL_ZONE_SCOPE,
);
export const LAUNCH_CREATE_BEST_ACCOUNT_SIGNATURE = spawnSignature(
  "terminal-launch-menu.create-best-account",
  "one-way",
  TERMINAL_ZONE_SCOPE,
);
export const LAUNCH_CREATE_WITH_COMMAND_SIGNATURE = spawnSignature(
  "terminal-launch-menu.create-with-command",
  "one-way",
  TERMINAL_ZONE_SCOPE,
);
