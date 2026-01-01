/**
 * TauriFindingsListener
 *
 * Listens to Tauri events from the Rust backend for findings:
 * - finding_detected: A new finding was parsed from AI output
 * - finding_resolved: A finding was marked as resolved
 *
 * This bridges the Rust-side finding parsing to the TypeScript FindingsTracker.
 */

import { listen, UnlistenFn } from "@tauri-apps/api/event";
import type {
  Finding,
  FindingSeverity,
  FindingStatus,
  ActionType,
  CodeContext,
} from "../types/findings";
import { findingsTracker } from "./FindingsTracker";

/**
 * Payload from Rust for finding_detected event
 */
interface FindingDetectedPayload {
  id: string;
  task_run_id: string;
  session_num: number;
  category: string;
  severity: string;
  status: string;
  action_type: string;
  title: string;
  description: string;
  resolution?: string;
  file_path?: string;
  line_number?: number;
  column_number?: number;
  code_snippet?: string;
  signature_hash: string;
  needs_input: boolean;
  question?: string;
  input_options?: string[];
  user_response?: string;
  detected_at: string;
  resolved_at?: string;
  resolved_in_session?: number;
}

/**
 * Payload from Rust for finding_resolved event
 */
interface FindingResolvedPayload {
  id: string;
  task_run_id: string;
  resolution?: string;
}

/**
 * Convert Rust payload to TypeScript Finding
 */
function payloadToFinding(payload: FindingDetectedPayload): Finding {
  // Note: Rust payload has column_number but frontend CodeContext doesn't use it
  const codeContext: CodeContext | undefined =
    payload.file_path || payload.line_number
      ? {
          file: payload.file_path,
          line: payload.line_number,
          snippet: payload.code_snippet,
        }
      : undefined;

  const actionType = payload.action_type as ActionType;
  const status = payload.status as FindingStatus;
  const severity = payload.severity as FindingSeverity;

  const finding: Finding = {
    id: payload.id,
    categoryId: payload.category,
    severity,
    status,
    title: payload.title,
    description: payload.description,
    sourceSessionId: payload.task_run_id,
    detectedAt: new Date(payload.detected_at).getTime(),
    actionType,
    actionable: actionType !== "informational",
    codeContext,
    resolution: payload.resolution,
    userResponse: payload.user_response,
  };

  // Create pending question if needs_input
  if (payload.needs_input && payload.question) {
    finding.pendingQuestion = {
      id: `q-${payload.id}`,
      findingId: payload.id,
      question: payload.question,
      inputType: payload.input_options ? "choice" : "text",
      options: payload.input_options?.map((opt) => ({
        value: opt,
        label: opt,
      })),
      required: true,
    };
  }

  if (payload.resolved_at) {
    finding.resolvedAt = new Date(payload.resolved_at).getTime();
  }

  return finding;
}

/**
 * TauriFindingsListener class
 *
 * Manages Tauri event subscriptions for findings.
 */
class TauriFindingsListener {
  private static instance: TauriFindingsListener | null = null;
  private unlistenFunctions: UnlistenFn[] = [];
  private isListening = false;

  private constructor() {}

  static getInstance(): TauriFindingsListener {
    if (!TauriFindingsListener.instance) {
      TauriFindingsListener.instance = new TauriFindingsListener();
    }
    return TauriFindingsListener.instance;
  }

  /**
   * Start listening to Tauri events
   */
  async start(): Promise<void> {
    if (this.isListening) {
      console.log("[TauriFindingsListener] Already listening");
      return;
    }

    console.log("[TauriFindingsListener] Starting event listeners");

    try {
      // Listen for finding_detected events
      const unlistenDetected = await listen<FindingDetectedPayload>("finding_detected", (event) => {
        console.log(
          "[TauriFindingsListener] Received finding_detected event:",
          event.payload.title,
        );
        const finding = payloadToFinding(event.payload);

        // Add to FindingsTracker (if not already present)
        // The tracker will handle deduplication
        if (!findingsTracker.getFinding(finding.id)) {
          // Use internal method to add without re-parsing
          this.addFindingToTracker(finding);
        }
      });
      this.unlistenFunctions.push(unlistenDetected);

      // Listen for finding_resolved events
      const unlistenResolved = await listen<FindingResolvedPayload>("finding_resolved", (event) => {
        console.log("[TauriFindingsListener] Received finding_resolved event:", event.payload.id);
        const { id, resolution } = event.payload;

        // Update in FindingsTracker
        if (resolution) {
          findingsTracker.resolveFinding(id, resolution);
        } else {
          findingsTracker.updateFindingStatus(id, "resolved");
        }
      });
      this.unlistenFunctions.push(unlistenResolved);

      this.isListening = true;
      console.log("[TauriFindingsListener] Event listeners started");
    } catch (error) {
      console.error("[TauriFindingsListener] Failed to start listeners:", error);
    }
  }

  /**
   * Stop listening to Tauri events
   */
  async stop(): Promise<void> {
    console.log("[TauriFindingsListener] Stopping event listeners");

    for (const unlisten of this.unlistenFunctions) {
      unlisten();
    }
    this.unlistenFunctions = [];
    this.isListening = false;

    console.log("[TauriFindingsListener] Event listeners stopped");
  }

  /**
   * Add a finding directly to the tracker without going through processLine
   */
  private addFindingToTracker(finding: Finding): void {
    // We use createFinding to add it to the tracker's internal state
    // But since we already have a fully-formed Finding, we'll use a workaround
    // The tracker will emit events and update reports

    // Use the tracker's internal method if available, otherwise use createFinding
    // For now, we'll directly call the public methods
    const params = {
      categoryId: finding.categoryId,
      severity: finding.severity,
      title: finding.title,
      description: finding.description,
      codeContext: finding.codeContext,
    };

    // Create via public API (this will generate a new ID, which is fine)
    // The Rust-side ID is authoritative and stored in SQLite
    // The frontend ID is for local state management only
    findingsTracker.createFinding(params);
  }
}

// Export singleton instance
export const tauriFindingsListener = TauriFindingsListener.getInstance();
