import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { HardDrive, FolderOpen, Trash2, Trash, Loader2 } from "lucide-react";
import { SectionHeader } from "./SectionHeader";
import { getAccentColors } from "@/design-system";
import type { StorageUsage, StoragePaths, LogFunction } from "./types";

interface TauriResult<T> {
  success: boolean;
  data?: T;
  message?: string;
}

interface StorageUsageData {
  screenshots_mb: number;
  videos_mb: number;
  screenshot_count: number;
  video_count: number;
}

interface StoragePathsData {
  screenshot_path: string;
  video_path: string;
  max_screenshot_mb: number;
  max_video_mb: number;
  auto_cleanup: boolean;
}

interface StorageSettingsProps {
  onLog: LogFunction;
}

export function StorageSettings({ onLog }: StorageSettingsProps) {
  const [storageUsage, setStorageUsage] = useState<StorageUsage>({
    screenshots: 0,
    videos: 0,
    screenshotCount: 0,
    videoCount: 0,
  });
  const [storagePaths, setStoragePaths] = useState<StoragePaths>({
    screenshot_path: "",
    video_path: "",
    max_screenshot_mb: 500,
    max_video_mb: 500,
    auto_cleanup: true,
  });
  const [storageLoading, setStorageLoading] = useState(false);

  const loadStorageInfo = async () => {
    try {
      setStorageLoading(true);

      const usageResult = await invoke<TauriResult<StorageUsageData>>("get_local_storage_usage");
      if (usageResult && usageResult.success && usageResult.data) {
        setStorageUsage({
          screenshots: usageResult.data.screenshots_mb || 0,
          videos: usageResult.data.videos_mb || 0,
          screenshotCount: usageResult.data.screenshot_count || 0,
          videoCount: usageResult.data.video_count || 0,
        });
      }

      const pathsResult = await invoke<TauriResult<StoragePathsData>>("get_storage_paths");
      if (pathsResult && pathsResult.success && pathsResult.data) {
        setStoragePaths({
          screenshot_path: pathsResult.data.screenshot_path || "",
          video_path: pathsResult.data.video_path || "",
          max_screenshot_mb: pathsResult.data.max_screenshot_mb || 500,
          max_video_mb: pathsResult.data.max_video_mb || 500,
          auto_cleanup: pathsResult.data.auto_cleanup ?? true,
        });
      }
    } catch (err) {
      console.error("Failed to load storage info:", err);
    } finally {
      setStorageLoading(false);
    }
  };

  useEffect(() => {
    loadStorageInfo();
  }, []);

  const handleDeleteOldSessions = async (type: "screenshots" | "videos", days: number) => {
    try {
      const result = await invoke<TauriResult<null>>("delete_old_sessions", {
        storageType: type,
        olderThanDays: days,
      });

      if (result && result.success) {
        onLog("success", `Deleted ${type} older than ${days} days`);
        await loadStorageInfo();
      } else {
        onLog("error", `Failed to delete old sessions: ${result?.message || "Unknown error"}`);
      }
    } catch (err) {
      console.error("Failed to delete old sessions:", err);
      onLog("error", `Failed to delete old sessions: ${err}`);
    }
  };

  const handleClearAllStorage = async () => {
    if (
      !confirm("Are you sure you want to delete all screenshots and videos? This cannot be undone.")
    ) {
      return;
    }

    try {
      const result = await invoke<TauriResult<null>>("clear_all_storage");

      if (result && result.success) {
        onLog("success", "All storage cleared successfully");
        await loadStorageInfo();
      } else {
        onLog("error", `Failed to clear storage: ${result?.message || "Unknown error"}`);
      }
    } catch (err) {
      console.error("Failed to clear storage:", err);
      onLog("error", `Failed to clear storage: ${err}`);
    }
  };

  const handleOpenStorageFolder = async (type: "screenshots" | "videos") => {
    try {
      const path = type === "screenshots" ? storagePaths.screenshot_path : storagePaths.video_path;
      await invoke("open_folder", { path });
    } catch (err) {
      console.error("Failed to open folder:", err);
      onLog("error", `Failed to open folder: ${err}`);
    }
  };

  return (
    <div className="space-y-6">
      <SectionHeader
        title="Storage"
        description="Manage local storage for screenshots and videos captured during automation sessions. Monitor usage and clean up old files."
        icon={<HardDrive className="w-6 h-6" />}
      />

      <div className="space-y-4 rounded-lg bg-card/50 p-4">
        <div className="flex items-center gap-2">
          <HardDrive className="w-4 h-4 text-primary" />
          <h4 className="font-medium text-sm">Local Storage</h4>
          {storageLoading && <Loader2 className="w-3.5 h-3.5 animate-spin text-muted-foreground" />}
        </div>

        <div className="space-y-4">
          <div>
            <div
              data-content-role="heading"
              data-content-label="storage usage heading"
              className="text-xs font-medium mb-2"
            >
              Storage Usage
            </div>

            {/* Screenshots */}
            <div className="space-y-2 mb-4">
              <div className="flex items-center justify-between text-sm">
                <span
                  data-content-role="label"
                  data-content-label="screenshots label"
                  className="text-muted-foreground"
                >
                  Screenshots
                </span>
                <span
                  data-content-role="metric"
                  data-content-label="screenshots usage"
                  className="font-medium"
                >
                  {storageUsage.screenshots.toFixed(2)} MB / {storagePaths.max_screenshot_mb} MB (
                  {storageUsage.screenshotCount} files)
                </span>
              </div>
              <div className="w-full bg-muted rounded-full h-2">
                <div
                  className="bg-primary h-2 rounded-full transition-all"
                  style={{
                    width: `${Math.min(
                      100,
                      (storageUsage.screenshots / storagePaths.max_screenshot_mb) * 100,
                    )}%`,
                  }}
                ></div>
              </div>
            </div>

            {/* Videos */}
            <div className="space-y-2">
              <div className="flex items-center justify-between text-sm">
                <span
                  data-content-role="label"
                  data-content-label="videos label"
                  className="text-muted-foreground"
                >
                  Videos
                </span>
                <span
                  data-content-role="metric"
                  data-content-label="videos usage"
                  className="font-medium"
                >
                  {storageUsage.videos.toFixed(2)} MB / {storagePaths.max_video_mb} MB (
                  {storageUsage.videoCount} files)
                </span>
              </div>
              <div className="w-full bg-muted rounded-full h-2">
                <div
                  className="bg-primary h-2 rounded-full transition-all"
                  style={{
                    width: `${Math.min(
                      100,
                      (storageUsage.videos / storagePaths.max_video_mb) * 100,
                    )}%`,
                  }}
                ></div>
              </div>
            </div>
          </div>

          {/* Storage Paths */}
          <div className="space-y-2">
            <div
              data-content-role="heading"
              data-content-label="storage locations heading"
              className="text-xs font-medium"
            >
              Storage Locations
            </div>
            <div className="space-y-2 text-xs">
              <div className="flex items-center justify-between">
                <span className="text-muted-foreground">Screenshots:</span>
                <div className="flex items-center gap-2">
                  <code className="px-2 py-1 bg-muted rounded text-xs max-w-md truncate">
                    {storagePaths.screenshot_path || "~/qontinui/screenshots/"}
                  </code>
                  <button
                    onClick={() => handleOpenStorageFolder("screenshots")}
                    className="px-2 py-1 bg-primary/10 hover:bg-primary/20 text-primary rounded-md transition-colors"
                    title="Open folder"
                  >
                    <FolderOpen className="w-3 h-3" />
                  </button>
                </div>
              </div>
              <div className="flex items-center justify-between">
                <span className="text-muted-foreground">Videos:</span>
                <div className="flex items-center gap-2">
                  <code className="px-2 py-1 bg-muted rounded text-xs max-w-md truncate">
                    {storagePaths.video_path || "~/qontinui/videos/"}
                  </code>
                  <button
                    onClick={() => handleOpenStorageFolder("videos")}
                    className="px-2 py-1 bg-primary/10 hover:bg-primary/20 text-primary rounded-md transition-colors"
                    title="Open folder"
                  >
                    <FolderOpen className="w-3 h-3" />
                  </button>
                </div>
              </div>
            </div>
          </div>

          {/* Cleanup Actions */}
          <div className="space-y-2">
            <div
              data-content-role="heading"
              data-content-label="storage cleanup heading"
              className="text-xs font-medium"
            >
              Storage Cleanup
            </div>
            <div className="flex flex-wrap gap-2">
              <button
                onClick={() => handleDeleteOldSessions("screenshots", 30)}
                className={`px-2.5 py-1.5 ${getAccentColors("amber").bg} hover:bg-amber-500/20 ${getAccentColors("amber").text} rounded-md transition-colors flex items-center gap-1.5 text-xs`}
              >
                <Trash2 className="w-3.5 h-3.5" />
                Delete Screenshots (30+ days)
              </button>
              <button
                onClick={() => handleDeleteOldSessions("videos", 30)}
                className={`px-2.5 py-1.5 ${getAccentColors("amber").bg} hover:bg-amber-500/20 ${getAccentColors("amber").text} rounded-md transition-colors flex items-center gap-1.5 text-xs`}
              >
                <Trash2 className="w-3.5 h-3.5" />
                Delete Videos (30+ days)
              </button>
              <button
                onClick={handleClearAllStorage}
                className={`px-2.5 py-1.5 ${getAccentColors("red").bg} hover:bg-red-500/20 ${getAccentColors("red").text} rounded-md transition-colors flex items-center gap-1.5 text-xs`}
              >
                <Trash className="w-3.5 h-3.5" />
                Clear All Storage
              </button>
            </div>
          </div>

          <div className="p-3 bg-primary/5 rounded-lg">
            <div className="text-xs text-muted-foreground">
              <strong className="text-foreground">Storage Info:</strong> Screenshots and videos are
              organized by session. Auto-cleanup removes oldest sessions when storage limits are
              reached. Files are stored locally on your machine at the paths shown above.
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}
