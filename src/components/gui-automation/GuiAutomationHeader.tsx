/**
 * GuiAutomationHeader Component
 *
 * Header section with config status indicator (animated ping, active badge).
 * Shows "GUI Automation" title with "Mission Control" subtitle.
 * Displays config status: loaded (green ping + badge) or not loaded (amber warning).
 */

import { useState } from "react";
import { Play, FileText, Settings, X, FolderOpen, Sparkles } from "lucide-react";
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
      <div className="flex-shrink-0 border-b border-border bg-card/50 px-6 py-4">
        <div className="flex items-center justify-between">
          {/* Left side: Logo + Title */}
          <div className="flex items-center gap-4">
            {/* Icon container */}
            <div className="flex h-12 w-12 items-center justify-center rounded-lg bg-primary/20">
              <Play className="h-6 w-6 text-primary" />
            </div>

            {/* Title + Subtitle */}
            <div>
              <h1 className="text-xl font-semibold text-foreground">GUI Automation</h1>
              <p className="text-sm text-muted-foreground">Mission Control</p>
            </div>
          </div>

          {/* Right side: Config status or Load button */}
          <div className="flex items-center gap-3">
            {configLoaded && config ? (
              // Config loaded state
              <div className="flex items-center gap-3">
                {/* Status indicator with animated ping */}
                <div className="flex items-center gap-2">
                  <span className="relative flex h-2.5 w-2.5">
                    <span className="absolute inline-flex h-full w-full animate-ping rounded-full bg-primary opacity-75" />
                    <span className="relative inline-flex h-2.5 w-2.5 rounded-full bg-primary" />
                  </span>
                  <span className="text-sm text-muted-foreground">Active Config:</span>
                  <Badge variant="info" size="md">
                    {config.name}
                  </Badge>
                </div>

                {/* Change button - dropdown menu */}
                <DropdownMenu.Root open={loadMenuOpen} onOpenChange={setLoadMenuOpen}>
                  <DropdownMenu.Trigger asChild>
                    <button className="px-3 py-1.5 text-sm rounded-md border border-border bg-secondary/50 hover:bg-secondary transition-colors">
                      Change
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
                {onUnloadConfig && (
                  <button
                    onClick={onUnloadConfig}
                    className="px-3 py-1.5 text-sm rounded-md border border-destructive/50 text-destructive hover:bg-destructive/10 transition-colors"
                  >
                    <X className="w-4 h-4" />
                  </button>
                )}
              </div>
            ) : (
              // No config loaded state - warning style
              <div className="flex items-center gap-3">
                <div className="flex items-center gap-2 px-3 py-2 rounded-lg border border-amber-500/50 bg-amber-500/10">
                  <span className="relative flex h-2.5 w-2.5">
                    <span className="relative inline-flex h-2.5 w-2.5 rounded-full bg-amber-500" />
                  </span>
                  <span className="text-sm text-amber-500">No config loaded</span>
                </div>

                {/* Load config dropdown */}
                <DropdownMenu.Root open={loadMenuOpen} onOpenChange={setLoadMenuOpen}>
                  <DropdownMenu.Trigger asChild>
                    <button
                      className="flex items-center gap-2 px-4 py-2 text-sm font-medium rounded-lg bg-primary text-primary-foreground hover:bg-primary/90 transition-colors"
                      data-tutorial-id="load-config-button"
                    >
                      <FileText className="w-4 h-4" />
                      Load Configuration
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
              </div>
            )}

            {/* Settings button */}
            <button
              className="p-2 rounded-lg text-muted-foreground hover:text-foreground hover:bg-muted/50 transition-colors"
              title="Settings"
            >
              <Settings className="w-5 h-5" />
            </button>
          </div>
        </div>
      </div>

      {/* RAG Project Modal */}
      <RAGProjectModal isOpen={showRAGModal} onClose={() => setShowRAGModal(false)} onLog={onLog} />
    </>
  );
}
