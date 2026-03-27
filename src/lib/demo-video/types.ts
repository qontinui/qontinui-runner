/**
 * Demo Video Generation Types
 *
 * Types for AI-driven demo video generation from UI Bridge page specs.
 */

// =============================================================================
// Demo Script Types
// =============================================================================

export interface DemoScript {
  title: string;
  description: string;
  targetPage: string;
  estimatedDurationSecs: number;
  steps: DemoStep[];
}

export interface DemoStep {
  id: string;
  narration: string;
  actions: DemoAction[];
  pauseAfterMs: number;
  highlight?: {
    elementId: string;
    type: "spotlight" | "pulse" | "border";
  };
}

export type DemoAction =
  | { type: "navigate"; target: string }
  | { type: "click"; elementId: string }
  | { type: "type"; elementId: string; text: string }
  | { type: "select"; elementId: string; value: string }
  | { type: "scroll"; direction: "up" | "down"; amount: number }
  | { type: "wait"; ms: number }
  | { type: "waitForElement"; selector: string; timeout: number }
  | { type: "waitForIdle"; timeout: number };

// =============================================================================
// Recording Configuration
// =============================================================================

export interface DemoRecordingConfig {
  resolution: string;
  framerate: number;
  quality: "low" | "medium" | "high";
  codec: "h264" | "h265" | "vp9";
  maxDurationSecs: number;
  startDelaySecs: number;
  pauseBetweenStepsMs: number;
}

export const DEFAULT_RECORDING_CONFIG: DemoRecordingConfig = {
  resolution: "1920x1080",
  framerate: 24,
  quality: "medium",
  codec: "h264",
  maxDurationSecs: 300,
  startDelaySecs: 2,
  pauseBetweenStepsMs: 2000,
};

// =============================================================================
// Execution Result Types
// =============================================================================

export interface StepTimestamp {
  stepId: string;
  narration: string;
  startMs: number;
  endMs: number;
}

export interface DemoExecutionResult {
  videoPath: string;
  steps: StepTimestamp[];
  totalDurationMs: number;
}

// =============================================================================
// Generation State
// =============================================================================

export type DemoGenerationPhase =
  | "idle"
  | "planning"
  | "previewing"
  | "recording"
  | "post-processing"
  | "done"
  | "error";

export interface DemoGenerationState {
  phase: DemoGenerationPhase;
  script: DemoScript | null;
  result: DemoExecutionResult | null;
  currentStepIndex: number;
  elapsedMs: number;
  error: string | null;
}

export const INITIAL_GENERATION_STATE: DemoGenerationState = {
  phase: "idle",
  script: null,
  result: null,
  currentStepIndex: -1,
  elapsedMs: 0,
  error: null,
};
