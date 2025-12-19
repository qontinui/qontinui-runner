/**
 * VerificationService
 *
 * Manages AI self-healing verification workflow. Handles saving, loading,
 * and triggering verification after runner restarts.
 */

import { invoke } from "@tauri-apps/api/core";
import type {
  PendingVerification,
  VerificationPendingMarker,
  VerificationCompletedMarker,
  VerificationFailedMarker,
  RunnerRestartMarker,
} from "../types/verification";

// Re-export types for convenience
export type {
  PendingVerification,
  VerificationPendingMarker,
  VerificationCompletedMarker,
  VerificationFailedMarker,
  RunnerRestartMarker,
};

type VerificationEventType =
  | "verification_pending"
  | "verification_started"
  | "verification_completed"
  | "verification_failed"
  | "restart_requested";

interface VerificationEvent {
  type: VerificationEventType;
  data?: PendingVerification | VerificationCompletedMarker | VerificationFailedMarker;
}

type VerificationListener = (event: VerificationEvent) => void;

/**
 * Service for managing AI verification state across restarts
 */
class VerificationService {
  private listeners = new Set<VerificationListener>();
  private currentVerification: PendingVerification | null = null;

  /**
   * Save a pending verification to disk
   */
  async savePendingVerification(marker: VerificationPendingMarker): Promise<boolean> {
    const verification: PendingVerification = {
      id: `verify-${Date.now()}-${Math.random().toString(36).substring(2, 9)}`,
      created_at: Date.now(),
      reason: marker.reason,
      conversation_summary: marker.conversation_summary || "",
      issues_fixed: marker.issues_fixed || [],
      files_modified: marker.files_modified,
      verification_prompt: marker.verification_prompt,
      status: "pending",
    };

    try {
      const result = await invoke<{ success: boolean; message?: string }>(
        "save_pending_verification",
        { verification }
      );

      if (result.success) {
        this.currentVerification = verification;
        this.emit({ type: "verification_pending", data: verification });
        console.log("[VerificationService] Saved pending verification:", verification.id);
        return true;
      } else {
        console.error("[VerificationService] Failed to save:", result.message);
        return false;
      }
    } catch (error) {
      console.error("[VerificationService] Error saving verification:", error);
      return false;
    }
  }

  /**
   * Load pending verification from disk (called on startup)
   */
  async loadPendingVerification(): Promise<PendingVerification | null> {
    try {
      const result = await invoke<{
        success: boolean;
        message?: string;
        data?: { verification: PendingVerification | null };
      }>("load_pending_verification");

      if (result.success && result.data?.verification) {
        this.currentVerification = result.data.verification;
        console.log(
          "[VerificationService] Loaded pending verification:",
          this.currentVerification.id,
          "status:",
          this.currentVerification.status
        );
        return this.currentVerification;
      }

      console.log("[VerificationService] No pending verification found");
      return null;
    } catch (error) {
      console.error("[VerificationService] Error loading verification:", error);
      return null;
    }
  }

  /**
   * Clear the pending verification
   */
  async clearPendingVerification(): Promise<boolean> {
    try {
      const result = await invoke<{ success: boolean }>("clear_pending_verification");
      if (result.success) {
        this.currentVerification = null;
        console.log("[VerificationService] Cleared pending verification");
        return true;
      }
      return false;
    } catch (error) {
      console.error("[VerificationService] Error clearing verification:", error);
      return false;
    }
  }

  /**
   * Update the status of the pending verification
   */
  async updateStatus(
    status: "pending" | "in_progress" | "completed" | "failed"
  ): Promise<boolean> {
    try {
      const result = await invoke<{ success: boolean }>("update_verification_status", {
        status,
      });

      if (result.success && this.currentVerification) {
        this.currentVerification.status = status;
        console.log("[VerificationService] Updated status to:", status);
        return true;
      }
      return false;
    } catch (error) {
      console.error("[VerificationService] Error updating status:", error);
      return false;
    }
  }

  /**
   * Trigger verification by sending prompt to AI
   * Returns the prompt that should be sent
   */
  async triggerVerification(): Promise<string | null> {
    if (!this.currentVerification) {
      console.warn("[VerificationService] No pending verification to trigger");
      return null;
    }

    if (this.currentVerification.status !== "pending") {
      console.warn(
        "[VerificationService] Verification already in progress or completed:",
        this.currentVerification.status
      );
      return null;
    }

    await this.updateStatus("in_progress");
    this.emit({ type: "verification_started", data: this.currentVerification });

    // Build the verification prompt with context
    const contextPrompt = this.buildVerificationPrompt();
    console.log("[VerificationService] Triggering verification with prompt");

    return contextPrompt;
  }

  /**
   * Build the verification prompt with context about what was fixed
   */
  private buildVerificationPrompt(): string {
    if (!this.currentVerification) return "";

    const { files_modified, issues_fixed, verification_prompt, conversation_summary } =
      this.currentVerification;

    let prompt = "## Verification Request\n\n";
    prompt +=
      "The runner was restarted after applying fixes. Please verify the changes worked.\n\n";

    if (conversation_summary) {
      prompt += `### Previous Context\n${conversation_summary}\n\n`;
    }

    if (files_modified.length > 0) {
      prompt += `### Files Modified\n`;
      files_modified.forEach((f) => (prompt += `- ${f}\n`));
      prompt += "\n";
    }

    if (issues_fixed.length > 0) {
      prompt += `### Issues Addressed\n`;
      issues_fixed.forEach((i) => (prompt += `- ${i}\n`));
      prompt += "\n";
    }

    prompt += `### Verification Task\n${verification_prompt}\n\n`;
    prompt += `When done, output one of:\n`;
    prompt += `- [VERIFICATION:COMPLETED] {"success":true,"message":"<what worked>"}\n`;
    prompt += `- [VERIFICATION:FAILED] {"success":false,"message":"<what failed>","next_steps":"<suggestion>"}\n`;

    return prompt;
  }

  /**
   * Handle verification completed
   */
  async handleVerificationCompleted(marker: VerificationCompletedMarker): Promise<void> {
    console.log("[VerificationService] Verification completed:", marker.message);
    await this.updateStatus("completed");
    this.emit({ type: "verification_completed", data: marker });
    await this.clearPendingVerification();
  }

  /**
   * Handle verification failed
   */
  async handleVerificationFailed(marker: VerificationFailedMarker): Promise<void> {
    console.log("[VerificationService] Verification failed:", marker.message);
    await this.updateStatus("failed");
    this.emit({ type: "verification_failed", data: marker });
    // Don't clear - keep for retry or debugging
  }

  /**
   * Get current verification state
   */
  getCurrentVerification(): PendingVerification | null {
    return this.currentVerification;
  }

  /**
   * Check if there's a pending verification that needs to run
   */
  hasPendingVerification(): boolean {
    return this.currentVerification?.status === "pending";
  }

  /**
   * Subscribe to verification events
   */
  subscribe(listener: VerificationListener): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  /**
   * Emit event to all listeners
   */
  private emit(event: VerificationEvent): void {
    this.listeners.forEach((listener) => listener(event));
  }

  /**
   * Trigger a runner restart
   */
  async triggerRestart(marker: RunnerRestartMarker): Promise<boolean> {
    console.log("[VerificationService] Triggering runner restart:", marker.reason);
    this.emit({ type: "restart_requested" });

    try {
      const response = await fetch("http://localhost:9876/restart-runner", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          reason: marker.reason,
          delay_seconds: marker.delay_seconds || 3,
        }),
      });

      const result = await response.json();
      return result.success === true;
    } catch (error) {
      console.error("[VerificationService] Failed to trigger restart:", error);
      return false;
    }
  }
}

// Export singleton instance
export const verificationService = new VerificationService();
