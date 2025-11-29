import { invoke } from "@tauri-apps/api/core";

export interface StorageConfig {
  screenshotPath: string; // ~/qontinui/screenshots/
  videoPath: string; // ~/qontinui/videos/
  maxScreenshotMB: number; // 500 MB default
  maxVideoMB: number; // 500 MB default
  autoCleanup: boolean; // Delete oldest when limit reached
}

export interface StorageUsage {
  screenshots: number; // in MB
  videos: number; // in MB
  screenshotCount: number;
  videoCount: number;
}

export interface CommandResponse {
  success: boolean;
  message?: string;
  data?: any;
}

export class LocalStorageService {
  private config: StorageConfig;

  constructor(config?: Partial<StorageConfig>) {
    this.config = {
      screenshotPath: "~/qontinui/screenshots/",
      videoPath: "~/qontinui/videos/",
      maxScreenshotMB: 500,
      maxVideoMB: 500,
      autoCleanup: true,
      ...config,
    };
  }

  /**
   * Save a screenshot to disk
   * @param sessionId The session identifier
   * @param screenshotId The screenshot identifier
   * @param data The image data as a base64 string or Uint8Array
   * @returns The path where the screenshot was saved
   */
  async saveScreenshot(
    sessionId: string,
    screenshotId: string,
    data: string | Uint8Array,
  ): Promise<string> {
    try {
      const result: CommandResponse = await invoke("save_screenshot_to_disk", {
        sessionId,
        screenshotId,
        data: typeof data === "string" ? data : Array.from(data),
      });

      if (!result.success) {
        throw new Error(result.message || "Failed to save screenshot");
      }

      return result.data?.path || "";
    } catch (error) {
      console.error("Failed to save screenshot:", error);
      throw error;
    }
  }

  /**
   * Save a video to disk
   * @param sessionId The session identifier
   * @param data The video data as a Uint8Array
   * @returns The path where the video was saved
   */
  async saveVideo(sessionId: string, data: Uint8Array): Promise<string> {
    try {
      const result: CommandResponse = await invoke("save_video_to_disk", {
        sessionId,
        data: Array.from(data),
      });

      if (!result.success) {
        throw new Error(result.message || "Failed to save video");
      }

      return result.data?.path || "";
    } catch (error) {
      console.error("Failed to save video:", error);
      throw error;
    }
  }

  /**
   * Get current storage usage
   * @returns Storage usage in MB for screenshots and videos
   */
  async getStorageUsage(): Promise<StorageUsage> {
    try {
      const result: CommandResponse = await invoke("get_local_storage_usage");

      if (!result.success) {
        throw new Error(result.message || "Failed to get storage usage");
      }

      return {
        screenshots: result.data?.screenshots_mb || 0,
        videos: result.data?.videos_mb || 0,
        screenshotCount: result.data?.screenshot_count || 0,
        videoCount: result.data?.video_count || 0,
      };
    } catch (error) {
      console.error("Failed to get storage usage:", error);
      throw error;
    }
  }

  /**
   * Delete old sessions
   * @param type The type of storage to clean ('screenshots' or 'videos')
   * @param olderThanDays Delete sessions older than this many days
   */
  async deleteOldSessions(type: "screenshots" | "videos", olderThanDays: number): Promise<void> {
    try {
      const result: CommandResponse = await invoke("delete_old_sessions", {
        storageType: type,
        olderThanDays,
      });

      if (!result.success) {
        throw new Error(result.message || "Failed to delete old sessions");
      }
    } catch (error) {
      console.error("Failed to delete old sessions:", error);
      throw error;
    }
  }

  /**
   * Check if there's enough space available
   * @param type The type of storage to check
   * @param sizeBytes The size in bytes to check
   * @returns True if space is available
   */
  async hasSpaceAvailable(type: "screenshots" | "videos", sizeBytes: number): Promise<boolean> {
    try {
      const usage = await this.getStorageUsage();
      const sizeMB = sizeBytes / (1024 * 1024);

      if (type === "screenshots") {
        return usage.screenshots + sizeMB <= this.config.maxScreenshotMB;
      } else {
        return usage.videos + sizeMB <= this.config.maxVideoMB;
      }
    } catch (error) {
      console.error("Failed to check space availability:", error);
      return false;
    }
  }

  /**
   * Get the current storage configuration
   */
  getConfig(): StorageConfig {
    return { ...this.config };
  }

  /**
   * Update the storage configuration
   */
  updateConfig(config: Partial<StorageConfig>): void {
    this.config = { ...this.config, ...config };
  }

  /**
   * Clear all storage (screenshots and videos)
   * WARNING: This will delete all stored data
   */
  async clearAllStorage(): Promise<void> {
    try {
      const result: CommandResponse = await invoke("clear_all_storage");

      if (!result.success) {
        throw new Error(result.message || "Failed to clear storage");
      }
    } catch (error) {
      console.error("Failed to clear all storage:", error);
      throw error;
    }
  }
}

// Export a singleton instance
export const localStorage = new LocalStorageService();
