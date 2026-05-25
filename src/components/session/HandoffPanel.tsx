/**
 * HandoffPanel.tsx
 *
 * Operator surface for the cross-machine session handoff feature shipped
 * in PR #258 (plan: `D:/qontinui-root/qontinui-dev-notes/plans/
 * 2026-05-23-coord-native-sessions-phase-7-10.md`, §Phase 7).
 *
 * Lists the SessionContext mirror of `coord.sessions` for this device and
 * exposes a "Transfer to <machine>" affordance per session. Selecting a
 * target device and confirming POSTs to coord via the `session_handoff`
 * Tauri command (`commands::session::session_handoff` →
 * `session::handoff::trigger_handoff`).
 *
 * Incoming `handoff_request` events are surfaced as toasts by
 * `IncomingHandoffToastBridge` (sibling component) so a peer pushing a
 * session to this device shows up as a notification, not a silent
 * SessionEvent stream entry.
 *
 * Scope (iter-1): the operator must paste the target device UUID by
 * hand. A device-picker that lists peers from `/devices` is iter-2 work —
 * the underlying API surface isn't all the way wired yet and the panel
 * is functional for the manual-test loop today.
 */

import { useCallback, useMemo, useState } from "react";
import { ArrowRightLeft, Loader2, AlertCircle } from "lucide-react";
import { useSession, type SessionDescriptor } from "@/contexts/SessionContext";
import { getStatusColors } from "@/design-system";

const UUID_PATTERN = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

interface HandoffPanelProps {
  /** Optional log sink — wired from the parent settings page. */
  onLog?: (msg: string) => void;
}

type RowState =
  | { kind: "idle" }
  | { kind: "submitting" }
  | { kind: "error"; message: string }
  | { kind: "submitted"; targetDeviceId: string };

export function HandoffPanel({ onLog }: HandoffPanelProps) {
  const { sessions, handoffSession, refresh } = useSession();

  // Per-session draft state (target device id input + submission state).
  const [draftTarget, setDraftTarget] = useState<Record<string, string>>({});
  const [rowState, setRowState] = useState<Record<string, RowState>>({});

  // Show only sessions that are actually transferable — closed/stale sessions
  // cannot be handed off (coord rejects). The Phase 7 trigger applies to
  // `active` and `pending_resolution` sessions per the source intent.
  const eligibleSessions = useMemo(
    () => sessions.filter((s) => s.state === "active" || s.state === "pending_resolution"),
    [sessions],
  );

  const handleTransfer = useCallback(
    async (session: SessionDescriptor) => {
      const target = (draftTarget[session.id] ?? "").trim();
      if (!UUID_PATTERN.test(target)) {
        setRowState((prev) => ({
          ...prev,
          [session.id]: {
            kind: "error",
            message: "Target device id must be a UUID (e.g. 84c02292-…).",
          },
        }));
        return;
      }
      setRowState((prev) => ({ ...prev, [session.id]: { kind: "submitting" } }));
      try {
        await handoffSession(session.id, target);
        setRowState((prev) => ({
          ...prev,
          [session.id]: { kind: "submitted", targetDeviceId: target },
        }));
        onLog?.(`Handoff requested: ${session.id} → ${target}`);
        // Drop the draft once submission succeeded.
        setDraftTarget((prev) => {
          const next = { ...prev };
          delete next[session.id];
          return next;
        });
      } catch (err) {
        const message = err instanceof Error ? err.message : String(err);
        setRowState((prev) => ({
          ...prev,
          [session.id]: { kind: "error", message },
        }));
        onLog?.(`Handoff failed: ${message}`);
      }
    },
    [draftTarget, handoffSession, onLog],
  );

  return (
    <section
      data-page-element="handoff-panel"
      className="rounded-lg border border-border bg-card p-4 space-y-3"
    >
      <header className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <ArrowRightLeft className="w-4 h-4 text-primary" />
          <h3 className="font-semibold text-foreground">Cross-machine session handoff</h3>
        </div>
        <button
          type="button"
          onClick={() => void refresh()}
          className="text-xs text-muted-foreground hover:text-foreground"
        >
          Refresh
        </button>
      </header>

      <p className="text-xs text-muted-foreground">
        Move an active session to another runner. The target machine materializes
        a child session (cwd, held claims, recent PTY scrollback follow); this
        device's source session closes. Plan §Phase 7.
      </p>

      {eligibleSessions.length === 0 ? (
        <div className="text-sm text-muted-foreground italic py-3">
          No transferable sessions on this device.
        </div>
      ) : (
        <ul
          data-page-element="handoff-session-list"
          className="space-y-2"
        >
          {eligibleSessions.map((session) => {
            const state = rowState[session.id] ?? { kind: "idle" as const };
            const isSubmitting = state.kind === "submitting";
            const draft = draftTarget[session.id] ?? "";
            return (
              <li
                key={session.id}
                data-page-element="handoff-session-row"
                data-session-id={session.id}
                className="rounded border border-border bg-background p-3 space-y-2"
              >
                <div className="flex items-center justify-between gap-3">
                  <div className="min-w-0">
                    <div className="text-sm font-medium truncate">
                      {session.intent.purpose || session.kind}
                    </div>
                    <div className="text-[11px] text-muted-foreground font-mono truncate">
                      {session.id}
                    </div>
                  </div>
                  <span className="text-[10px] uppercase tracking-wider rounded px-2 py-0.5 bg-muted text-muted-foreground">
                    {session.kind}
                  </span>
                </div>
                <div className="flex items-center gap-2">
                  <input
                    type="text"
                    value={draft}
                    onChange={(e) =>
                      setDraftTarget((prev) => ({
                        ...prev,
                        [session.id]: e.target.value,
                      }))
                    }
                    placeholder="Target device UUID"
                    aria-label={`Target device id for session ${session.id}`}
                    data-page-element="handoff-target-input"
                    className="flex-1 text-xs font-mono px-2 py-1.5 rounded border border-border bg-background focus:outline-hidden focus:ring-2 focus:ring-primary"
                    disabled={isSubmitting}
                  />
                  <button
                    type="button"
                    onClick={() => void handleTransfer(session)}
                    disabled={isSubmitting || !draft.trim()}
                    data-page-element="handoff-submit-button"
                    className="flex items-center gap-1.5 px-3 py-1.5 text-xs rounded-lg bg-primary text-primary-foreground hover:bg-primary/90 disabled:bg-muted disabled:text-muted-foreground disabled:cursor-not-allowed transition-colors"
                  >
                    {isSubmitting ? (
                      <>
                        <Loader2 className="w-3 h-3 animate-spin" />
                        Submitting…
                      </>
                    ) : (
                      <>
                        <ArrowRightLeft className="w-3 h-3" />
                        Transfer
                      </>
                    )}
                  </button>
                </div>
                {state.kind === "error" && (
                  <div
                    className={`flex items-start gap-1.5 text-xs ${getStatusColors("error").text}`}
                    role="alert"
                  >
                    <AlertCircle className="w-3.5 h-3.5 mt-0.5 shrink-0" />
                    <span>{state.message}</span>
                  </div>
                )}
                {state.kind === "submitted" && (
                  <div
                    className={`text-xs ${getStatusColors("success").text}`}
                    role="status"
                  >
                    Handoff requested → {state.targetDeviceId}. Source will close once
                    the target materializes the child session.
                  </div>
                )}
              </li>
            );
          })}
        </ul>
      )}
    </section>
  );
}

export default HandoffPanel;
