/**
 * BackupSettings.tsx
 *
 * Backup and restore functionality for user data including:
 * - Settings
 * - Prompts
 * - Playwright Scripts
 * - AI Workflows
 */

import { useState } from "react";
import { Archive, Download, Upload, FileArchive, AlertCircle, CheckCircle } from "lucide-react";
import { SectionHeader } from "./SectionHeader";
import type { LogFunction } from "./types";

const API_BASE = "http://localhost:9876";

interface BackupResult {
  success: boolean;
  files_backed_up: string[];
  errors: string[];
}

interface RestoreResult {
  success: boolean;
  files_restored: string[];
  errors: string[];
  backup_version: string;
  backup_date: string;
}

interface BackupManifest {
  version: string;
  created_at: string;
  app_version: string;
  files: {
    name: string;
    original_path: string;
    size: number;
  }[];
}

interface BackupResponse {
  data: string; // Base64-encoded ZIP
  filename: string;
  result: BackupResult;
}

interface ApiResponse<T> {
  success: boolean;
  data?: T;
  message?: string;
}

interface BackupSettingsProps {
  onLog: LogFunction;
}

export function BackupSettings({ onLog }: BackupSettingsProps) {
  const [isCreatingBackup, setIsCreatingBackup] = useState(false);
  const [isRestoring, setIsRestoring] = useState(false);
  const [lastBackupResult, setLastBackupResult] = useState<BackupResult | null>(null);
  const [lastRestoreResult, setLastRestoreResult] = useState<RestoreResult | null>(null);
  const [previewManifest, setPreviewManifest] = useState<BackupManifest | null>(null);

  const handleCreateBackup = async () => {
    setIsCreatingBackup(true);
    setLastBackupResult(null);

    try {
      const response = await fetch(`${API_BASE}/backup`);
      const result: ApiResponse<BackupResponse> = await response.json();

      if (!result.success || !result.data) {
        throw new Error(result.message || "Failed to create backup");
      }

      const { data: base64Data, filename, result: backupResult } = result.data;

      // Convert base64 to blob and trigger download
      const byteCharacters = atob(base64Data);
      const byteNumbers = new Array(byteCharacters.length);
      for (let i = 0; i < byteCharacters.length; i++) {
        byteNumbers[i] = byteCharacters.charCodeAt(i);
      }
      const byteArray = new Uint8Array(byteNumbers);
      const blob = new Blob([byteArray], { type: "application/zip" });

      // Create download link
      const url = URL.createObjectURL(blob);
      const a = document.createElement("a");
      a.href = url;
      a.download = filename;
      document.body.appendChild(a);
      a.click();
      document.body.removeChild(a);
      URL.revokeObjectURL(url);

      setLastBackupResult(backupResult);
      onLog("success", `Backup created: ${backupResult.files_backed_up.length} files saved`);
    } catch (err) {
      console.error("Failed to create backup:", err);
      onLog("error", `Failed to create backup: ${err}`);
    } finally {
      setIsCreatingBackup(false);
    }
  };

  const handleFileSelect = async (event: React.ChangeEvent<HTMLInputElement>) => {
    const file = event.target.files?.[0];
    if (!file) return;

    // Reset previous results
    setPreviewManifest(null);
    setLastRestoreResult(null);

    try {
      // Read file as base64
      const base64 = await fileToBase64(file);

      // Get backup info first
      const infoResponse = await fetch(`${API_BASE}/backup/info`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ data: base64 }),
      });
      const infoResult: ApiResponse<BackupManifest> = await infoResponse.json();

      if (!infoResult.success || !infoResult.data) {
        throw new Error(infoResult.message || "Invalid backup file");
      }

      setPreviewManifest(infoResult.data);

      // Store base64 for later restore
      (window as { _pendingRestore?: string })._pendingRestore = base64;
    } catch (err) {
      console.error("Failed to read backup file:", err);
      onLog("error", `Failed to read backup file: ${err}`);
    }

    // Reset the input so the same file can be selected again
    event.target.value = "";
  };

  const handleRestore = async () => {
    const base64 = (window as { _pendingRestore?: string })._pendingRestore;
    if (!base64) {
      onLog("error", "No backup file selected");
      return;
    }

    if (
      !confirm(
        "Are you sure you want to restore from this backup? This will overwrite your current settings, prompts, scripts, and workflows.",
      )
    ) {
      return;
    }

    setIsRestoring(true);
    setLastRestoreResult(null);

    try {
      const response = await fetch(`${API_BASE}/restore`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ data: base64 }),
      });
      const result: ApiResponse<RestoreResult> = await response.json();

      if (!result.success || !result.data) {
        throw new Error(result.message || "Failed to restore backup");
      }

      setLastRestoreResult(result.data);
      setPreviewManifest(null);
      delete (window as { _pendingRestore?: string })._pendingRestore;

      if (result.data.success) {
        onLog("success", `Restore complete: ${result.data.files_restored.length} files restored`);
        // Suggest reload to apply settings
        if (
          confirm(
            "Restore complete! The application needs to reload to apply the restored settings. Reload now?",
          )
        ) {
          window.location.reload();
        }
      } else {
        onLog(
          "warning",
          `Restore completed with errors: ${result.data.files_restored.length} files restored, ${result.data.errors.length} errors`,
        );
      }
    } catch (err) {
      console.error("Failed to restore backup:", err);
      onLog("error", `Failed to restore backup: ${err}`);
    } finally {
      setIsRestoring(false);
    }
  };

  const cancelRestore = () => {
    setPreviewManifest(null);
    delete (window as { _pendingRestore?: string })._pendingRestore;
  };

  return (
    <div className="space-y-6">
      <SectionHeader
        title="Backup & Restore"
        description="Create backups of your settings, prompts, scripts, and workflows. Restore from a backup to recover your data or transfer to another machine."
        icon={<Archive className="w-6 h-6" />}
      />

      {/* Create Backup Section */}
      <div className="space-y-4 bg-card rounded-lg border border-border/50 p-6">
        <div className="flex items-center gap-3">
          <Download className="w-5 h-5 text-primary" />
          <h4 className="font-semibold text-lg">Create Backup</h4>
        </div>

        <p className="text-sm text-muted-foreground">
          Download a backup file containing all your user data. This includes settings, prompts,
          Playwright scripts, and AI workflows.
        </p>

        <button
          onClick={handleCreateBackup}
          disabled={isCreatingBackup}
          className="px-4 py-2 bg-primary hover:bg-primary/90 text-primary-foreground rounded-md transition-colors flex items-center gap-2 disabled:opacity-50 disabled:cursor-not-allowed"
        >
          {isCreatingBackup ? (
            <>
              <div className="w-4 h-4 border-2 border-primary-foreground/30 border-t-primary-foreground rounded-full animate-spin" />
              Creating Backup...
            </>
          ) : (
            <>
              <FileArchive className="w-4 h-4" />
              Download Backup
            </>
          )}
        </button>

        {lastBackupResult && (
          <div className="p-3 bg-green-500/10 border border-green-500/30 rounded-lg">
            <div className="flex items-center gap-2 text-green-600 mb-2">
              <CheckCircle className="w-4 h-4" />
              <span className="font-medium">Backup Created Successfully</span>
            </div>
            <div className="text-sm text-muted-foreground">
              <p>Files backed up: {lastBackupResult.files_backed_up.join(", ")}</p>
              {lastBackupResult.errors.length > 0 && (
                <p className="text-amber-600 mt-1">
                  Warnings: {lastBackupResult.errors.join("; ")}
                </p>
              )}
            </div>
          </div>
        )}
      </div>

      {/* Restore Section */}
      <div className="space-y-4 bg-card rounded-lg border border-border/50 p-6">
        <div className="flex items-center gap-3">
          <Upload className="w-5 h-5 text-primary" />
          <h4 className="font-semibold text-lg">Restore from Backup</h4>
        </div>

        <p className="text-sm text-muted-foreground">
          Select a backup file to restore your data. This will overwrite your current settings,
          prompts, scripts, and workflows.
        </p>

        {!previewManifest && (
          <label className="inline-flex items-center gap-2 px-4 py-2 bg-muted hover:bg-muted/80 text-foreground rounded-md transition-colors cursor-pointer">
            <Upload className="w-4 h-4" />
            Select Backup File
            <input
              type="file"
              accept=".zip"
              onChange={handleFileSelect}
              className="hidden"
              disabled={isRestoring}
            />
          </label>
        )}

        {/* Backup Preview */}
        {previewManifest && (
          <div className="p-4 bg-muted/50 border border-border rounded-lg space-y-4">
            <div className="flex items-center justify-between">
              <h5 className="font-medium">Backup Preview</h5>
              <button
                onClick={cancelRestore}
                className="text-sm text-muted-foreground hover:text-foreground"
              >
                Cancel
              </button>
            </div>

            <div className="grid grid-cols-2 gap-4 text-sm">
              <div>
                <span className="text-muted-foreground">Version:</span>{" "}
                <span className="font-medium">{previewManifest.version}</span>
              </div>
              <div>
                <span className="text-muted-foreground">App Version:</span>{" "}
                <span className="font-medium">{previewManifest.app_version}</span>
              </div>
              <div className="col-span-2">
                <span className="text-muted-foreground">Created:</span>{" "}
                <span className="font-medium">
                  {new Date(previewManifest.created_at).toLocaleString()}
                </span>
              </div>
            </div>

            <div>
              <span className="text-sm text-muted-foreground">
                Files ({previewManifest.files.length}):
              </span>
              <ul className="mt-1 space-y-1">
                {previewManifest.files.map((file) => (
                  <li key={file.name} className="text-sm flex items-center justify-between">
                    <span>{file.name}</span>
                    <span className="text-muted-foreground">
                      {(file.size / 1024).toFixed(1)} KB
                    </span>
                  </li>
                ))}
              </ul>
            </div>

            <div className="flex items-center gap-2 p-2 bg-amber-500/10 border border-amber-500/30 rounded text-amber-600 text-sm">
              <AlertCircle className="w-4 h-4 shrink-0" />
              <span>Restoring will overwrite your current data. This cannot be undone.</span>
            </div>

            <button
              onClick={handleRestore}
              disabled={isRestoring}
              className="w-full px-4 py-2 bg-amber-600 hover:bg-amber-700 text-white rounded-md transition-colors flex items-center justify-center gap-2 disabled:opacity-50 disabled:cursor-not-allowed"
            >
              {isRestoring ? (
                <>
                  <div className="w-4 h-4 border-2 border-white/30 border-t-white rounded-full animate-spin" />
                  Restoring...
                </>
              ) : (
                <>
                  <Upload className="w-4 h-4" />
                  Restore Backup
                </>
              )}
            </button>
          </div>
        )}

        {/* Restore Result */}
        {lastRestoreResult && (
          <div
            className={`p-3 rounded-lg ${
              lastRestoreResult.success
                ? "bg-green-500/10 border border-green-500/30"
                : "bg-amber-500/10 border border-amber-500/30"
            }`}
          >
            <div
              className={`flex items-center gap-2 mb-2 ${
                lastRestoreResult.success ? "text-green-600" : "text-amber-600"
              }`}
            >
              {lastRestoreResult.success ? (
                <CheckCircle className="w-4 h-4" />
              ) : (
                <AlertCircle className="w-4 h-4" />
              )}
              <span className="font-medium">
                {lastRestoreResult.success ? "Restore Successful" : "Restore Completed with Errors"}
              </span>
            </div>
            <div className="text-sm text-muted-foreground">
              <p>
                Restored from backup v{lastRestoreResult.backup_version} (
                {new Date(lastRestoreResult.backup_date).toLocaleString()})
              </p>
              <p className="mt-1">Files restored: {lastRestoreResult.files_restored.join(", ")}</p>
              {lastRestoreResult.errors.length > 0 && (
                <p className="text-red-600 mt-1">Errors: {lastRestoreResult.errors.join("; ")}</p>
              )}
            </div>
          </div>
        )}
      </div>

      {/* Info Section */}
      <div className="p-4 bg-primary/5 border border-primary/20 rounded-lg">
        <div className="text-sm text-muted-foreground">
          <strong className="text-foreground">What's included in a backup:</strong>
          <ul className="list-disc list-inside mt-2 space-y-1">
            <li>Settings (debug options, AI settings, preferences)</li>
            <li>Prompts (your saved prompt library)</li>
            <li>Playwright Scripts (your automation test scripts)</li>
            <li>AI Workflows (your AI-assisted automation workflows)</li>
          </ul>
          <p className="mt-3 text-xs">
            Note: API keys stored in the system keychain are not included in backups for security
            reasons.
          </p>
        </div>
      </div>
    </div>
  );
}

// Helper function to convert file to base64
function fileToBase64(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => {
      const dataUrl = reader.result as string;
      // Remove the data:application/zip;base64, prefix
      const base64 = dataUrl.split(",")[1];
      resolve(base64);
    };
    reader.onerror = reject;
    reader.readAsDataURL(file);
  });
}
