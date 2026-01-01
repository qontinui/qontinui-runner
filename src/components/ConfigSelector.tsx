/**
 * ConfigSelector.tsx
 *
 * A reusable component for selecting stored configurations.
 * Shows a dropdown with available configs from ConfigStorageService.
 * Includes warnings when GUI steps exist but no config is selected.
 */

import { useState, useEffect, useCallback } from "react";
import {
  AlertTriangle,
  ChevronDown,
  Database,
  FileUp,
  FolderOpen,
  Loader2,
  RefreshCw,
  X,
} from "lucide-react";
import { open } from "@tauri-apps/plugin-dialog";
import { ConfigStorageService, type StoredConfigMetadata } from "../services/ConfigStorageService";

interface ConfigSelectorProps {
  /** Currently selected config ID */
  selectedConfigId: string | null;
  /** Called when a config is selected */
  onConfigSelect: (configId: string | null) => void;
  /** Whether the workflow/prompt has GUI steps that require a config */
  hasGuiSteps?: boolean;
  /** Compact mode for smaller spaces */
  compact?: boolean;
  /** Optional class name */
  className?: string;
  /** Show import button */
  showImport?: boolean;
  /** Callback when a new config is imported */
  onConfigImported?: (configId: string) => void;
}

export function ConfigSelector({
  selectedConfigId,
  onConfigSelect,
  hasGuiSteps = false,
  compact = false,
  className = "",
  showImport = true,
  onConfigImported,
}: ConfigSelectorProps) {
  const [configs, setConfigs] = useState<StoredConfigMetadata[]>([]);
  const [loading, setLoading] = useState(true);
  const [isOpen, setIsOpen] = useState(false);
  const [importing, setImporting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Fetch configs on mount
  const loadConfigs = useCallback(async () => {
    try {
      setLoading(true);
      setError(null);
      const configList = await ConfigStorageService.listConfigs();
      setConfigs(configList);
    } catch (err) {
      console.error("Failed to load configs:", err);
      setError(err instanceof Error ? err.message : "Failed to load configs");
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    loadConfigs();
  }, [loadConfigs]);

  // Handle import config
  const handleImportConfig = async () => {
    try {
      setImporting(true);
      setError(null);

      // Open file dialog
      const selected = await open({
        multiple: false,
        filters: [
          {
            name: "Config Files",
            extensions: ["json", "yaml", "yml"],
          },
        ],
      });

      if (!selected) {
        setImporting(false);
        return;
      }

      const filePath = typeof selected === "string" ? selected : selected;

      // Extract name from path
      const fileName = filePath.split(/[\\/]/).pop() || "Imported Config";
      const configName = fileName.replace(/\.(json|yaml|yml)$/, "");

      // Import the config
      const result = await ConfigStorageService.importConfig(filePath, configName);

      // Reload configs
      await loadConfigs();

      // Select the newly imported config
      onConfigSelect(result.id);
      onConfigImported?.(result.id);

      setIsOpen(false);
    } catch (err) {
      console.error("Failed to import config:", err);
      setError(err instanceof Error ? err.message : "Failed to import config");
    } finally {
      setImporting(false);
    }
  };

  // Get selected config details
  const selectedConfig = configs.find((c) => c.id === selectedConfigId);

  // Show warning when GUI steps exist but no config is selected
  const showWarning = hasGuiSteps && !selectedConfigId;

  const buttonPadding = compact ? "px-2 py-1.5" : "px-3 py-2";
  const iconSize = compact ? "w-3 h-3" : "w-4 h-4";
  const textSize = compact ? "text-xs" : "text-sm";

  return (
    <div className={`space-y-2 ${className}`}>
      {/* Warning when GUI steps exist without config */}
      {showWarning && (
        <div className="flex items-start gap-2 p-2 bg-amber-500/10 border border-amber-500/30 rounded-lg text-xs">
          <AlertTriangle className="w-4 h-4 flex-shrink-0 text-amber-600 mt-0.5" />
          <div className="text-amber-600">
            <span className="font-medium">Config Required:</span> This workflow contains GUI
            automation steps (workflows, states, or click actions) that require a stored
            configuration. Please select or import a configuration.
          </div>
        </div>
      )}

      {/* Config Selector Dropdown */}
      <div className="relative">
        <button
          onClick={() => setIsOpen(!isOpen)}
          disabled={loading}
          className={`w-full flex items-center justify-between gap-2 ${buttonPadding} ${textSize} bg-card border border-border rounded-lg hover:border-primary/50 transition-colors ${
            showWarning ? "border-amber-500/50" : ""
          }`}
        >
          <div className="flex items-center gap-2 min-w-0">
            {loading ? (
              <Loader2 className={`${iconSize} animate-spin text-muted-foreground`} />
            ) : (
              <Database
                className={`${iconSize} ${selectedConfig ? "text-primary" : "text-muted-foreground"}`}
              />
            )}
            {selectedConfig ? (
              <div className="flex items-center gap-2 min-w-0">
                <span className="font-medium truncate">{selectedConfig.name}</span>
                <span className="text-muted-foreground text-xs">
                  ({selectedConfig.workflow_count} workflows, {selectedConfig.state_count} states)
                </span>
              </div>
            ) : (
              <span className="text-muted-foreground">
                {loading ? "Loading configs..." : "Select a configuration..."}
              </span>
            )}
          </div>
          <div className="flex items-center gap-1">
            {selectedConfig && (
              <button
                onClick={(e) => {
                  e.stopPropagation();
                  onConfigSelect(null);
                }}
                className="p-1 hover:bg-muted rounded transition-colors"
                title="Clear selection"
              >
                <X className="w-3 h-3 text-muted-foreground" />
              </button>
            )}
            <ChevronDown
              className={`${iconSize} text-muted-foreground transition-transform ${isOpen ? "rotate-180" : ""}`}
            />
          </div>
        </button>

        {/* Dropdown Menu */}
        {isOpen && (
          <div className="absolute z-20 w-full mt-1 bg-card border border-border rounded-lg shadow-lg max-h-64 overflow-y-auto">
            {/* Refresh Button */}
            <div className="sticky top-0 bg-card border-b border-border px-3 py-2 flex items-center justify-between">
              <span className="text-xs font-medium text-muted-foreground">
                Stored Configurations
              </span>
              <button
                onClick={(e) => {
                  e.stopPropagation();
                  loadConfigs();
                }}
                className="p-1 hover:bg-muted rounded transition-colors"
                title="Refresh list"
              >
                <RefreshCw className={`${iconSize} text-muted-foreground`} />
              </button>
            </div>

            {/* Error State */}
            {error && (
              <div className="px-3 py-2 text-xs text-red-500 bg-red-500/10">{error}</div>
            )}

            {/* Config List */}
            {configs.length === 0 ? (
              <div className="px-3 py-4 text-center text-sm text-muted-foreground">
                <FolderOpen className="w-8 h-8 mx-auto mb-2 opacity-50" />
                <p>No stored configurations found.</p>
                <p className="text-xs mt-1">Import a configuration to get started.</p>
              </div>
            ) : (
              configs.map((config) => (
                <button
                  key={config.id}
                  onClick={() => {
                    onConfigSelect(config.id);
                    setIsOpen(false);
                  }}
                  className={`w-full flex flex-col items-start gap-1 px-3 py-2 text-left hover:bg-muted/50 transition-colors ${
                    config.id === selectedConfigId ? "bg-primary/10" : ""
                  }`}
                >
                  <div className="flex items-center gap-2 w-full">
                    <Database
                      className={`${iconSize} flex-shrink-0 ${
                        config.id === selectedConfigId ? "text-primary" : "text-muted-foreground"
                      }`}
                    />
                    <span className="font-medium truncate flex-1">{config.name}</span>
                  </div>
                  <div className="flex items-center gap-3 text-xs text-muted-foreground pl-6">
                    <span>{config.workflow_count} workflows</span>
                    <span>{config.state_count} states</span>
                    <span>{config.image_count} images</span>
                  </div>
                  {config.description && (
                    <p className="text-xs text-muted-foreground pl-6 line-clamp-1">
                      {config.description}
                    </p>
                  )}
                </button>
              ))
            )}

            {/* Import Button */}
            {showImport && (
              <div className="sticky bottom-0 bg-card border-t border-border px-3 py-2">
                <button
                  onClick={handleImportConfig}
                  disabled={importing}
                  className="w-full flex items-center justify-center gap-2 px-3 py-2 text-sm bg-primary/10 text-primary rounded hover:bg-primary/20 transition-colors disabled:opacity-50"
                >
                  {importing ? (
                    <Loader2 className={`${iconSize} animate-spin`} />
                  ) : (
                    <FileUp className={iconSize} />
                  )}
                  {importing ? "Importing..." : "Import Configuration"}
                </button>
              </div>
            )}
          </div>
        )}
      </div>
    </div>
  );
}

/**
 * Helper hook to determine if execution steps have GUI steps
 * that require a config to be loaded.
 */
export function hasGuiStepsInExecutionList(
  steps: Array<{ type: string }>
): boolean {
  const guiStepTypes = ["workflow", "state", "action"];
  return steps.some((step) => guiStepTypes.includes(step.type));
}

export default ConfigSelector;
