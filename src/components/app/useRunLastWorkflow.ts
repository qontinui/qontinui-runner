import { useCallback, useRef, useState } from "react";
import { getApiBase, tracedFetch } from "@/lib/runner-api";
import type { MainTabId } from "./tab-types";

import type { TaskRun } from "@/types/aiData";

interface UseRunLastWorkflowReturn {
  isRunningLastWorkflow: boolean;
  runLastWorkflowError: string | null;
  setRunLastWorkflowError: (error: string | null) => void;
  handleRunLastWorkflow: () => Promise<void>;
  lastInlineWorkflowRef: React.MutableRefObject<{
    name: string;
    description?: string;
    setup_steps: unknown[];
    verification_steps: unknown[];
    agentic_steps: unknown[];
    completion_steps: unknown[];
    max_iterations?: number;
  } | null>;
}

export function useRunLastWorkflow(
  lastRun: TaskRun | null,
  setActiveTab: (tab: MainTabId) => void,
): UseRunLastWorkflowReturn {
  const [isRunningLastWorkflow, setIsRunningLastWorkflow] = useState(false);
  const [runLastWorkflowError, setRunLastWorkflowError] = useState<string | null>(null);

  const lastInlineWorkflowRef = useRef<{
    name: string;
    description?: string;
    setup_steps: unknown[];
    verification_steps: unknown[];
    agentic_steps: unknown[];
    completion_steps: unknown[];
    max_iterations?: number;
  } | null>(null);

  const handleRunLastWorkflow = useCallback(async () => {
    if (!lastRun?.workflow_name) return;

    setIsRunningLastWorkflow(true);

    try {
      const searchResponse = await tracedFetch(
        `${getApiBase()}/unified-workflows/search?q=${encodeURIComponent(lastRun.workflow_name)}`,
      );

      if (!searchResponse.ok) {
        throw new Error(`Failed to search workflows: ${searchResponse.statusText}`);
      }

      const searchResult = await searchResponse.json();
      const workflows = searchResult.data ?? [];

      const workflow = workflows.find((w: { name: string }) => w.name === lastRun.workflow_name);

      if (workflow?.id) {
        tracedFetch(`${getApiBase()}/unified-workflows/${workflow.id}/run`, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({}),
        }).catch((error) => {
          console.error("[APP] Failed to run workflow:", error);
        });

        setActiveTab("active");
      } else {
        const rawName = lastRun.workflow_name.replace(/^\[Inline\]\s*/, "");
        let inlinePayload = lastInlineWorkflowRef.current;

        if (!inlinePayload || inlinePayload.name !== rawName) {
          try {
            const inlineResponse = await tracedFetch(
              `${getApiBase()}/unified-workflows/last-inline`,
            );
            if (inlineResponse.ok) {
              const inlineResult = await inlineResponse.json();
              if (inlineResult.data?.name === rawName) {
                inlinePayload = inlineResult.data;
              }
            }
          } catch {
            // Ignore fetch errors for fallback
          }
        }

        if (inlinePayload && inlinePayload.name === rawName) {
          tracedFetch(`${getApiBase()}/unified-workflows/execute-inline`, {
            method: "POST",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify(inlinePayload),
          }).catch((error) => {
            console.error("[APP] Failed to re-execute inline workflow:", error);
          });

          setActiveTab("active");
        } else {
          console.warn("[APP] Workflow not found in DB or inline cache:", lastRun.workflow_name);
          setRunLastWorkflowError(
            `Workflow "${lastRun.workflow_name}" not found. It may have been lost after a runner restart.`,
          );
          setTimeout(() => setRunLastWorkflowError(null), 6000);
        }
      }
    } catch (error) {
      console.error("[APP] Failed to run last workflow:", error);
      setRunLastWorkflowError(
        `Failed to run workflow: ${error instanceof Error ? error.message : String(error)}`,
      );
      setTimeout(() => setRunLastWorkflowError(null), 6000);
    } finally {
      setIsRunningLastWorkflow(false);
    }
  }, [lastRun, setActiveTab]);

  return {
    isRunningLastWorkflow,
    runLastWorkflowError,
    setRunLastWorkflowError,
    handleRunLastWorkflow,
    lastInlineWorkflowRef,
  };
}
