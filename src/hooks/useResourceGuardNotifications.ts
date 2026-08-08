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
  /** `"warn"` — spawned below the warn floor. `"override"` — spawned below the
   *  critical floor because the caller overrode the refusal. */
  severity: string;
  lane: string;
  freeBytes: number;
  floorBytes: number;
  /** Fully-composed operator-facing text; already names lane/headroom/floor. */
  message: string;
}

/**
 * Jump to the Resource Guard settings panel.
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
      // An override means the spawn went ahead below the CRITICAL floor — the
      // heavier of the two states, so it gets the error styling. A plain warn is
      // informational: the spawn was fine, the headroom is not.
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
