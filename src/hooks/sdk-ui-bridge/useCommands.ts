/**
 * useCommands — Command dispatching sub-hook.
 *
 * Manages executeAction, sendCommand (raw API), command history,
 * and coordinates with capture sessions for transition tracking.
 */

import { useState, useCallback, useRef } from "react";
import type {
  CommandResult,
  CommandHistoryEntry,
  ExternalElement,
  CaptureSessionRef,
} from "./types";
import { extractFingerprintHashes } from "../../lib/ui-bridge/fingerprintGenerator";
import { getApiBase, tracedFetch } from "@/lib/runner-api";

const MAX_COMMAND_HISTORY = 50;

export interface UseCommandsReturn {
  lastCommandResult: CommandResult | null;
  commandHistory: CommandHistoryEntry[];
  executeAction: (
    elementId: string,
    action: string,
    params?: Record<string, unknown>,
  ) => Promise<CommandResult>;
  sendCommand: <T = unknown>(
    action: string,
    params?: Record<string, unknown>,
  ) => Promise<CommandResult<T>>;
  clearCommandHistory: () => void;
}

export function useCommands(
  elements: ExternalElement[],
  fetchElements: () => Promise<void>,
  captureSessionRef: React.MutableRefObject<CaptureSessionRef | null>,
): UseCommandsReturn {
  const [lastCommandResult, setLastCommandResult] = useState<CommandResult | null>(null);
  const [commandHistory, setCommandHistory] = useState<CommandHistoryEntry[]>([]);
  const commandIdRef = useRef(0);

  // =========================================================================
  // executeAction
  // =========================================================================

  const executeAction = useCallback(
    async (
      elementId: string,
      action: string,
      params?: Record<string, unknown>,
    ): Promise<CommandResult> => {
      const startTime = Date.now();

      // Capture before-state for transition tracking
      const session = captureSessionRef.current;
      const beforeCaptureId = session?.lastCaptureId || null;
      const beforeHashes = session ? new Set(extractFingerprintHashes(elements)) : null;
      const targetFingerprint = elements.find((e) => e.id === elementId)?.fingerprint?.hash || null;

      try {
        const resp = await tracedFetch(
          `${getApiBase()}/ui-bridge/sdk/element/${encodeURIComponent(elementId)}/action`,
          {
            method: "POST",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify({ action, params }),
          },
        );

        const json = await resp.json();
        const result: CommandResult = {
          success: json.success !== false,
          data: json.data,
          error: json.error,
          duration: Date.now() - startTime,
        };

        setLastCommandResult(result);
        setCommandHistory((prev) =>
          [
            {
              id: ++commandIdRef.current,
              timestamp: Date.now(),
              action: `${action} on ${elementId}`,
              params,
              result,
            },
            ...prev,
          ].slice(0, MAX_COMMAND_HISTORY),
        );

        // After action, refresh elements to capture the new state
        // This triggers auto-capture if session is active
        if (session && result.success) {
          // Small delay for the UI to update after the action
          await new Promise((r) => setTimeout(r, 500));
          await fetchElements();

          // Record the transition if we have before/after captures
          const afterCaptureId = captureSessionRef.current?.lastCaptureId || null;
          if (
            beforeCaptureId &&
            afterCaptureId &&
            beforeCaptureId !== afterCaptureId &&
            beforeHashes
          ) {
            const afterHashes = new Set(extractFingerprintHashes(elements));
            const appeared = [...afterHashes].filter((h) => !beforeHashes.has(h));
            const disappeared = [...beforeHashes].filter((h) => !afterHashes.has(h));

            // Update the after-capture with transition info
            const afterCapture = session.captures.find((c) => c.captureId === afterCaptureId);
            if (afterCapture) {
              afterCapture.triggeredBy = {
                actionType: action,
                targetFingerprint: targetFingerprint || "",
                previousCaptureId: beforeCaptureId,
              };
            }

            // Log transition for debugging
            if (appeared.length > 0 || disappeared.length > 0) {
              console.log(
                `[useCommands] Transition: ${action} on ${elementId} — +${appeared.length} -${disappeared.length} fingerprints`,
              );
            }
          }
        }

        return result;
      } catch (e) {
        const result: CommandResult = {
          success: false,
          error: e instanceof Error ? e.message : "Action failed",
          duration: Date.now() - startTime,
        };
        setLastCommandResult(result);
        return result;
      }
    },
    [elements, fetchElements, captureSessionRef],
  );

  // =========================================================================
  // sendCommand (Raw API for RawApiPanel compatibility)
  // =========================================================================

  const sendCommand = useCallback(
    async <T = unknown>(
      action: string,
      params: Record<string, unknown> = {},
    ): Promise<CommandResult<T>> => {
      const startTime = Date.now();

      try {
        // Map common actions to SDK endpoints
        let url: string;
        let method: string = "GET";
        let body: unknown = undefined;

        switch (action) {
          case "getElements":
            url = `${getApiBase()}/ui-bridge/sdk/elements`;
            break;
          case "getElement":
            url = `${getApiBase()}/ui-bridge/sdk/element/${params?.elementId || params?.id}`;
            break;
          case "executeAction":
            url = `${getApiBase()}/ui-bridge/sdk/element/${params?.elementId || params?.id}/action`;
            method = "POST";
            body = { action: params?.action, params: params?.actionParams };
            break;
          case "getSnapshot":
            url = `${getApiBase()}/ui-bridge/sdk/snapshot`;
            break;
          case "getComponents":
            url = `${getApiBase()}/ui-bridge/sdk/components`;
            break;
          case "discover":
            url = `${getApiBase()}/ui-bridge/sdk/discover`;
            method = "POST";
            body = params;
            break;
          case "aiSearch":
            url = `${getApiBase()}/ui-bridge/sdk/ai/search`;
            method = "POST";
            body = params;
            break;
          case "aiExecute":
            url = `${getApiBase()}/ui-bridge/sdk/ai/execute`;
            method = "POST";
            body = params;
            break;
          case "getHealth":
            url = `${getApiBase()}/ui-bridge/sdk/health`;
            break;
          case "getMetrics":
            url = `${getApiBase()}/ui-bridge/sdk/debug/metrics`;
            break;
          default:
            // Generic: try as a GET to /ui-bridge/sdk/{action}
            url = `${getApiBase()}/ui-bridge/sdk/${action}`;
            if (params && Object.keys(params).length > 0) {
              method = "POST";
              body = params;
            }
            break;
        }

        const resp = await tracedFetch(url, {
          method,
          headers: method === "POST" ? { "Content-Type": "application/json" } : undefined,
          body: body ? JSON.stringify(body) : undefined,
        });

        const json = await resp.json();
        const duration = Date.now() - startTime;
        const result: CommandResult<T> = {
          success: json.success !== false,
          data: (json.data ?? json) as T,
          error: json.error,
          duration,
        };

        setLastCommandResult(result as CommandResult);
        setCommandHistory((prev) =>
          [
            {
              id: ++commandIdRef.current,
              timestamp: Date.now(),
              action,
              params: Object.keys(params).length > 0 ? params : undefined,
              result: result as CommandResult,
            },
            ...prev,
          ].slice(0, MAX_COMMAND_HISTORY),
        );

        return result;
      } catch (e) {
        const duration = Date.now() - startTime;
        const result: CommandResult<T> = {
          success: false,
          error: e instanceof Error ? e.message : "Command failed",
          duration,
        };

        setLastCommandResult(result as CommandResult);
        setCommandHistory((prev) =>
          [
            {
              id: ++commandIdRef.current,
              timestamp: Date.now(),
              action,
              params: Object.keys(params).length > 0 ? params : undefined,
              result: result as CommandResult,
            },
            ...prev,
          ].slice(0, MAX_COMMAND_HISTORY),
        );

        return result;
      }
    },
    [],
  );

  // =========================================================================
  // clearCommandHistory
  // =========================================================================

  const clearCommandHistory = useCallback(() => {
    setCommandHistory([]);
    setLastCommandResult(null);
  }, []);

  return {
    lastCommandResult,
    commandHistory,
    executeAction,
    sendCommand,
    clearCommandHistory,
  };
}
