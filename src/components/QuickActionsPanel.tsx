/**
 * QuickActionsPanel.tsx
 *
 * Panel for running ad-hoc single GUI actions without saving a workflow.
 * Provides quick access to CLICK, TYPE, HOTKEY, and GO_TO_STATE actions.
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
} from "lucide-react";
import { useExecution } from "../contexts/ExecutionContext";
import { getAccentColors } from "@/design-system";

const API_BASE = "http://localhost:9876";

type ActionType = "click" | "double_click" | "right_click" | "type" | "hotkey" | "go_to_state";

interface StateInfo {
  id: string;
  name: string;
}

interface ImageInfo {
  id: string;
  name: string;
  stateName: string;
}

interface QuickActionsPanelProps {
  onLog?: (level: "info" | "success" | "warning" | "error", message: string) => void;
}

export function QuickActionsPanel({ onLog }: QuickActionsPanelProps) {
  const execution = useExecution();

  // Action states
  const [clickAction, setClickAction] = useState<"click" | "double_click" | "right_click">("click");
  const [selectedImageId, setSelectedImageId] = useState<string>("");
  const [typeText, setTypeText] = useState("");
  const [hotkeyText, setHotkeyText] = useState("");
  const [selectedStateId, setSelectedStateId] = useState<string>("");

  // Running states
  const [runningAction, setRunningAction] = useState<ActionType | null>(null);

  // Parse states and images from config
  const { states, images } = useMemo(() => {
    const stateList: StateInfo[] = [];
    const imageList: ImageInfo[] = [];

    if (execution.config?.states && Array.isArray(execution.config.states)) {
      for (const state of execution.config.states) {
        const stateName = state.name || state.id || "Unknown";
        stateList.push({
          id: state.id || "",
          name: stateName,
        });

        // Extract images from state
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
  }, [execution.config]);

  const log = (level: "info" | "success" | "warning" | "error", message: string) => {
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
          monitor_index: execution.selectedMonitors[0] ?? 0,
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
          monitor_index: execution.selectedMonitors[0] ?? 0,
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

  if (!execution.configLoaded) {
    return (
      <div className="panel">
        <div className="panel-header">
          <div className="panel-title">
            <MousePointer2 className={`w-4 h-4 ${getAccentColors("orange").text}`} />
            <span>Quick Actions</span>
          </div>
        </div>
        <div className="panel-content">
          <div className="flex items-center gap-2 text-sm text-muted-foreground">
            <AlertCircle className="w-4 h-4" />
            <span>Load a configuration to use quick actions</span>
          </div>
        </div>
      </div>
    );
  }

  return (
    <div className="panel">
      <div className="panel-header">
        <div className="panel-title">
          <MousePointer2 className={`w-4 h-4 ${getAccentColors("orange").text}`} />
          <span>Quick Actions</span>
        </div>
      </div>
      <div className="panel-content space-y-3">
        {/* Click Action */}
        <div className="flex items-center gap-2">
          <div className="flex items-center gap-1.5 min-w-[100px]">
            <MousePointer2 className={`w-3.5 h-3.5 ${getAccentColors("blue").text}`} />
            <select
              value={clickAction}
              onChange={(e) => setClickAction(e.target.value as typeof clickAction)}
              className="bg-muted/50 border border-border rounded px-2 py-1 text-xs focus:outline-none focus:ring-1 focus:ring-primary"
            >
              <option value="click">Click</option>
              <option value="double_click">Double Click</option>
              <option value="right_click">Right Click</option>
            </select>
          </div>
          <select
            value={selectedImageId}
            onChange={(e) => setSelectedImageId(e.target.value)}
            className="flex-1 bg-muted/50 border border-border rounded px-2 py-1 text-xs focus:outline-none focus:ring-1 focus:ring-primary"
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
            className={`flex items-center gap-1 px-2 py-1 text-xs rounded transition-colors disabled:opacity-50 disabled:cursor-not-allowed ${getAccentColors("blue").bg} ${getAccentColors("blue").text} hover:opacity-80`}
          >
            {runningAction === clickAction ? (
              <Loader2 className="w-3 h-3 animate-spin" />
            ) : (
              <Play className="w-3 h-3" />
            )}
          </button>
        </div>

        {/* Type Action */}
        <div className="flex items-center gap-2">
          <div className="flex items-center gap-1.5 min-w-[100px]">
            <Type className={`w-3.5 h-3.5 ${getAccentColors("amber").text}`} />
            <span className="text-xs">Type</span>
          </div>
          <input
            type="text"
            value={typeText}
            onChange={(e) => setTypeText(e.target.value)}
            placeholder="Enter text to type..."
            className="flex-1 bg-muted/50 border border-border rounded px-2 py-1 text-xs focus:outline-none focus:ring-1 focus:ring-primary"
            onKeyDown={(e) => {
              if (e.key === "Enter" && typeText.trim()) {
                executeType();
              }
            }}
          />
          <button
            onClick={executeType}
            disabled={!typeText.trim() || runningAction !== null}
            className={`flex items-center gap-1 px-2 py-1 text-xs rounded transition-colors disabled:opacity-50 disabled:cursor-not-allowed ${getAccentColors("amber").bg} ${getAccentColors("amber").text} hover:opacity-80`}
          >
            {runningAction === "type" ? (
              <Loader2 className="w-3 h-3 animate-spin" />
            ) : (
              <Play className="w-3 h-3" />
            )}
          </button>
        </div>

        {/* Hotkey Action */}
        <div className="flex items-center gap-2">
          <div className="flex items-center gap-1.5 min-w-[100px]">
            <Keyboard className={`w-3.5 h-3.5 ${getAccentColors("purple").text}`} />
            <span className="text-xs">Hotkey</span>
          </div>
          <input
            type="text"
            value={hotkeyText}
            onChange={(e) => setHotkeyText(e.target.value)}
            placeholder="e.g., ctrl+c, alt+tab"
            className="flex-1 bg-muted/50 border border-border rounded px-2 py-1 text-xs focus:outline-none focus:ring-1 focus:ring-primary"
            onKeyDown={(e) => {
              if (e.key === "Enter" && hotkeyText.trim()) {
                executeHotkey();
              }
            }}
          />
          <button
            onClick={executeHotkey}
            disabled={!hotkeyText.trim() || runningAction !== null}
            className={`flex items-center gap-1 px-2 py-1 text-xs rounded transition-colors disabled:opacity-50 disabled:cursor-not-allowed ${getAccentColors("purple").bg} ${getAccentColors("purple").text} hover:opacity-80`}
          >
            {runningAction === "hotkey" ? (
              <Loader2 className="w-3 h-3 animate-spin" />
            ) : (
              <Play className="w-3 h-3" />
            )}
          </button>
        </div>

        {/* Go To State Action */}
        <div className="flex items-center gap-2">
          <div className="flex items-center gap-1.5 min-w-[100px]">
            <Navigation className={`w-3.5 h-3.5 ${getAccentColors("green").text}`} />
            <span className="text-xs">Go To</span>
          </div>
          <select
            value={selectedStateId}
            onChange={(e) => setSelectedStateId(e.target.value)}
            className="flex-1 bg-muted/50 border border-border rounded px-2 py-1 text-xs focus:outline-none focus:ring-1 focus:ring-primary"
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
            className={`flex items-center gap-1 px-2 py-1 text-xs rounded transition-colors disabled:opacity-50 disabled:cursor-not-allowed ${getAccentColors("green").bg} ${getAccentColors("green").text} hover:opacity-80`}
          >
            {runningAction === "go_to_state" ? (
              <Loader2 className="w-3 h-3 animate-spin" />
            ) : (
              <Play className="w-3 h-3" />
            )}
          </button>
        </div>
      </div>
    </div>
  );
}

export default QuickActionsPanel;
