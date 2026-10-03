import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { TerminalTab } from "./useTerminalManager";
import {
  applyFailureNotice,
  resumeReports,
  SESSION_FAILURE_EVENT,
  setTerminalFailures,
  type FailuresByTerminal,
  type ServedSessionFailure,
  type SessionFailureNotice,
} from "./sessionFailures";

/**
 * Active session failures for this page's tabs, as the runner records them
 * (plan `2026-09-20-ai-session-handling-is-claude-shaped`, Phase 7).
 *
 * - Reads each tab's active failures once when the tab appears
 *   (`terminal_failures`), so a failure raised before this page mounted is
 *   shown.
 * - Follows every later change on the `session-failure` event.
 * - Reports the resume verifier's outcome to the runner: a tab whose
 *   `resumeFailed` flag sets becomes a `resume_failed` failure through the one
 *   classifier, and the verified retry that clears it is the evidence that
 *   ends it. The banner therefore renders a resume failure from the same
 *   payload as every other failure.
 */
export function useSessionFailures(tabs: TerminalTab[]): FailuresByTerminal {
  const [failures, setFailures] = useState<FailuresByTerminal>({});
  const loaded = useRef(new Set<string>());
  const reportedResume = useRef(new Set<string>());

  useEffect(() => {
    let unlisten: UnlistenFn | undefined;
    let cancelled = false;
    void listen<SessionFailureNotice>(SESSION_FAILURE_EVENT, (event) => {
      setFailures((prev) => applyFailureNotice(prev, event.payload));
    }).then((fn) => {
      if (cancelled) fn();
      else unlisten = fn;
    });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  useEffect(() => {
    for (const tab of tabs) {
      if (loaded.current.has(tab.id)) continue;
      loaded.current.add(tab.id);
      invoke<ServedSessionFailure[]>("terminal_failures", { terminalId: tab.id })
        .then((list) => {
          // A notice that arrived first is newer than this read; merge rather
          // than overwrite.
          setFailures((prev) =>
            prev[tab.id] ? prev : setTerminalFailures(prev, tab.id, list ?? []),
          );
        })
        .catch((err) => {
          console.warn(`[SessionFailures] terminal_failures failed for ${tab.id}:`, err);
        });
    }

    const { failed, verified } = resumeReports(tabs, reportedResume.current);
    for (const tabId of failed) {
      reportedResume.current.add(tabId);
      const provider = tabs.find((t) => t.id === tabId)?.sessionProvider;
      invoke<ServedSessionFailure | null>("terminal_report_resume_failure", {
        terminalId: tabId,
        provider: provider ?? null,
      })
        .then((failure) => {
          if (failure) {
            setFailures((prev) =>
              applyFailureNotice(prev, { terminalId: tabId, failure, active: true }),
            );
          }
        })
        .catch((err) => {
          console.warn(`[SessionFailures] resume-failure report failed for ${tabId}:`, err);
        });
    }
    for (const tabId of verified) {
      reportedResume.current.delete(tabId);
      invoke("terminal_report_resume_verified", { terminalId: tabId }).catch((err) => {
        console.warn(`[SessionFailures] resume-verified report failed for ${tabId}:`, err);
      });
    }
  }, [tabs]);

  return failures;
}

/** Operator acknowledgement of one failure. */
export function dismissSessionFailure(terminalId: string, failureId: string): Promise<boolean> {
  return invoke<boolean>("terminal_failure_dismiss", { terminalId, failureId });
}
