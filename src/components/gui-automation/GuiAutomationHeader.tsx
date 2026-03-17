/**
 * GuiAutomationHeader Component
 *
 * Compact header matching the runner's modern page header pattern.
 * Shows title + config status badge and load dropdown.
 */

import { useState } from "react";
import { FileText, FolderOpen, Sparkles, X } from "lucide-react";
import * as DropdownMenu from "@radix-ui/react-dropdown-menu";
import { RAGProjectModal } from "../RAGProjectModal";
import { Badge } from "../ui/Badge";
import type { Config } from "../../contexts/ExecutionContext";

type LogLevel = "info" | "warning" | "error" | "debug" | "success";

interface GuiAutomationHeaderProps {
  config: Config | null;
  configLoaded: boolean;
  onLoadConfiguration: () => void;
  onLoadLastConfiguration: () => Promise<void>;
  onUnloadConfig?: () => void;
  onLog?: (level: LogLevel, message: string) => void;
}

export function GuiAutomationHeader({
  config,
  configLoaded,
  onLoadConfiguration,
  onLoadLastConfiguration,
  onUnloadConfig,
  onLog,
}: GuiAutomationHeaderProps) {
  const [showRAGModal, setShowRAGModal] = useState(false);
  const [isLoadingLastConfig, setIsLoadingLastConfig] = useState(false);
  const [loadMenuOpen, setLoadMenuOpen] = useState(false);

  const handleLoadLastConfig = async () => {
    if (isLoadingLastConfig) return;
    setIsLoadingLastConfig(true);
    try {
      await onLoadLastConfiguration();
    } catch (error) {
      console.error("[GuiAutomationHeader] Load failed:", error);
    } finally {
      setIsLoadingLastConfig(false);
    }
  };

  return (
    <>
      <div className="flex items-center justify-between px-4 py-3 border-b border-border">
        <h1 className="text-h2 text-foreground">GUI Automation</h1>

        <div className="flex items-center gap-2">
          {/* Config status indicator */}
          {configLoaded && config ? (
            <div className="flex items-center gap-2">
              <span className="relative flex h-2 w-2">
                <span className="absolute inline-flex h-full w-full animate-ping rounded-full bg-green-500 opacity-75" />
                <span className="relative inline-flex h-2 w-2 rounded-full bg-green-500" />
              </span>
              <Badge variant="info" size="sm">
                {config.name}
              </Badge>
            </div>
          ) : (
            <Badge variant="warning" size="sm">
              No config
            </Badge>
          )}

          {/* Load/Change config dropdown */}
          <DropdownMenu.Root open={loadMenuOpen} onOpenChange={setLoadMenuOpen}>
            <DropdownMenu.Trigger asChild>
              <button
                className={`${configLoaded ? "btn-secondary" : "btn-primary"} flex items-center gap-2`}
                data-tutorial-id="load-config-button"
              >
                <FolderOpen className="w-4 h-4" />
                {configLoaded ? "Change" : "Load Config"}
              </button>
            </DropdownMenu.Trigger>
            <DropdownMenu.Portal>
              <DropdownMenu.Content
                className="min-w-[200px] bg-card/95 backdrop-blur rounded-lg shadow-xl p-1 animate-slideDown z-50"
                sideOffset={5}
                align="end"
              >
                <DropdownMenu.Item
                  className="flex items-center gap-2 px-3 py-2 text-sm rounded-md cursor-pointer outline-none hover:bg-muted/50 transition-colors"
                  onSelect={() => {
                    setLoadMenuOpen(false);
                    onLoadConfiguration();
                  }}
                >
                  <FolderOpen className="w-4 h-4 text-blue-400" />
                  <span className="flex-1">Load from File</span>
                </DropdownMenu.Item>
                <DropdownMenu.Item
                  className="flex items-center gap-2 px-3 py-2 text-sm rounded-md cursor-pointer outline-none hover:bg-muted/50 transition-colors"
                  onSelect={() => {
                    setLoadMenuOpen(false);
                    handleLoadLastConfig();
                  }}
                >
                  <FileText className="w-4 h-4 text-blue-400" />
                  <span className="flex-1">
                    {isLoadingLastConfig ? "Loading..." : "Load Last Config"}
                  </span>
                </DropdownMenu.Item>
                <DropdownMenu.Separator className="h-px bg-border/50 my-1" />
                <DropdownMenu.Item
                  className="flex items-center gap-2 px-3 py-2 text-sm rounded-md cursor-pointer outline-none hover:bg-muted/50 transition-colors"
                  onSelect={() => {
                    setLoadMenuOpen(false);
                    setShowRAGModal(true);
                  }}
                >
                  <Sparkles className="w-4 h-4 text-purple-400" />
                  <span className="flex-1">Load RAG Project</span>
                </DropdownMenu.Item>
              </DropdownMenu.Content>
            </DropdownMenu.Portal>
          </DropdownMenu.Root>

          {/* Unload button */}
          {configLoaded && onUnloadConfig && (
            <button
              onClick={onUnloadConfig}
              className="p-1.5 rounded-md text-muted-foreground hover:text-destructive hover:bg-destructive/10 transition-colors"
              title="Unload configuration"
            >
              <X className="w-4 h-4" />
            </button>
          )}
        </div>
      </div>

      <RAGProjectModal isOpen={showRAGModal} onClose={() => setShowRAGModal(false)} onLog={onLog} />
    </>
  );
}
