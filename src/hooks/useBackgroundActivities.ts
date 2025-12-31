/**
 * useBackgroundActivities.ts
 *
 * Hook for tracking all background activities in the application.
 * Aggregates RAG processing, web extraction, AI tasks, and data transmission states
 * into a unified interface for display in the status bar.
 *
 * Only returns active activities - no notifications for inactivity.
 */

import { useState, useEffect, useCallback, useMemo } from "react";
import { listen, UnlistenFn } from "@tauri-apps/api/event";

export type ActivityType = "rag" | "extraction" | "ai" | "sync";

export interface BackgroundActivity {
  id: string;
  type: ActivityType;
  label: string;
  progress?: number; // 0-100, optional
  detail?: string; // Additional context (e.g., current URL, model name)
  startedAt: Date;
}

interface UseBackgroundActivitiesProps {
  // RAG processing state (passed as props for initial state)
  ragStatus: "idle" | "processing" | "completed" | "failed";
  ragProgress?: number;
  ragProjectName?: string | null;

  // Web extraction state
  isExtracting: boolean;
  extractionUrl?: string;
  extractionProgress?: { pages_extracted: number; total_pages: number };

  // AI task tracking is handled internally via events
}

interface UseBackgroundActivitiesReturn {
  activities: BackgroundActivity[];
  hasActiveActivities: boolean;
  activityCount: number;
}

// RAG event status enum values (must match @qontinui/schemas/rag)
const RAG_STATUS = {
  IN_PROGRESS: "in_progress",
  COMPLETED: "completed",
  FAILED: "failed",
} as const;

interface RagProgressEvent {
  project_id: string;
  status: string;
  message: string;
  percent?: number;
  elements_processed?: number;
  total_elements?: number;
  error?: string;
}

/**
 * Hook to track and aggregate all background activities
 */
export function useBackgroundActivities({
  ragStatus: propRagStatus,
  ragProgress: propRagProgress,
  ragProjectName,
  isExtracting,
  extractionUrl,
  extractionProgress,
}: UseBackgroundActivitiesProps): UseBackgroundActivitiesReturn {
  // Track AI sessions that are currently active
  const [activeAiSessions, setActiveAiSessions] = useState<
    Map<string, { name: string; startedAt: Date }>
  >(new Map());

  // Track web sync operations
  const [activeSync, setActiveSync] = useState<{
    label: string;
    startedAt: Date;
  } | null>(null);

  // Track RAG processing via events (more reliable than props)
  const [ragState, setRagState] = useState<{
    isProcessing: boolean;
    progress?: number;
    projectName?: string;
    startedAt?: Date;
  }>({ isProcessing: false });

  // Track web extraction via events (more reliable than props)
  const [extractionState, setExtractionState] = useState<{
    isExtracting: boolean;
    progress?: number;
    currentUrl?: string;
    totalPages?: number;
    pagesExtracted?: number;
    startedAt?: Date;
  }>({ isExtracting: false });

  // Listen for RAG progress events directly
  useEffect(() => {
    let unlistenProgress: UnlistenFn | null = null;
    let unlistenCompletion: UnlistenFn | null = null;

    const setupListeners = async () => {
      // Listen for RAG progress events
      unlistenProgress = await listen<RagProgressEvent>("rag-progress", (event) => {
        const payload = event.payload;

        if (payload.status === RAG_STATUS.IN_PROGRESS) {
          setRagState({
            isProcessing: true,
            progress: payload.percent,
            projectName: payload.project_id,
            startedAt: new Date(),
          });
        } else if (
          payload.status === RAG_STATUS.COMPLETED ||
          payload.status === RAG_STATUS.FAILED
        ) {
          setRagState({ isProcessing: false });
        }
      });

      // Listen for RAG completion events
      unlistenCompletion = await listen("rag-completion", () => {
        setRagState({ isProcessing: false });
      });
    };

    setupListeners();

    return () => {
      unlistenProgress?.();
      unlistenCompletion?.();
    };
  }, []);

  // Sync RAG with props if they indicate processing (fallback)
  useEffect(() => {
    if (propRagStatus === "processing" && !ragState.isProcessing) {
      setRagState({
        isProcessing: true,
        progress: propRagProgress,
        projectName: ragProjectName || undefined,
        startedAt: new Date(),
      });
    } else if (
      propRagStatus !== "processing" &&
      propRagStatus !== "idle" &&
      ragState.isProcessing
    ) {
      // completed or failed from props
      setRagState({ isProcessing: false });
    }
  }, [propRagStatus, propRagProgress, ragProjectName, ragState.isProcessing]);

  // Sync extraction with props if they indicate extracting (fallback)
  useEffect(() => {
    if (isExtracting && !extractionState.isExtracting) {
      setExtractionState({
        isExtracting: true,
        currentUrl: extractionUrl,
        totalPages: extractionProgress?.total_pages,
        pagesExtracted: extractionProgress?.pages_extracted,
        startedAt: new Date(),
      });
    } else if (!isExtracting && extractionState.isExtracting) {
      setExtractionState({ isExtracting: false });
    }
  }, [isExtracting, extractionUrl, extractionProgress, extractionState.isExtracting]);

  // Listen for AI output events to track active AI sessions
  useEffect(() => {
    let unlisten: UnlistenFn | null = null;

    const setupListener = async () => {
      unlisten = await listen<{
        line?: string;
        source?: string;
        actionId?: string;
        sessionId?: string;
        sessionName?: string;
      }>("ai-output", (event) => {
        const { sessionId, sessionName, line } = event.payload;

        if (sessionId) {
          // Check for session end markers
          if (
            line?.includes("[TASK_COMPLETE]") ||
            line?.includes("Session ended") ||
            line?.includes("Error:")
          ) {
            setActiveAiSessions((prev) => {
              const next = new Map(prev);
              next.delete(sessionId);
              return next;
            });
          } else {
            // Session is active
            setActiveAiSessions((prev) => {
              if (!prev.has(sessionId)) {
                const next = new Map(prev);
                next.set(sessionId, {
                  name: sessionName || "AI Analysis",
                  startedAt: new Date(),
                });
                return next;
              }
              return prev;
            });
          }
        }
      });
    };

    setupListener();

    return () => {
      unlisten?.();
    };
  }, []);

  // Listen for executor events (extraction and sync operations)
  useEffect(() => {
    let unlisten: UnlistenFn | null = null;

    const setupListener = async () => {
      unlisten = await listen<{
        event_type: string;
        data?: {
          target?: string;
          status?: string;
          extraction_id?: string;
          total_pages?: number;
          current_url?: string;
          result?: unknown;
          error?: string;
        };
      }>("executor-event", (event) => {
        const { event_type, data } = event.payload;

        // Track web extraction events
        if (event_type === "extraction_started") {
          setExtractionState({
            isExtracting: true,
            totalPages: data?.total_pages,
            currentUrl: data?.current_url,
            pagesExtracted: 0,
            startedAt: new Date(),
          });
        } else if (event_type === "extraction_progress") {
          setExtractionState((prev) => ({
            ...prev,
            isExtracting: true,
            currentUrl: data?.current_url || prev.currentUrl,
            // Update progress if we have status data
          }));
        } else if (event_type === "extraction_completed") {
          setExtractionState({ isExtracting: false });
        } else if (event_type === "extraction_failed") {
          setExtractionState({ isExtracting: false });
        }

        // Track web sync events
        if (event_type === "sync_started" || event_type === "web_sync_started") {
          setActiveSync({
            label: data?.target || "Web Sync",
            startedAt: new Date(),
          });
        } else if (
          event_type === "sync_completed" ||
          event_type === "sync_failed" ||
          event_type === "web_sync_completed" ||
          event_type === "web_sync_failed"
        ) {
          setActiveSync(null);
        }
      });
    };

    setupListener();

    return () => {
      unlisten?.();
    };
  }, []);

  // Build the list of active activities
  const activities = useMemo(() => {
    const result: BackgroundActivity[] = [];

    // RAG Processing - use internal state (more reliable)
    if (ragState.isProcessing) {
      result.push({
        id: "rag-processing",
        type: "rag",
        label: "RAG Processing",
        progress: ragState.progress,
        detail: ragState.projectName || ragProjectName || undefined,
        startedAt: ragState.startedAt || new Date(),
      });
    }

    // Web Extraction - use internal state (more reliable)
    if (extractionState.isExtracting) {
      let progress: number | undefined;
      let detail: string | undefined;

      const totalPages = extractionState.totalPages || extractionProgress?.total_pages;
      const pagesExtracted = extractionState.pagesExtracted || extractionProgress?.pages_extracted;

      if (totalPages && totalPages > 0 && pagesExtracted !== undefined) {
        progress = Math.round((pagesExtracted / totalPages) * 100);
      }

      const currentUrl = extractionState.currentUrl || extractionUrl;
      if (currentUrl) {
        // Truncate URL for display
        try {
          const url = new URL(currentUrl);
          detail = url.hostname;
        } catch {
          detail = currentUrl.substring(0, 30);
        }
      }

      result.push({
        id: "web-extraction",
        type: "extraction",
        label: "Web Extraction",
        progress,
        detail,
        startedAt: extractionState.startedAt || new Date(),
      });
    }

    // AI Sessions
    activeAiSessions.forEach((session, sessionId) => {
      result.push({
        id: `ai-${sessionId}`,
        type: "ai",
        label: session.name,
        startedAt: session.startedAt,
      });
    });

    // Web Sync
    if (activeSync) {
      result.push({
        id: "web-sync",
        type: "sync",
        label: activeSync.label,
        startedAt: activeSync.startedAt,
      });
    }

    return result;
  }, [
    ragState,
    ragProjectName,
    extractionState,
    extractionUrl,
    extractionProgress,
    activeAiSessions,
    activeSync,
  ]);

  // Clear stale AI sessions after 10 minutes of no updates
  const clearStaleAiSessions = useCallback(() => {
    const tenMinutesAgo = new Date(Date.now() - 10 * 60 * 1000);
    setActiveAiSessions((prev) => {
      const next = new Map(prev);
      let changed = false;
      next.forEach((session, id) => {
        if (session.startedAt < tenMinutesAgo) {
          next.delete(id);
          changed = true;
        }
      });
      return changed ? next : prev;
    });
  }, []);

  // Periodically clean up stale sessions
  useEffect(() => {
    const interval = setInterval(clearStaleAiSessions, 60000); // Every minute
    return () => clearInterval(interval);
  }, [clearStaleAiSessions]);

  return {
    activities,
    hasActiveActivities: activities.length > 0,
    activityCount: activities.length,
  };
}
