/**
 * Startup session-recovery banner — Phase 4 of
 * `2026-06-06-runner-dev-loop-and-restart-resilience`.
 *
 * After a runner restart, `resume_ai_sessions` auto-reattaches interrupted
 * chat sessions. This banner surfaces the result to the operator on launch,
 * subscribing to the one-shot `session-recovery-summary` Tauri event via
 * {@link useSessionRecovery}.
 *
 * Honesty gate: per-session resume FIDELITY (lossless `--resume` vs a lossy
 * "resumed from last N turns" summary), CLAIM status (re-acquired / degraded /
 * peer-conflict), and WIP status (restored / kept-for-manual) are shown
 * verbatim — a lossy or claim-degraded resume is NEVER presented as fully
 * clean.
 *
 * Two visual modes:
 *   - crashRecovery=true  → a PROMINENT red-accented banner ("Recovered N
 *     session(s) after an unexpected restart") listing each session + status.
 *   - crashRecovery=false → a QUIET, subtle "N session(s) resumed" note (a
 *     planned restart is expected; no alarm).
 *
 * Dismissible. Renders nothing when there is nothing to report (zero resumed
 * sessions, or after dismissal). Visual language stacks with
 * {@link CoordWarningBanner} (same top-right advisory column).
 */

import { useEffect, useState } from "react";
import { AlertTriangle, RotateCcw, X } from "lucide-react";
import {
  useSessionRecovery,
  type ClaimStatus,
  type SessionRecoveryOutcome,
  type WipStatus,
} from "@/hooks/useSessionRecovery";

/** Human-readable, HONEST resume-fidelity label for one session. */
export function fidelityLabel(s: SessionRecoveryOutcome): string {
  if (s.fidelity === "lossless") return "fully restored";
  return `resumed from last ${s.turnsCovered} turn${s.turnsCovered === 1 ? "" : "s"}`;
}

/** Whether a session's fidelity is lossy (drives the warning tint). */
export function isLossy(s: SessionRecoveryOutcome): boolean {
  return s.fidelity === "summary";
}

/** Human-readable claim-status label, or null when there's nothing to flag. */
export function claimLabel(status: ClaimStatus): string | null {
  switch (status) {
    case "reacquired":
      return null; // clean — no callout needed
    case "peer-conflict":
      return "worktree now held by another agent — shared, read-mostly";
    case "degraded":
      return "worktree claim not re-acquired — shared, read-mostly";
    case "none":
    default:
      return null;
  }
}

/** Human-readable WIP-status label, or null when there's nothing to flag. */
export function wipLabel(status: WipStatus): string | null {
  switch (status) {
    case "restored":
      return "uncommitted work restored";
    case "kept-for-manual":
      return "uncommitted work kept at refs/wip/* for manual recovery";
    case "none":
    default:
      return null;
  }
}

/** A session is "degraded" (warrants a warning tint) when claim/WIP/fidelity
 *  is anything other than the fully-clean path. */
export function isDegraded(s: SessionRecoveryOutcome): boolean {
  return (
    isLossy(s) ||
    s.claimStatus === "peer-conflict" ||
    s.claimStatus === "degraded" ||
    s.wipStatus === "kept-for-manual"
  );
}

export function SessionRecoveryBanner() {
  const summary = useSessionRecovery();
  const [dismissed, setDismissed] = useState(false);

  // Re-show the banner when a NEW summary arrives (e.g. an unlikely second
  // boot event) by clearing the dismissal on payload identity change.
  useEffect(() => {
    if (summary) setDismissed(false);
  }, [summary]);

  if (!summary || dismissed) return null;
  if (summary.resumedCount === 0) return null;

  const crash = summary.crashRecovery;
  const sessions = summary.sessions;

  // Accent: red for crash recovery, soft slate/blue for a planned restart.
  const accent = crash ? "#f7768e" : "#7aa2f7";
  const containerBg = crash ? "bg-[#f7768e]/10 border-[#f7768e]/40" : "bg-[#7aa2f7]/8 border-[#7aa2f7]/30";

  const heading = crash
    ? `Recovered ${summary.resumedCount} session${summary.resumedCount === 1 ? "" : "s"} after an unexpected restart`
    : `${summary.resumedCount} session${summary.resumedCount === 1 ? "" : "s"} resumed`;

  return (
    <div
      data-ui-bridge-id="terminal.session-recovery-banner"
      data-crash-recovery={crash ? "true" : "false"}
      className={`absolute top-2 right-2 z-30 w-[360px] rounded border shadow-lg p-2.5 ${containerBg}`}
    >
      <div className="flex items-start gap-2">
        {crash ? (
          <AlertTriangle className="w-3.5 h-3.5 shrink-0 mt-0.5" style={{ color: accent }} />
        ) : (
          <RotateCcw className="w-3.5 h-3.5 shrink-0 mt-0.5" style={{ color: accent }} />
        )}
        <div className="flex-1 min-w-0">
          <div className="text-[12px] font-semibold text-[#c0caf5] leading-snug">{heading}</div>

          {/* Quiet mode: when nothing is degraded, the heading is enough — but
              we still list per-session honesty so a lossy/degraded resume is
              never hidden behind a calm summary line. */}
          <ul className="mt-1.5 space-y-1">
            {sessions.map((s) => {
              const degraded = isDegraded(s);
              const claim = claimLabel(s.claimStatus);
              const wip = wipLabel(s.wipStatus);
              return (
                <li
                  key={s.taskRunId}
                  data-ui-bridge-id="terminal.session-recovery-banner-item"
                  data-task-run-id={s.taskRunId}
                  data-degraded={degraded ? "true" : "false"}
                  className="text-[11px] leading-snug"
                >
                  <div className="flex items-baseline gap-1.5">
                    <span className="text-[#c0caf5] font-medium truncate">{s.title}</span>
                    <span
                      className={isLossy(s) ? "text-[#e0af68]" : "text-[#9ece6a]"}
                      title={isLossy(s) ? "Earlier context may be missing" : "Full transcript restored"}
                    >
                      {fidelityLabel(s)}
                    </span>
                  </div>
                  {claim && (
                    <div className="text-[10px] text-[#e0af68]/90 mt-0.5">{claim}</div>
                  )}
                  {wip && (
                    <div
                      className={`text-[10px] mt-0.5 ${
                        s.wipStatus === "kept-for-manual" ? "text-[#e0af68]/90" : "text-[#9ece6a]/80"
                      }`}
                    >
                      {wip}
                    </div>
                  )}
                </li>
              );
            })}
          </ul>
        </div>
        <button
          type="button"
          aria-label="Dismiss recovery summary"
          data-ui-bridge-id="terminal.session-recovery-banner-dismiss"
          onClick={() => setDismissed(true)}
          className="text-[#c0caf5]/70 hover:text-[#c0caf5] leading-none px-1"
        >
          <X className="w-3 h-3" />
        </button>
      </div>
    </div>
  );
}
