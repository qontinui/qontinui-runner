import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Download, RefreshCw, CheckCircle, AlertCircle, Info } from "lucide-react";
import { SectionHeader } from "./SectionHeader";
import { getStatusColors } from "@/design-system";
import type { LogFunction, UpdateInfo, UpdateStatus } from "./types";

interface UpdateSettingsProps {
  onLog: LogFunction;
}

export function UpdateSettings({ onLog }: UpdateSettingsProps) {
  const [updateInfo, setUpdateInfo] = useState<UpdateInfo | null>(null);
  const [status, setStatus] = useState<UpdateStatus>("idle");
  const [error, setError] = useState<string | null>(null);
  const [lastChecked, setLastChecked] = useState<Date | null>(null);

  useEffect(() => {
    checkForUpdates();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const checkForUpdates = async () => {
    setStatus("checking");
    setError(null);

    try {
      const result = (await invoke("check_for_updates")) as {
        success: boolean;
        message?: string;
        data?: UpdateInfo;
      };

      if (result.success && result.data) {
        setUpdateInfo(result.data);
        setLastChecked(new Date());

        if (result.data.available) {
          onLog("info", `Update available: v${result.data.version}`);
        } else if (result.data.development) {
          onLog("info", "Update checking disabled in development mode");
        } else {
          onLog("success", "You're running the latest version");
        }
      }
      setStatus("idle");
    } catch (err) {
      const errorMessage = err instanceof Error ? err.message : String(err);
      setError(errorMessage);
      setStatus("error");
      onLog("error", `Failed to check for updates: ${errorMessage}`);
    }
  };

  const installUpdate = async () => {
    if (!updateInfo?.available) return;

    setStatus("installing");
    setError(null);

    try {
      onLog("info", "Downloading and installing update...");
      const result = (await invoke("install_update")) as {
        success: boolean;
        message?: string;
        data?: { installed: boolean; version?: string; development?: boolean };
      };

      if (result.success && result.data?.installed) {
        onLog("success", "Update installed. The application will restart.");
      } else if (result.data?.development) {
        onLog("warning", "Updates are disabled in development mode");
        setStatus("idle");
      } else {
        setError(result.message || "Failed to install update");
        setStatus("error");
        onLog("error", result.message || "Failed to install update");
      }
    } catch (err) {
      const errorMessage = err instanceof Error ? err.message : String(err);
      setError(errorMessage);
      setStatus("error");
      onLog("error", `Failed to install update: ${errorMessage}`);
    }
  };

  const formatLastChecked = () => {
    if (!lastChecked) return "Never";
    return lastChecked.toLocaleString();
  };

  return (
    <div className="space-y-6">
      <SectionHeader
        title="Updates"
        description="Check for and install application updates to get the latest features and bug fixes."
        icon={<Download className="w-6 h-6" />}
      />

      {/* Current Version Card */}
      <div className="rounded-lg bg-card/50 p-4" data-ui-id="settings-update-version-section">
        <h4 className="font-medium text-sm mb-3">Current Version</h4>

        <div className="flex items-center justify-between">
          <div className="space-y-1">
            <div
              data-content-role="metric"
              data-content-label="current version"
              className="text-xl font-bold text-primary"
            >
              v{updateInfo?.current_version || "0.1.0"}
            </div>
            <div
              data-content-role="body-text"
              data-content-label="last checked time"
              className="text-xs text-muted-foreground"
            >
              Last checked: {formatLastChecked()}
            </div>
          </div>

          <button
            onClick={checkForUpdates}
            disabled={status === "checking"}
            className={`
              flex items-center gap-2 px-4 py-2 rounded-lg font-medium
              transition-colors
              ${
                status === "checking"
                  ? "bg-muted text-muted-foreground cursor-not-allowed"
                  : "bg-secondary text-secondary-foreground hover:bg-secondary/80"
              }
            `}
            data-ui-id="settings-update-check-btn"
          >
            <RefreshCw className={`w-4 h-4 ${status === "checking" ? "animate-spin" : ""}`} />
            {status === "checking" ? "Checking..." : "Check for Updates"}
          </button>
        </div>
      </div>

      {/* Update Status Card */}
      {updateInfo && (
        <div className="rounded-lg bg-card/50 p-4" data-ui-id="settings-update-status-section">
          <h4 className="font-medium text-sm mb-3">Update Status</h4>

          {updateInfo.development ? (
            <div className="flex items-start gap-2 p-3 bg-muted/30 rounded-lg">
              <Info className="w-4 h-4 text-muted-foreground mt-0.5 shrink-0" />
              <div>
                <div
                  data-content-role="status"
                  data-content-label="development mode"
                  className="text-sm font-medium"
                >
                  Development Mode
                </div>
                <div
                  data-content-role="description"
                  data-content-label="development mode info"
                  className="text-xs text-muted-foreground"
                >
                  Update checking is disabled in development builds. Build a release version to
                  enable automatic updates.
                </div>
              </div>
            </div>
          ) : updateInfo.available ? (
            <div className="space-y-4">
              <div className="flex items-start gap-2 p-3 bg-primary/10 rounded-lg">
                <Download className="w-4 h-4 text-primary mt-0.5 shrink-0" />
                <div className="flex-1">
                  <div
                    data-content-role="status"
                    data-content-label="update available"
                    className="text-sm font-medium text-primary"
                  >
                    Update Available
                  </div>
                  <div
                    data-content-role="description"
                    data-content-label="update version info"
                    className="text-xs text-muted-foreground mt-1"
                  >
                    Version <span className="font-semibold">{updateInfo.version}</span> is available
                    for download.
                  </div>
                </div>
              </div>

              {updateInfo.notes && (
                <div className="space-y-2">
                  <div
                    data-content-role="label"
                    data-content-label="release notes label"
                    className="text-sm font-medium"
                  >
                    Release Notes:
                  </div>
                  <div className="p-3 bg-muted/50 rounded-lg text-sm text-muted-foreground whitespace-pre-wrap max-h-48 overflow-y-auto">
                    {updateInfo.notes}
                  </div>
                </div>
              )}

              <button
                onClick={installUpdate}
                disabled={status === "installing"}
                className={`
                  flex items-center justify-center gap-2 w-full px-4 py-3 rounded-lg font-medium
                  transition-colors
                  ${
                    status === "installing"
                      ? "bg-muted text-muted-foreground cursor-not-allowed"
                      : "bg-primary text-primary-foreground hover:bg-primary/90"
                  }
                `}
                data-ui-id="settings-update-install-btn"
              >
                {status === "installing" ? (
                  <>
                    <RefreshCw className="w-4 h-4 animate-spin" />
                    Installing Update...
                  </>
                ) : (
                  <>
                    <Download className="w-4 h-4" />
                    Download and Install Update
                  </>
                )}
              </button>

              <p className="text-xs text-muted-foreground text-center">
                The application will restart automatically after the update is installed.
              </p>
            </div>
          ) : (
            <div
              className={`flex items-start gap-2 p-3 ${getStatusColors("success").bg} rounded-lg`}
            >
              <CheckCircle
                className={`w-4 h-4 ${getStatusColors("success").icon} mt-0.5 shrink-0`}
              />
              <div>
                <div
                  data-content-role="status"
                  data-content-label="up to date"
                  className={`text-sm font-medium ${getStatusColors("success").text}`}
                >
                  Up to Date
                </div>
                <div
                  data-content-role="description"
                  data-content-label="up to date info"
                  className="text-xs text-muted-foreground"
                >
                  You're running the latest version of Qontinui Runner.
                </div>
              </div>
            </div>
          )}
        </div>
      )}

      {/* Error Display */}
      {error && (
        <div className="flex items-start gap-2 p-3 bg-destructive/10 rounded-lg">
          <AlertCircle className="w-4 h-4 text-destructive mt-0.5 shrink-0" />
          <div>
            <div
              data-content-role="status"
              data-content-label="update error"
              className="text-sm font-medium text-destructive"
            >
              Update Error
            </div>
            <div
              data-content-role="body-text"
              data-content-label="error message"
              className="text-xs text-muted-foreground"
            >
              {error}
            </div>
          </div>
        </div>
      )}

      {/* Info Box */}
      <div className="p-3 bg-primary/5 rounded-lg">
        <div className="text-xs text-muted-foreground">
          <strong className="text-foreground">Tip:</strong> Updates are checked automatically when
          this settings tab is opened. You can also manually check for updates using the button
          above.
        </div>
      </div>
    </div>
  );
}
