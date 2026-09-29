/**
 * RunnerDrainBanner — "this runner is draining" notice (plan
 * `2026-09-13-drained-runner-never-reaches-idle`, Phase 3, D3).
 *
 * While coord has drained this device, or its drain state is unknown, the
 * runner defers every AUTONOMOUS spawn (looping agents, stewards started by
 * anything but this UI, boot resume/restore, the scheduler, continuations).
 * Operator actions — this UI's own new terminal, new chat and steward start —
 * still run, so the banner says so rather than leaving a quiet runner to be
 * mistaken for a broken one.
 *
 * Renders nothing when the drain is clear, or when this runner is not a coord
 * device at all (no drain can apply).
 */

import { AlertTriangle, PauseCircle } from "lucide-react";
import { StatusBanner } from "@/components/ui/StatusBanner";
import { useCoordDrainState, type CoordDrainSnapshot } from "@/hooks/useCoordDrainState";

export interface DrainBannerModel {
  status: "warning" | "error";
  heading: string;
  /** Until / reason (drained) or cause (unknown). */
  detail: string | null;
  /** "3 deferred work items", or null when nothing has been deferred yet. */
  deferredLabel: string | null;
  /**
   * The restore note (review N3), or null when the state does not withhold
   * anything. Rendered verbatim.
   */
  restoreNote: string | null;
}

/** The copy the UNKNOWN state must carry, verbatim (plan Phase 3). */
export const UNKNOWN_HEADING = "drain state unknown — autonomous spawns paused";

/**
 * Review N3. While the drain holds, `terminal_session_list_open` withholds the
 * RESTORABLE set, so a page whose only tabs are unrestored sessions shows no
 * tab at all for the drain's duration — a blank grid that reads as lost work
 * rather than as deferred work. The restore re-runs by itself when the drain
 * lifts (`autonomousResumeDetector` → `drainResumeEpoch`); this line is what
 * says so while the operator is looking at the gap.
 */
export const RESTORE_NOTE =
  "saved session tabs are not restored while this holds — they come back when it lifts, nothing is lost";

export function deferredLabel(count: number, capped = false): string | null {
  if (count <= 0) return null;
  // At the backend's cap the count is a FLOOR, not a total — say so with a `+`
  // rather than presenting it as the whole number.
  const n = capped ? `${count}+` : `${count}`;
  return `${n} deferred work item${count === 1 && !capped ? "" : "s"}`;
}

function formatUntil(until: string): string {
  const d = new Date(until);
  return Number.isNaN(d.getTime()) ? until : d.toLocaleString();
}

/** PURE: what the banner shows for a snapshot; `null` renders nothing. */
export function drainBannerModel(s: CoordDrainSnapshot | null): DrainBannerModel | null {
  if (!s) return null;
  if (s.state === "drained") {
    const parts: string[] = [];
    if (s.until) parts.push(`until ${formatUntil(s.until)}`);
    if (s.reason) parts.push(s.reason);
    return {
      status: "warning",
      heading: "Runner drained by coord — autonomous spawns deferred",
      detail: parts.length > 0 ? parts.join(" · ") : null,
      deferredLabel: deferredLabel(s.deferredCount, s.deferredCapped ?? false),
      restoreNote: RESTORE_NOTE,
    };
  }
  // At boot the state is "not read yet" until the first drain read lands; that
  // is not news, so the banner does not flash for it.
  if (s.state === "unknown" && s.bootReadPending) return null;
  if (s.state === "unknown") {
    return {
      status: "error",
      heading: UNKNOWN_HEADING,
      detail: s.cause,
      deferredLabel: deferredLabel(s.deferredCount, s.deferredCapped ?? false),
      restoreNote: RESTORE_NOTE,
    };
  }
  return null;
}

export function RunnerDrainBanner() {
  const model = drainBannerModel(useCoordDrainState());
  if (!model) return null;
  const Icon = model.status === "error" ? AlertTriangle : PauseCircle;
  return (
    <div
      className="absolute top-2 left-1/2 -translate-x-1/2 z-30 w-[440px] max-w-[90%] shadow-lg"
      data-ui-bridge-id="terminal.runner-drain-banner"
      data-drain-state={model.status === "error" ? "unknown" : "drained"}
    >
      <StatusBanner status={model.status} icon={<Icon className="w-4 h-4" />}>
        <div className="text-[12px] font-semibold leading-snug">{model.heading}</div>
        {model.detail && <div className="text-[11px] leading-snug mt-0.5">{model.detail}</div>}
        <div className="text-[11px] leading-snug mt-0.5 opacity-80">
          {model.deferredLabel ? `${model.deferredLabel} · ` : ""}
          your own new terminals, chats and steward starts still run
        </div>
        {model.restoreNote && (
          <div
            className="text-[11px] leading-snug mt-0.5 opacity-80"
            data-ui-bridge-id="terminal.runner-drain-banner.restore-note"
          >
            {model.restoreNote}
          </div>
        )}
      </StatusBanner>
    </div>
  );
}
