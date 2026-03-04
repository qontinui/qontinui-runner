/**
 * NewRunDialog Component
 *
 * Dialog for creating a new workflow run with mode selection.
 * Allows choosing between GUI mode (exclusive, takes GUI lock) and
 * Headless mode (parallel, no GUI control).
 */

import { useState, useCallback, useEffect } from "react";
import { X, Monitor, Cloud, AlertTriangle, Loader2 } from "lucide-react";
import { Button } from "../ui";
import { getAccentColors } from "@/design-system";
import { useActiveRuns } from "../../contexts/ActiveRunsContext";
import { getApiBase, tracedFetch } from "@/lib/runner-api";

type RunMode = "gui" | "headless";

interface CreateBridgeRequest {
  mode: RunMode;
  run_id?: string;
  monitor_indices: number[];
  force_gui_lock: boolean;
}

interface CreateBridgeResponse {
  success: boolean;
  data?: {
    bridge_id: string;
    started: boolean;
    error: string | null;
  };
  error?: string;
}

export interface NewRunDialogProps {
  /** Whether the dialog is open */
  open: boolean;
  /** Called when the dialog is closed */
  onClose: () => void;
  /** Called after successful bridge creation */
  onSuccess?: (bridgeId: string) => void;
}

/**
 * Dialog for creating a new workflow run.
 *
 * Features:
 * - Mode selection: GUI (exclusive) or Headless (parallel)
 * - GUI lock warning when another bridge holds the lock
 * - Force-acquire option for GUI lock
 */
export function NewRunDialog({ open, onClose, onSuccess }: NewRunDialogProps) {
  const { guiLockHolderId, refresh } = useActiveRuns();

  const [selectedMode, setSelectedMode] = useState<RunMode>("headless");
  const [forceGuiLock, setForceGuiLock] = useState(false);
  const [isLoading, setIsLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Reset state when dialog opens
  useEffect(() => {
    if (open) {
      setSelectedMode("headless");
      setForceGuiLock(false);
      setError(null);
    }
  }, [open]);

  // Check if GUI lock is held by another bridge
  const guiLockConflict = selectedMode === "gui" && guiLockHolderId !== null;

  /**
   * Create the bridge via API.
   */
  const handleCreate = useCallback(async () => {
    setIsLoading(true);
    setError(null);

    try {
      const request: CreateBridgeRequest = {
        mode: selectedMode,
        monitor_indices: [0], // Default to primary monitor
        force_gui_lock: selectedMode === "gui" && forceGuiLock,
      };

      const response = await tracedFetch(`${getApiBase()}/bridges`, {
        method: "POST",
        headers: {
          "Content-Type": "application/json",
        },
        body: JSON.stringify(request),
      });

      const data: CreateBridgeResponse = await response.json();

      if (!response.ok || !data.success) {
        throw new Error(data.error || data.data?.error || "Failed to create bridge");
      }

      // Refresh the active runs list to show the new bridge
      await refresh();

      // Call success callback
      if (onSuccess && data.data?.bridge_id) {
        onSuccess(data.data.bridge_id);
      }

      onClose();
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to create bridge");
    } finally {
      setIsLoading(false);
    }
  }, [selectedMode, forceGuiLock, refresh, onSuccess, onClose]);

  if (!open) return null;

  const amberColors = getAccentColors("amber");
  const blueColors = getAccentColors("blue");
  const emeraldColors = getAccentColors("emerald");

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center">
      {/* Backdrop */}
      <div className="absolute inset-0 bg-black/50" onClick={isLoading ? undefined : onClose} />

      {/* Dialog */}
      <div
        data-ui-id="dialog-new-run"
        className="relative bg-zinc-900 border border-zinc-700 rounded-lg shadow-xl w-full max-w-md mx-4 overflow-hidden"
      >
        {/* Header */}
        <div className="flex items-center justify-between px-6 py-4 border-b border-zinc-700 bg-zinc-800/50">
          <h3 className="text-lg font-semibold text-zinc-100">New Run</h3>
          <button
            data-ui-id="dialog-new-run-close-btn"
            onClick={onClose}
            disabled={isLoading}
            className="p-1 text-zinc-400 hover:text-zinc-200 transition-colors disabled:opacity-50"
          >
            <X className="w-5 h-5" />
          </button>
        </div>

        {/* Content */}
        <div className="p-6 space-y-6">
          {/* Mode Selection */}
          <div className="space-y-3">
            <label className="text-sm font-medium text-zinc-300">Select Run Mode</label>

            {/* GUI Mode Option */}
            <button
              type="button"
              onClick={() => setSelectedMode("gui")}
              disabled={isLoading}
              className={`w-full p-4 rounded-lg border transition-all text-left ${
                selectedMode === "gui"
                  ? `${emeraldColors.border} ${emeraldColors.bg} ring-1 ring-emerald-500/50`
                  : "border-zinc-700 bg-zinc-800/50 hover:border-zinc-600"
              } disabled:opacity-50 disabled:cursor-not-allowed`}
            >
              <div className="flex items-start gap-3">
                <div
                  className={`p-2 rounded-md ${
                    selectedMode === "gui" ? emeraldColors.bg : "bg-zinc-700"
                  }`}
                >
                  <Monitor
                    className={`w-5 h-5 ${
                      selectedMode === "gui" ? emeraldColors.text : "text-zinc-400"
                    }`}
                  />
                </div>
                <div className="flex-1">
                  <div className="flex items-center gap-2">
                    <span
                      className={`font-medium ${
                        selectedMode === "gui" ? "text-zinc-100" : "text-zinc-300"
                      }`}
                    >
                      GUI Mode
                    </span>
                    <span
                      className={`text-xs px-1.5 py-0.5 rounded ${amberColors.bg} ${amberColors.text}`}
                    >
                      Exclusive
                    </span>
                  </div>
                  <p className="text-sm text-zinc-400 mt-1">
                    Takes control of the screen for visual automation. Only one GUI run can be
                    active at a time.
                  </p>
                </div>
                {/* Radio indicator */}
                <div
                  className={`w-4 h-4 rounded-full border-2 flex items-center justify-center ${
                    selectedMode === "gui" ? "border-emerald-500" : "border-zinc-600"
                  }`}
                >
                  {selectedMode === "gui" && (
                    <div className="w-2 h-2 rounded-full bg-emerald-500" />
                  )}
                </div>
              </div>
            </button>

            {/* Headless Mode Option */}
            <button
              type="button"
              onClick={() => setSelectedMode("headless")}
              disabled={isLoading}
              className={`w-full p-4 rounded-lg border transition-all text-left ${
                selectedMode === "headless"
                  ? `${blueColors.border} ${blueColors.bg} ring-1 ring-blue-500/50`
                  : "border-zinc-700 bg-zinc-800/50 hover:border-zinc-600"
              } disabled:opacity-50 disabled:cursor-not-allowed`}
            >
              <div className="flex items-start gap-3">
                <div
                  className={`p-2 rounded-md ${
                    selectedMode === "headless" ? blueColors.bg : "bg-zinc-700"
                  }`}
                >
                  <Cloud
                    className={`w-5 h-5 ${
                      selectedMode === "headless" ? blueColors.text : "text-zinc-400"
                    }`}
                  />
                </div>
                <div className="flex-1">
                  <div className="flex items-center gap-2">
                    <span
                      className={`font-medium ${
                        selectedMode === "headless" ? "text-zinc-100" : "text-zinc-300"
                      }`}
                    >
                      Headless Mode
                    </span>
                    <span
                      className={`text-xs px-1.5 py-0.5 rounded ${blueColors.bg} ${blueColors.text}`}
                    >
                      Parallel
                    </span>
                  </div>
                  <p className="text-sm text-zinc-400 mt-1">
                    Runs without screen control. Multiple headless runs can execute simultaneously.
                  </p>
                </div>
                {/* Radio indicator */}
                <div
                  className={`w-4 h-4 rounded-full border-2 flex items-center justify-center ${
                    selectedMode === "headless" ? "border-blue-500" : "border-zinc-600"
                  }`}
                >
                  {selectedMode === "headless" && (
                    <div className="w-2 h-2 rounded-full bg-blue-500" />
                  )}
                </div>
              </div>
            </button>
          </div>

          {/* GUI Lock Warning */}
          {guiLockConflict && (
            <div className={`p-4 rounded-lg ${amberColors.bg} border ${amberColors.border}`}>
              <div className="flex items-start gap-3">
                <AlertTriangle className={`w-5 h-5 ${amberColors.text} flex-shrink-0 mt-0.5`} />
                <div className="flex-1">
                  <p className={`text-sm font-medium ${amberColors.text}`}>GUI Lock Conflict</p>
                  <p className="text-sm text-zinc-400 mt-1">
                    Another bridge currently holds the GUI lock. You can force-acquire the lock,
                    which will interrupt the other run&apos;s GUI control.
                  </p>
                  {/* Force acquire checkbox */}
                  <label className="flex items-center gap-2 mt-3 cursor-pointer">
                    <input
                      type="checkbox"
                      checked={forceGuiLock}
                      onChange={(e) => setForceGuiLock(e.target.checked)}
                      disabled={isLoading}
                      className="w-4 h-4 rounded border-zinc-600 bg-zinc-800 text-amber-500 focus:ring-amber-500/50"
                    />
                    <span className="text-sm text-zinc-300">Force-acquire GUI lock</span>
                  </label>
                </div>
              </div>
            </div>
          )}

          {/* Error Display */}
          {error && (
            <div className="p-3 rounded-lg bg-red-500/10 border border-red-500/30">
              <p className="text-sm text-red-400">{error}</p>
            </div>
          )}
        </div>

        {/* Footer */}
        <div className="flex items-center justify-end gap-3 px-6 py-4 border-t border-zinc-700 bg-zinc-800/50">
          <Button
            data-ui-id="dialog-new-run-cancel-btn"
            variant="ghost"
            onClick={onClose}
            disabled={isLoading}
          >
            Cancel
          </Button>
          <Button
            data-ui-id="dialog-new-run-create-btn"
            variant="primary"
            onClick={handleCreate}
            disabled={isLoading || (guiLockConflict && !forceGuiLock)}
          >
            {isLoading ? (
              <>
                <Loader2 className="w-4 h-4 animate-spin" />
                Creating...
              </>
            ) : (
              "Create Run"
            )}
          </Button>
        </div>
      </div>
    </div>
  );
}

export default NewRunDialog;
