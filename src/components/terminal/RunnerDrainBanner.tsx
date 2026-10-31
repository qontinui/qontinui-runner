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
 * device at all (no drain can apply) — unless coord's dispatch role for this
 * machine (`bench` / `ci_node`) closes the agent lane, which keeps autonomous
 * spawns off with the drain clear, and says so — or that role is UNKNOWN (never
 * served to this runner, none persisted), which keeps them off too.
 */

import { AlertTriangle, PauseCircle } from "lucide-react";
import { StatusBanner } from "@/components/ui/StatusBanner";
import { useCoordDrainState, type CoordDrainSnapshot } from "@/hooks/useCoordDrainState";

export interface DrainBannerModel {
  /** What holds autonomous spawns: a drain, an unknown drain, or a dispatch role. */
  kind: "drained" | "unknown" | "role_closed";
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

/** The heading while the drain is clear but the dispatch role is unknown. */
export const ROLE_UNKNOWN_HEADING = "dispatch role unknown — autonomous spawns paused";

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

/**
 * The restore note while a dispatch role (not a drain) holds. A role is a
 * standing operator choice, not a window, so it does not promise a lift.
 */
export const ROLE_RESTORE_NOTE =
  "saved session tabs are not restored on this machine while its role holds — nothing is lost, and they come back if the role changes";

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
      kind: "drained",
      status: "warning",
      heading: "Runner drained by coord — autonomous spawns deferred",
      detail: parts.length > 0 ? parts.join(" · ") : null,
      deferredLabel: deferredLabel(s.deferredCount, s.deferredCapped ?? false),
      // Lifting the drain does not lift a closing dispatch role, so do not
      // promise that it does.
      restoreNote: s.agentLaneClosedByRole === true ? ROLE_RESTORE_NOTE : RESTORE_NOTE,
    };
  }
  // At boot the state is "not read yet" until the first drain read lands; that
  // is not news, so the banner does not flash for it.
  if (s.state === "unknown" && s.bootReadPending) return null;
  if (s.state === "unknown") {
    return {
      kind: "unknown",
      status: "error",
      heading: UNKNOWN_HEADING,
      detail: s.cause,
      deferredLabel: deferredLabel(s.deferredCount, s.deferredCapped ?? false),
      // A readable drain does not lift a closing dispatch role either.
      restoreNote: s.agentLaneClosedByRole === true ? ROLE_RESTORE_NOTE : RESTORE_NOTE,
    };
  }
  // The drain is clear (or this is no coord device), but coord's dispatch role
  // for this machine closes the agent lane, so autonomous spawns stay off.
  if (s.agentLaneClosedByRole === true) {
    return {
      kind: "role_closed",
      status: "warning",
      heading: `Dispatch role ${s.dispatchRole ?? "unknown"} — autonomous spawns off on this machine`,
      // A role carried over from before this runner started, which coord has
      // not re-served since, says so rather than reading as fresh.
      detail:
        s.dispatchRoleSource === "persisted"
          ? `as coord last served it${
              s.dispatchRoleObservedAt ? ` (${formatUntil(s.dispatchRoleObservedAt)})` : ""
            }, before this runner started — not re-confirmed yet`
          : null,
      deferredLabel: deferredLabel(s.deferredCount, s.deferredCapped ?? false),
      restoreNote: ROLE_RESTORE_NOTE,
    };
  }
  // The drain is clear, but coord has never served this machine's dispatch
  // role and none was persisted: the runner fails closed, as coord itself does
  // on an unreadable role.
  if (s.dispatchRoleSource === "unknown" && s.state === "clear" && !s.autonomousSpawnsAllowed) {
    return {
      kind: "unknown",
      status: "error",
      heading: ROLE_UNKNOWN_HEADING,
      detail: "coord has not served this machine's dispatch role since the runner started",
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
      data-drain-state={model.kind}
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
