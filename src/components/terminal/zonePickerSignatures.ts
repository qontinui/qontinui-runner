/**
 * Effect-calculus signatures for the `zone-layout-picker` and
 * `zone-profile-picker` UI Bridge components.
 *
 * Plan `2026-10-22-runner-ui-bridge-makes-an-agent-guess-its-own-vocabulary`,
 * Phase 2. The conventions — what a prediction claims, glob criteria, scope
 * widening instead of invented ids, `<componentId>.<actionId>` ids — are
 * documented in `./terminalPageSignatures.ts`.
 *
 * Both pickers mount inside a `display: none` container and register no
 * elements of their own, so their handlers are visible in this registry only
 * through what they do to the terminal grid.
 */

import type {
  ActionParams,
  EffectSignature,
  PredictedDelta,
  SemanticSnapshot,
} from "@qontinui/ui-bridge";

import { LAYOUT_PRESETS, resolveLayoutArg } from "./useZoneLayout";
import {
  readSignature,
  preHasPrefix,
  type Live,
  PTY_SPAWN_SETTLE_MS,
  TERMINAL_INPUT_GLOB,
  ZONE_HEADER_GLOB,
} from "./terminalPageSignatures";
import type { ZoneProfile } from "./zoneProfileStorage";

const ZONE_HEADER_PREFIX = "terminal-zone-header-";

function textParam(params: ActionParams, key: string): string | undefined {
  const value = params.params?.[key];
  return typeof value === "string" && value.length > 0 ? value : undefined;
}

// ---------------------------------------------------------------------------
// zone-layout-picker
// ---------------------------------------------------------------------------

/**
 * Pure core of `select-layout`'s prediction.
 *
 * Zone headers render only in a multi-zone layout (`showLabels =
 * isMultiZone`), but a single-zone choice is a starting point, not a latch:
 * `useZoneLayout`'s auto-grow effect re-expands any layout the page's live
 * tabs overflow (`computeAutoGrowLayoutId`, keyed on every tab, plan tabs
 * included). So switching to a single-zone preset removes the rendered
 * headers ONLY when the page has at most one tab; with more, the grid grows
 * straight back and the headers stay. Between multi-zone presets `applyLayout`
 * preserves in-range assignments and compacts the rest, and which headers that
 * leaves is not stated by the handler, so it is not predicted (an unknown
 * layout id throws before any effect, so nothing is predicted for it either).
 *
 * The layout argument resolves through `resolveLayoutArg`, the handler's own
 * resolver, so a window count (`"1"`) predicts what `"single"` does. A
 * `pageId` makes no prediction: it may name a page that is not on screen, whose
 * grid renders none of the headers this registry sees, and `tabCount` is the
 * on-screen page's count, not that page's.
 */
export function predictSelectLayout(
  params: ActionParams,
  pre: SemanticSnapshot,
  tabCount: number,
): PredictedDelta {
  if (textParam(params, "pageId") !== undefined) return {};
  const raw = textParam(params, "layoutId");
  const layoutId = raw === undefined ? null : resolveLayoutArg(raw);
  const preset = LAYOUT_PRESETS.find((l) => l.id === layoutId);
  if (!preset) return {};
  if (preset.zones.length <= 1 && tabCount <= 1 && preHasPrefix(pre, ZONE_HEADER_PREFIX)) {
    return { elementsDisappear: [{ id: ZONE_HEADER_GLOB }] };
  }
  return {};
}

/**
 * `select-layout` — switches the grid preset; another `select-layout` undoes
 * it. `tabCount` reads the on-screen page's tab count when `predict` runs,
 * which is the same count auto-grow keys on.
 */
export function selectLayoutSignature(tabCount: Live<number>): EffectSignature {
  return {
    id: "zone-layout-picker.select-layout",
    predicts: (params, pre) => predictSelectLayout(params, pre, tabCount.current),
    scope: { elementIds: [ZONE_HEADER_GLOB] },
    reversibility: "reversible",
    provenance: "declared",
  };
}

export const LIST_LAYOUTS_SIGNATURE = readSignature("zone-layout-picker.list-layouts");

/** `get-page-layout` — reports one page's grid state and changes nothing. */
export const GET_PAGE_LAYOUT_SIGNATURE = readSignature("zone-layout-picker.get-page-layout");

// ---------------------------------------------------------------------------
// zone-profile-picker
// ---------------------------------------------------------------------------

/**
 * Pure core of `load-profile`'s prediction.
 *
 * `useApplyZoneProfile` replaces layout/labels/notes/pins/auto-approve, and
 * spawns `sessions.length - liveTabs.length` terminals when the profile carries
 * more sessions than the page has tabs. Only that spawn is an element change
 * the handler fixes: each new PTY tab surfaces a `terminal-input-*` element.
 * The layout swap's effect on zone headers depends on the live assignments,
 * so the scope widens to the headers instead of predicting them.
 */
export function predictLoadProfile(
  params: ActionParams,
  profiles: Readonly<Record<string, ZoneProfile>>,
  liveTabCount: number,
): PredictedDelta {
  const name = textParam(params, "name");
  const profile = name === undefined ? undefined : profiles[name];
  if (!profile) return {};
  const sessions = profile.sessions?.length ?? 0;
  return sessions > liveTabCount ? { elementsAppear: [{ id: TERMINAL_INPUT_GLOB }] } : {};
}

/**
 * `load-profile` — `destructive`, so `one-way`: the arrangement it overwrites
 * was never saved, and it can launch `claude --resume` into new tabs. The
 * profiles map and live tab count are read when `predict` runs. These are the
 * values the handler acts on, not the ones from the render that registered
 * the action, where `profiles` is still the empty map from before the DB
 * load. The handler awaits the whole apply (each spawn included), and the
 * PTY settle window covers the last spawned tab's element registering.
 */
export interface LoadProfileFacts {
  profiles: Readonly<Record<string, ZoneProfile>>;
  liveTabCount: number;
}

export function loadProfileSignature(facts: Live<LoadProfileFacts>): EffectSignature {
  return {
    id: "zone-profile-picker.load-profile",
    predicts: (params) => {
      const { profiles, liveTabCount } = facts.current;
      return predictLoadProfile(params, profiles, liveTabCount);
    },
    scope: { elementIds: [TERMINAL_INPUT_GLOB, ZONE_HEADER_GLOB] },
    settleMs: PTY_SPAWN_SETTLE_MS,
    reversibility: "one-way",
    provenance: "declared",
  };
}

/**
 * `save-profile` / `delete-profile` — persisted settings only. The picker's
 * own list renders no registered elements (and is `display: none`), so this
 * registry is predicted unchanged. Both are `destructive` and `one-way`: an
 * overwritten or deleted profile has no copy to restore from.
 */
export const SAVE_PROFILE_SIGNATURE: EffectSignature = {
  id: "zone-profile-picker.save-profile",
  predicts: (): PredictedDelta => ({}),
  scope: {},
  reversibility: "one-way",
  provenance: "declared",
};

export const DELETE_PROFILE_SIGNATURE: EffectSignature = {
  id: "zone-profile-picker.delete-profile",
  predicts: (): PredictedDelta => ({}),
  scope: {},
  reversibility: "one-way",
  provenance: "declared",
};

export const LIST_PROFILES_SIGNATURE = readSignature("zone-profile-picker.list-profiles");
