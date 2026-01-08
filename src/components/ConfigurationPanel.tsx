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
        <div className="space-y-3">
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
            className="w-full flex items-center justify-center gap-2 px-3 py-2 text-xs rounded-lg bg-muted/50 hover:bg-muted transition-colors disabled:opacity-50"
          >
            <FileText className="w-3.5 h-3.5" />
            {isLoadingLastConfig ? "Loading..." : "Load Last Config"}
          </button>

          {config && (
            <div className="p-3 rounded-lg bg-blue-500/10">
              <p className="font-medium text-sm text-blue-400 mb-1">{config.name}</p>
              <div className="text-xs text-muted-foreground flex gap-3">
                <span>States: {config.statesCount}</span>
                <span>Workflows: {config.workflowsCount}</span>
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
