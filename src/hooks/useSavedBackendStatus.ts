/**
 * The saved-backend status the two OPERATOR sign-in surfaces (the in-app
 * WebIntegrationAuthBanner and Settings → Account) decide their sign-in target
 * from — one copy of the read, the `web-integration-changed` listener, the
 * issue-order guard and Retry, which each surface used to carry on its own.
 *
 * Returned fields (two stale-status readings, named, never a mode flag):
 *   - `lastStatus` — the last SUCCESSFUL read. A failed re-read keeps it. The
 *     banner reads it for visibility, so a failed re-read neither hides nor
 *     flashes the banner.
 *   - `phase` — `loading` (the first read has not settled and nothing has
 *     failed), `unknown` (the latest read failed, or answered with a
 *     `settingsFault` — the decision's re-readable arms), `known` otherwise.
 *   - `decisionStatus` — the status the sign-in target is decided from:
 *     `lastStatus` when `phase === "known"`, else `null`. Account displays it;
 *     both surfaces take the target from it.
 *   - `reading` — true from a Retry until the LATEST-ISSUED read settles,
 *     whichever trigger issued it: a `web-integration-changed` read issued
 *     after the Retry supersedes the Retry's read, and "Reading…" stays up
 *     until that later read settles.
 *   - `retry()` — re-read the status; a no-op while `loading` or `reading`.
 *
 * A status missing the runner's verdict fields is `known` here: the operator
 * surfaces name that doubt beside the effective target before the click
 * (`signInTargetExplanation`), because a re-read cannot fix build skew.
 */

import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

import {
  isRereadableArm,
  signInDecisionFrom,
  type SavedBackendStatus,
} from "@/lib/savedBackendUrl";

export type SavedBackendPhase = "loading" | "known" | "unknown";

/** What an operator surface's sign-in control does right now. */
export type SignInControl = "wait" | "retry" | "reading" | "sign-in";

/**
 * The sign-in control for an operator surface. Both surfaces' handlers branch
 * on this FIRST, and their renders derive the label from it, so the guard and
 * the label cannot disagree.
 *
 *   - `busy` (a sign-in is in flight) wins: `wait` — never Retry beside it.
 *   - `reading` (a Retry's re-read is pending): `reading` — "Reading…", no-op.
 *   - `loading`: `wait` — disabled under the normal label.
 *   - `unknown`: `retry` — re-read instead of guessing a target.
 *   - `known`: `sign-in`.
 */
export function signInControl(
  phase: SavedBackendPhase,
  reading: boolean,
  busy: boolean,
): SignInControl {
  if (busy) return "wait";
  if (reading) return "reading";
  if (phase === "loading") return "wait";
  if (phase === "unknown") return "retry";
  return "sign-in";
}

export interface SavedBackendStatusState<T extends SavedBackendStatus> {
  lastStatus: T | null;
  phase: SavedBackendPhase;
  decisionStatus: T | null;
  reading: boolean;
  retry: () => void;
}

export function useSavedBackendStatus<T extends SavedBackendStatus = SavedBackendStatus>({
  onReadError,
}: {
  /** Called with the error when the LATEST-issued read fails. */
  onReadError?: (err: unknown) => void;
} = {}): SavedBackendStatusState<T> {
  const [lastStatus, setLastStatus] = useState<T | null>(null);
  const [latestFailed, setLatestFailed] = useState(false);
  // Set by `retry()`; cleared when the latest-issued read settles. While it is
  // set, `settledSeq < issuedSeq` holds, so `reading` is exactly "a Retry was
  // made and the latest-issued read has not settled".
  const [retryArmed, setRetryArmed] = useState(false);
  // Issue order of reads (mount + each `web-integration-changed` + Retry). Reads
  // overlap and may settle out of order; only the latest-issued may set state.
  const issuedSeq = useRef(0);
  const settledSeq = useRef(0);
  // The live effect run's read, so `retry()` can issue one under the same
  // `cancelled` + issue-order guards; null once unmounted.
  const issueReadRef = useRef<(() => void) | null>(null);
  const onReadErrorRef = useRef(onReadError);
  useEffect(() => {
    onReadErrorRef.current = onReadError;
  }, [onReadError]);

  useEffect(() => {
    let cancelled = false;
    const issueRead = () => {
      // A superseded effect run's listener must not bump the counter and drop
      // the live run's read.
      if (cancelled) return;
      const seq = ++issuedSeq.current;
      const isLatest = () => !cancelled && seq === issuedSeq.current;
      invoke<T>("get_web_integration_status").then(
        (next) => {
          if (!isLatest()) return;
          settledSeq.current = seq;
          setLastStatus(next);
          setLatestFailed(false);
          setRetryArmed(false);
        },
        (err: unknown) => {
          if (!isLatest()) return;
          settledSeq.current = seq;
          setLatestFailed(true);
          setRetryArmed(false);
          onReadErrorRef.current?.(err);
        },
      );
    };
    issueReadRef.current = issueRead;
    issueRead();
    const unlisten = listen("web-integration-changed", () => issueRead());
    return () => {
      cancelled = true;
      if (issueReadRef.current === issueRead) issueReadRef.current = null;
      unlisten
        .then((fn) => fn())
        .catch(() => {
          /* listener cleanup is best-effort */
        });
    };
  }, []);

  let phase: SavedBackendPhase;
  if (latestFailed) phase = "unknown";
  else if (lastStatus === null) phase = "loading";
  else
    phase = isRereadableArm(signInDecisionFrom(lastStatus, "operator").arm) ? "unknown" : "known";

  const reading = retryArmed;

  const retry = useCallback(() => {
    if (phase === "loading" || reading) return;
    const issueRead = issueReadRef.current;
    if (!issueRead) return;
    setRetryArmed(true);
    issueRead();
  }, [phase, reading]);

  return {
    lastStatus,
    phase,
    decisionStatus: phase === "known" ? lastStatus : null,
    reading,
    retry,
  };
}
