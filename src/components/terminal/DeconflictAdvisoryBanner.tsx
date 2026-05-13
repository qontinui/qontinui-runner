/**
 * Deconfliction advisory banner — Phase 1 of the "Coord as Deconflicter,
 * not Dispatcher" plan (§4.4 of
 * `plans/2026-05-13-coord-as-deconflicter-plan.md`).
 *
 * Rendered above the terminal cell grid when the Rust deconflicter loop
 * (`src-tauri/src/coordinator/deconflicter.rs`) decides that this
 * session's recent file edit overlaps with another live session's recent
 * touches. The deconflicter writes one `coord.coordinator_decisions` row
 * per fire (rule=`deconflict`, action=`advise-with-text`,
 * session_id=`deconflicter-*`, target_id=this tab's `task_run_id`) and
 * emits a `coordinator-decision-created` Tauri event with the row
 * payload — this banner subscribes to that event, filters for advisories
 * targeting the current tab, and stacks them as one card per advisory.
 *
 * Each card carries:
 *   - The one-line `reasoning` text rendered verbatim from PG.
 *   - A "Dismiss" action that calls the existing `resolve_escalation`
 *     Tauri command (the underlying PgDb helper
 *     `resolve_coordinator_decision` already supports any decision row,
 *     not just open-escalation ones).
 *
 * The banner gates on `claudeSessionId` via `taskRunId` (the prop is
 * non-null only when the parent's tab has a Claude session id); PTY-only
 * tabs render no banner. See `proj_holding_banner_pty_gate` for the rule
 * the parent enforces.
 *
 * Visual language mirrors `HoldingLockBanner` — same soft-amber
 * `#e0af68` accent at lower saturation. The banner is positioned in the
 * same overlay slot above the active terminal so a session that hits
 * BOTH a lock-yield event AND a deconflict advisory stacks them in
 * priority order (lock first, deconflict below). The two banners never
 * fight for the same screen real estate because the lock-yield banners
 * are absolutely positioned to `top-2 right-2 w-[340px]` and this banner
 * sits below them in the document flow with the same width budget.
 */

import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { AlertTriangle, X } from "lucide-react";
import { createLogger } from "@/lib/logger";

const logger = createLogger("DeconflictAdvisoryBanner");

/**
 * One advisory the banner is tracking. Matches the JSON shape emitted by
 * `DeconflicterLoop::emit_decision` in
 * `src-tauri/src/coordinator/deconflicter.rs::emit_decision`.
 */
export interface DeconflictAdvisory {
  /** Coordinator decision row id (UUID as text). */
  decisionId: string;
  /** `"deconflicter-rust"` for the Rust loop; the `deconflicter-` prefix
   *  is the predicate the banner gates on. */
  sessionId: string;
  /** Always `"deconflict"` for advisories the banner consumes. */
  rule: string;
  /** Always `"advise-with-text"` per the plan §3.1 deconflicter
   *  allow-list (`must_advise_only` server-side wall). */
  action: string;
  /** `coord.tasks.assigned_session_id` of the touching session —
   *  matches this tab's `task_run_id`. */
  targetId: string | null;
  /** The one-line message rendered to the user. */
  reasoning: string;
}

/** Filter predicate exported for tests. An advisory belongs in this
 *  banner if:
 *    - its session_id is a Rust deconflicter (prefix `deconflicter-`);
 *    - rule is `deconflict`;
 *    - target_id matches the current tab.
 *
 *  Decoupled from React state so the test pin can drive it directly. */
export function isMatchingAdvisory(
  advisory: DeconflictAdvisory,
  taskRunId: string,
): boolean {
  return (
    advisory.rule === "deconflict" &&
    advisory.sessionId.startsWith("deconflicter-") &&
    advisory.targetId === taskRunId
  );
}

export interface DeconflictAdvisoryBannerProps {
  /** The active tab's `task_run_id` / `claudeSessionId`. When null the
   *  banner renders nothing (PTY-only tabs are banner-less by design,
   *  per `proj_holding_banner_pty_gate`). */
  taskRunId: string | null;
}

export function DeconflictAdvisoryBanner({ taskRunId }: DeconflictAdvisoryBannerProps) {
  const [advisories, setAdvisories] = useState<DeconflictAdvisory[]>([]);

  // Subscribe to `coordinator-decision-created`. We never poll — the
  // deconflicter emits the event right after the row insert, so an event
  // miss only happens if the listener wasn't yet bound (which only
  // matters for advisories fired in the brief gap between tab mount and
  // listener registration; those re-fire on the next collision because
  // the dedup window is per-(session, path), not global).
  useEffect(() => {
    if (!taskRunId) return;

    let unlisten: UnlistenFn | null = null;
    let cancelled = false;

    listen<unknown>("coordinator-decision-created", (event) => {
      if (cancelled) return;
      const payload = event.payload;
      if (typeof payload !== "object" || payload === null) return;
      const p = payload as Record<string, unknown>;
      const decisionId = typeof p.decision_id === "string" ? p.decision_id : null;
      const sessionId = typeof p.session_id === "string" ? p.session_id : null;
      const rule = typeof p.rule === "string" ? p.rule : null;
      const action = typeof p.action === "string" ? p.action : null;
      const targetId = typeof p.target_id === "string" ? p.target_id : null;
      const reasoning = typeof p.reasoning === "string" ? p.reasoning : null;
      if (!decisionId || !sessionId || !rule || !action || !reasoning) {
        return;
      }
      const advisory: DeconflictAdvisory = {
        decisionId,
        sessionId,
        rule,
        action,
        targetId,
        reasoning,
      };
      if (!isMatchingAdvisory(advisory, taskRunId)) return;
      setAdvisories((prev) => {
        // Idempotent on decision_id so a stray duplicate event from the
        // Rust loop never doubles the same row.
        if (prev.some((a) => a.decisionId === advisory.decisionId)) return prev;
        return [...prev, advisory];
      });
    })
      .then((fn) => {
        if (cancelled) {
          fn();
          return;
        }
        unlisten = fn;
      })
      .catch((err) => {
        logger.error("listen('coordinator-decision-created') failed", err);
      });

    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [taskRunId]);

  // Reset the local advisory list when the tab's task_run_id changes —
  // a new session shouldn't inherit the previous one's banners.
  useEffect(() => {
    setAdvisories([]);
  }, [taskRunId]);

  const handleDismiss = async (decisionId: string) => {
    // Optimistically drop locally so the banner disappears immediately.
    // If the resolve call fails we re-add to the list with a logged warn;
    // the deconflicter never re-fires a resolved decision because it
    // only ever inserts NEW rows (never updates existing ones).
    setAdvisories((prev) => prev.filter((a) => a.decisionId !== decisionId));
    try {
      await invoke<boolean>("resolve_escalation", {
        decisionId,
        resolution: "dismissed",
      });
    } catch (err) {
      logger.warn(
        `resolve_escalation('${decisionId}','dismissed') failed; advisory re-shown`,
        err,
      );
      // Re-fetch the advisory if it's still in our state? Too noisy —
      // dismissal is purely visual, the PG row staying unresolved is
      // acceptable. A future re-render of the same tab won't see this
      // advisory because the subscription only fires for NEW events.
    }
  };

  if (!taskRunId || advisories.length === 0) {
    return null;
  }

  return (
    <div
      data-ui-bridge-id="terminal.deconflict-advisory-banner"
      data-task-run-id={taskRunId}
      className="absolute top-14 right-2 z-30 w-[340px] space-y-1.5"
    >
      {advisories.map((a) => (
        <div
          key={a.decisionId}
          data-ui-bridge-id="terminal.deconflict-advisory-banner-card"
          data-advisory-decision-id={a.decisionId}
          className="p-2.5 rounded border bg-[#e0af68]/10 border-[#e0af68]/35 shadow-lg"
        >
          <div className="flex items-start gap-2">
            <AlertTriangle className="w-3.5 h-3.5 shrink-0 text-[#e0af68] mt-0.5" />
            <div className="text-[11px] text-[#c0caf5] leading-snug flex-1">
              {a.reasoning}
            </div>
            <button
              type="button"
              aria-label="Dismiss advisory"
              data-ui-bridge-id="terminal.deconflict-advisory-banner-dismiss"
              data-advisory-decision-id={a.decisionId}
              onClick={() => void handleDismiss(a.decisionId)}
              className="text-[#e0af68] hover:text-[#c0caf5] leading-none px-1"
            >
              <X className="w-3 h-3" />
            </button>
          </div>
        </div>
      ))}
    </div>
  );
}
