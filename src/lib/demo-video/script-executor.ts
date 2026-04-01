/**
 * Script Executor
 *
 * Executes a DemoScript by controlling the video recorder and driving the UI via UI Bridge endpoints.
 */

import { invoke } from "@tauri-apps/api/core";
import type {
  DemoScript,
  DemoAction,
  DemoRecordingConfig,
  DemoExecutionResult,
  StepTimestamp,
  DemoVisualOverlayDetail,
} from "./types";
import { DEFAULT_RECORDING_CONFIG, DEMO_VISUAL_OVERLAY_EVENT } from "./types";
import { getApiBase } from "@/lib/runner-api";

// =============================================================================
// Helpers
// =============================================================================

function sleep(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms));
}

async function uiBridgePost(path: string, body?: Record<string, unknown>): Promise<unknown> {
  const resp = await fetch(`${getApiBase()}/ui-bridge${path}`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    ...(body ? { body: JSON.stringify(body) } : {}),
  });
  return resp.json();
}

// =============================================================================
// Action Execution
// =============================================================================

async function executeAction(action: DemoAction): Promise<void> {
  switch (action.type) {
    case "navigate":
      await uiBridgePost("/control/page/navigate", { url: action.target });
      break;

    case "click":
      await uiBridgePost(`/control/element/${action.elementId}/action`, {
        action: "click",
      });
      break;

    case "type":
      await uiBridgePost(`/control/element/${action.elementId}/action`, {
        action: "type",
        params: { text: action.text, clear: true },
      });
      break;

    case "select":
      await uiBridgePost(`/control/element/${action.elementId}/action`, {
        action: "select",
        params: { value: action.value },
      });
      break;

    case "scroll":
      await uiBridgePost("/control/scroll", {
        direction: action.direction,
        amount: action.amount,
      });
      break;

    case "wait":
      await sleep(action.ms);
      break;

    case "waitForElement":
      await uiBridgePost("/control/waitForElement", {
        selector: action.selector,
        timeout: action.timeout,
      });
      break;

    case "waitForIdle": {
      // Poll idle status until ready or timeout
      const deadline = Date.now() + action.timeout;
      while (Date.now() < deadline) {
        try {
          const resp = await fetch(`${getApiBase()}/ui-bridge/ai/idle-status`);
          const data = await resp.json();
          if (data.data?.idle || data.idle) break;
        } catch {
          // ignore fetch errors during polling
        }
        await sleep(500);
      }
      break;
    }

    case "visual": {
      // Show a pre-generated image/video as a fullscreen overlay
      const showDetail: DemoVisualOverlayDetail = {
        action: "show",
        filePath: action.filePath,
        caption: action.caption,
        durationMs: action.durationMs,
      };
      window.dispatchEvent(new CustomEvent(DEMO_VISUAL_OVERLAY_EVENT, { detail: showDetail }));
      // Hold for the specified duration while the overlay is visible on screen
      await sleep(action.durationMs);
      // Dismiss the overlay
      const hideDetail: DemoVisualOverlayDetail = { action: "hide" };
      window.dispatchEvent(new CustomEvent(DEMO_VISUAL_OVERLAY_EVENT, { detail: hideDetail }));
      break;
    }
  }
}

// =============================================================================
// Script Execution
// =============================================================================

export interface ExecutionCallbacks {
  onStepStart?: (stepIndex: number, stepId: string) => void;
  onStepComplete?: (stepIndex: number, stepId: string) => void;
  onError?: (error: string, stepIndex: number) => void;
}

let abortRequested = false;

export function abortExecution(): void {
  abortRequested = true;
}

export async function executeScript(
  script: DemoScript,
  config: Partial<DemoRecordingConfig> = {},
  callbacks?: ExecutionCallbacks,
): Promise<DemoExecutionResult> {
  abortRequested = false;
  const mergedConfig = { ...DEFAULT_RECORDING_CONFIG, ...config };
  const sessionId = `demo-${Date.now()}`;

  // Start recording
  await invoke("start_video_recording", {
    sessionId,
    config: {
      enabled: true,
      start_delay_seconds: mergedConfig.startDelaySecs,
      max_duration_seconds: mergedConfig.maxDurationSecs,
      fps: mergedConfig.framerate,
      quality: mergedConfig.quality,
      codec: mergedConfig.codec,
    },
  });

  // Wait for recording to stabilize
  await sleep(mergedConfig.startDelaySecs * 1000);

  const recordingStartTime = Date.now();
  const stepTimestamps: StepTimestamp[] = [];

  let executionError: unknown;
  try {
    for (let i = 0; i < script.steps.length; i++) {
      if (abortRequested) break;

      const step = script.steps[i];
      callbacks?.onStepStart?.(i, step.id);

      const stepStartMs = Date.now() - recordingStartTime;

      // Execute all actions in the step sequentially
      for (const action of step.actions) {
        if (abortRequested) break;
        try {
          await executeAction(action);
        } catch (err) {
          const msg = err instanceof Error ? err.message : String(err);
          callbacks?.onError?.(`Step ${step.id} action failed: ${msg}`, i);
          // Continue to next action — demo should be resilient
        }
      }

      // Pause for visual clarity
      const pauseMs = step.pauseAfterMs ?? mergedConfig.pauseBetweenStepsMs;
      await sleep(pauseMs);

      const stepEndMs = Date.now() - recordingStartTime;

      stepTimestamps.push({
        stepId: step.id,
        narration: step.narration,
        startMs: stepStartMs,
        endMs: stepEndMs,
      });

      callbacks?.onStepComplete?.(i, step.id);
    }
  } catch (err) {
    executionError = err;
  }

  // Always stop recording, even on error
  const stopResult = await invoke<{ success: boolean; path: string }>("stop_video_recording").catch(
    () => ({ success: false, path: "" }),
  );

  const totalDurationMs = Date.now() - recordingStartTime;

  if (executionError) throw executionError;

  return {
    videoPath: stopResult.path,
    steps: stepTimestamps,
    totalDurationMs,
  };
}
