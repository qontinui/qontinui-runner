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

import { LAYOUT_PRESETS } from "./useZoneLayout";
import {
  readSignature,
  preHasPrefix,
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
 */
export function predictSelectLayout(
  params: ActionParams,
  pre: SemanticSnapshot,
  tabCount: number,
): PredictedDelta {
  const layoutId = textParam(params, "layoutId");
  const preset = LAYOUT_PRESETS.find((l) => l.id === layoutId);
  if (!preset) return {};
  if (preset.zones.length <= 1 && tabCount <= 1 && preHasPrefix(pre, ZONE_HEADER_PREFIX)) {
    return { elementsDisappear: [{ id: ZONE_HEADER_GLOB }] };
  }
  return {};
}

/**
 * `select-layout` — switches the grid preset; another `select-layout` undoes
 * it. `tabCount` is the page's tab count the picker is rendered with — the
 * same count auto-grow keys on.
 */
export function selectLayoutSignature(tabCount: number): EffectSignature {
  return {
    id: "zone-layout-picker.select-layout",
    predicts: (params, pre) => predictSelectLayout(params, pre, tabCount),
    scope: { elementIds: [ZONE_HEADER_GLOB] },
    reversibility: "reversible",
    provenance: "declared",
  };
}

export const LIST_LAYOUTS_SIGNATURE = readSignature("zone-layout-picker.list-layouts");

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
 * profiles map and live tab count are the ones the handler closes over. The
 * handler awaits the whole apply (each spawn included), and the PTY settle
 * window covers the last spawned tab's element registering.
 */
export function loadProfileSignature(
  profiles: Readonly<Record<string, ZoneProfile>>,
  liveTabCount: number,
): EffectSignature {
  return {
    id: "zone-profile-picker.load-profile",
    predicts: (params) => predictLoadProfile(params, profiles, liveTabCount),
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
