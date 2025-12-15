/**
 * useWebExtraction
 *
 * Hook for managing web extraction operations.
 * Responsibility: Control web UI extraction lifecycle and track extraction progress.
 */

import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { WebExtractionConfig, ExtractionStatus, ExtractionResult } from "../types/extraction";

interface UseWebExtractionReturn {
  extractionStatus: ExtractionStatus | null;
  isExtracting: boolean;
  error: string | null;
  lastResult: ExtractionResult | null;
  startExtraction: (config: WebExtractionConfig) => Promise<void>;
  stopExtraction: () => Promise<void>;
  getExtractionStatus: () => Promise<void>;
  listExtractions: () => Promise<void>;
  clearError: () => void;
}

interface ExecutorEventPayload {
  event_type: string;
  event: string;
  data: any;
}

/**
 * Hook to manage web extraction operations
 */
export function useWebExtraction(): UseWebExtractionReturn {
  const [extractionStatus, setExtractionStatus] = useState<ExtractionStatus | null>(null);
  const [isExtracting, setIsExtracting] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [lastResult, setLastResult] = useState<ExtractionResult | null>(null);

  /**
   * Start a web extraction
   */
  const startExtraction = useCallback(async (config: WebExtractionConfig): Promise<void> => {
    try {
      setError(null);
      setIsExtracting(true);

      const result: any = await invoke("start_web_extraction", { config });

      if (!result.success) {
        throw new Error(result.message || "Failed to start web extraction");
      }

      console.log("[WEB_EXTRACTION] Extraction started successfully");
    } catch (err) {
      const errorMessage = err instanceof Error ? err.message : String(err);
      console.error("[WEB_EXTRACTION] Failed to start extraction:", errorMessage);
      setError(errorMessage);
      setIsExtracting(false);
    }
  }, []);

  /**
   * Stop the current web extraction
   */
  const stopExtraction = useCallback(async (): Promise<void> => {
    try {
      setError(null);

      const result: any = await invoke("stop_web_extraction");

      if (!result.success) {
        throw new Error(result.message || "Failed to stop web extraction");
      }

      setIsExtracting(false);
      console.log("[WEB_EXTRACTION] Extraction stopped successfully");
    } catch (err) {
      const errorMessage = err instanceof Error ? err.message : String(err);
      console.error("[WEB_EXTRACTION] Failed to stop extraction:", errorMessage);
      setError(errorMessage);
    }
  }, []);

  /**
   * Get the current extraction status
   */
  const getExtractionStatus = useCallback(async (): Promise<void> => {
    try {
      setError(null);

      const result: any = await invoke("get_extraction_status");

      if (!result.success) {
        throw new Error(result.message || "Failed to get extraction status");
      }

      console.log("[WEB_EXTRACTION] Status request sent");
    } catch (err) {
      const errorMessage = err instanceof Error ? err.message : String(err);
      console.error("[WEB_EXTRACTION] Failed to get extraction status:", errorMessage);
      setError(errorMessage);
    }
  }, []);

  /**
   * List available extractions
   */
  const listExtractions = useCallback(async (): Promise<void> => {
    try {
      setError(null);

      const result: any = await invoke("list_extractions");

      if (!result.success) {
        throw new Error(result.message || "Failed to list extractions");
      }

      console.log("[WEB_EXTRACTION] List request sent");
    } catch (err) {
      const errorMessage = err instanceof Error ? err.message : String(err);
      console.error("[WEB_EXTRACTION] Failed to list extractions:", errorMessage);
      setError(errorMessage);
    }
  }, []);

  /**
   * Clear the error state
   */
  const clearError = useCallback(() => {
    setError(null);
  }, []);

  /**
   * Set up event listeners for extraction events
   */
  useEffect(() => {
    let unlisten: (() => void) | undefined;

    const setupListener = async () => {
      unlisten = await listen<ExecutorEventPayload>("executor-event", (event) => {
        const { event_type, data } = event.payload;

        console.log("[WEB_EXTRACTION] Received event:", event_type, data);

        switch (event_type) {
          case "extraction_started":
            setIsExtracting(true);
            setError(null);
            if (data.extraction_id) {
              setExtractionStatus({
                extraction_id: data.extraction_id,
                status: "running",
                progress: {
                  pages_extracted: 0,
                  total_pages: data.total_pages || 0,
                  current_url: data.current_url || "",
                },
              });
            }
            break;

          case "extraction_progress":
            if (data.status) {
              setExtractionStatus(data.status);
            }
            break;

          case "extraction_completed":
            setIsExtracting(false);
            if (data.result) {
              setLastResult(data.result);
            }
            if (data.status) {
              setExtractionStatus(data.status);
            }
            break;

          case "extraction_failed":
            setIsExtracting(false);
            setError(data.error || "Extraction failed");
            if (data.status) {
              setExtractionStatus(data.status);
            }
            break;

          default:
            // Ignore other event types
            break;
        }
      });
    };

    setupListener();

    return () => {
      if (unlisten) {
        unlisten();
      }
    };
  }, []);

  return {
    extractionStatus,
    isExtracting,
    error,
    lastResult,
    startExtraction,
    stopExtraction,
    getExtractionStatus,
    listExtractions,
    clearError,
  };
}
