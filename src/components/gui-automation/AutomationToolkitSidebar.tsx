/**
 * AutomationToolkitSidebar Component
 *
 * Sidebar with tabbed interface containing Quick Actions and Macros.
 * Features:
 * - Tabs for Quick Actions | Macros
 * - Quick Actions: Click (with image dropdown), Type, Hotkey, Go To State
 * - Macros: Dropdown selector with play button, macro cards grid
 */

import { useState, useMemo } from "react";
import {
  MousePointer2,
  Type,
  Keyboard,
  Navigation,
  Play,
  Loader2,
  AlertCircle,
  Wrench,
  FolderOpen,
} from "lucide-react";
import { Card, CardHeader, CardTitle, CardContent } from "../ui/Card";
import { Tabs, TabsList, TabsTrigger, TabsContent } from "../ui/Tabs";
import { getAccentColors } from "@/design-system";
import { cn } from "../../lib/utils";
import type { SavedMacro } from "../../types";
import type { Config } from "../../contexts/ExecutionContext";

const API_BASE = "http://localhost:9876";

type ActionType = "click" | "double_click" | "right_click" | "type" | "hotkey" | "go_to_state";
type LogLevel = "info" | "warning" | "error" | "success";

interface StateInfo {
  id: string;
  name: string;
}

interface ImageInfo {
  id: string;
  name: string;
  stateName: string;
}

interface AutomationToolkitSidebarProps {
  config: Config | null;
  configLoaded: boolean;
  selectedMonitors: number[];
  macros: SavedMacro[];
  macrosLoading?: boolean;
  onRunMacro: (macro: SavedMacro) => Promise<void>;
  runningMacroId?: string | null;
  onLog?: (level: LogLevel, message: string) => void;
}

export function AutomationToolkitSidebar({
  config,
  configLoaded,
  selectedMonitors,
  macros,
  macrosLoading = false,
  onRunMacro,
  runningMacroId,
  onLog,
}: AutomationToolkitSidebarProps) {
  // Quick Actions state
  const [clickAction, setClickAction] = useState<"click" | "double_click" | "right_click">("click");
  const [selectedImageId, setSelectedImageId] = useState<string>("");
  const [typeText, setTypeText] = useState("");
  const [hotkeyText, setHotkeyText] = useState("");
  const [selectedStateId, setSelectedStateId] = useState<string>("");
  const [runningAction, setRunningAction] = useState<ActionType | null>(null);

  // Macros state
  const [selectedMacroId, setSelectedMacroId] = useState<string | null>(null);

  // Parse states and images from config
  const { states, images } = useMemo(() => {
    const stateList: StateInfo[] = [];
    const imageList: ImageInfo[] = [];

    if (config?.states && Array.isArray(config.states)) {
      for (const state of config.states) {
        const stateName = state.name || state.id || "Unknown";
        stateList.push({
          id: state.id || "",
          name: stateName,
        });

        const stateImages = state.stateImages || state.images;
        if (stateImages && Array.isArray(stateImages)) {
          for (const img of stateImages) {
            const imgId = img.id || "";
            const imgName = img.name || img.id || "";
            if (imgId && imgName) {
              imageList.push({ id: imgId, name: imgName, stateName });
            }
          }
        }
      }
    }

    return { states: stateList, images: imageList };
  }, [config]);

  const selectedMacro = macros.find((m) => m.id === selectedMacroId) || null;

  const log = (level: LogLevel, message: string) => {
    if (onLog) {
      onLog(level, message);
    } else {
      console.log(`[${level}] ${message}`);
    }
  };

  // Execute click action
  const executeClick = async () => {
    if (!selectedImageId) {
      log("warning", "Please select an image to click");
      return;
    }

    const image = images.find((img) => img.id === selectedImageId);
    if (!image) return;

    setRunningAction(clickAction);
    try {
      const response = await fetch(`${API_BASE}/execute-action`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          action_type: clickAction,
          image_id: selectedImageId,
          monitor_index: selectedMonitors[0] ?? 0,
        }),
      });

      const result = await response.json();
      if (result.success) {
        log("success", `${clickAction} on "${image.name}" completed`);
      } else {
        throw new Error(result.error || "Action failed");
      }
    } catch (error) {
      log("error", `Failed to execute ${clickAction}: ${error}`);
    } finally {
      setRunningAction(null);
    }
  };

  // Execute type action
  const executeType = async () => {
    if (!typeText.trim()) {
      log("warning", "Please enter text to type");
      return;
    }

    setRunningAction("type");
    try {
      const response = await fetch(`${API_BASE}/execute-action`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          action_type: "type",
          text_input: typeText,
        }),
      });

      const result = await response.json();
      if (result.success) {
        log("success", `Typed "${typeText.substring(0, 30)}${typeText.length > 30 ? "..." : ""}"`);
      } else {
        throw new Error(result.error || "Action failed");
      }
    } catch (error) {
      log("error", `Failed to type: ${error}`);
    } finally {
      setRunningAction(null);
    }
  };

  // Execute hotkey action
  const executeHotkey = async () => {
    if (!hotkeyText.trim()) {
      log("warning", "Please enter a hotkey combination");
      return;
    }

    setRunningAction("hotkey");
    try {
      const response = await fetch(`${API_BASE}/execute-action`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          action_type: "hotkey",
          hotkey: hotkeyText,
        }),
      });

      const result = await response.json();
      if (result.success) {
        log("success", `Hotkey "${hotkeyText}" executed`);
      } else {
        throw new Error(result.error || "Action failed");
      }
    } catch (error) {
      log("error", `Failed to execute hotkey: ${error}`);
    } finally {
      setRunningAction(null);
    }
  };

  // Execute go to state action
  const executeGoToState = async () => {
    if (!selectedStateId) {
      log("warning", "Please select a target state");
      return;
    }

    const state = states.find((s) => s.id === selectedStateId);
    if (!state) return;

    setRunningAction("go_to_state");
    try {
      const response = await fetch(`${API_BASE}/go-to-state`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          target_state_ids: [selectedStateId],
          target_state_names: [state.name],
          monitor_index: selectedMonitors[0] ?? 0,
        }),
      });

      const result = await response.json();
      if (result.success) {
        log("success", `Navigation to "${state.name}" completed`);
      } else {
        throw new Error(result.error || "Navigation failed");
      }
    } catch (error) {
      log("error", `Failed to navigate: ${error}`);
    } finally {
      setRunningAction(null);
    }
  };

  const handleRunMacro = async () => {
    if (selectedMacro) {
      await onRunMacro(selectedMacro);
    }
  };

  return (
    <Card>
      <CardHeader>
        <CardTitle>
          <Wrench className="w-5 h-5 text-primary" />
          Automation Toolkit
        </CardTitle>
      </CardHeader>
      <CardContent>
        <Tabs defaultValue="quick-actions" className="w-full">
          <TabsList className="grid w-full grid-cols-2" data-ui-id="toolkit-tabs">
            <TabsTrigger value="quick-actions" data-ui-id="toolkit-tab-actions">
              Quick Actions
            </TabsTrigger>
            <TabsTrigger value="macros" data-ui-id="toolkit-tab-macros">
              Macros
            </TabsTrigger>
          </TabsList>

          {/* Quick Actions Tab */}
          <TabsContent value="quick-actions">
            {!configLoaded ? (
              <div className="flex items-center gap-2 p-4 text-sm text-muted-foreground">
                <AlertCircle className="w-4 h-4" />
                <span>Load a configuration to use quick actions</span>
              </div>
            ) : (
              <div className="space-y-4 pt-4">
                {/* Action Type Grid */}
                <div className="grid grid-cols-2 gap-2">
                  <button
                    onClick={() => setClickAction("click")}
                    className={cn(
                      "flex items-center gap-2 px-3 py-2 text-sm rounded-lg border transition-colors",
                      clickAction === "click"
                        ? "border-blue-500/50 bg-blue-500/10 text-blue-400"
                        : "border-border bg-secondary/30 hover:bg-secondary/50",
                    )}
                  >
                    <MousePointer2 className="w-4 h-4" />
                    Click
                  </button>
                  <button
                    onClick={() => setClickAction("double_click")}
                    className={cn(
                      "flex items-center gap-2 px-3 py-2 text-sm rounded-lg border transition-colors",
                      clickAction === "double_click"
                        ? "border-blue-500/50 bg-blue-500/10 text-blue-400"
                        : "border-border bg-secondary/30 hover:bg-secondary/50",
                    )}
                  >
                    <MousePointer2 className="w-4 h-4" />
                    Double
                  </button>
                  <button
                    onClick={() => setClickAction("right_click")}
                    className={cn(
                      "flex items-center gap-2 px-3 py-2 text-sm rounded-lg border transition-colors",
                      clickAction === "right_click"
                        ? "border-blue-500/50 bg-blue-500/10 text-blue-400"
                        : "border-border bg-secondary/30 hover:bg-secondary/50",
                    )}
                  >
                    <MousePointer2 className="w-4 h-4" />
                    Right
                  </button>
                  <button
                    disabled
                    className="flex items-center gap-2 px-3 py-2 text-sm rounded-lg border border-border bg-secondary/30 text-muted-foreground"
                  >
                    <Type className="w-4 h-4" />
                    Type
                  </button>
                </div>

                {/* Target Image Dropdown (for click actions) */}
                <div className="space-y-2">
                  <label className="text-xs text-muted-foreground">Target Image</label>
                  <select
                    value={selectedImageId}
                    onChange={(e) => setSelectedImageId(e.target.value)}
                    className="w-full px-3 py-2 text-sm bg-secondary/50 border border-border rounded-lg focus:outline-none focus:ring-1 focus:ring-primary"
                  >
                    <option value="">Select image...</option>
                    {images.map((img) => (
                      <option key={img.id} value={img.id}>
                        {img.name} ({img.stateName})
                      </option>
                    ))}
                  </select>
                  <button
                    onClick={executeClick}
                    disabled={!selectedImageId || runningAction !== null}
                    className={cn(
                      "w-full flex items-center justify-center gap-2 px-4 py-2.5 text-sm font-medium rounded-lg transition-colors",
                      "bg-blue-600 text-white hover:bg-blue-700",
                      "disabled:opacity-50 disabled:cursor-not-allowed",
                    )}
                  >
                    {runningAction === clickAction ? (
                      <Loader2 className="w-4 h-4 animate-spin" />
                    ) : (
                      <Play className="w-4 h-4" />
                    )}
                    Execute Click
                  </button>
                </div>

                {/* Divider */}
                <div className="border-t border-border" />

                {/* Type Action */}
                <div className="space-y-2">
                  <div className="flex items-center gap-2">
                    <Type className={`w-4 h-4 ${getAccentColors("amber").text}`} />
                    <label className="text-xs text-muted-foreground">Type Text</label>
                  </div>
                  <div className="flex gap-2">
                    <input
                      type="text"
                      value={typeText}
                      onChange={(e) => setTypeText(e.target.value)}
                      placeholder="Enter text to type..."
                      className="flex-1 px-3 py-2 text-sm bg-secondary/50 border border-border rounded-lg focus:outline-none focus:ring-1 focus:ring-primary"
                      onKeyDown={(e) => {
                        if (e.key === "Enter" && typeText.trim()) {
                          executeType();
                        }
                      }}
                    />
                    <button
                      onClick={executeType}
                      disabled={!typeText.trim() || runningAction !== null}
                      className={cn(
                        "px-3 py-2 rounded-lg transition-colors",
                        getAccentColors("amber").bg,
                        getAccentColors("amber").text,
                        "hover:opacity-80 disabled:opacity-50 disabled:cursor-not-allowed",
                      )}
                    >
                      {runningAction === "type" ? (
                        <Loader2 className="w-4 h-4 animate-spin" />
                      ) : (
                        <Play className="w-4 h-4" />
                      )}
                    </button>
                  </div>
                </div>

                {/* Hotkey Action */}
                <div className="space-y-2">
                  <div className="flex items-center gap-2">
                    <Keyboard className={`w-4 h-4 ${getAccentColors("purple").text}`} />
                    <label className="text-xs text-muted-foreground">Hotkey</label>
                  </div>
                  <div className="flex gap-2">
                    <input
                      type="text"
                      value={hotkeyText}
                      onChange={(e) => setHotkeyText(e.target.value)}
                      placeholder="e.g., ctrl+c, alt+tab"
                      className="flex-1 px-3 py-2 text-sm bg-secondary/50 border border-border rounded-lg focus:outline-none focus:ring-1 focus:ring-primary"
                      onKeyDown={(e) => {
                        if (e.key === "Enter" && hotkeyText.trim()) {
                          executeHotkey();
                        }
                      }}
                    />
                    <button
                      onClick={executeHotkey}
                      disabled={!hotkeyText.trim() || runningAction !== null}
                      className={cn(
                        "px-3 py-2 rounded-lg transition-colors",
                        getAccentColors("purple").bg,
                        getAccentColors("purple").text,
                        "hover:opacity-80 disabled:opacity-50 disabled:cursor-not-allowed",
                      )}
                    >
                      {runningAction === "hotkey" ? (
                        <Loader2 className="w-4 h-4 animate-spin" />
                      ) : (
                        <Play className="w-4 h-4" />
                      )}
                    </button>
                  </div>
                </div>

                {/* Go To State Action */}
                <div className="space-y-2">
                  <div className="flex items-center gap-2">
                    <Navigation className={`w-4 h-4 ${getAccentColors("green").text}`} />
                    <label className="text-xs text-muted-foreground">Go To State</label>
                  </div>
                  <div className="flex gap-2">
                    <select
                      value={selectedStateId}
                      onChange={(e) => setSelectedStateId(e.target.value)}
                      className="flex-1 px-3 py-2 text-sm bg-secondary/50 border border-border rounded-lg focus:outline-none focus:ring-1 focus:ring-primary"
                    >
                      <option value="">Select state...</option>
                      {states.map((state) => (
                        <option key={state.id} value={state.id}>
                          {state.name}
                        </option>
                      ))}
                    </select>
                    <button
                      onClick={executeGoToState}
                      disabled={!selectedStateId || runningAction !== null}
                      className={cn(
                        "px-3 py-2 rounded-lg transition-colors",
                        getAccentColors("green").bg,
                        getAccentColors("green").text,
                        "hover:opacity-80 disabled:opacity-50 disabled:cursor-not-allowed",
                      )}
                    >
                      {runningAction === "go_to_state" ? (
                        <Loader2 className="w-4 h-4 animate-spin" />
                      ) : (
                        <Play className="w-4 h-4" />
                      )}
                    </button>
                  </div>
                </div>
              </div>
            )}
          </TabsContent>

          {/* Macros Tab */}
          <TabsContent value="macros">
            <div className="space-y-4 pt-4">
              {/* Macro Selector */}
              <div className="space-y-2">
                <select
                  value={selectedMacroId || ""}
                  onChange={(e) => setSelectedMacroId(e.target.value || null)}
                  disabled={macros.length === 0}
                  className="w-full px-3 py-2.5 text-sm bg-secondary/50 border border-border rounded-lg focus:outline-none focus:ring-1 focus:ring-primary disabled:opacity-50"
                >
                  <option value="">
                    {macros.length === 0 ? "No macros available" : "Select a macro..."}
                  </option>
                  {macros.map((macro) => (
                    <option key={macro.id} value={macro.id}>
                      {macro.name} ({macro.steps.length} steps)
                    </option>
                  ))}
                </select>

                <button
                  onClick={handleRunMacro}
                  disabled={!selectedMacro || !configLoaded || runningMacroId !== null}
                  className={cn(
                    "w-full flex items-center justify-center gap-2 px-4 py-2.5 text-sm font-medium rounded-lg transition-colors",
                    getAccentColors("orange").bgSolid,
                    "text-white hover:opacity-90",
                    "disabled:opacity-50 disabled:cursor-not-allowed",
                  )}
                >
                  {runningMacroId === selectedMacroId ? (
                    <Loader2 className="w-4 h-4 animate-spin" />
                  ) : (
                    <Play className="w-4 h-4" />
                  )}
                  {!configLoaded
                    ? "Load Config First"
                    : !selectedMacro
                      ? "Select a Macro"
                      : "Run Macro"}
                </button>
              </div>

              {/* Macro Cards Grid */}
              {macrosLoading ? (
                <div className="flex items-center justify-center py-8">
                  <Loader2 className="w-6 h-6 animate-spin text-muted-foreground" />
                </div>
              ) : macros.length === 0 ? (
                <div className="text-center py-8">
                  <FolderOpen className="w-10 h-10 mx-auto text-muted-foreground/50 mb-2" />
                  <p className="text-sm text-muted-foreground">No macros found</p>
                  <p className="text-xs text-muted-foreground/70">
                    Create macros in the Macro Builder
                  </p>
                </div>
              ) : (
                <div className="space-y-2 max-h-64 overflow-y-auto scrollbar-dark">
                  {macros.map((macro) => (
                    <div
                      key={macro.id}
                      className={cn(
                        "group flex items-center justify-between p-3 rounded-lg border border-border bg-secondary/20",
                        "hover:bg-secondary/40 transition-colors",
                        !configLoaded && "opacity-60",
                      )}
                    >
                      <div className="flex items-center gap-3 min-w-0">
                        <MousePointer2
                          className={`w-4 h-4 flex-shrink-0 ${getAccentColors("orange").text}`}
                        />
                        <div className="min-w-0">
                          <div className="flex items-center gap-2">
                            <span className="text-sm font-medium truncate">{macro.name}</span>
                            <span
                              className={`text-[10px] ${getAccentColors("orange").bg} ${getAccentColors("orange").text} px-1.5 py-0.5 rounded flex-shrink-0`}
                            >
                              {macro.steps.length} steps
                            </span>
                          </div>
                          {macro.description && (
                            <p className="text-xs text-muted-foreground truncate">
                              {macro.description}
                            </p>
                          )}
                        </div>
                      </div>
                      <button
                        onClick={() => onRunMacro(macro)}
                        disabled={!configLoaded || runningMacroId === macro.id}
                        className={cn(
                          "p-2 rounded-lg opacity-0 group-hover:opacity-100 transition-opacity",
                          getAccentColors("orange").bg,
                          getAccentColors("orange").text,
                          "hover:opacity-80 disabled:opacity-50",
                        )}
                      >
                        {runningMacroId === macro.id ? (
                          <Loader2 className="w-4 h-4 animate-spin" />
                        ) : (
                          <Play className="w-4 h-4" />
                        )}
                      </button>
                    </div>
                  ))}
                </div>
              )}
            </div>
          </TabsContent>
        </Tabs>
      </CardContent>
    </Card>
  );
}
