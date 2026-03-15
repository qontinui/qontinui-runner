/**
 * ExportPanel — Runner-specific export for state machine configs.
 *
 * - Export to file: Uses Tauri dialog + fs plugins
 * - Load into runtime: POST to /state-machine/load via tracedFetch
 */

import { useState, useCallback } from "react";
import { Download, Loader2, FileJson, Upload } from "lucide-react";
import { save } from "@tauri-apps/plugin-dialog";
import { writeTextFile } from "@tauri-apps/plugin-fs";
import { buildExportConfig } from "@qontinui/workflow-utils";
import { getApiBase, tracedFetch } from "@/lib/runner-api";
import type { StateMachineConfigFull } from "@qontinui/shared-types";
import type { ShowToastFn } from "@/hooks/useToast";

interface ExportPanelProps {
  config: StateMachineConfigFull | null;
  showToast?: ShowToastFn;
}

export function ExportPanel({ config, showToast }: ExportPanelProps) {
  const [isExporting, setIsExporting] = useState(false);
  const [isLoading, setIsLoading] = useState(false);

  const handleExportToFile = useCallback(async () => {
    if (!config) return;
    setIsExporting(true);
    try {
      const exportData = buildExportConfig(config, config.states, config.transitions);
      const json = JSON.stringify(exportData, null, 2);

      const filePath = await save({
        defaultPath: `${config.name.replace(/\s+/g, "_")}_state_machine.json`,
        filters: [{ name: "JSON", extensions: ["json"] }],
      });

      if (filePath) {
        await writeTextFile(filePath, json);
        showToast?.(`Exported to ${filePath}`, "success");
        console.log(`Exported to ${filePath}`);
      }
    } catch (err) {
      const errMsg = `Export failed: ${err instanceof Error ? err.message : "Unknown error"}`;
      showToast?.(errMsg, "error");
      console.warn(errMsg);
    } finally {
      setIsExporting(false);
    }
  }, [config, showToast]);

  const handleLoadIntoRuntime = useCallback(async () => {
    if (!config) return;
    setIsLoading(true);
    try {
      const exportData = buildExportConfig(config, config.states, config.transitions);
      const res = await tracedFetch(`${getApiBase()}/state-machine/load`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(exportData),
      });

      if (!res.ok) {
        const err = await res.json().catch(() => ({}));
        throw new Error(err.error || `HTTP ${res.status}`);
      }

      showToast?.("State machine loaded into runtime", "success");
      console.log("State machine loaded into runtime");
    } catch (err) {
      const loadErrMsg = `Load failed: ${err instanceof Error ? err.message : "Unknown error"}`;
      showToast?.(loadErrMsg, "error");
      console.warn(loadErrMsg);
    } finally {
      setIsLoading(false);
    }
  }, [config, showToast]);

  return (
    <div className="p-6 space-y-6 max-w-lg mx-auto">
      <div className="flex items-center gap-2 mb-4">
        <FileJson className="size-5 text-brand-primary" />
        <h2 className="text-lg font-semibold text-text-primary">Export</h2>
      </div>

      <div className="rounded-lg border border-border-primary bg-surface-secondary p-4 space-y-3">
        <p className="text-sm text-text-primary">
          Export the state machine as JSON compatible with{" "}
          <code className="text-xs bg-surface-primary px-1 py-0.5 rounded">
            UIBridgeRuntime.from_dict()
          </code>
        </p>

        <div className="text-xs text-text-muted space-y-1">
          {config && (
            <div>
              Config: <strong>{config.name}</strong>
            </div>
          )}
          <div>{config?.states.length ?? 0} states</div>
          <div>{config?.transitions.length ?? 0} transitions</div>
        </div>

        <div className="flex gap-2">
          <button
            onClick={handleExportToFile}
            disabled={isExporting || !config}
            className="inline-flex items-center justify-center gap-1.5 rounded-md px-3 py-2 text-sm font-medium bg-brand-primary text-white hover:bg-brand-primary/90 disabled:opacity-50"
          >
            {isExporting ? (
              <Loader2 className="size-4 animate-spin" />
            ) : (
              <Download className="size-4" />
            )}
            Save to File
          </button>

          <button
            onClick={handleLoadIntoRuntime}
            disabled={isLoading || isExporting || !config}
            className="inline-flex items-center justify-center gap-1.5 rounded-md px-3 py-2 text-sm font-medium border border-border-primary text-text-primary hover:bg-surface-secondary disabled:opacity-50"
          >
            {isLoading ? (
              <Loader2 className="size-4 animate-spin" />
            ) : (
              <Upload className="size-4" />
            )}
            Load into Runtime
          </button>
        </div>
      </div>
    </div>
  );
}
