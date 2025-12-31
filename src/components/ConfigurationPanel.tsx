/**
 * ConfigurationPanel Component
 *
 * Handles configuration loading and display.
 * Single responsibility: Configuration management UI.
 */

import { useState } from "react";
import { FileText } from "lucide-react";
import CollapsiblePanel from "./CollapsiblePanel";
import { ConfigurationLoadMenu } from "./ConfigurationLoadMenu";
import { RAGProjectModal } from "./RAGProjectModal";
import type { Config } from "../contexts/ExecutionContext";

export interface ConfigurationPanelProps {
  config: Config | null;
  onLoadConfiguration: () => void;
  onLoadLastConfiguration: () => Promise<void>;
  onLog?: (level: "info" | "warning" | "error" | "debug" | "success", message: string) => void;
}

export function ConfigurationPanel({
  config,
  onLoadConfiguration,
  onLoadLastConfiguration,
  onLog,
}: ConfigurationPanelProps) {
  const [showRAGModal, setShowRAGModal] = useState(false);
  const [isLoadingLastConfig, setIsLoadingLastConfig] = useState(false);

  const handleLoadLastConfig = async () => {
    if (isLoadingLastConfig) {
      return;
    }

    setIsLoadingLastConfig(true);

    try {
      await onLoadLastConfiguration();
    } catch (error: unknown) {
      console.error("[ConfigurationPanel] Load failed:", error);
    } finally {
      setIsLoadingLastConfig(false);
    }
  };

  return (
    <>
      <CollapsiblePanel
        title="Configuration"
        icon={<FileText className="w-4 h-4" />}
        collapsible={false}
      >
        <div className="space-y-4">
          {/* Configuration Load Menu */}
          <ConfigurationLoadMenu
            onLoadFromFile={onLoadConfiguration}
            onLoadRAG={() => setShowRAGModal(true)}
          />

          <button
            key="load-last-config-btn"
            onClick={handleLoadLastConfig}
            disabled={isLoadingLastConfig}
            type="button"
            className="w-full btn-secondary flex items-center justify-center gap-2 disabled:opacity-50"
          >
            <FileText className="w-4 h-4" />
            {isLoadingLastConfig ? "Loading..." : "Load Last Config"}
          </button>

          {config && (
            <div className="space-y-2">
              <div className="p-3 bg-accent/50 rounded-lg border border-border/50">
                <p className="font-medium text-sm mb-2">{config.name}</p>
                <div className="text-sm space-y-1">
                  <p className="text-muted-foreground">States: {config.statesCount}</p>
                  <p className="text-muted-foreground">Workflows: {config.workflowsCount}</p>
                </div>
              </div>
            </div>
          )}
        </div>
      </CollapsiblePanel>

      {/* RAG Project Modal */}
      <RAGProjectModal isOpen={showRAGModal} onClose={() => setShowRAGModal(false)} onLog={onLog} />
    </>
  );
}
