/**
 * CoordConnectionRequired — the single disabled affordance every
 * coord-backed surface renders when the runner is in isolated mode.
 *
 * Plan `2026-08-18-runner-embedded-pg-parity-and-coord-http-migration` §6.4.
 *
 * The plan ranked three options against the UX priority order
 * (predictability → discoverability without clutter → no-surprise
 * reversibility → honesty about uncertainty) and chose this one:
 *
 *   - *Hiding* the surface fails discoverability — the operator cannot learn
 *     the capability exists or what would turn it on, and the UI silently
 *     changes shape between two runners of the same version.
 *   - *Showing it live over a no-op sink* fails honesty outright — the
 *     operator clicks and nothing happens.
 *   - *Disabled + a stated reason* is predictable (the state is visible and
 *     stable), discoverable without clutter, reversible with no surprise
 *     (configuring a coord URL enables it, and the panel says so), and
 *     honest about why the feature is inert.
 *
 * ONE component, not per-panel ad-hoc copy: two runners disagreeing about
 * why the same thing is off is its own predictability failure. That applies
 * to TOOLTIPS too — `coordDisabledCopy` owns the `tooltip` string, and every
 * disabled control reads it from here rather than hard-coding one. A
 * hard-coded "connect an account" tooltip beside a body that says "this is
 * NOT a missing account" puts two contradictory diagnoses on one screen.
 *
 * Two distinct reasons, because they are two distinct problems with two
 * distinct fixes — see `coordDisabledCopy`.
 */

import { useId } from "react";
import { PlugZap, FileWarning } from "lucide-react";

import { COORD_SOURCE_SETTINGS_UNREADABLE } from "@/contexts/CoordModeContext";

/** Which of the two isolated problems the operator is looking at. */
export type CoordDisabledReason = "no-account" | "settings-unreadable";

/** Fully-resolved copy for one disabled surface. */
export interface CoordDisabledCopy {
  reason: CoordDisabledReason;
  /** Short heading — states that the feature is off and, broadly, why. */
  title: string;
  /** What is actually wrong and what the operator should do about it. */
  body: string;
  /**
   * One-line `title=` for the surface's own disabled controls. Derived from
   * the SAME `source` as {@link CoordDisabledCopy.body} so a tooltip can
   * never contradict the notice beside it.
   */
  tooltip: string;
  /**
   * Label of the in-app action, or `null` when the fix is not in the app.
   * A settings.json repair happens on disk; offering a button that cannot
   * perform it would be the same dishonesty this component exists to end.
   */
  actionLabel: string | null;
  /** Runner tab id the action button activates. `null` when no action. */
  actionTab: string | null;
}

/** Shape options for {@link coordDisabledCopy}. */
export interface CoordDisabledCopyOptions {
  /**
   * Whether the disabled surface actually has controls the operator could
   * have clicked. Default `true`. A read-only section (the File Activity
   * live heatmap) has none, and telling its reader there is "nothing for
   * these controls to act on" names controls that do not exist.
   */
  hasControls?: boolean;
}

/**
 * Where the runner's `settings.json` lives, phrased so it stays true on a
 * supervisor-spawned secondary instance.
 *
 * `settings::get_config_dir()` honours a `QONTINUI_CONFIG_DIR` override, so
 * naming `com.qontinui.runner/settings.json` unconditionally would point a
 * secondary runner's operator at the wrong file. The frontend has no
 * resolved path to show — surfacing the real one needs a backend command,
 * and this PR touches no Rust — so the wording names both possibilities
 * instead of asserting one.
 */
const SETTINGS_PATH_HINT =
  "its settings.json (under com.qontinui.runner/ in your OS config directory, " +
  "or wherever QONTINUI_CONFIG_DIR points if this instance sets it)";

/**
 * Map a `CoordMode.source` onto operator-facing copy.
 *
 * `unknown_tier_prod_default` means the runner could not READ its
 * `settings.json`, so its tier is unknown and the resolver refused to treat
 * the production default as a real membership. The operator may well have a
 * perfectly good account — telling them to go connect one would send them to
 * a screen that cannot help, and would leave the real fault (an unreadable
 * config file) undiagnosed. Every other isolated source means what it looks
 * like: nothing is configured.
 *
 * Pure, so it is testable under the runner's `environment: "node"` vitest
 * config (no jsdom — see FleetHealthPanel.test.tsx for the same constraint).
 */
export function coordDisabledCopy(
  source: string | null,
  surface: string,
  options: CoordDisabledCopyOptions = {},
): CoordDisabledCopy {
  const hasControls = options.hasControls ?? true;

  if (source === COORD_SOURCE_SETTINGS_UNREADABLE) {
    return {
      reason: "settings-unreadable",
      title: `${surface} is off — this runner could not read its settings.json`,
      body:
        `${surface} needs a connected qontinui account, and this runner cannot tell whether it ` +
        `has one: ${SETTINGS_PATH_HINT} could not be read or parsed, so its tier is unknown and ` +
        "it refused to assume fleet membership. This is NOT a missing account — connecting one " +
        "will not fix it. Repair or restore that file, then restart the runner.",
      tooltip:
        "Disabled — this runner could not read its settings.json, so its tier is unknown. " +
        "Repair that file and restart; connecting an account will not fix it.",
      actionLabel: null,
      actionTab: null,
    };
  }

  const nothingToActOn = hasControls
    ? `and nothing for these controls to act on`
    : `and nothing for it to show`;

  return {
    reason: "no-account",
    title: `${surface} needs a connected qontinui account`,
    body:
      `This runner is in isolated mode — no qontinui account is connected, so there is no ` +
      `fleet behind ${surface.toLowerCase()} ${nothingToActOn}. Connect an account under ` +
      "Settings → Account (or point COORD_HTTP_URL at a coordinator) and this surface enables " +
      "itself; nothing else about the runner changes.",
    tooltip:
      "Disabled — no qontinui account is connected. Connect one under Settings → Account to " +
      "enable this.",
    actionLabel: "Open Settings → Account",
    actionTab: "settings-account",
  };
}

/**
 * Fire the notice's in-app action.
 *
 * `onDismiss` runs FIRST and unconditionally. The action navigates the
 * runner to another tab, and a host that is an overlay — `SpawnFromPlanModal`
 * is `fixed inset-0` with `aria-modal="true"` — would otherwise keep covering
 * the tab it just switched to, so the operator sees nothing happen. That is
 * precisely the failure this component exists to remove, so the dismiss is
 * part of the action rather than an optional extra.
 *
 * `dispatch` is injected so the ordering contract is testable under
 * `environment: "node"` (no jsdom, so the button cannot be clicked).
 */
export function runCoordDisabledAction(args: {
  copy: CoordDisabledCopy;
  onDismiss?: () => void;
  dispatch: (tab: string) => void;
}): void {
  const { copy, onDismiss, dispatch } = args;
  if (!copy.actionTab) return;
  onDismiss?.();
  dispatch(copy.actionTab);
}

/** Default `dispatch` — the same window event `Settings.tsx` fires for its
 *  own sub-tab switches; `useAppNavigation` resolves it against the
 *  `MainTabId` union. Avoids threading a navigate callback through every
 *  gated panel. */
function dispatchSetTab(tab: string): void {
  window.dispatchEvent(
    new CustomEvent<{ tab: string }>("ui-bridge-set-tab", { detail: { tab } }),
  );
}

export interface CoordConnectionRequiredProps {
  /** `CoordMode.source` — selects which of the two messages applies. */
  source: string | null;
  /**
   * Human name of the disabled surface, capitalised as a sentence start
   * (e.g. "Fleet health"). Woven into both messages so the operator knows
   * which panel the notice belongs to when several are stacked.
   */
  surface: string;
  /** See {@link CoordDisabledCopyOptions.hasControls}. */
  hasControls?: boolean;
  /**
   * Run immediately before the action navigates away. A host that covers the
   * screen (a modal) MUST pass its close handler, or the navigation lands
   * behind it. See {@link runCoordDisabledAction}.
   */
  onDismiss?: () => void;
  /** UI Bridge id for the notice block. */
  uiBridgeId?: string;
  className?: string;
}

/**
 * Render the isolated-mode notice.
 *
 * Keyboard + screen reader: the notice is a `role="note"` labelled by its own
 * heading, so it sits in the reading order of the panel region it belongs to.
 * That placement is load-bearing — the controls it explains are genuinely
 * `disabled` and therefore NOT focusable, so the explanation has to be
 * reachable as content rather than as a tooltip on an unreachable button.
 * The action, when there is one, is a real focusable `<button>`.
 */
export function CoordConnectionRequired({
  source,
  surface,
  hasControls,
  onDismiss,
  uiBridgeId = "coord.connection-required",
  className,
}: CoordConnectionRequiredProps) {
  const titleId = useId();
  const copy = coordDisabledCopy(source, surface, { hasControls });
  const Icon = copy.reason === "settings-unreadable" ? FileWarning : PlugZap;

  return (
    <div
      role="note"
      aria-labelledby={titleId}
      data-ui-bridge-id={uiBridgeId}
      data-coord-disabled-reason={copy.reason}
      className={`flex flex-col gap-2 rounded-md border border-border/60 bg-muted/10 p-3 ${
        className ?? ""
      }`}
    >
      <div className="flex items-start gap-2">
        <Icon className="w-4 h-4 mt-0.5 shrink-0 text-muted-foreground" aria-hidden="true" />
        <p id={titleId} className="text-xs font-semibold text-foreground">
          {copy.title}
        </p>
      </div>
      <p className="text-xs text-muted-foreground">{copy.body}</p>
      {copy.actionLabel && copy.actionTab ? (
        <div>
          <button
            type="button"
            onClick={() => runCoordDisabledAction({ copy, onDismiss, dispatch: dispatchSetTab })}
            data-ui-bridge-id={`${uiBridgeId}-action`}
            className="inline-flex items-center gap-1 rounded-md border border-border bg-muted/20 px-2 py-1 text-xs text-foreground hover:bg-muted/40"
          >
            {copy.actionLabel}
          </button>
        </div>
      ) : null}
    </div>
  );
}

export default CoordConnectionRequired;
