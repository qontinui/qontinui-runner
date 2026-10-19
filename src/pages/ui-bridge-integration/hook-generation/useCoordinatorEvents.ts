/**
 * useCoordinatorEvents — HookGenerationPanel's window-event broadcast to the
 * top-level integration coordinator. Moved verbatim out of the panel.
 */

import { useEffect, useRef, type MutableRefObject } from "react";
import type { WriteHooksResult } from "../types";
import type { GeneratedFile } from "./parse";
import type { PanelPhase, StepStatus } from "./steps/types";

export function useCoordinatorEvents(params: {
  phase: PanelPhase;
  stepStatuses: StepStatus[];
  writeResult: WriteHooksResult | null;
  error: string | null;
  allGeneratedFilesRef: MutableRefObject<GeneratedFile[]>;
}): void {
  const { phase, stepStatuses, writeResult, error, allGeneratedFilesRef } = params;

  // Broadcast phase transitions so the top-level coordinator (one-click
  // "Integrate this Project" flow) can react without reading our internal
  // state. We emit three events:
  //   - ui-bridge-generate-pages-complete : phase → "preview" (files ready)
  //   - ui-bridge-generate-pages-applied  : phase → "applied" (files on disk)
  //   - ui-bridge-generate-pages-error    : error set while generating
  // The detail payload uses the step-statuses array which already tracks
  // per-page success/failure, so the coordinator can render "Retry failed"
  // without duplicating state here.
  const prevPhaseRef = useRef<PanelPhase>(phase);
  useEffect(() => {
    const prev = prevPhaseRef.current;
    prevPhaseRef.current = phase;
    if (prev === phase) return;
    if (phase === "preview") {
      const failedLabels = stepStatuses.filter((s) => s.state === "error").map((s) => s.label);
      const doneLabels = stepStatuses.filter((s) => s.state === "done").map((s) => s.label);
      window.dispatchEvent(
        new CustomEvent("ui-bridge-generate-pages-complete", {
          detail: {
            filesGenerated: allGeneratedFilesRef.current.length,
            doneSteps: doneLabels,
            failedSteps: failedLabels,
          },
        }),
      );
    } else if (phase === "applied") {
      window.dispatchEvent(
        new CustomEvent("ui-bridge-generate-pages-applied", {
          detail: { filesWritten: writeResult?.files_written ?? [] },
        }),
      );
    }
  }, [phase, stepStatuses, writeResult, allGeneratedFilesRef]);

  // Mirror errors during generation onto the coordinator's progress UI.
  const prevErrorRef = useRef<string | null>(null);
  useEffect(() => {
    if (error && error !== prevErrorRef.current) {
      window.dispatchEvent(
        new CustomEvent("ui-bridge-generate-pages-error", {
          detail: { message: error },
        }),
      );
    }
    prevErrorRef.current = error;
  }, [error]);
}
