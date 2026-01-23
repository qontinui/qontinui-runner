/**
 * Import/Export Dialog
 *
 * Dialog for importing and exporting verification tests to/from JSON files.
 * Supports file selection via Tauri dialog API.
 */

import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open, save } from "@tauri-apps/plugin-dialog";
import { X, Upload, Download, FileJson, Check, AlertCircle, Info } from "lucide-react";
import type { CommandResponse, VerificationTest } from "./types";
import { getStatusColors, getAccentColors } from "@/design-system";

interface ImportExportDialogProps {
  mode: "import" | "export";
  tests: VerificationTest[];
  selectedTestIds?: string[];
  onClose: () => void;
  onComplete: () => void;
}

interface ImportResult {
  name: string;
  status: "created" | "updated" | "skipped" | "error";
  message?: string;
  new_id?: string;
}

interface ImportSummary {
  total: number;
  created: number;
  updated: number;
  skipped: number;
  errors: number;
  results: ImportResult[];
}

export function ImportExportDialog({
  mode,
  tests,
  selectedTestIds = [],
  onClose,
  onComplete,
}: ImportExportDialogProps) {
  const [isProcessing, setIsProcessing] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [success, setSuccess] = useState<string | null>(null);
  const [importSummary, setImportSummary] = useState<ImportSummary | null>(null);
  const [updateExisting, setUpdateExisting] = useState(false);
  const [exportAll, setExportAll] = useState(selectedTestIds.length === 0);

  const handleExport = async () => {
    setIsProcessing(true);
    setError(null);
    setSuccess(null);

    try {
      // Show save dialog
      const filePath = await save({
        defaultPath: "verification-tests.json",
        filters: [{ name: "JSON", extensions: ["json"] }],
      });

      if (!filePath) {
        setIsProcessing(false);
        return;
      }

      let response: CommandResponse;

      if (exportAll) {
        // Export all tests
        response = await invoke<CommandResponse>("export_all_tests_to_file", {
          filePath,
        });
      } else {
        // Export selected tests
        response = await invoke<CommandResponse>("export_tests_to_file", {
          testIds: selectedTestIds,
          filePath,
        });
      }

      if (response.success) {
        setSuccess(
          `Exported ${exportAll ? tests.length : selectedTestIds.length} tests to ${filePath}`,
        );
        setTimeout(() => {
          onComplete();
          onClose();
        }, 1500);
      } else {
        setError(response.message || "Export failed");
      }
    } catch (err) {
      setError(String(err));
    } finally {
      setIsProcessing(false);
    }
  };

  const handleImport = async () => {
    setIsProcessing(true);
    setError(null);
    setSuccess(null);
    setImportSummary(null);

    try {
      // Show open dialog
      const filePath = await open({
        filters: [{ name: "JSON", extensions: ["json"] }],
        multiple: false,
      });

      if (!filePath) {
        setIsProcessing(false);
        return;
      }

      const response = await invoke<CommandResponse<ImportSummary>>("import_tests_from_file", {
        filePath,
        updateExisting,
      });

      if (response.success && response.data) {
        setImportSummary(response.data);
        setSuccess(response.message || "Import completed");
        onComplete();
      } else {
        setError(response.message || "Import failed");
      }
    } catch (err) {
      setError(String(err));
    } finally {
      setIsProcessing(false);
    }
  };

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50">
      <div
        data-ui-id="dialog-test-import-export"
        className="bg-card border border-border rounded-lg shadow-xl w-full max-w-lg mx-4">
        {/* Header */}
        <div className="flex items-center justify-between p-4 border-b border-border">
          <div className="flex items-center gap-2">
            {mode === "export" ? (
              <Download className="w-5 h-5 text-primary" />
            ) : (
              <Upload className="w-5 h-5 text-primary" />
            )}
            <h2 className="text-lg font-semibold">
              {mode === "export" ? "Export Tests" : "Import Tests"}
            </h2>
          </div>
          <button
            onClick={onClose}
            className="p-1 hover:bg-muted rounded-md transition-colors"
            disabled={isProcessing}
            data-ui-id="test-builder-import-export-close-btn"
          >
            <X className="w-5 h-5" />
          </button>
        </div>

        {/* Content */}
        <div className="p-4 space-y-4">
          {mode === "export" ? (
            <>
              <div className="flex items-start gap-3 p-3 bg-muted/30 rounded-lg">
                <FileJson className="w-5 h-5 text-muted-foreground flex-shrink-0 mt-0.5" />
                <div className="text-sm">
                  <p className="font-medium">Export to JSON</p>
                  <p className="text-muted-foreground mt-1">
                    Tests will be exported to a JSON file for version control or backup.
                  </p>
                </div>
              </div>

              {tests.length > 0 && (
                <div className="space-y-2">
                  <label className="flex items-center gap-2 cursor-pointer">
                    <input
                      type="radio"
                      name="exportScope"
                      checked={exportAll}
                      onChange={() => setExportAll(true)}
                      className="w-4 h-4"
                      data-ui-id="test-builder-export-all-radio"
                    />
                    <span className="text-sm">Export all tests ({tests.length})</span>
                  </label>
                  {selectedTestIds.length > 0 && (
                    <label className="flex items-center gap-2 cursor-pointer">
                      <input
                        type="radio"
                        name="exportScope"
                        checked={!exportAll}
                        onChange={() => setExportAll(false)}
                        className="w-4 h-4"
                        data-ui-id="test-builder-export-selected-radio"
                      />
                      <span className="text-sm">
                        Export selected tests ({selectedTestIds.length})
                      </span>
                    </label>
                  )}
                </div>
              )}
            </>
          ) : (
            <>
              <div className="flex items-start gap-3 p-3 bg-muted/30 rounded-lg">
                <FileJson className="w-5 h-5 text-muted-foreground flex-shrink-0 mt-0.5" />
                <div className="text-sm">
                  <p className="font-medium">Import from JSON</p>
                  <p className="text-muted-foreground mt-1">
                    Import tests from a previously exported JSON file.
                  </p>
                </div>
              </div>

              <label className="flex items-center gap-2 cursor-pointer">
                <input
                  type="checkbox"
                  checked={updateExisting}
                  onChange={(e) => setUpdateExisting(e.target.checked)}
                  className="w-4 h-4"
                  data-ui-id="test-builder-update-existing-checkbox"
                />
                <span className="text-sm">Update existing tests with same name</span>
              </label>

              {!updateExisting && (
                <div
                  className={`flex items-start gap-2 p-2 ${getAccentColors("blue").bg} border ${getAccentColors("blue").border} rounded-md`}
                >
                  <Info
                    className={`w-4 h-4 ${getAccentColors("blue").text} flex-shrink-0 mt-0.5`}
                  />
                  <p className={`text-xs ${getAccentColors("blue").text}`}>
                    Tests with duplicate names will be skipped. Enable update to replace them.
                  </p>
                </div>
              )}
            </>
          )}

          {/* Error message */}
          {error && (
            <div className="flex items-start gap-2 p-3 bg-destructive/10 border border-destructive/20 rounded-md">
              <AlertCircle className="w-4 h-4 text-destructive flex-shrink-0 mt-0.5" />
              <p className="text-sm text-destructive">{error}</p>
            </div>
          )}

          {/* Success message */}
          {success && !importSummary && (
            <div
              className={`flex items-start gap-2 p-3 ${getStatusColors("success").bg} border ${getStatusColors("success").border} rounded-md`}
            >
              <Check
                className={`w-4 h-4 ${getStatusColors("success").icon} flex-shrink-0 mt-0.5`}
              />
              <p className={`text-sm ${getStatusColors("success").text}`}>{success}</p>
            </div>
          )}

          {/* Import summary */}
          {importSummary && (
            <div className="space-y-3">
              <div
                className={`flex items-start gap-2 p-3 ${getStatusColors("success").bg} border ${getStatusColors("success").border} rounded-md`}
              >
                <Check
                  className={`w-4 h-4 ${getStatusColors("success").icon} flex-shrink-0 mt-0.5`}
                />
                <div className="text-sm">
                  <p className={`font-medium ${getStatusColors("success").text}`}>
                    Import Complete
                  </p>
                  <p className="text-muted-foreground mt-1">
                    {importSummary.created} created, {importSummary.updated} updated,{" "}
                    {importSummary.skipped} skipped
                    {importSummary.errors > 0 && `, ${importSummary.errors} errors`}
                  </p>
                </div>
              </div>

              {/* Detailed results */}
              {importSummary.results.length > 0 && (
                <div className="max-h-48 overflow-y-auto border border-border rounded-md">
                  <table className="w-full text-sm">
                    <thead className="bg-muted/50 sticky top-0">
                      <tr>
                        <th className="text-left px-3 py-2 font-medium">Test</th>
                        <th className="text-left px-3 py-2 font-medium">Status</th>
                      </tr>
                    </thead>
                    <tbody>
                      {importSummary.results.map((result, idx) => (
                        <tr key={idx} className="border-t border-border/50">
                          <td className="px-3 py-2 truncate max-w-[200px]">{result.name}</td>
                          <td className="px-3 py-2">
                            <span
                              className={`px-2 py-0.5 rounded text-xs font-medium ${
                                result.status === "created"
                                  ? `${getStatusColors("success").bg} ${getStatusColors("success").text}`
                                  : result.status === "updated"
                                    ? `${getAccentColors("blue").bg} ${getAccentColors("blue").text}`
                                    : result.status === "skipped"
                                      ? `${getStatusColors("warning").bg} ${getStatusColors("warning").text}`
                                      : `${getStatusColors("error").bg} ${getStatusColors("error").text}`
                              }`}
                            >
                              {result.status}
                            </span>
                          </td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </div>
              )}
            </div>
          )}
        </div>

        {/* Footer */}
        <div className="flex items-center justify-end gap-2 p-4 border-t border-border">
          <button
            onClick={onClose}
            className="px-4 py-2 text-sm hover:bg-muted rounded-md transition-colors"
            disabled={isProcessing}
            data-ui-id="test-builder-import-export-cancel-btn"
          >
            {importSummary ? "Close" : "Cancel"}
          </button>
          {!importSummary && (
            <button
              onClick={mode === "export" ? handleExport : handleImport}
              disabled={isProcessing || (mode === "export" && tests.length === 0)}
              className="px-4 py-2 text-sm bg-primary text-primary-foreground rounded-md
                       hover:bg-primary/90 disabled:opacity-50 disabled:cursor-not-allowed
                       flex items-center gap-2 transition-colors"
              data-ui-id="test-builder-import-export-confirm-btn"
            >
              {isProcessing ? (
                <>
                  <div className="w-4 h-4 border-2 border-white/30 border-t-white rounded-full animate-spin" />
                  {mode === "export" ? "Exporting..." : "Importing..."}
                </>
              ) : (
                <>
                  {mode === "export" ? (
                    <Download className="w-4 h-4" />
                  ) : (
                    <Upload className="w-4 h-4" />
                  )}
                  {mode === "export" ? "Export" : "Import"}
                </>
              )}
            </button>
          )}
        </div>
      </div>
    </div>
  );
}
