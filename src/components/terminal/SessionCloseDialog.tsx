/**
 * The Session Manager's close confirmation — D7 of plan
 * `2026-10-13-session-manager-chrome-style-tab-organization-and-operator-search`.
 *
 * It names every session it would close with its activity word, and a row
 * reading `Needs you` / `Working` / `Unknown` needs its own second tick
 * before Close enables. The rows are passed in LIVE by the panel, so a
 * session that turns `Needs you` while the dialog is open grows that tick
 * requirement (the tick is keyed by session AND activity, so a changed
 * activity is never covered by an earlier tick).
 *
 * Close is the graceful path (`terminal_graceful_close`); Force close (the
 * kill) is a separate button that leads to its own confirmation, which keeps
 * the same second-tick requirement — a kill is never the way around it. After either
 * runs, the dialog lists each session's outcome — closed, not closed with the
 * reason, or unknown — and never one "done" for a partial batch (served
 * policy `ux-priorities` `an-action-must-acknowledge-itself`).
 *
 * A close over several rows (the bulk bar's Close, the Ready-to-close review)
 * opens with every row included and lets each be left out. Whatever the button
 * showed, the confirm itself re-reads the rows as they are at that instant
 * (`idsToClose`): a row that turned `Needs you` between the last render and
 * the click is left OUT of the close and answered "not closed" with why, while
 * the rows the operator confirmed still close.
 */

import { Fragment, useState } from "react";
import { AlertTriangle, Loader2 } from "lucide-react";

import { idsToClose, type CloseConfirmation } from "./sessionBulkActions";
import {
  closeNeedsSecondTick,
  closeTickKey,
  summarizeCloseResults,
  untickedRiskyTargets,
  type CloseItemResult,
  type CloseTarget,
} from "./sessionClose";
import { sessionOrgControlAttrs } from "./sessionOrgControls";

interface SessionCloseDialogProps {
  /** e.g. "Close 3 sessions" / "Close group Refactor". */
  heading: string;
  targets: readonly CloseTarget[];
  onClose: (sessionIds: string[]) => Promise<CloseItemResult[]>;
  onForceClose: (sessionIds: string[]) => Promise<CloseItemResult[]>;
  onDismiss: () => void;
  /** Each row gets an include box, all ticked at open (a multi-row close). */
  excludable?: boolean;
}

type Phase =
  | { kind: "confirm" }
  | { kind: "force-confirm" }
  | { kind: "running" }
  | { kind: "done"; results: CloseItemResult[]; forced: boolean };

const RESULT_COLOR: Record<CloseItemResult["state"], string> = {
  closed: "#9ece6a",
  "not-closed": "#e0af68",
  unknown: "#565f89",
};

export function SessionCloseDialog({
  heading,
  targets,
  onClose,
  onForceClose,
  onDismiss,
  excludable = false,
}: SessionCloseDialogProps) {
  const [phase, setPhase] = useState<Phase>({ kind: "confirm" });
  const [ticks, setTicks] = useState<Set<string>>(() => new Set());
  const [excluded, setExcluded] = useState<Set<string>>(() => new Set());
  const included = targets.filter((t) => !excluded.has(t.sessionId));
  const closable = included.filter((t) => t.closable);
  const unticked = untickedRiskyTargets(included, ticks);
  const canClose = closable.length > 0 && unticked.length === 0;
  /**
   * The re-check at confirm (`idsToClose`). `null` = close nothing and stay
   * on the confirmation: every row left reads a risky activity it was not
   * ticked for, and the list now shows that tick.
   */
  const confirmNow = (): CloseConfirmation | null => {
    // The handler is the current render's, so these are the rows as the
    // panel passed them in LIVE — not as they were when the dialog opened.
    const now = idsToClose(targets, excluded, ticks);
    if (now.ids.length === 0 && (now.needsTick.length > 0 || now.skipped.length === 0)) return null;
    return now;
  };
  const toggleExcluded = (id: string) =>
    setExcluded((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });

  const toggleTick = (key: string) =>
    setTicks((prev) => {
      const next = new Set(prev);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });

  // Rows the re-check left out are answered beside the close's own results.
  const runClose = async () => {
    const now = confirmNow();
    if (!now) return;
    if (now.ids.length === 0) {
      setPhase({ kind: "done", results: now.skipped, forced: false });
      return;
    }
    setPhase({ kind: "running" });
    const results = await onClose(now.ids);
    setPhase({ kind: "done", results: [...results, ...now.skipped], forced: false });
  };
  const runForce = async () => {
    const now = confirmNow();
    if (!now) return;
    if (now.ids.length === 0) {
      setPhase({ kind: "done", results: now.skipped, forced: true });
      return;
    }
    // A kill is reported only once the close path answered, so Force close
    // waits like Close does rather than claiming "closed" up front.
    setPhase({ kind: "running" });
    const results = await onForceClose(now.ids);
    setPhase({ kind: "done", results: [...results, ...now.skipped], forced: true });
  };

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center">
      <div
        className="absolute inset-0 bg-black/50"
        onClick={phase.kind === "running" ? undefined : onDismiss}
        role="button"
        tabIndex={-1}
        aria-label="Dismiss close confirmation"
      />
      <div
        {...sessionOrgControlAttrs("closeDialog")}
        role="dialog"
        aria-label={heading}
        data-phase={phase.kind}
        data-target-count={targets.length}
        data-unticked-count={unticked.length}
        onKeyDown={(e) => {
          if (e.key === "Escape" && phase.kind !== "running") onDismiss();
        }}
        className="relative w-full max-w-md mx-4 rounded-lg border border-[#2a2d3d] bg-[#13141f] shadow-xl overflow-hidden"
      >
        <div className="px-3 py-2 border-b border-[#2a2d3d] text-xs font-semibold text-[#c0caf5]">
          {heading}
        </div>

        {/* Each phase is KEYED so React mounts its buttons fresh instead of
            reusing the previous phase's nodes: the UI Bridge registers a
            control by its node, so a reused node kept its old id — the done
            phase's Done button stayed registered as `force-close` and
            `close-cancel` was unreachable after a close (Phase 8 live run). */}
        {phase.kind === "done" ? (
          <Fragment key="done">
            <p className="px-3 pt-2 text-[11px] text-[#a9b1d6]" data-testid="close-summary">
              {summarizeCloseResults(phase.results)}
            </p>
            <ul className="max-h-[50vh] overflow-y-auto px-3 py-2 space-y-1">
              {phase.results.map((r) => (
                <li
                  key={r.sessionId}
                  data-session-id={r.sessionId}
                  data-close-state={r.state}
                  className="text-[11px]"
                >
                  <span className="text-[#c0caf5]">{r.title}</span>
                  <span className="block text-[10px]" style={{ color: RESULT_COLOR[r.state] }}>
                    {r.detail}
                  </span>
                </li>
              ))}
            </ul>
            <div className="flex justify-end px-3 py-2 border-t border-[#2a2d3d]">
              <button
                {...sessionOrgControlAttrs("closeCancel")}
                onClick={onDismiss}
                className="px-2 py-0.5 rounded text-[11px] text-[#a9b1d6] hover:bg-[#2a2d3d]"
              >
                Done
              </button>
            </div>
          </Fragment>
        ) : phase.kind === "force-confirm" ? (
          <Fragment key="force-confirm">
            <div className="px-3 py-2 text-[11px] text-[#e0af68] flex gap-2">
              <AlertTriangle className="w-4 h-4 shrink-0" />
              <span>
                Force close kills each pane&apos;s process tree without typing /exit. Its in-flight
                context is lost and the transcript&apos;s last turn may not be written.
              </span>
            </div>
            <TargetList
              targets={closable}
              ticks={ticks}
              onToggle={toggleTick}
              excluded={excluded}
            />
            <div className="flex justify-end gap-2 px-3 py-2 border-t border-[#2a2d3d]">
              <button
                {...sessionOrgControlAttrs("closeCancel")}
                onClick={() => setPhase({ kind: "confirm" })}
                className="px-2 py-0.5 rounded text-[11px] text-[#a9b1d6] hover:bg-[#2a2d3d]"
              >
                Back
              </button>
              <button
                {...sessionOrgControlAttrs("forceCloseConfirm")}
                onClick={() => void runForce()}
                disabled={!canClose}
                title={
                  unticked.length > 0
                    ? `Tick the ${unticked.length} session${unticked.length === 1 ? "" : "s"} still marked above`
                    : undefined
                }
                className="px-2 py-0.5 rounded text-[11px] bg-[#f7768e]/20 text-[#f7768e] hover:bg-[#f7768e]/30 disabled:opacity-40"
              >
                Force close {closable.length}
              </button>
            </div>
          </Fragment>
        ) : (
          <Fragment key="confirm">
            <p className="px-3 pt-2 text-[11px] text-[#a9b1d6]">
              Closing ends each session&apos;s live conversation. Claude is asked to /exit at an
              empty prompt; nothing is killed. The transcript stays and can be resumed.
            </p>
            <TargetList
              targets={targets}
              ticks={ticks}
              onToggle={toggleTick}
              excluded={excluded}
              onToggleExcluded={excludable ? toggleExcluded : undefined}
              disabled={phase.kind === "running"}
            />
            <div className="flex items-center gap-2 px-3 py-2 border-t border-[#2a2d3d]">
              <button
                {...sessionOrgControlAttrs("forceClose")}
                onClick={() => setPhase({ kind: "force-confirm" })}
                disabled={phase.kind === "running" || closable.length === 0}
                className="px-2 py-0.5 rounded text-[10px] text-[#565f89] hover:text-[#f7768e] hover:bg-[#2a2d3d] disabled:opacity-40"
              >
                Force close…
              </button>
              <div className="flex-1" />
              <button
                {...sessionOrgControlAttrs("closeCancel")}
                onClick={onDismiss}
                disabled={phase.kind === "running"}
                className="px-2 py-0.5 rounded text-[11px] text-[#a9b1d6] hover:bg-[#2a2d3d] disabled:opacity-40"
              >
                Cancel
              </button>
              <button
                {...sessionOrgControlAttrs("closeConfirm")}
                onClick={() => void runClose()}
                disabled={!canClose || phase.kind === "running"}
                title={
                  unticked.length > 0
                    ? `Tick the ${unticked.length} session${unticked.length === 1 ? "" : "s"} still marked above`
                    : undefined
                }
                className="flex items-center gap-1 px-2 py-0.5 rounded text-[11px] bg-[#f7768e]/15 text-[#f7768e] hover:bg-[#f7768e]/25 disabled:opacity-40"
              >
                {phase.kind === "running" && <Loader2 className="w-3 h-3 animate-spin" />}
                {phase.kind === "running" ? "Closing…" : `Close ${closable.length}`}
              </button>
            </div>
          </Fragment>
        )}
      </div>
    </div>
  );
}

function TargetList({
  targets,
  ticks,
  onToggle,
  excluded,
  onToggleExcluded,
  disabled = false,
}: {
  targets: readonly CloseTarget[];
  ticks: ReadonlySet<string>;
  onToggle: (key: string) => void;
  excluded: ReadonlySet<string>;
  /** Present for a multi-row close: each row's include box. */
  onToggleExcluded?: (sessionId: string) => void;
  disabled?: boolean;
}) {
  return (
    <ul className="max-h-[50vh] overflow-y-auto px-3 py-2 space-y-1.5">
      {targets.map((t) => {
        const key = closeTickKey(t);
        const out = excluded.has(t.sessionId);
        const risky = t.closable && !out && closeNeedsSecondTick(t.activity);
        return (
          <li
            key={t.sessionId}
            data-session-id={t.sessionId}
            data-activity={t.activity}
            data-included={out ? "false" : "true"}
            className="text-[11px]"
          >
            <div className="flex items-baseline gap-2 min-w-0">
              {onToggleExcluded && (
                <input
                  type="checkbox"
                  {...sessionOrgControlAttrs("closeInclude", t.sessionId)}
                  aria-label={`Include ${t.title}`}
                  checked={!out}
                  disabled={disabled}
                  onChange={() => onToggleExcluded(t.sessionId)}
                  className="w-3 h-3 shrink-0 self-center"
                />
              )}
              <span
                className={`truncate ${out ? "text-[#565f89] line-through" : "text-[#c0caf5]"}`}
                title={t.title}
              >
                {t.title}
              </span>
              <span className="shrink-0 text-[10px] text-[#565f89]">{t.activityWord}</span>
            </div>
            {t.reason && (
              <span className="block text-[10px] text-[#9ece6a]" title={t.reason}>
                {t.reason}
              </span>
            )}
            {!t.closable && (
              <span className="block text-[10px] text-[#565f89]">
                no live tab in this window — nothing to close
              </span>
            )}
            {risky && (
              <label className="flex items-center gap-1 mt-0.5 text-[10px] text-[#e0af68]">
                <input
                  type="checkbox"
                  {...sessionOrgControlAttrs("closeTick", t.sessionId)}
                  checked={ticks.has(key)}
                  disabled={disabled}
                  onChange={() => onToggle(key)}
                  className="w-3 h-3"
                />
                Close it anyway — it reads {t.activityWord}
              </label>
            )}
          </li>
        );
      })}
    </ul>
  );
}
