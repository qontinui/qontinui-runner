/**
 * useObservationServices.ts
 *
 * Initializes observation-related services (session summary + learning bridge)
 * on app startup and tears them down on unmount. Updates project context
 * when the selected project changes.
 *
 * Usage: call once in AppContent with the current project ID.
 */

import { useEffect, useRef } from "react";
import { sessionSummaryService } from "@/services/session-summary-service";
import { learningObservationBridge } from "@/services/learning-observation-bridge";

/**
 * Initialize and manage observation persistence services.
 *
 * @param projectId - Current project ID (null if no project selected)
 */
export function useObservationServices(projectId: string | null): void {
  const startedRef = useRef(false);

  // Start services once on mount
  useEffect(() => {
    if (startedRef.current) return;
    startedRef.current = true;

    sessionSummaryService.start({ projectId: projectId ?? undefined });
    learningObservationBridge.start({ projectId: projectId ?? undefined });

    return () => {
      sessionSummaryService.stop();
      learningObservationBridge.stop();
      startedRef.current = false;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps -- one-time init
  }, []);

  // Update project context when selection changes
  useEffect(() => {
    sessionSummaryService.setProjectId(projectId);
    learningObservationBridge.setProjectId(projectId);
  }, [projectId]);
}
