/**
 * Dismissible notice for a remote tab whose close did NOT demonstrably hand
 * the target's terminal back.
 *
 * Closing a remote tab is an action with a remote dependency: the tab vanishes
 * immediately (the LOCAL close always succeeds), but the relay binding that
 * makes the target's terminal busy for everyone else is only dropped when the
 * detach frame actually reaches the relay. `terminal_close` reports which of
 * those happened (`remoteDetach.outcome` — plan
 * `2026-09-16-remote-tab-cannot-be-released-so-the-target-terminal-stays-claimed`,
 * Phase 1); until this component existed the frontend discarded that answer,
 * so a failed detach looked exactly like a clean one.
 *
 * Rendered ONLY for the unclean outcomes — see `isCleanRemoteClose`. A detach
 * queued on a steadily-attached relay pump is the ordinary case and says
 * nothing worth interrupting for; the tab disappearing is its own
 * acknowledgement.
 *
 * WHY THIS IS AN IN-FLOW STRIP AND NOT A FLOATING CORNER WIDGET.
 * It is rendered as a sibling of `OutputSearchBar`, in the page's flex
 * column ABOVE the zone container, so it occludes nothing at all
 * [policy: ux-priorities no-widget-may-hide-identifying-text] — which is the
 * only placement that needs no occlusion argument. Two facts drove that:
 *
 *  - Neither corner of the zone container is free. `top-2 right-2` already
 *    holds `MidSessionToast`, `HoldingLockBanner`, `WaitingLockBanner`,
 *    `ResumeFailedBanner` and `SessionRecoveryBanner`, all at `z-30` and NOT
 *    mutually exclusive with each other. `bottom-2 right-2` is the anchor
 *    `ZoneMinimap` was deliberately moved OFF (`ZoneMinimap.tsx:116-126`)
 *    because in flow mode the grid routinely puts a tile HEADER there and the
 *    widget covered the session name — and the bottom of a pane is the live
 *    prompt line besides.
 *  - More fundamentally: this notice is about a tab that NO LONGER EXISTS.
 *    Floating it over the zone grid would occlude a pane it does not
 *    describe, which is the wrong relationship regardless of which corner
 *    happens to be emptiest.
 *
 * It is still registered with UI Bridge so an automated occlusion sweep can
 * see this element as a potential occluder rather than only the things
 * beneath it — the same reasoning `ZoneMinimap.tsx:60-69` records.
 */

import { useUIElement } from "@qontinui/ui-bridge";

import type { RemoteCloseNoticeState } from "./useTerminalManager";

interface RemoteCloseNoticeProps {
  notice: RemoteCloseNoticeState;
  onDismiss: () => void;
}

export function RemoteCloseNotice({ notice, onDismiss }: RemoteCloseNoticeProps) {
  const { ref } = useUIElement({
    id: "terminal-remote-close-notice",
    type: "generic",
    label: "Remote close notice",
  });

  return (
    <div
      ref={ref}
      data-testid="remote-close-notice"
      data-ui-bridge-id={`terminal.remote-close-notice-${notice.tabId}`}
      className="flex items-start gap-2 px-3 py-1.5 bg-[#e0af68]/15 border-b border-[#e0af68]/40 shrink-0"
    >
      <span className="text-[10px] uppercase tracking-wider text-[#e0af68] shrink-0 mt-px">
        Remote terminal may still be claimed
      </span>
      {/* The runner's own sentence, not a re-derivation: it is the one place
          that knows which of queued / failed / not-attempted / unknown
          happened, and it never claims a release it did not observe. */}
      <p className="text-[10px] leading-snug text-[#c0caf5] flex-1">{notice.message}</p>
      <button
        type="button"
        data-testid="remote-close-notice-dismiss"
        data-ui-bridge-id={`terminal.remote-close-notice-dismiss-${notice.tabId}`}
        aria-label="Dismiss remote close notice"
        onClick={onDismiss}
        className="text-[#e0af68] hover:text-[#c0caf5] leading-none px-1 shrink-0"
      >
        ×
      </button>
    </div>
  );
}
