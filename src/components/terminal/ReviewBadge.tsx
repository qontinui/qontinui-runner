/**
 * Phase 3 of the productivity-stack plan — `ReviewBadge`.
 *
 * Small per-tab indicator showing the latest `/auto-review` verdict for
 * the tab's session. States per plan §6:
 *
 *   - empty (no reviews yet)               → render nothing.
 *   - pending                              → spinner.
 *   - approved high-confidence (≥ 0.85)    → green check + confidence.
 *   - approved medium-confidence ([0.7,    → yellow check; click opens
 *       0.85))                                the recommendations queue
 *                                            with this row pre-selected.
 *   - needs_fix                            → orange exclamation; tooltip =
 *                                            top-defect line.
 *   - escalated                            → red flag.
 *
 * Subscribes to the `review-completed` Tauri event for instant updates
 * AND polls `GET /sessions/<id>/latest-review` every 30 s as a stale-
 * resilience fallback. Both managed by `useReviewState`.
 */

import { AlertTriangle, CheckCircle2, Flag, Loader2 } from "lucide-react";
import { useReviewState, type ReviewState } from "./useReviewState";

interface ReviewBadgeProps {
  sessionId: string | undefined;
  /** Optional size (px). Default 14 — matches the per-tab control density. */
  size?: number;
}

/** Dispatch the cross-component event the Productivity tab listens for in
 *  order to switch to the Coordinator view with the recommendation pre-
 *  selected. The Productivity tab subscribes via a window event listener. */
function openRecommendationsQueue(reviewId: string) {
  // First nudge the Productivity tab into the coordinator sub-view.
  window.dispatchEvent(new CustomEvent("productivity-set-view", { detail: "coordinator" }));
  // Then ship a separate event with the review id so the Recommendations
  // panel can scroll/highlight that row. Two events instead of one nested
  // detail object so older listeners that only know about
  // `productivity-set-view` continue working.
  window.dispatchEvent(
    new CustomEvent("productivity-select-recommendation", {
      detail: { reviewId },
    }),
  );
}

function badgeContent(state: ReviewState, size: number) {
  const conf = `${(state.confidence * 100).toFixed(0)}%`;
  switch (state.status) {
    case "approved-high":
      return {
        icon: <CheckCircle2 width={size} height={size} className="text-emerald-400" />,
        text: conf,
        textClass: "text-emerald-300",
        title: `Approved (high confidence ${conf}). Auto-merge gate.`,
        clickable: false,
      };
    case "approved-medium":
      return {
        icon: <CheckCircle2 width={size} height={size} className="text-amber-300" />,
        text: conf,
        textClass: "text-amber-300",
        title: `Approved (medium confidence ${conf}). Click to open the recommendations queue.`,
        clickable: true,
      };
    case "needs-fix":
      return {
        icon: <AlertTriangle width={size} height={size} className="text-orange-400" />,
        text: null,
        textClass: "",
        title: state.topDefect ? `Needs fix: ${state.topDefect}` : "Needs fix",
        clickable: false,
      };
    case "escalated":
      return {
        icon: <Flag width={size} height={size} className="text-red-400" />,
        text: null,
        textClass: "",
        title: state.topDefect ? `Escalated: ${state.topDefect}` : "Escalated to user",
        clickable: false,
      };
    case "pending":
    default:
      return {
        icon: <Loader2 width={size} height={size} className="text-[#7aa2f7] animate-spin" />,
        text: null,
        textClass: "",
        title: "Review pending",
        clickable: false,
      };
  }
}

export function ReviewBadge({ sessionId, size = 12 }: ReviewBadgeProps) {
  const state = useReviewState(sessionId);

  // Empty state is load-bearing: render nothing (not a placeholder pixel).
  if (!state) return null;

  const { icon, text, textClass, title, clickable } = badgeContent(state, size);

  const inner = (
    <span
      className={`inline-flex items-center gap-0.5 ${clickable ? "cursor-pointer" : ""}`}
      title={title}
      data-ui-bridge-id="terminal-tab.review-badge"
      data-review-status={state.status}
      data-review-confidence={state.confidence.toFixed(2)}
    >
      {icon}
      {text && <span className={`text-[9px] leading-none ${textClass}`}>{text}</span>}
    </span>
  );

  if (clickable) {
    return (
      <button
        type="button"
        onClick={(e) => {
          e.stopPropagation();
          openRecommendationsQueue(state.reviewId);
        }}
        className="inline-flex items-center gap-0.5 rounded hover:bg-amber-500/10 transition-colors"
        aria-label={title}
      >
        {inner}
      </button>
    );
  }

  return inner;
}
