/**
 * useRagProcessing.ts
 *
 * Hook for managing RAG (embedding) processing state.
 * Listens for Tauri events and provides methods to trigger processing.
 */

import { useState, useCallback, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen, UnlistenFn } from "@tauri-apps/api/event";
import type { RagProcessingState, RagLogEntry, RagProcessingStatus } from "../components/RagLogTab";
import {
  RagProcessingStatus as SharedRagProcessingStatus,
  type RagProgressEvent as SharedRagProgressEvent,
  type RagCompletionEvent as SharedRagCompletionEvent,
  type EmbeddingResultItem,
} from "@qontinui/schemas/rag";
import type { Config } from "./useConfiguration";

// Extend the shared type to include state_image_name for UI display
interface EmbeddingResultWithName extends EmbeddingResultItem {
  state_image_name?: string;
}

// Tauri event payloads - use shared types with minor extensions
type RagProgressEvent = SharedRagProgressEvent;

interface RagCompletionEvent extends Omit<SharedRagCompletionEvent, "results"> {
  results: EmbeddingResultWithName[];
}

const initialState: RagProcessingState = {
  status: "idle",
  projectId: null,
  projectName: null,
  percent: 0,
  elementsProcessed: 0,
  totalElements: 0,
  lastError: null,
  logs: [],
};

export function useRagProcessing() {
  const [state, setState] = useState<RagProcessingState>(initialState);
  const [configHasRagImages, setConfigHasRagImages] = useState(false);
  const [currentConfig, setCurrentConfig] = useState<Config | null>(null);

  // Add a log entry
  const addLog = useCallback(
    (type: RagLogEntry["type"], message: string, details?: RagLogEntry["details"]) => {
      const entry: RagLogEntry = {
        id: `${Date.now()}-${Math.random().toString(36).substr(2, 9)}`,
        timestamp: new Date(),
        type,
        message,
        details,
      };
      setState((prev) => ({
        ...prev,
        logs: [...prev.logs, entry],
      }));
    },
    [],
  );

  // Clear logs
  const clearLogs = useCallback(() => {
    setState((prev) => ({
      ...prev,
      logs: [],
      lastError: null,
    }));
  }, []);

  // Start RAG processing
  const startProcessing = useCallback(async () => {
    if (!state.projectId) {
      addLog("error", "No project loaded");
      return;
    }

    setState((prev) => ({
      ...prev,
      status: "processing",
      percent: 0,
      elementsProcessed: 0,
      lastError: null,
    }));

    addLog(
      "info",
      `Starting RAG embedding generation for project: ${state.projectName || state.projectId}`,
    );

    try {
      // Build the QontinuiConfig format expected by Rust
      // Convert the frontend config format to the Rust QontinuiConfig format
      let ragConfig = null;
      if (currentConfig) {
        ragConfig = {
          version: currentConfig.version || "2.0.0",
          metadata: {
            name: currentConfig.name || state.projectName || "unknown",
            description: "",
            author: "",
            created: new Date().toISOString(),
            modified: new Date().toISOString(),
            tags: [],
            compatible_versions: {
              runner: "2.0.0",
              website: "2.0.0",
            },
          },
          images: currentConfig.images || [],
          workflows: currentConfig.workflows || [],
          states: currentConfig.states || [],
          transitions: [],
        };
      }

      // Call Tauri command to start embedding generation
      await invoke("start_rag_processing", { projectId: state.projectId, config: ragConfig });
    } catch (error) {
      const errorMessage = error instanceof Error ? error.message : String(error);
      setState((prev) => ({
        ...prev,
        status: "failed",
        lastError: errorMessage,
      }));
      addLog("error", `Failed to start RAG processing: ${errorMessage}`);
    }
  }, [state.projectId, state.projectName, currentConfig, addLog]);

  // Set project info when config is loaded
  const setProject = useCallback(
    (
      projectId: string | null,
      projectName: string | null,
      hasRagImages: boolean,
      config?: Config,
    ) => {
      setState((prev) => ({
        ...prev,
        projectId,
        projectName,
        status: prev.status === "processing" ? prev.status : "idle",
      }));
      setConfigHasRagImages(hasRagImages);
      setCurrentConfig(config || null);

      if (projectId && hasRagImages) {
        addLog("info", `Project loaded: ${projectName || projectId} (has RAG-enabled StateImages)`);
      }
    },
    [addLog],
  );

  // Listen for RAG progress events
  useEffect(() => {
    let unlistenProgress: UnlistenFn | null = null;
    let unlistenCompletion: UnlistenFn | null = null;

    const setupListeners = async () => {
      // Listen for progress updates
      unlistenProgress = await listen<RagProgressEvent>("rag-progress", (event) => {
        const payload = event.payload;

        // Update state based on status
        // Map shared status to internal UI status
        let newStatus: RagProcessingStatus = "idle";
        switch (payload.status) {
          case SharedRagProcessingStatus.IN_PROGRESS:
            newStatus = "processing";
            break;
          case SharedRagProcessingStatus.COMPLETED:
            newStatus = "completed";
            break;
          case SharedRagProcessingStatus.FAILED:
            newStatus = "failed";
            break;
          default:
            newStatus = "idle";
        }

        setState((prev) => ({
          ...prev,
          status: newStatus,
          percent: payload.percent ?? prev.percent,
          elementsProcessed: payload.elements_processed ?? prev.elementsProcessed,
          totalElements: payload.total_elements ?? prev.totalElements,
          lastError: payload.error ?? prev.lastError,
        }));

        // Add log entry
        if (payload.status === SharedRagProcessingStatus.IN_PROGRESS) {
          addLog("progress", payload.message, {
            projectId: payload.project_id,
            percent: payload.percent,
            elementsProcessed: payload.elements_processed,
            totalElements: payload.total_elements,
          });
        } else if (payload.status === SharedRagProcessingStatus.COMPLETED) {
          addLog("success", payload.message);
        } else if (payload.status === SharedRagProcessingStatus.FAILED) {
          addLog("error", payload.message || payload.error || "Unknown error");
        }
      });

      // Listen for completion with results
      unlistenCompletion = await listen<RagCompletionEvent>("rag-completion", (event) => {
        const payload = event.payload;

        addLog(
          payload.success ? "success" : "warning",
          `Embedding generation complete: ${payload.successful}/${payload.total_processed} successful`,
        );

        // Log web sync status
        if (payload.success) {
          if (payload.web_sync_success) {
            addLog("success", "Embeddings synced to web backend successfully");
          } else if (payload.web_sync_error) {
            addLog("warning", `Failed to sync embeddings to web: ${payload.web_sync_error}`);
          }
        }

        // Log individual results for failed items
        payload.results.forEach((result) => {
          if (!result.success) {
            addLog("error", `Failed: ${result.state_image_name || result.state_image_id}`, {
              stateImageId: result.state_image_id,
              stateImageName: result.state_image_name,
            });
          }
        });
      });
    };

    setupListeners();

    return () => {
      unlistenProgress?.();
      unlistenCompletion?.();
    };
  }, [addLog]);

  return {
    state,
    configHasRagImages,
    startProcessing,
    clearLogs,
    setProject,
    addLog,
  };
}
