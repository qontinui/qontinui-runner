import { invoke } from "@tauri-apps/api/core";
import { createLogger } from "@/lib/logger";

const logger = createLogger("VideoRecordingService");

export interface VideoRecordingConfig {
  enabled: boolean;
  startDelaySeconds: number; // 0-60 seconds
  maxDurationSeconds: number; // 0 = unlimited, default 300 (5 min)
  fps: number; // 15-30 fps
  quality: "low" | "medium" | "high";
  codec: string; // 'h264' default
}

export interface VideoRecordingStatus {
  recording: boolean;
  duration: number; // seconds
  path: string;
}

export class VideoRecordingService {
  private static instance: VideoRecordingService;
  private currentSessionId: string | null = null;

  private constructor() {}

  /**
   * Get singleton instance of VideoRecordingService
   */
  public static getInstance(): VideoRecordingService {
    if (!VideoRecordingService.instance) {
      VideoRecordingService.instance = new VideoRecordingService();
    }
    return VideoRecordingService.instance;
  }

  /**
   * Start recording after configured delay
   * @param sessionId Unique identifier for this recording session
   * @param config Recording configuration
   */
  public async startRecording(sessionId: string, config: VideoRecordingConfig): Promise<void> {
    try {
      // Validate configuration
      if (config.startDelaySeconds < 0 || config.startDelaySeconds > 60) {
        throw new Error("Start delay must be between 0 and 60 seconds");
      }

      if (config.fps < 15 || config.fps > 30) {
        throw new Error("FPS must be between 15 and 30");
      }

      if (!["low", "medium", "high"].includes(config.quality)) {
        throw new Error("Quality must be 'low', 'medium', or 'high'");
      }

      this.currentSessionId = sessionId;

      const result = await invoke<{ success: boolean; message?: string }>("start_video_recording", {
        sessionId,
        config,
      });

      if (!result.success) {
        throw new Error(result.message || "Failed to start recording");
      }

      logger.info(
        `Video recording started for session: ${sessionId} with delay: ${config.startDelaySeconds}s`,
      );
    } catch (error) {
      console.error("Failed to start video recording:", error);
      throw error;
    }
  }

  /**
   * Stop the current recording
   * @returns Path to the recorded video file
   */
  public async stopRecording(): Promise<string> {
    try {
      const result = await invoke<{
        success: boolean;
        message?: string;
        data?: { path: string };
      }>("stop_video_recording");

      if (!result.success || !result.data?.path) {
        throw new Error(result.message || "Failed to stop recording");
      }

      logger.info(`Video recording stopped. Saved to: ${result.data.path}`);
      this.currentSessionId = null;
      return result.data.path;
    } catch (error) {
      console.error("Failed to stop video recording:", error);
      throw error;
    }
  }

  /**
   * Get current recording status
   * @returns Recording status information
   */
  public async getStatus(): Promise<VideoRecordingStatus> {
    try {
      const result = await invoke<{
        success: boolean;
        data?: VideoRecordingStatus;
      }>("get_video_recording_status");

      if (!result.success || !result.data) {
        throw new Error("Failed to get recording status");
      }

      return result.data;
    } catch (error) {
      console.error("Failed to get recording status:", error);
      throw error;
    }
  }

  /**
   * Check if currently recording
   * @returns True if recording is in progress
   */
  public async isRecording(): Promise<boolean> {
    try {
      const status = await this.getStatus();
      return status.recording;
    } catch (error) {
      console.error("Failed to check recording status:", error);
      return false;
    }
  }

  /**
   * Get the current session ID
   */
  public getCurrentSessionId(): string | null {
    return this.currentSessionId;
  }

  /**
   * Create a default configuration
   */
  public static createDefaultConfig(): VideoRecordingConfig {
    return {
      enabled: false,
      startDelaySeconds: 0,
      maxDurationSeconds: 300, // 5 minutes
      fps: 15,
      quality: "medium",
      codec: "h264",
    };
  }
}

// Export singleton instance
export const videoRecorder = VideoRecordingService.getInstance();
