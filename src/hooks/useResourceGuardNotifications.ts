/**
 * useResourceGuardNotifications — surfaces the spawn-time resource gate's
 * notices as toasts (plan
 * `2026-08-07-runner-resource-guard-and-session-protection.md` §Part D step 2).
 *
 * Mount once at App level, beside `useErrorNotifications(showToast)`, and pass
 * `showToast` from `useToast()`. This is the runner's real toast system
 * (`useToast` + `ToastContainer`, rendered `fixed bottom-4 right-4`) — not
 * `components/terminal/MidSessionToast.tsx`, which is a single-purpose un-queued
 * overlay for file-claim collisions rendered from one place inside the terminal
 * pane, and not `components/app/AppToasts.tsx`, which is two hardcoded divs for
 * two specific events.
 *
 * The Rust side emits on this channel for the WARN verdict (the spawn proceeded,
 * but the box is below the warn floor) and for a CRITICAL verdict that an
 * override let through. It does NOT emit for a CRITICAL refusal: that returns as
 * a typed `Err` and becomes the blocking `ResourceGuardDialog`, and stacking a
 * self-dismissing toast on top of the modal asking the operator to decide would
 * be noise at the exact moment they are reading.
 */

import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import type { ShowToastFn } from "./useToast";

/** Must match `resource_guard::RESOURCE_GUARD_EVENT`. */
const RESOURCE_GUARD_EVENT = "resource-guard-notice";

/** Tab id of the `Settings > Resource Guard` panel (`tab-types.ts`). */
const RESOURCE_GUARD_TAB = "settings-resource-guard";

/** Payload of {@link RESOURCE_GUARD_EVENT}, mirroring `emit_notice`. */
interface ResourceGuardNotice {
  /** `"warn"` — spawned past the warn limit. `"override"` — spawned past the
   *  critical limit because the caller overrode the refusal. */
  severity: string;
  /** `"host"` | `"wsl"` | `"threads"` — `resource_sample::Lane::as_str`. */
  lane: string;
  /**
   * Which quantity {@link observed} and {@link limit} are in, and therefore
   * which way is bad: `"free_commit_bytes"` (a FLOOR — the reading is below the
   * limit) or `"thread_count"` (a CEILING — the reading is above it).
   *
   * The gate reads two sensors that disagree about direction, so the payload
   * cannot use direction-carrying names. There is deliberately no legacy
   * `freeBytes` / `floorBytes` alias: those names are wrong for half the events
   * they would carry, and a field whose name lies half the time is worse than a
   * rename.
   */
  metric: string;
  /** The reading, in {@link metric}'s unit. */
  observed: number;
  /** The floor it fell below, or the ceiling it rose above. */
  limit: number;
  /** Fully-composed operator-facing text; already names the lane, the reading
   *  and the limit in the right unit and direction. Render this, do not
   *  re-derive it from the numbers — the Rust side owns the phrasing. */
  message: string;
}

/**
 * Jump to the Resource Guard settings panel.
 *
 * Attached to EVERY notice, both lanes. That action was a half-truth while
 * `ResourceGuardSettings.tsx` rendered the two GiB floors only: a thread-lane
 * toast sent the operator to a panel that could neither show nor change the
 * ceiling it had just quoted, leaving them a choice between disabling the whole
 * guard and hand-editing `settings.json`. The panel now edits all four limits
 * (`2026-08-30-load-aware-spawn-admission-control`), which is what makes both
 * this action and `resource_guard::critical_refusal`'s closing sentence true.
 *
 * Uses the same `ui-bridge-set-tab` window event `Settings.tsx` fires when its
 * own sub-nav changes; `useAppNavigation` listens for it and persists the tab.
 * `TabContent` routes the whole `settings-*` family through one arm
 * (`isSettingsTabId`), so this id cannot be "valid but unrendered" — the trap
 * `CiRunnerSettings.tsx`'s header documents.
 */
function openResourceGuardSettings(): void {
  window.dispatchEvent(
    new CustomEvent<{ tab: string }>("ui-bridge-set-tab", {
      detail: { tab: RESOURCE_GUARD_TAB },
    }),
  );
}

export function useResourceGuardNotifications(showToast: ShowToastFn) {
  useEffect(() => {
    const unlisten = listen<ResourceGuardNotice>(RESOURCE_GUARD_EVENT, (event) => {
      const notice = event.payload;
      if (!notice?.message) return;
      // An override means the spawn went ahead past the CRITICAL limit — the
      // heavier of the two states, so it gets the error styling. A plain warn is
      // informational: the spawn was fine, the headroom is not. Neither branch
      // reads `metric`: the message is already composed for the lane that spoke,
      // which is what keeps this hook lane-agnostic.
      showToast(notice.message, notice.severity === "override" ? "error" : "info", {
        label: "Open Resource Guard settings",
        onClick: openResourceGuardSettings,
      });
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, [showToast]);
}
