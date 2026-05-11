/**
 * Phase 3 of the productivity-stack plan — `useReviewState`.
 *
 * Per-session hook backing the `ReviewBadge` indicator on terminal tabs.
 * Subscribes to the runner's `review-completed` Tauri event for real-time
 * updates AND polls `GET /sessions/<id>/latest-review` every 30 seconds as
 * a stale-resilience fallback.
 *
 * The hook returns `null` when no review row exists for the session yet
 * (the badge renders nothing in that case). Empty is a load-bearing state:
 * see plan §6 "Review verdict surface" — empty → no badge, not a
 * placeholder.
 */

import { useEffect, useState } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { resolvePort } from "./LaunchMenu";

/** Verdict tag emitted by `/auto-review`. Mirrors the wire format from
 *  `pg::reviews::ReviewRow`. */
export type ReviewVerdict = "approved" | "needs_fix" | "escalate";

/** Visual status the badge renders. We project the raw verdict +
 *  confidence pair into a discriminator the UI can switch on. */
export type ReviewStatus =
  | "approved-high"
  | "approved-medium"
  | "needs-fix"
  | "escalated"
  | "pending";

/** Confidence threshold above which an `approved` verdict auto-merges
 *  per plan §4 Rule D. Below this and ≥ 0.7, it queues for user
 *  approval. */
const HIGH_CONF_THRESHOLD = 0.85;

/** Lower bound for the medium-confidence band. Approvals below this with
 *  `verdict=approved` are unusual (the reviewer almost always escalates
 *  in that range), but we still classify them as `pending` if they show
 *  up to avoid the badge claiming false certainty. */
const MEDIUM_CONF_THRESHOLD = 0.7;

/** State returned to the badge. `null` is "no review yet, render nothing". */
export interface ReviewState {
  status: ReviewStatus;
  /** Raw `[0, 1]` confidence as recorded by the reviewer. */
  confidence: number;
  /** First non-empty line of the reviewer's reasoning, used as the
   *  tooltip's top defect line. Truncated to a reasonable cap. */
  topDefect: string;
  /** Review row id. Click → recommendations queue uses this to
   *  pre-select. */
  reviewId: string;
}

/** Wire shape of a row from `pg::reviews::ReviewRow`. */
interface ReviewRowWire {
  id: string;
  taskId: string;
  reviewerSessionId: string;
  reviewedSessionId: string;
  verdict: ReviewVerdict;
  confidence: number;
  reasoning: string;
  diffSummary: unknown;
  testResults: unknown;
  userDecision: string | null;
  userDecidedAt: string | null;
  createdAt: string;
}

/** Wire shape of `GET /sessions/<id>/latest-review`. */
interface LatestReviewResponse {
  taskRunId: string;
  review: ReviewRowWire | null;
}

/** Wire shape of the `review-completed` Tauri event payload. Only the
 *  fields the badge actually needs. */
interface ReviewCompletedEvent {
  id: string;
  taskId: string;
  reviewerSessionId: string;
  reviewedSessionId: string;
  verdict: ReviewVerdict;
  confidence: number;
  createdAt: string;
}

/** Project a wire row into the badge's `ReviewState`. Returns `null` for
 *  unrecognised verdicts so a future verdict that wasn't compiled into the
 *  client doesn't crash. */
function projectReview(row: ReviewRowWire): ReviewState | null {
  let status: ReviewStatus;
  switch (row.verdict) {
    case "approved":
      status =
        row.confidence >= HIGH_CONF_THRESHOLD
          ? "approved-high"
          : row.confidence >= MEDIUM_CONF_THRESHOLD
            ? "approved-medium"
            : "pending";
      break;
    case "needs_fix":
      status = "needs-fix";
      break;
    case "escalate":
      status = "escalated";
      break;
    default:
      return null;
  }

  const firstLine =
    row.reasoning
      .split(/\r?\n/)
      .map((l) => l.trim())
      .find((l) => l.length > 0) ?? "";
  const topDefect = firstLine.slice(0, 200);

  return {
    status,
    confidence: row.confidence,
    topDefect,
    reviewId: row.id,
  };
}

/** Poll URL for the runner. Resolves the port via the centralized
 *  helper so secondary/temp runners on non-default ports get their
 *  actually-bound port (populated asynchronously by `useApiReady` via
 *  the `api-ready` Tauri event). */
function runnerBaseUrl(): string {
  return `http://127.0.0.1:${resolvePort()}`;
}

/** Hook: returns the latest review state for a session, or `null` when
 *  there's none (or it hasn't arrived yet). */
export function useReviewState(sessionId: string | undefined): ReviewState | null {
  const [state, setState] = useState<ReviewState | null>(null);

  useEffect(() => {
    if (!sessionId) {
      // eslint-disable-next-line react-hooks/set-state-in-effect -- resetting state when session clears
      setState(null);
      return;
    }

    let active = true;
    let pollTimer: ReturnType<typeof setInterval> | null = null;
    let unlisten: UnlistenFn | null = null;

    const fetchLatest = async () => {
      try {
        const resp = await fetch(
          `${runnerBaseUrl()}/sessions/${encodeURIComponent(sessionId)}/latest-review`,
        );
        if (!resp.ok || !active) return;
        const body = (await resp.json()) as LatestReviewResponse;
        if (!active) return;
        if (!body.review) {
          setState(null);
          return;
        }
        const next = projectReview(body.review);
        setState(next);
      } catch {
        // Silently ignore — the poll fires again in 30 s.
      }
    };

    // Subscribe to real-time updates. Filter to events targeting this tab.
    listen<ReviewCompletedEvent>("review-completed", (event) => {
      if (!active) return;
      if (event.payload.reviewedSessionId !== sessionId) return;
      // The event payload omits user_decision / reasoning to keep it
      // small. Re-fetch the full row so the projector can compute
      // `topDefect` from the reasoning body.
      void fetchLatest();
    })
      .then((fn) => {
        if (!active) {
          fn();
          return;
        }
        unlisten = fn;
      })
      .catch(() => {
        // Tauri unavailable (e.g. tests) → poll-only mode.
      });

    // Initial fetch + periodic poll.
    void fetchLatest();
    pollTimer = setInterval(fetchLatest, 30_000);

    return () => {
      active = false;
      if (pollTimer) clearInterval(pollTimer);
      if (unlisten) {
        try {
          unlisten();
        } catch {
          // ignore
        }
      }
    };
  }, [sessionId]);

  return state;
}
