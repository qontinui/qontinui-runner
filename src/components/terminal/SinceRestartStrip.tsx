/**
 * The "Since restart" strip (plan `2026-10-04-runner-session-roster-restore-picker`,
 * Phase 4): once the boot restore has settled for every page holding a session
 * from before the restart, it says what came back.
 *
 * - "N of M sessions from before the restart are back · K didn't come back —
 *   Review" when K > 0. The strip lives in the app-wide advisory column, so
 *   Review first brings the Terminal view up, then opens Previous Sessions
 *   with the roster section scrolled to the top.
 * - "All M sessions from before the restart are back" when K = 0, fading by
 *   itself — confirmation rather than silence is what makes the roster
 *   trustworthy.
 * - "Restore deferred by drain — N waiting" while the coord device drain
 *   holds the restore back.
 *
 * Dismissible (remembered per restart). A sibling of `ResumeFailedBanner` in
 * the top-right advisory column; the model is pure (`stripModel`).
 */

import { useEffect } from "react";
import { CheckCircle2, History, PauseCircle, X } from "lucide-react";
import { useUIComponent } from "@qontinui/ui-bridge";
import { AdvisorySlot } from "./AdvisoryStack";
import { useSinceRestart } from "./SinceRestartContext";
import {
  SINCE_RESTART_DISMISS_ID,
  SINCE_RESTART_REVIEW_ID,
  SINCE_RESTART_STRIP_ID,
  type StripModel,
} from "./sinceRestart";

/** How long the all-back confirmation stays before it fades. */
export const ALL_BACK_FADE_MS = 8_000;

/** Colour per form — amber for misses, green for all back, blue while deferred. */
function tone(kind: StripModel["kind"]): { box: string; icon: string } {
  switch (kind) {
    case "some-missing":
      return { box: "bg-[#e0af68]/10 border-[#e0af68]/40", icon: "text-[#e0af68]" };
    case "all-back":
      return { box: "bg-[#9ece6a]/10 border-[#9ece6a]/40", icon: "text-[#9ece6a]" };
    case "deferred":
      return { box: "bg-[#7aa2f7]/10 border-[#7aa2f7]/40", icon: "text-[#7aa2f7]" };
  }
}

export function SinceRestartStrip() {
  const roster = useSinceRestart();
  const strip = roster?.strip ?? null;
  const dismiss = roster?.dismissStrip;
  const review = roster?.review;

  // The all-back form is a confirmation, not a task: it fades by itself.
  const fades = strip?.kind === "all-back";
  useEffect(() => {
    if (!fades || !dismiss) return;
    const t = setTimeout(dismiss, ALL_BACK_FADE_MS);
    return () => clearTimeout(t);
  }, [fades, dismiss]);

  useUIComponent({
    id: "since-restart-strip",
    name: "Since restart strip",
    description:
      "What came back after the last runner restart. `review` opens Previous Sessions on the 'Before the last restart' roster.",
    actions: [
      {
        id: "review",
        label: "Review",
        description:
          "Bring the Terminal view up (from any page) and open the 'Before the last restart' roster in Previous Sessions.",
        // `read` — opens a panel; nothing is resumed or written.
        effect: "read",
        handler: () => {
          review?.();
        },
      },
      {
        id: "dismiss",
        label: "Dismiss",
        description: "Hide this strip for this restart.",
        // `write` — remembers the dismissal in per-instance storage.
        effect: "write",
        handler: () => {
          dismiss?.();
        },
      },
    ],
  });

  if (!strip || !roster) return null;
  const colours = tone(strip.kind);
  const Icon =
    strip.kind === "deferred" ? PauseCircle : strip.kind === "all-back" ? CheckCircle2 : History;

  return (
    <AdvisorySlot>
      <div
        data-ui-bridge-id={SINCE_RESTART_STRIP_ID}
        data-strip-kind={strip.kind}
        className={`w-[360px] rounded border shadow-lg px-2.5 py-2 transition-opacity ${colours.box}`}
      >
        <div className="flex items-start gap-2">
          <Icon className={`w-3.5 h-3.5 shrink-0 mt-0.5 ${colours.icon}`} />
          <div className="flex-1 min-w-0 text-[12px] font-semibold text-[#c0caf5] leading-snug">
            {strip.text}
          </div>
          {strip.kind !== "all-back" && (
            <button
              type="button"
              data-ui-bridge-id={SINCE_RESTART_REVIEW_ID}
              onClick={roster.review}
              className={`flex items-center gap-1 px-1.5 py-0.5 rounded border text-[10px] shrink-0 hover:bg-[#2a2d3d] ${
                strip.kind === "deferred"
                  ? "border-[#7aa2f7]/40 text-[#7aa2f7]"
                  : "border-[#e0af68]/40 text-[#e0af68]"
              }`}
              title="Open Previous Sessions on the roster from before the restart"
            >
              Review
            </button>
          )}
          <button
            type="button"
            data-ui-bridge-id={SINCE_RESTART_DISMISS_ID}
            onClick={roster.dismissStrip}
            className="p-0.5 rounded text-[#565f89] hover:text-[#c0caf5] hover:bg-[#2a2d3d] shrink-0"
            title="Dismiss — the roster stays in Previous Sessions"
          >
            <X className="w-3 h-3" />
          </button>
        </div>
      </div>
    </AdvisorySlot>
  );
}
