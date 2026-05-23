/**
 * Spawn-time claim-conflict toast.
 *
 * Plan reference:
 * `qontinui-dev-notes/plans/2026-05-22-coord-native-session-coordination.md`
 * Phase 6.
 *
 * Previously this component was a full in-place modal that owned the
 * Wait/Steal/Retry resolution flow. Phase 6 splits that responsibility:
 *
 *  - The runner only **notifies** that a conflict occurred (this file).
 *  - The **resolution UX** (two-sided ConflictRow + StealModal) lives
 *    on the dashboard at `demo.staging.qontinui.io/sessions/<id>`.
 *
 * Why: the dashboard has cross-machine context (the other operator's
 * intent, started_at, file claims) that the runner does not. Keeping
 * the runner UI focused on "tell me something happened, take me where
 * I can act on it" instead of duplicating the conflict-resolution flow
 * means a single canonical surface (per plan §D9).
 *
 * Tauri event: `agent-claim-conflict` (unchanged subject; payload
 * gains `reason`, `session_metadata` fields per Phase 6).
 *
 * The component name stays `ConflictModal` for backward-compatible
 * imports (App.tsx is the only caller). The on-screen rendering is a
 * toast banner pinned bottom-right via Tailwind, matching the existing
 * `AppToasts.tsx` styling.
 */

import { useCallback, useEffect, useState } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { openUrl } from "@tauri-apps/plugin-opener";

/** Snake-case ClaimKind discriminator on the wire (matches coord). */
type ClaimKind =
  | "phase"
  | "file_glob"
  | "worktree"
  | "branch_name"
  | "alembic_revision"
  | "ci_wait"
  | "session"
  | "repo_branch";

/**
 * Metadata about the *other* session holding the contested claim.
 * Phase 6 enrichment — see plan §Phase 6:
 * "EVENT_CLAIM_CONFLICT / EVENT_CLAIM_STOLEN payloads gain reason,
 *  session_metadata".
 */
interface SessionMetadata {
  session_id?: string;
  kind?: string;
  purpose?: string;
  repo?: string;
  branch?: string;
}

/** Payload of the runner's `agent-claim-conflict` Tauri event. */
interface ConflictEvent {
  kind: ClaimKind | string;
  resourceKey: string;
  currentHolder: string;
  intent?: string | null;
  /** Phase 6: optional reason text from the other side. */
  reason?: string | null;
  /** Phase 6: metadata about the holder's session. */
  sessionMetadata?: SessionMetadata | null;
}

/**
 * Dashboard URL used to open the full conflict-resolution surface.
 *
 * Pinned to the staging dashboard for now. When the runner gets a
 * setting for dashboard-base-URL (Phase 7+ tenant/SSO work) the const
 * will become a setting read.
 */
const DASHBOARD_BASE_URL = "https://demo.staging.qontinui.io";

function dashboardUrlFor(conflict: ConflictEvent): string {
  // If we know the session id, open the session-detail panel directly;
  // otherwise drop to the list and let the operator find it.
  const sessionId = conflict.sessionMetadata?.session_id;
  if (sessionId) {
    return `${DASHBOARD_BASE_URL}/sessions/${encodeURIComponent(sessionId)}`;
  }
  return `${DASHBOARD_BASE_URL}/sessions`;
}

export function ConflictModal() {
  const [conflict, setConflict] = useState<ConflictEvent | null>(null);
  const [opening, setOpening] = useState(false);
  const [openError, setOpenError] = useState<string | null>(null);

  useEffect(() => {
    let unlisten: UnlistenFn | undefined;
    (async () => {
      try {
        unlisten = await listen<ConflictEvent>(
          "agent-claim-conflict",
          (event) => {
            setConflict(event.payload);
            setOpenError(null);
            setOpening(false);
          }
        );
      } catch (e) {
        console.error("ConflictModal toast: listen failed", e);
      }
    })();
    return () => {
      if (unlisten) unlisten();
    };
  }, []);

  const onDismiss = useCallback(() => {
    setConflict(null);
    setOpenError(null);
    setOpening(false);
  }, []);

  const onOpenDashboard = useCallback(async () => {
    if (!conflict) return;
    setOpening(true);
    setOpenError(null);
    try {
      await openUrl(dashboardUrlFor(conflict));
      // Auto-dismiss once we hand off to the dashboard.
      setConflict(null);
    } catch (e) {
      setOpenError(e instanceof Error ? e.message : String(e));
    } finally {
      setOpening(false);
    }
  }, [conflict]);

  if (!conflict) return null;

  const holderHost = conflict.currentHolder
    ? conflict.currentHolder.slice(0, 8)
    : "unknown";
  const purpose = conflict.sessionMetadata?.purpose ?? conflict.intent ?? null;

  return (
    <div
      role="alert"
      aria-live="polite"
      className="fixed bottom-4 right-4 z-toast w-[min(28rem,90vw)] rounded-lg border border-orange-500/50 bg-card p-4 shadow-lg"
      data-ui-bridge-id="conflict-toast"
      data-claim-kind={conflict.kind}
      data-resource-key={conflict.resourceKey}
    >
      <div className="flex items-start gap-3">
        <div className="flex-1 min-w-0 space-y-2">
          <h4 className="font-semibold text-sm text-orange-500 dark:text-orange-400">
            Claim conflict
          </h4>
          <p className="text-sm text-muted-foreground">
            Machine{" "}
            <code className="font-mono text-xs bg-muted px-1.5 py-0.5 rounded">
              {holderHost}
            </code>{" "}
            holds{" "}
            <code className="font-mono text-xs bg-muted px-1.5 py-0.5 rounded break-all">
              {conflict.kind}:{conflict.resourceKey}
            </code>
            .
          </p>
          {purpose && (
            <p
              className="text-xs italic text-muted-foreground"
              data-ui-bridge-id="conflict-toast.purpose"
            >
              “{purpose}”
            </p>
          )}
          {conflict.reason && (
            <p
              className="text-xs text-muted-foreground border-l-2 border-orange-500/40 pl-2"
              data-ui-bridge-id="conflict-toast.reason"
            >
              Reason: {conflict.reason}
            </p>
          )}
          {openError && (
            <p
              className="text-xs text-red-500"
              data-ui-bridge-id="conflict-toast.error"
            >
              Could not open dashboard: {openError}
            </p>
          )}
          <div className="flex items-center gap-2 pt-1">
            <button
              type="button"
              onClick={onOpenDashboard}
              disabled={opening}
              className="text-xs font-medium rounded-md px-2.5 py-1.5 bg-orange-500 text-white hover:bg-orange-600 disabled:opacity-50"
              data-ui-bridge-id="conflict-toast.resolve"
            >
              {opening ? "Opening…" : "Resolve in dashboard"}
            </button>
            <button
              type="button"
              onClick={onDismiss}
              className="text-xs rounded-md px-2.5 py-1.5 text-muted-foreground hover:text-foreground"
              data-ui-bridge-id="conflict-toast.dismiss"
            >
              Dismiss
            </button>
          </div>
        </div>
        <button
          onClick={onDismiss}
          aria-label="Dismiss conflict toast"
          className="text-muted-foreground hover:text-foreground shrink-0"
          data-ui-bridge-id="conflict-toast.close"
        >
          &times;
        </button>
      </div>
    </div>
  );
}
