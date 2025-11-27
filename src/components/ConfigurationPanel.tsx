/**
 * ConfigurationPanel Component
 *
 * Handles configuration loading and display.
 * Single responsibility: Configuration management UI.
 */

import { FileText } from "lucide-react";
import CollapsiblePanel from "./CollapsiblePanel";
import type { Config } from "../contexts/ExecutionContext";

export interface ConfigurationPanelProps {
  config: Config | null;
  collapsed: boolean;
  onToggle: (collapsed: boolean) => void;
  onLoadConfiguration: () => void;
  onLoadLastConfiguration: () => void;
}

export function ConfigurationPanel({
  config,
  collapsed,
  onToggle,
  onLoadConfiguration,
  onLoadLastConfiguration,
}: ConfigurationPanelProps) {
  return (
    <CollapsiblePanel
      title="Configuration"
      icon={<FileText className="w-4 h-4" />}
      collapsed={collapsed}
      onToggle={onToggle}
    >
      <div className="space-y-4">
        <button
          onClick={onLoadConfiguration}
          className="w-full btn-primary flex items-center justify-center gap-2"
        >
          <FileText className="w-4 h-4" />
          Load Configuration
        </button>

        <button
          onClick={onLoadLastConfiguration}
          className="w-full btn-secondary flex items-center justify-center gap-2"
        >
          <FileText className="w-4 h-4" />
          Load Last Config
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
  );
}
