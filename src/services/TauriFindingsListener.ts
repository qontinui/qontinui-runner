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
import { invoke } from "@tauri-apps/api/core";
import { createLogger } from "@/lib/logger";
import type {
  Finding,
  FindingSeverity,
  FindingStatus,
  ActionType,
  CodeContext,
} from "../types/findings";
import { findingsTracker } from "./FindingsTracker";

/**
 * Payload from Rust for finding_detected / finding_resolved events.
 *
 * Mirrors `qontinui-runner/src-tauri/src/findings/types.rs::Finding`, which
 * uses `#[serde(rename_all = "camelCase")]` (with `category` explicitly
 * renamed to `categoryId`). Tauri's wire format ships these keys as
 * camelCase regardless of the Rust field names, so reading snake_case
 * (e.g. `event.payload.task_run_id`) silently evaluates to `undefined`.
 * See memory `proj_tauri_event_payload_camelcase.md` and the sibling fix
 * in commit ed1b45d56 (TerminalInstance.tsx).
 *
 * `codeContext` and `userInput` are nested objects on the Rust struct, not
 * flattened — there is no `file_path` / `needs_input` / etc. on the wire.
 */
interface FindingCodeContextWire {
  file?: string;
  line?: number;
  column?: number;
  snippet?: string;
}

interface FindingUserInputWire {
  question: string;
  input_type: string;
  options?: string[];
}

interface FindingDetectedPayload {
  id: string;
  taskRunId: string;
  sessionNum: number;
  /** Renamed from `category` via `#[serde(rename = "categoryId")]`. */
  categoryId: string;
  severity: string;
  status: string;
  actionType: string;
  title: string;
  description: string;
  resolution?: string;
  codeContext?: FindingCodeContextWire;
  signatureHash: string;
  userInput?: FindingUserInputWire;
  userResponse?: string;
  detectedAt: string;
  resolvedAt?: string;
  resolvedInSession?: number;
  updatedAt: string;
}

/**
 * Payload from Rust for finding_resolved event. Same wire shape as
 * `finding_detected` (the Rust emit hands the entire `Finding` struct to
 * both event channels), but the consumer only needs id / resolution.
 */
type FindingResolvedPayload = FindingDetectedPayload;

/**
 * Payload emitted by the dev-only `dev_seed_finding` Tauri command.
 * Uses camelCase fields so they can be mapped directly onto a `Finding`.
 */
interface DevSeedFindingPayload {
  id: string;
  categoryId: string;
  severity: string;
  status: string;
  title: string;
  description: string;
  detectedAt: number;
  actionType: string;
  actionable: boolean;
  sourceSessionId?: string;
}

/**
 * Convert a dev-seed payload into a `Finding` shape.
 * The tracker's `createFinding` is used for registration; this helper just
 * normalizes the shape so downstream consumers see the same structure as
 * real findings.
 */
function seedPayloadToFinding(payload: DevSeedFindingPayload): Finding {
  return {
    id: payload.id,
    categoryId: payload.categoryId,
    severity: payload.severity as FindingSeverity,
    status: payload.status as FindingStatus,
    title: payload.title,
    description: payload.description,
    sourceSessionId: payload.sourceSessionId,
    detectedAt: payload.detectedAt,
    actionType: payload.actionType as ActionType,
    actionable: payload.actionable,
  };
}

/**
 * Convert Rust payload to TypeScript Finding.
 *
 * Rust ships `codeContext` and `userInput` as nested objects (not flattened
 * snake_case fields) — see the Wire shape comment on `FindingDetectedPayload`.
 */
function payloadToFinding(payload: FindingDetectedPayload): Finding {
  // Note: Rust payload exposes column on codeContext but frontend CodeContext doesn't use it
  const codeContext: CodeContext | undefined =
    payload.codeContext && (payload.codeContext.file || payload.codeContext.line)
      ? {
          file: payload.codeContext.file,
          line: payload.codeContext.line,
          snippet: payload.codeContext.snippet,
        }
      : undefined;

  const actionType = payload.actionType as ActionType;
  const status = payload.status as FindingStatus;
  const severity = payload.severity as FindingSeverity;

  const finding: Finding = {
    id: payload.id,
    categoryId: payload.categoryId,
    severity,
    status,
    title: payload.title,
    description: payload.description,
    sourceSessionId: payload.taskRunId,
    detectedAt: new Date(payload.detectedAt).getTime(),
    actionType,
    actionable: actionType !== "informational",
    codeContext,
    resolution: payload.resolution,
    userResponse: payload.userResponse,
  };

  // Create pending question if user_input is present (the Rust `Finding`
  // sets `user_input: Some(_)` whenever a question is required; absence
  // means no input requested).
  if (payload.userInput && payload.userInput.question) {
    finding.pendingQuestion = {
      id: `q-${payload.id}`,
      findingId: payload.id,
      question: payload.userInput.question,
      inputType: payload.userInput.options ? "choice" : "text",
      options: payload.userInput.options?.map((opt) => ({
        value: opt,
        label: opt,
      })),
      required: true,
    };
  }

  if (payload.resolvedAt) {
    finding.resolvedAt = new Date(payload.resolvedAt).getTime();
  }

  return finding;
}

/**
 * TauriFindingsListener class
 *
 * Manages Tauri event subscriptions for findings.
 */
const logger = createLogger("TauriFindingsListener");

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
      logger.debug("Already listening");
      return;
    }

    logger.info("Starting event listeners");

    try {
      // Listen for finding_detected events
      const unlistenDetected = await listen<FindingDetectedPayload>("finding_detected", (event) => {
        logger.debug("Received finding_detected event:", event.payload.title);
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
        logger.debug("Received finding_resolved event:", event.payload.id);
        const { id, resolution } = event.payload;

        // Update in FindingsTracker
        if (resolution) {
          findingsTracker.resolveFinding(id, resolution);
        } else {
          findingsTracker.updateFindingStatus(id, "resolved");
        }
      });
      this.unlistenFunctions.push(unlistenResolved);

      // Dev-only: listen for dev:seed-finding events when the backend is running
      // with QONTINUI_DEV_ENDPOINTS=1. The env gate is read on the Rust side
      // via the is_dev_endpoints_enabled command, so the frontend doesn't have
      // direct access to the env var itself.
      let devEnabled = false;
      try {
        devEnabled = await invoke<boolean>("is_dev_endpoints_enabled");
      } catch (error) {
        logger.debug("is_dev_endpoints_enabled not available; skipping dev listener", error);
      }

      if (devEnabled) {
        const unlistenSeed = await listen<DevSeedFindingPayload>("dev:seed-finding", (event) => {
          logger.info("Received dev:seed-finding event:", event.payload.id);
          const finding = seedPayloadToFinding(event.payload);
          findingsTracker.createFinding({
            categoryId: finding.categoryId,
            severity: finding.severity,
            title: finding.title,
            description: finding.description,
            codeContext: finding.codeContext,
          });
        });
        this.unlistenFunctions.push(unlistenSeed);
        logger.info("Dev findings seed listener registered (QONTINUI_DEV_ENDPOINTS=1)");
      }

      this.isListening = true;
      logger.info("Event listeners started");
    } catch (error) {
      console.error("[TauriFindingsListener] Failed to start listeners:", error);
    }
  }

  /**
   * Stop listening to Tauri events
   */
  async stop(): Promise<void> {
    logger.info("Stopping event listeners");

    for (const unlisten of this.unlistenFunctions) {
      unlisten();
    }
    this.unlistenFunctions = [];
    this.isListening = false;

    logger.info("Event listeners stopped");
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
