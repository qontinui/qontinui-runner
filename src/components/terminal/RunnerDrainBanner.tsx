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

import { useState } from "react";
import { AlertTriangle, PauseCircle, X } from "lucide-react";
import { StatusBanner } from "@/components/ui/StatusBanner";
import { AdvisorySlot } from "./AdvisoryStack";
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
  /**
   * Identity of the drain episode this banner describes, for dismissal. Built
   * from the fields that stay fixed for an episode — not from `detail`, whose
   * unknown-state cause is rewritten on every read (failure count, read age).
   */
  episodeKey: string;
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
      episodeKey: `drained|${s.until ?? ""}|${s.reason ?? ""}`,
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
      episodeKey: `unknown|${s.since ?? ""}`,
    };
  }
  return null;
}

export function RunnerDrainBanner() {
  const model = drainBannerModel(useCoordDrainState());
  // Dismissal is per drain episode (`episodeKey`): a new drain, a
  // drained→unknown change, or the drain clearing and returning shows it again.
  // Dismissing only collapses the notice to a small icon — spawns stay
  // deferred, so the runner keeps saying so; clicking it brings the banner back.
  const [dismissedKey, setDismissedKey] = useState<string | null>(null);
  if (!model) {
    if (dismissedKey !== null) setDismissedKey(null);
    return null;
  }
  const key = model.episodeKey;
  const Icon = model.status === "error" ? AlertTriangle : PauseCircle;
  const drainState = model.status === "error" ? "unknown" : "drained";
  if (dismissedKey === key) {
    // Rendered through the advisory stack, the single owner of the top-right
    // corner (finding c91550f0), so it never sits over another advisory, the
    // tab bar or a zone's hover actions. A solid base under the 10% status
    // tint keeps it legible over the terminal grid. Classes are literal so
    // Tailwind's source scan (which skips node_modules) always emits them.
    const tone =
      model.status === "error"
        ? "border-red-500/40 bg-red-500/15 text-red-500"
        : "border-yellow-500/40 bg-yellow-500/15 text-yellow-500";
    return (
      <AdvisorySlot>
        <button
          type="button"
          aria-label={`${model.heading} — show details`}
          aria-expanded={false}
          title={`${model.heading} — click for details`}
          onClick={() => setDismissedKey(null)}
          className="rounded-full shadow-lg bg-[#16161e]"
          data-ui-bridge-id="terminal.runner-drain-indicator"
          data-drain-state={drainState}
        >
          <span className={`block rounded-full border p-1.5 ${tone}`}>
            <Icon className="w-4 h-4" />
          </span>
        </button>
      </AdvisorySlot>
    );
  }
  return (
    <div
      className="absolute top-2 left-1/2 -translate-x-1/2 z-30 w-max min-w-[min(440px,95%)] max-w-[95%] shadow-lg"
      data-ui-bridge-id="terminal.runner-drain-banner"
      data-drain-state={drainState}
    >
      <StatusBanner status={model.status} icon={<Icon className="w-4 h-4" />}>
        <button
          type="button"
          aria-label="Dismiss drain notice"
          aria-expanded={true}
          title="Dismiss"
          onClick={() => setDismissedKey(key)}
          className="float-right ml-2 -mt-0.5 opacity-70 hover:opacity-100"
          data-ui-bridge-id="terminal.runner-drain-banner.dismiss"
        >
          <X className="w-4 h-4" />
        </button>
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
