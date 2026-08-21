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
 * why the same thing is off is its own predictability failure.
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
   * Label of the in-app action, or `null` when the fix is not in the app.
   * A settings.json repair happens on disk; offering a button that cannot
   * perform it would be the same dishonesty this component exists to end.
   */
  actionLabel: string | null;
  /** Runner tab id the action button activates. `null` when no action. */
  actionTab: string | null;
}

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
export function coordDisabledCopy(source: string | null, surface: string): CoordDisabledCopy {
  if (source === COORD_SOURCE_SETTINGS_UNREADABLE) {
    return {
      reason: "settings-unreadable",
      title: `${surface} is off — this runner could not read its settings.json`,
      body:
        `${surface} needs a connected qontinui account, and this runner cannot tell whether it ` +
        "has one: its settings.json could not be read or parsed, so its tier is unknown and it " +
        "refused to assume fleet membership. This is NOT a missing account — connecting one " +
        "will not fix it. Repair or restore com.qontinui.runner/settings.json in your OS " +
        "config directory, then restart the runner.",
      actionLabel: null,
      actionTab: null,
    };
  }
  return {
    reason: "no-account",
    title: `${surface} needs a connected qontinui account`,
    body:
      `This runner is in isolated mode — no qontinui account is connected, so there is no ` +
      `fleet behind ${surface.toLowerCase()} and nothing for these controls to act on. Connect ` +
      "an account under Settings → Account (or point COORD_HTTP_URL at a coordinator) and this " +
      "surface enables itself; nothing else about the runner changes.",
    actionLabel: "Open Settings → Account",
    actionTab: "settings-account",
  };
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
  uiBridgeId = "coord.connection-required",
  className,
}: CoordConnectionRequiredProps) {
  const titleId = useId();
  const copy = coordDisabledCopy(source, surface);
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
            onClick={() => {
              // Same window event Settings.tsx fires for its own sub-tab
              // switches; `useAppNavigation` resolves it against the
              // MainTabId union. Avoids threading a navigate callback
              // through every gated panel.
              window.dispatchEvent(
                new CustomEvent<{ tab: string }>("ui-bridge-set-tab", {
                  detail: { tab: copy.actionTab as string },
                }),
              );
            }}
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
