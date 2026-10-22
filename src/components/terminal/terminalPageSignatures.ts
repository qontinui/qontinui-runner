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
 * holds but sees extra unpredicted changes reports `Surprise`; a wrong
 * signature is fixed here, never in the handler.
 *
 * ## What glob criteria do NOT protect against
 *
 * A glob matches ANY element of its family, not only the one this action
 * touched. So an unrelated change inside the settle window is attributed to
 * the action: a `terminal-input-*` appear-prediction met by an unrelated tab
 * CLOSING in the same window (a `terminal-input-*` disappearance) can classify
 * as `Contradiction` even though this action did exactly what it should, and
 * an unrelated tab appearing can make a disappear-prediction read as held. A
 * `Contradiction` on a glob signature is therefore evidence to read against
 * the observed delta, not proof the handler is wrong.
 *
 * ## Settle windows
 *
 * A spawn resolves when the PTY exists, but its `terminal-input-<id>` element
 * registers only after the `terminal-created` event reaches React state and the
 * proxy mounts. Window moves land through a `session-assignment-changed`
 * event. Both overrun the SDK's fallback settle band, so those signatures
 * carry an explicit `settleMs`.
 */

import { getCurrentWindow } from "@tauri-apps/api/window";
import type {
  ActionParams,
  EffectSignature,
  PredictedDelta,
  SemanticSnapshot,
} from "@qontinui/ui-bridge";

import { FLOW_GRID_ID } from "./useZoneLayout";

/** Every terminal tab's UI Bridge element (`TerminalBridgeProxies`). */
export const TERMINAL_INPUT_GLOB = "terminal-input-*";
/** Every rendered zone header (`zoneHeaderElementSpec` in `zone-grid/ZoneLabel.tsx`). */
export const ZONE_HEADER_GLOB = "terminal-zone-header-*";

/** The terminal surface: tab elements plus the zone headers they render under. */
const TERMINAL_ZONE_SCOPE = { elementIds: [TERMINAL_INPUT_GLOB, ZONE_HEADER_GLOB] };

/**
 * Settle window for a PTY spawn: the PTY exists when the handler resolves, but
 * its element registers only after `terminal-created` reaches React state and
 * the proxy mounts.
 */
export const PTY_SPAWN_SETTLE_MS = 1500;

/** Settle window for a window move landing through `session-assignment-changed`. */
export const WINDOW_EVENT_SETTLE_MS = 750;

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
    settleMs: PTY_SPAWN_SETTLE_MS,
    reversibility,
    provenance: "declared",
  };
}

// ---------------------------------------------------------------------------
// terminal-page
// ---------------------------------------------------------------------------

/**
 * `create-terminal` — raw `createTerminal`. The handler itself assigns no
 * zone, but `useZoneLayout` does as the tab arrives (`reconcileAssignments`,
 * and auto-grow when it overflows the layout), so a header may follow — same
 * claim and widened scope as `create-plain-terminal`. Closing the tab undoes it.
 */
export const CREATE_TERMINAL_SIGNATURE = spawnSignature(
  "terminal-page.create-terminal",
  "reversible",
  TERMINAL_ZONE_SCOPE,
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
  settleMs: WINDOW_EVENT_SETTLE_MS,
  reversibility: "reversible",
  provenance: "declared",
};

/** Non-plan tabs: the ones that own a `terminal-input-*` element. */
export function countTerminalTabs(tabs: ReadonlyArray<{ type?: string }>): number {
  return tabs.filter((t) => t.type !== "plan").length;
}

/**
 * Pure core of `pop-out-page`'s prediction.
 *
 * Binding a page to a new window beats the per-terminal owner map, so all of
 * that page's tab elements leave this registry. Which `terminal-input-*` ids
 * belong to a page is not visible in the snapshot, so the criterion is the
 * glob. For the ACTIVE page (the default target) the page's own terminal count
 * is known, so an empty page predicts nothing. For another page only the
 * snapshot is available: a disappearance is predicted when this window renders
 * any tab at all — an over-prediction when that page happens to be empty.
 */
export function predictPopOutPage(
  params: ActionParams,
  pre: SemanticSnapshot,
  activePageId: string,
  activePageTerminals: number,
): PredictedDelta {
  const target = textParam(params, "pageId") ?? activePageId;
  const hasTabs =
    target === activePageId ? activePageTerminals > 0 : preHasPrefix(pre, TERMINAL_INPUT_PREFIX);
  return hasTabs ? { elementsDisappear: [{ id: TERMINAL_INPUT_GLOB }] } : {};
}

/** `pop-out-page`, built from the active page and its terminal count. */
export function popOutPageSignature(
  activePageId: string,
  activePageTerminals: number,
): EffectSignature {
  return {
    id: "terminal-page.pop-out-page",
    predicts: (params, pre) => predictPopOutPage(params, pre, activePageId, activePageTerminals),
    scope: TERMINAL_ZONE_SCOPE,
    settleMs: WINDOW_EVENT_SETTLE_MS,
    reversibility: "reversible",
    provenance: "declared",
  };
}

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
  settleMs: WINDOW_EVENT_SETTLE_MS,
  reversibility: "reversible",
  provenance: "declared",
};

/** The zone-view state the maximize/restore handlers act on. */
export interface ZoneViewFacts {
  maximizedZone: number | null;
  zoneCount: number;
  /**
   * How many zone headers the TILED grid renders — occupied zones whose cell
   * is not a compact card (`ZoneGrid`: `!showCompactCard && showLabels`).
   * `null` when that cannot be known: flow-grid virtualization decides which
   * cells mount from the viewport.
   */
  labelledZonesWhenTiled: number | null;
}

/** What {@link countLabelledZones} needs from the page. */
export interface ZoneLabelInputs {
  layoutId: string;
  zoneCount: number;
  assignments: Readonly<Record<number, string>>;
  tabs: ReadonlyArray<{ id: string; type?: string }>;
  focusedZone: number;
  pinnedZones: ReadonlySet<number>;
  /** `viewMode === "compact"` — `ZoneGrid`'s `forceCompact`. */
  compactView: boolean;
}

/**
 * Count the zone headers the tiled grid renders, mirroring `ZoneGrid`: no
 * headers in a single-zone layout; a header per OCCUPIED zone, except one
 * shown as a compact card — a non-plan tab that is neither focused nor pinned
 * while the view is forced compact. Flow-grid virtualization is decided by
 * the viewport, so it answers `null` (unknown) rather than a guess.
 */
export function countLabelledZones(input: ZoneLabelInputs): number | null {
  if (input.layoutId === FLOW_GRID_ID) return null;
  if (input.zoneCount <= 1) return 0;
  const tabsById = new Map(input.tabs.map((t) => [t.id, t]));
  let count = 0;
  for (let zone = 0; zone < input.zoneCount; zone++) {
    const tabId = input.assignments[zone];
    const tab = tabId === undefined ? undefined : tabsById.get(tabId);
    if (!tab) continue;
    const compact =
      tab.type !== "plan" &&
      zone !== input.focusedZone &&
      !input.pinnedZones.has(zone) &&
      input.compactView;
    if (!compact) count += 1;
  }
  return count;
}

/**
 * Zone headers render only in the tiled multi-zone view (`ZoneGrid`:
 * `showLabels = isMultiZone`, and the maximized branch renders its own
 * unregistered header instead of `ZoneLabel`). So entering the maximized view
 * removes every rendered header, and leaving it brings back the ones the
 * tiled grid renders — if that is known and non-zero.
 */
function enterMaximized(pre: SemanticSnapshot, facts: ZoneViewFacts): PredictedDelta {
  if (facts.maximizedZone !== null || !preHasPrefix(pre, ZONE_HEADER_PREFIX)) return {};
  return { elementsDisappear: [{ id: ZONE_HEADER_GLOB }] };
}

function leaveMaximized(facts: ZoneViewFacts): PredictedDelta {
  if (facts.maximizedZone === null || facts.zoneCount <= 1) return {};
  if (facts.labelledZonesWhenTiled === null || facts.labelledZonesWhenTiled === 0) return {};
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
 * predict. Built per render from the zone view as rendered (see
 * `useZoneViewFacts`), which is the state a predict between renders sees.
 */
export function zoneViewSignature(
  kind: "maximize" | "restore" | "toggle",
  facts: ZoneViewFacts,
): EffectSignature {
  const actionId = kind === "toggle" ? "toggle-maximize-zone" : `${kind}-zone`;
  return {
    id: `terminal-page.${actionId}`,
    predicts: (params, pre) => predictZoneView(kind, params, pre, facts),
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
