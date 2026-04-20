/**
 * BackupSettings.tsx
 *
 * Comprehensive backup and restore functionality for all user data including:
 * - Flows (orchestrator_flows)
 * - Flow executions (flow_executions)
 * - Checkpoints (orchestrator_checkpoints)
 * - Learning outcomes and patterns
 * - Settings, prompts, AI workflows
 * - Verification tests, scheduled tasks, and more
 */

import { useState, useEffect, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  Archive,
  Download,
  Upload,
  FileArchive,
  AlertCircle,
  CheckCircle,
  Database,
  Settings,
  ChevronDown,
  ChevronUp,
} from "lucide-react";
import { SectionHeader } from "./SectionHeader";
import { getAccentColors, getStatusColors } from "@/design-system";
import type { LogFunction } from "./types";

// Types for comprehensive export/import
interface ExportManifest {
  version: string;
  created_at: string;
  app_version: string;
}

interface ExportSummary {
  flows: number;
  flow_executions: number;
  checkpoints: number;
  learning_outcomes: number;
  learning_patterns: number;
  settings: number;
  prompts: number;
  unified_workflows: number;
  verification_tests: number;
  task_hooks: number;
  scheduled_tasks: number;
  saved_api_requests: number;
  configs: number;
}

interface ComprehensiveExport {
  manifest: ExportManifest;
  summary: ExportSummary;
  flows: unknown[];
  flow_executions: unknown[];
  checkpoints: unknown[];
  learning_outcomes: unknown[];
  learning_patterns: unknown[];
  settings: unknown[];
  prompts: unknown[];
  unified_workflows: unknown[];
  verification_tests: unknown[];
  task_hooks: unknown[];
  scheduled_tasks: unknown[];
  saved_api_requests: unknown[];
  configs: unknown[];
}

interface ExportOptions {
  flows: boolean;
  flow_executions: boolean;
  checkpoints: boolean;
  learning_outcomes: boolean;
  learning_patterns: boolean;
  settings: boolean;
  prompts: boolean;
  unified_workflows: boolean;
  verification_tests: boolean;
  task_hooks: boolean;
  scheduled_tasks: boolean;
  saved_api_requests: boolean;
  configs: boolean;
}

interface ImportOptions {
  conflict_mode: "skip" | "overwrite";
  flows: boolean;
  flow_executions: boolean;
  checkpoints: boolean;
  learning_outcomes: boolean;
  learning_patterns: boolean;
  settings: boolean;
  prompts: boolean;
  unified_workflows: boolean;
  verification_tests: boolean;
  task_hooks: boolean;
  scheduled_tasks: boolean;
  saved_api_requests: boolean;
  configs: boolean;
}

interface ImportResult {
  imported: number;
  skipped: number;
  errors: string[];
}

interface ComprehensiveImportResult {
  success: boolean;
  results: Record<string, ImportResult>;
  total_imported: number;
  total_skipped: number;
  total_errors: number;
}

interface ImportPreview {
  manifest: ExportManifest;
  counts: ExportSummary;
  is_compatible: boolean;
  compatibility_issues: string[];
}

interface BackupSettingsProps {
  onLog: LogFunction;
}

const defaultExportOptions: ExportOptions = {
  flows: true,
  flow_executions: true,
  checkpoints: true,
  learning_outcomes: true,
  learning_patterns: true,
  settings: true,
  prompts: true,
  unified_workflows: true,
  verification_tests: true,
  task_hooks: true,
  scheduled_tasks: true,
  saved_api_requests: true,
  configs: true,
};

const defaultImportOptions: ImportOptions = {
  conflict_mode: "skip",
  flows: true,
  flow_executions: false, // Execution history usually not wanted
  checkpoints: false, // Checkpoints usually not wanted
  learning_outcomes: true,
  learning_patterns: true,
  settings: true,
  prompts: true,
  unified_workflows: true,
  verification_tests: true,
  task_hooks: true,
  scheduled_tasks: true,
  saved_api_requests: true,
  configs: true,
};

// Label mapping for better UI display
const categoryLabels: Record<keyof ExportSummary, string> = {
  flows: "Flows",
  flow_executions: "Flow Executions",
  checkpoints: "Checkpoints",
  learning_outcomes: "Learning Outcomes",
  learning_patterns: "Learning Patterns",
  settings: "Settings",
  prompts: "Prompts",
  unified_workflows: "Unified Workflows",
  verification_tests: "Verification Tests",
  task_hooks: "Task Hooks",
  scheduled_tasks: "Scheduled Tasks",
  saved_api_requests: "Saved API Requests",
  configs: "Configurations",
};

export function BackupSettings({ onLog }: BackupSettingsProps) {
  const [exportSummary, setExportSummary] = useState<ExportSummary | null>(null);
  const [isLoadingSummary, setIsLoadingSummary] = useState(true);
  const [isExporting, setIsExporting] = useState(false);
  const [isImporting, setIsImporting] = useState(false);
  const [exportOptions, setExportOptions] = useState<ExportOptions>(defaultExportOptions);
  const [importOptions, setImportOptions] = useState<ImportOptions>(defaultImportOptions);
  const [importPreview, setImportPreview] = useState<ImportPreview | null>(null);
  const [importResult, setImportResult] = useState<ComprehensiveImportResult | null>(null);
  const [pendingImportData, setPendingImportData] = useState<ComprehensiveExport | null>(null);
  const [showExportOptions, setShowExportOptions] = useState(false);
  const [showImportOptions, setShowImportOptions] = useState(false);
  const [exportProgress, setExportProgress] = useState<string | null>(null);
  const [importProgress, setImportProgress] = useState<string | null>(null);
  const fileInputRef = useRef<HTMLInputElement>(null);

  // Load export summary on mount
  useEffect(() => {
    // eslint-disable-next-line react-hooks/immutability
    loadExportSummary();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const loadExportSummary = async () => {
    setIsLoadingSummary(true);
    try {
      const summary = await invoke<ExportSummary>("get_export_summary");
      setExportSummary(summary);
    } catch (err) {
      console.error("Failed to load export summary:", err);
      onLog("error", `Failed to load data summary: ${err}`);
    } finally {
      setIsLoadingSummary(false);
    }
  };

  const handleExportAllData = async () => {
    setIsExporting(true);
    setExportProgress("Exporting data from database...");

    try {
      // Export data using Tauri command
      const exportData = await invoke<ComprehensiveExport>("export_all_data", {
        options: exportOptions,
      });

      setExportProgress("Preparing download...");

      // Create blob and trigger download
      const jsonString = JSON.stringify(exportData, null, 2);
      const blob = new Blob([jsonString], { type: "application/json" });
      const url = URL.createObjectURL(blob);

      const a = document.createElement("a");
      a.href = url;
      a.download = `qontinui-backup-${new Date().toISOString().split("T")[0]}.json`;
      document.body.appendChild(a);
      a.click();
      document.body.removeChild(a);
      URL.revokeObjectURL(url);

      const totalItems = Object.values(exportData.summary).reduce((a, b) => a + b, 0);
      onLog("success", `Export complete: ${totalItems} items exported`);
    } catch (err) {
      console.error("Failed to export data:", err);
      onLog("error", `Failed to export data: ${err}`);
    } finally {
      setIsExporting(false);
      setExportProgress(null);
    }
  };

  const handleSelectImportFile = () => {
    fileInputRef.current?.click();
  };

  const handleFileInputChange = async (event: React.ChangeEvent<HTMLInputElement>) => {
    const file = event.target.files?.[0];
    if (!file) return;

    // Reset input so same file can be selected again
    event.target.value = "";

    try {
      setImportProgress("Reading file...");

      // Read file content
      const content = await file.text();
      const data = JSON.parse(content) as ComprehensiveExport;

      // Get preview
      const preview = await invoke<ImportPreview>("get_import_preview", { data });

      setImportPreview(preview);
      setPendingImportData(data);
      setImportResult(null);
      setImportProgress(null);
    } catch (err) {
      console.error("Failed to read import file:", err);
      onLog("error", `Failed to read import file: ${err}`);
      setImportProgress(null);
    }
  };

  const handleImportAllData = async () => {
    if (!pendingImportData) {
      onLog("error", "No import data loaded");
      return;
    }

    setIsImporting(true);
    setImportProgress("Importing data...");

    try {
      const result = await invoke<ComprehensiveImportResult>("import_all_data", {
        data: pendingImportData,
        options: importOptions,
      });

      setImportResult(result);
      setPendingImportData(null);
      setImportPreview(null);

      if (result.success) {
        onLog(
          "success",
          `Import complete: ${result.total_imported} imported, ${result.total_skipped} skipped`,
        );
      } else {
        onLog(
          "warning",
          `Import completed with ${result.total_errors} errors: ${result.total_imported} imported, ${result.total_skipped} skipped`,
        );
      }

      // Refresh summary
      await loadExportSummary();
    } catch (err) {
      console.error("Failed to import data:", err);
      onLog("error", `Failed to import data: ${err}`);
    } finally {
      setIsImporting(false);
      setImportProgress(null);
    }
  };

  const cancelImport = () => {
    setImportPreview(null);
    setPendingImportData(null);
    setImportResult(null);
  };

  const getTotalExportCount = () => {
    if (!exportSummary) return 0;
    return Object.entries(exportSummary)
      .filter(([key]) => exportOptions[key as keyof ExportOptions])
      .reduce((sum, [, count]) => sum + count, 0);
  };

  return (
    <div className="space-y-6">
      <SectionHeader
        title="Backup & Restore"
        description="Create comprehensive backups of all your data including flows, learning outcomes, settings, and more. Restore from a backup to recover your data or transfer to another machine."
        icon={<Archive className="w-6 h-6" />}
      />

      {/* Export Summary */}
      <div className="space-y-4 rounded-lg bg-card/50 p-4">
        <div className="flex items-center justify-between">
          <div className="flex items-center gap-2">
            <Database className="w-4 h-4 text-primary" />
            <h4 className="font-medium text-sm">Your Data</h4>
          </div>
          <button
            onClick={loadExportSummary}
            className="text-xs text-muted-foreground hover:text-foreground"
            disabled={isLoadingSummary}
          >
            {isLoadingSummary ? "Loading..." : "Refresh"}
          </button>
        </div>

        {exportSummary && (
          <div className="grid grid-cols-2 sm:grid-cols-3 md:grid-cols-4 gap-2 text-xs">
            {Object.entries(exportSummary).map(([key, count]) => (
              <div key={key} className="flex justify-between items-center p-2 bg-muted/30 rounded">
                <span
                  data-content-role="label"
                  data-content-label="data category"
                  className="text-muted-foreground"
                >
                  {categoryLabels[key as keyof ExportSummary]}
                </span>
                <span
                  data-content-role="metric"
                  data-content-label="data count"
                  className="font-medium"
                >
                  {count}
                </span>
              </div>
            ))}
          </div>
        )}
      </div>

      {/* Comprehensive Export Section */}
      <div className="space-y-4 rounded-lg bg-card/50 p-4">
        <div className="flex items-center gap-2">
          <Download className="w-4 h-4 text-primary" />
          <h4 className="font-medium text-sm">Export All Data</h4>
        </div>

        <p className="text-xs text-muted-foreground">
          Export all your data to a single JSON file. This comprehensive backup includes flows,
          learning data, settings, prompts, verification tests, and more.
        </p>

        {/* Export Options Toggle */}
        <button
          onClick={() => setShowExportOptions(!showExportOptions)}
          className="flex items-center gap-1 text-xs text-muted-foreground hover:text-foreground"
        >
          <Settings className="w-3 h-3" />
          Export Options
          {showExportOptions ? (
            <ChevronUp className="w-3 h-3" />
          ) : (
            <ChevronDown className="w-3 h-3" />
          )}
        </button>

        {showExportOptions && (
          <div className="grid grid-cols-2 sm:grid-cols-3 gap-2 p-3 bg-muted/30 rounded-lg">
            {Object.entries(exportOptions).map(([key, value]) => (
              <label key={key} className="flex items-center gap-2 text-xs cursor-pointer">
                <input
                  type="checkbox"
                  checked={value}
                  onChange={(e) =>
                    setExportOptions((prev) => ({
                      ...prev,
                      [key]: e.target.checked,
                    }))
                  }
                  className="rounded border-muted-foreground"
                />
                <span>{categoryLabels[key as keyof ExportSummary]}</span>
                {exportSummary && (
                  <span className="text-muted-foreground">
                    ({exportSummary[key as keyof ExportSummary]})
                  </span>
                )}
              </label>
            ))}
          </div>
        )}

        <div className="flex items-center gap-3">
          <button
            onClick={handleExportAllData}
            disabled={isExporting || getTotalExportCount() === 0}
            className="px-4 py-2 bg-primary hover:bg-primary/90 text-primary-foreground rounded-md transition-colors flex items-center gap-2 disabled:opacity-50 disabled:cursor-not-allowed"
          >
            {isExporting ? (
              <>
                <div className="w-4 h-4 border-2 border-primary-foreground/30 border-t-primary-foreground rounded-full animate-spin" />
                {exportProgress || "Exporting..."}
              </>
            ) : (
              <>
                <FileArchive className="w-4 h-4" />
                Export ({getTotalExportCount()} items)
              </>
            )}
          </button>
        </div>
      </div>

      {/* Import Section */}
      <div className="space-y-4 rounded-lg bg-card/50 p-4">
        <div className="flex items-center gap-2">
          <Upload className="w-4 h-4 text-primary" />
          <h4 className="font-medium text-sm">Import Data</h4>
        </div>

        <p className="text-xs text-muted-foreground">
          Import data from a backup file. You can choose to skip or overwrite existing items.
        </p>

        {!importPreview && (
          <>
            <input
              ref={fileInputRef}
              type="file"
              accept=".json"
              onChange={handleFileInputChange}
              className="hidden"
            />
            <button
              onClick={handleSelectImportFile}
              disabled={isImporting}
              className="px-4 py-2 bg-muted hover:bg-muted/80 text-foreground rounded-md transition-colors flex items-center gap-2 disabled:opacity-50"
            >
              <Upload className="w-4 h-4" />
              Select Backup File
            </button>
          </>
        )}

        {importProgress && !importPreview && (
          <div className="flex items-center gap-2 text-xs text-muted-foreground">
            <div className="w-3 h-3 border-2 border-muted-foreground/30 border-t-muted-foreground rounded-full animate-spin" />
            {importProgress}
          </div>
        )}

        {/* Import Preview */}
        {importPreview && (
          <div className="p-4 bg-muted/30 rounded-lg space-y-4">
            <div className="flex items-center justify-between">
              <h5 className="font-medium">Import Preview</h5>
              <button
                onClick={cancelImport}
                className="text-sm text-muted-foreground hover:text-foreground"
              >
                Cancel
              </button>
            </div>

            <div className="grid grid-cols-2 gap-4 text-sm">
              <div>
                <span
                  data-content-role="label"
                  data-content-label="version label"
                  className="text-muted-foreground"
                >
                  Version:
                </span>{" "}
                <span
                  data-content-role="body-text"
                  data-content-label="version value"
                  className="font-medium"
                >
                  {importPreview.manifest.version}
                </span>
              </div>
              <div>
                <span
                  data-content-role="label"
                  data-content-label="app version label"
                  className="text-muted-foreground"
                >
                  App Version:
                </span>{" "}
                <span
                  data-content-role="body-text"
                  data-content-label="app version value"
                  className="font-medium"
                >
                  {importPreview.manifest.app_version}
                </span>
              </div>
              <div className="col-span-2">
                <span
                  data-content-role="label"
                  data-content-label="created label"
                  className="text-muted-foreground"
                >
                  Created:
                </span>{" "}
                <span
                  data-content-role="body-text"
                  data-content-label="created date"
                  className="font-medium"
                >
                  {new Date(importPreview.manifest.created_at).toLocaleString()}
                </span>
              </div>
            </div>

            {!importPreview.is_compatible && (
              <div
                className={`flex items-center gap-2 p-2 ${getStatusColors("error").bg} rounded ${getStatusColors("error").text} text-xs`}
              >
                <AlertCircle className="w-3.5 h-3.5 shrink-0" />
                <span>Compatibility issues: {importPreview.compatibility_issues.join(", ")}</span>
              </div>
            )}

            {/* Import Options */}
            <div className="space-y-3">
              <button
                onClick={() => setShowImportOptions(!showImportOptions)}
                className="flex items-center gap-1 text-xs text-muted-foreground hover:text-foreground"
              >
                <Settings className="w-3 h-3" />
                Import Options
                {showImportOptions ? (
                  <ChevronUp className="w-3 h-3" />
                ) : (
                  <ChevronDown className="w-3 h-3" />
                )}
              </button>

              {showImportOptions && (
                <div className="space-y-3 p-3 bg-muted/50 rounded-lg">
                  <div className="flex items-center gap-4 text-xs">
                    <span className="text-muted-foreground">Conflict Resolution:</span>
                    <label className="flex items-center gap-1 cursor-pointer">
                      <input
                        type="radio"
                        name="conflict_mode"
                        checked={importOptions.conflict_mode === "skip"}
                        onChange={() =>
                          setImportOptions((prev) => ({
                            ...prev,
                            conflict_mode: "skip",
                          }))
                        }
                      />
                      Skip existing
                    </label>
                    <label className="flex items-center gap-1 cursor-pointer">
                      <input
                        type="radio"
                        name="conflict_mode"
                        checked={importOptions.conflict_mode === "overwrite"}
                        onChange={() =>
                          setImportOptions((prev) => ({
                            ...prev,
                            conflict_mode: "overwrite",
                          }))
                        }
                      />
                      Overwrite
                    </label>
                  </div>

                  <div className="grid grid-cols-2 sm:grid-cols-3 gap-2">
                    {Object.entries(importOptions)
                      .filter(([key]) => key !== "conflict_mode")
                      .map(([key, value]) => (
                        <label key={key} className="flex items-center gap-2 text-xs cursor-pointer">
                          <input
                            type="checkbox"
                            checked={value as boolean}
                            onChange={(e) =>
                              setImportOptions((prev) => ({
                                ...prev,
                                [key]: e.target.checked,
                              }))
                            }
                            className="rounded border-muted-foreground"
                          />
                          <span>{categoryLabels[key as keyof ExportSummary]}</span>
                          <span className="text-muted-foreground">
                            ({importPreview.counts[key as keyof ExportSummary]})
                          </span>
                        </label>
                      ))}
                  </div>
                </div>
              )}
            </div>

            <div className="grid grid-cols-2 sm:grid-cols-3 md:grid-cols-4 gap-2 text-xs">
              {Object.entries(importPreview.counts).map(([key, count]) => (
                <div
                  key={key}
                  className={`flex justify-between items-center p-2 rounded ${
                    importOptions[key as keyof Omit<ImportOptions, "conflict_mode">]
                      ? "bg-primary/10"
                      : "bg-muted/30 opacity-50"
                  }`}
                >
                  <span className="text-muted-foreground">
                    {categoryLabels[key as keyof ExportSummary]}
                  </span>
                  <span className="font-medium">{count}</span>
                </div>
              ))}
            </div>

            <div
              className={`flex items-center gap-2 p-2 ${getAccentColors("amber").bg} rounded ${getAccentColors("amber").text} text-xs`}
            >
              <AlertCircle className="w-3.5 h-3.5 shrink-0" />
              <span>
                {importOptions.conflict_mode === "overwrite"
                  ? "Existing items will be overwritten. This cannot be undone."
                  : "Existing items will be skipped. Only new items will be imported."}
              </span>
            </div>

            <button
              onClick={handleImportAllData}
              disabled={isImporting || !importPreview.is_compatible}
              className={`w-full px-4 py-2 ${
                importOptions.conflict_mode === "overwrite"
                  ? `${getAccentColors("amber").bgSolid} hover:bg-amber-700`
                  : "bg-primary hover:bg-primary/90"
              } text-white rounded-md transition-colors flex items-center justify-center gap-2 disabled:opacity-50 disabled:cursor-not-allowed`}
            >
              {isImporting ? (
                <>
                  <div className="w-4 h-4 border-2 border-white/30 border-t-white rounded-full animate-spin" />
                  {importProgress || "Importing..."}
                </>
              ) : (
                <>
                  <Upload className="w-4 h-4" />
                  Import Data
                </>
              )}
            </button>
          </div>
        )}

        {/* Import Result */}
        {importResult && (
          <div
            className={`p-3 rounded-lg ${
              importResult.success ? getStatusColors("success").bg : getAccentColors("amber").bg
            }`}
          >
            <div
              className={`flex items-center gap-2 mb-2 ${
                importResult.success
                  ? getStatusColors("success").text
                  : getAccentColors("amber").text
              }`}
            >
              {importResult.success ? (
                <CheckCircle className="w-3.5 h-3.5" />
              ) : (
                <AlertCircle className="w-3.5 h-3.5" />
              )}
              <span
                data-content-role="status"
                data-content-label="import result"
                className="text-xs font-medium"
              >
                {importResult.success ? "Import Successful" : "Import Completed with Errors"}
              </span>
            </div>
            <div className="text-xs text-muted-foreground space-y-1">
              <p>
                Imported: {importResult.total_imported} | Skipped: {importResult.total_skipped} |
                Errors: {importResult.total_errors}
              </p>
              {Object.entries(importResult.results).map(([category, result]) => (
                <p key={category} className="ml-2">
                  {categoryLabels[category as keyof ExportSummary]}: {result.imported} imported
                  {result.skipped > 0 && `, ${result.skipped} skipped`}
                  {result.errors.length > 0 && (
                    <span className={getStatusColors("error").text}>
                      {" "}
                      ({result.errors.length} errors)
                    </span>
                  )}
                </p>
              ))}
            </div>
          </div>
        )}
      </div>

      {/* Info Section */}
      <div className="p-4 bg-primary/5 rounded-lg">
        <div className="text-xs text-muted-foreground">
          <strong className="text-foreground">What's included in a comprehensive backup:</strong>
          <ul className="list-disc list-inside mt-2 space-y-1">
            <li>Flows and flow executions (workflow definitions and history)</li>
            <li>Checkpoints (orchestrator state snapshots)</li>
            <li>Learning outcomes and patterns (AI learning data)</li>
            <li>Settings and configurations</li>
            <li>Prompts and AI workflows</li>
            <li>Verification tests and scheduled tasks</li>
            <li>Saved API requests and task hooks</li>
          </ul>
          <p className="mt-3 text-[10px]">
            Note: API keys stored in the system keychain are not included in backups for security
            reasons. The backup file is in JSON format and can be inspected before importing.
          </p>
        </div>
      </div>
    </div>
  );
}
