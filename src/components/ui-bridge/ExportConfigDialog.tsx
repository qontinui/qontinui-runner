/**
 * Export Config Dialog
 *
 * Dialog for exporting state discovery data to automation config format.
 * Supports both raw state candidates and discovered states from Python analysis.
 */

import { useState, useMemo, useCallback } from "react";
import { X, Download, Copy, Check, FileJson, ChevronDown, ChevronRight, Eye } from "lucide-react";
import { Badge } from "../ui/Badge";
import { Button } from "../ui/Button";
import { getStatusColors } from "@/design-system";
import type { CooccurrenceExport } from "../../types/ui-bridge-types";
import type { FingerprintDiscoveryResult } from "./discovery-types";
import {
  exportToAutomationConfig,
  getAvailableStatesForExport,
  getDefaultExportOptions,
  type StateExportOptions,
} from "../../lib/ui-bridge/stateExporter";

interface ExportConfigDialogProps {
  isOpen: boolean;
  onClose: () => void;
  cooccurrenceData: CooccurrenceExport | null;
  discoveryResult: FingerprintDiscoveryResult | null;
}

export function ExportConfigDialog({
  isOpen,
  onClose,
  cooccurrenceData,
  discoveryResult,
}: ExportConfigDialogProps) {
  const [options, setOptions] = useState<StateExportOptions>(getDefaultExportOptions());
  const [showPreview, setShowPreview] = useState(false);
  const [copied, setCopied] = useState(false);
  const [downloadSuccess, setDownloadSuccess] = useState(false);

  // Get list of available states for selection
  const availableStates = useMemo(
    () => getAvailableStatesForExport(cooccurrenceData, discoveryResult),
    [cooccurrenceData, discoveryResult],
  );

  // Generate the export config
  const exportConfig = useMemo(
    () => exportToAutomationConfig(cooccurrenceData, discoveryResult, options),
    [cooccurrenceData, discoveryResult, options],
  );

  // Handle option changes
  const updateOption = useCallback(
    <K extends keyof StateExportOptions>(key: K, value: StateExportOptions[K]) => {
      setOptions((prev) => ({ ...prev, [key]: value }));
    },
    [],
  );

  // Toggle state selection
  const toggleStateSelection = useCallback((stateId: string) => {
    setOptions((prev) => {
      const selected = prev.selectedStateIds;
      if (selected.includes(stateId)) {
        return { ...prev, selectedStateIds: selected.filter((id) => id !== stateId) };
      } else {
        return { ...prev, selectedStateIds: [...selected, stateId] };
      }
    });
  }, []);

  // Select/deselect all states
  const selectAllStates = useCallback(() => {
    setOptions((prev) => ({
      ...prev,
      selectedStateIds: availableStates.map((s) => s.id),
    }));
  }, [availableStates]);

  const deselectAllStates = useCallback(() => {
    setOptions((prev) => ({ ...prev, selectedStateIds: [] }));
  }, []);

  // Copy to clipboard
  const handleCopy = useCallback(async () => {
    try {
      await navigator.clipboard.writeText(JSON.stringify(exportConfig, null, 2));
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    } catch {
      // Ignore copy errors
    }
  }, [exportConfig]);

  // Download as JSON
  const handleDownload = useCallback(() => {
    const blob = new Blob([JSON.stringify(exportConfig, null, 2)], {
      type: "application/json",
    });
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    const sanitizedName = options.configName.replace(/[^a-zA-Z0-9-_]/g, "-").toLowerCase();
    a.download = `${sanitizedName}-automation-config.json`;
    a.click();
    URL.revokeObjectURL(url);
    setDownloadSuccess(true);
    setTimeout(() => setDownloadSuccess(false), 2000);
  }, [exportConfig, options.configName]);

  if (!isOpen) return null;

  const hasDiscoveredStates = discoveryResult && discoveryResult.states.length > 0;
  const sourceLabel = hasDiscoveredStates ? "Discovered States" : "State Candidates";

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50">
      <div className="bg-card border border-border rounded-lg shadow-xl w-full max-w-2xl mx-4 max-h-[90vh] flex flex-col">
        {/* Header */}
        <div className="flex items-center justify-between p-4 border-b border-border flex-shrink-0">
          <div className="flex items-center gap-2">
            <Download className="w-5 h-5 text-primary" />
            <h2 className="text-lg font-semibold">Export to Automation Config</h2>
          </div>
          <button onClick={onClose} className="p-1 hover:bg-muted rounded-md transition-colors">
            <X className="w-5 h-5" />
          </button>
        </div>

        {/* Content */}
        <div className="p-4 space-y-4 overflow-y-auto flex-1">
          {/* Source info */}
          <div className="flex items-start gap-3 p-3 bg-muted/30 rounded-lg">
            <FileJson className="w-5 h-5 text-muted-foreground flex-shrink-0 mt-0.5" />
            <div className="text-sm">
              <p className="font-medium">Export {sourceLabel}</p>
              <p className="text-muted-foreground mt-1">
                {availableStates.length} states available for export with{" "}
                {exportConfig.transitions.length} transitions.
              </p>
            </div>
          </div>

          {/* Config name input */}
          <div className="space-y-2">
            <label className="text-sm font-medium">Config Name</label>
            <input
              type="text"
              value={options.configName}
              onChange={(e) => updateOption("configName", e.target.value)}
              className="w-full px-3 py-2 bg-muted/30 border border-border rounded-md text-sm
                       focus:outline-none focus:ring-2 focus:ring-primary/50"
              placeholder="Enter a name for this config..."
            />
          </div>

          {/* Options */}
          <div className="space-y-3">
            <label className="text-sm font-medium">Export Options</label>

            <label className="flex items-center gap-2 cursor-pointer">
              <input
                type="checkbox"
                checked={options.includeGlobalStates}
                onChange={(e) => updateOption("includeGlobalStates", e.target.checked)}
                className="w-4 h-4 rounded"
              />
              <span className="text-sm">Include global states (header/footer)</span>
              <Badge variant="info" size="sm">
                Always active
              </Badge>
            </label>

            <label className="flex items-center gap-2 cursor-pointer">
              <input
                type="checkbox"
                checked={options.includeModalStates}
                onChange={(e) => updateOption("includeModalStates", e.target.checked)}
                className="w-4 h-4 rounded"
              />
              <span className="text-sm">Include modal states</span>
              <Badge variant="warning" size="sm">
                Blocking
              </Badge>
            </label>

            <label className="flex items-center gap-2 cursor-pointer">
              <input
                type="checkbox"
                checked={options.includeElementDetails}
                onChange={(e) => updateOption("includeElementDetails", e.target.checked)}
                className="w-4 h-4 rounded"
              />
              <span className="text-sm">Include element details (fingerprints)</span>
            </label>
          </div>

          {/* State selection */}
          <div className="space-y-2">
            <div className="flex items-center justify-between">
              <label className="text-sm font-medium">
                Select States (
                {options.selectedStateIds.length === 0 ? "All" : options.selectedStateIds.length}{" "}
                selected)
              </label>
              <div className="flex gap-2">
                <button onClick={selectAllStates} className="text-xs text-primary hover:underline">
                  Select All
                </button>
                <span className="text-muted-foreground">|</span>
                <button
                  onClick={deselectAllStates}
                  className="text-xs text-primary hover:underline"
                >
                  Select None
                </button>
              </div>
            </div>

            <div className="max-h-48 overflow-y-auto border border-border rounded-md p-2 space-y-1">
              {availableStates.length === 0 ? (
                <p className="text-sm text-muted-foreground text-center py-4">
                  No states available for export
                </p>
              ) : (
                availableStates.map((state) => {
                  const isSelected =
                    options.selectedStateIds.length === 0 ||
                    options.selectedStateIds.includes(state.id);

                  return (
                    <label
                      key={state.id}
                      className={`flex items-center gap-2 p-2 rounded cursor-pointer transition-colors ${
                        isSelected ? "bg-primary/10" : "hover:bg-muted/30"
                      }`}
                    >
                      <input
                        type="checkbox"
                        checked={isSelected}
                        onChange={() => toggleStateSelection(state.id)}
                        className="w-4 h-4 rounded"
                      />
                      <span className="text-sm flex-1 truncate">{state.name}</span>
                      <div className="flex items-center gap-1">
                        {state.isGlobal && (
                          <Badge variant="info" size="sm">
                            Global
                          </Badge>
                        )}
                        {state.isModal && (
                          <Badge variant="warning" size="sm">
                            Modal
                          </Badge>
                        )}
                        <Badge variant="muted" size="sm">
                          {state.elementCount} elements
                        </Badge>
                      </div>
                    </label>
                  );
                })
              )}
            </div>
          </div>

          {/* Preview toggle */}
          <div className="border border-border rounded-lg overflow-hidden">
            <button
              onClick={() => setShowPreview(!showPreview)}
              className="w-full flex items-center gap-2 p-3 hover:bg-muted/30 transition-colors text-left"
            >
              {showPreview ? (
                <ChevronDown className="w-4 h-4 text-muted-foreground" />
              ) : (
                <ChevronRight className="w-4 h-4 text-muted-foreground" />
              )}
              <Eye className="w-4 h-4 text-muted-foreground" />
              <span className="text-sm font-medium">Preview Export</span>
              <Badge variant="muted" size="sm" className="ml-auto">
                {exportConfig.states.length} states, {exportConfig.transitions.length} transitions
              </Badge>
            </button>

            {showPreview && (
              <div className="border-t border-border">
                <pre className="p-3 text-xs bg-muted/30 overflow-auto max-h-64 font-mono">
                  {JSON.stringify(exportConfig, null, 2)}
                </pre>
              </div>
            )}
          </div>

          {/* Export summary */}
          <div className="grid grid-cols-3 gap-3">
            <div className="p-3 bg-muted/30 rounded-lg text-center">
              <div className="text-2xl font-bold">{exportConfig.states.length}</div>
              <div className="text-xs text-muted-foreground">States</div>
            </div>
            <div className="p-3 bg-muted/30 rounded-lg text-center">
              <div className="text-2xl font-bold">{exportConfig.transitions.length}</div>
              <div className="text-xs text-muted-foreground">Transitions</div>
            </div>
            <div className="p-3 bg-muted/30 rounded-lg text-center">
              <div className="text-2xl font-bold">
                {Object.keys(exportConfig.fingerprintDetails || {}).length}
              </div>
              <div className="text-xs text-muted-foreground">Fingerprints</div>
            </div>
          </div>

          {/* Success messages */}
          {downloadSuccess && (
            <div
              className={`flex items-center gap-2 p-3 ${getStatusColors("success").bg} border ${getStatusColors("success").border} rounded-md`}
            >
              <Check className={`w-4 h-4 ${getStatusColors("success").icon}`} />
              <span className={`text-sm ${getStatusColors("success").text}`}>
                Config downloaded successfully!
              </span>
            </div>
          )}
        </div>

        {/* Footer */}
        <div className="flex items-center justify-between gap-2 p-4 border-t border-border flex-shrink-0">
          <div className="text-xs text-muted-foreground">
            Export format: Qontinui Automation Config v1.0
          </div>
          <div className="flex items-center gap-2">
            <Button variant="outline" size="sm" onClick={onClose}>
              Cancel
            </Button>
            <Button
              variant="outline"
              size="sm"
              onClick={handleCopy}
              disabled={exportConfig.states.length === 0}
            >
              {copied ? (
                <>
                  <Check className="w-4 h-4 mr-1.5" />
                  Copied!
                </>
              ) : (
                <>
                  <Copy className="w-4 h-4 mr-1.5" />
                  Copy JSON
                </>
              )}
            </Button>
            <Button
              variant="primary"
              size="sm"
              onClick={handleDownload}
              disabled={exportConfig.states.length === 0}
            >
              <Download className="w-4 h-4 mr-1.5" />
              Download
            </Button>
          </div>
        </div>
      </div>
    </div>
  );
}

export default ExportConfigDialog;
