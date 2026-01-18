/**
 * ActionButtons.tsx
 *
 * Main action buttons for starting/stopping automation and saving workflows.
 */

import {
  AlertTriangle,
  Copy,
  FilePlus2,
  Loader2,
  Play,
  RefreshCw,
  Save,
  Square,
} from "lucide-react";
import { useAiBuilder } from "./AiBuilderContext";
import { getAccentColors, getStatusColors } from "@/design-system";

export function ActionButtons() {
  const {
    isRunning,
    currentSessionId,
    sessionState,
    currentWorkflowId,
    currentWorkflowName,
    isSaving,
    runAutomation,
    stopSession,
    copyPrompt,
    handleNewWorkflow,
    handleSaveWorkflow,
    clearSessionForNew,
    hasGuiSteps,
    selectedConfigId,
    setLastResult,
  } = useAiBuilder();

  // Check if we can run (no GUI steps without config)
  const canRun = !hasGuiSteps || !!selectedConfigId;

  // Handler that validates before running
  const handleRunAutomation = () => {
    if (hasGuiSteps && !selectedConfigId) {
      setLastResult({
        success: false,
        message:
          "Cannot start: GUI steps require a stored configuration. Please select a config above.",
      });
      return;
    }
    runAutomation();
  };

  return (
    <div className="flex gap-3">
      {isRunning && currentSessionId ? (
        // Stop button when THIS task has a running session
        <button
          onClick={stopSession}
          className={`flex-1 flex items-center justify-center gap-2 px-4 py-3 ${getAccentColors("red").bgSolid} text-white rounded-md font-medium hover:bg-red-600 transition-colors`}
        >
          <Square className="w-4 h-4" />
          Stop Session
        </button>
      ) : sessionState &&
        (sessionState.status === "complete" || sessionState.status === "stopped") ? (
        // New session button when previous session completed
        <button
          onClick={clearSessionForNew}
          className="flex-1 flex items-center justify-center gap-2 px-4 py-3 bg-primary text-primary-foreground rounded-md font-medium hover:bg-primary/90 transition-colors"
        >
          <RefreshCw className="w-4 h-4" />
          New Session
        </button>
      ) : (
        // Start button
        <button
          onClick={handleRunAutomation}
          disabled={isRunning}
          className={`flex-1 flex items-center justify-center gap-2 px-4 py-3 rounded-md font-medium disabled:opacity-50 disabled:cursor-not-allowed transition-colors ${
            canRun
              ? "bg-primary text-primary-foreground hover:bg-primary/90"
              : `${getAccentColors("amber").bg} ${getAccentColors("amber").text} hover:bg-amber-500/30`
          }`}
          title={canRun ? "Start AI analysis" : "Select a config before running"}
        >
          {canRun ? <Play className="w-4 h-4" /> : <AlertTriangle className="w-4 h-4" />}
          {canRun ? "Start AI Analysis" : "Config Required"}
        </button>
      )}

      <button
        onClick={handleNewWorkflow}
        className={`flex items-center gap-2 px-4 py-3 ${getAccentColors("blue").bg} ${getAccentColors("blue").text} rounded-md font-medium hover:bg-blue-500/20 transition-colors`}
        title="Create new workflow"
      >
        <FilePlus2 className="w-4 h-4" />
      </button>

      <button
        onClick={copyPrompt}
        className="flex items-center gap-2 px-4 py-3 bg-muted text-foreground rounded-md font-medium hover:bg-muted/80 transition-colors"
        title="Copy prompt to clipboard"
      >
        <Copy className="w-4 h-4" />
      </button>

      <button
        onClick={handleSaveWorkflow}
        disabled={isSaving}
        className={`flex items-center gap-2 px-4 py-3 ${getStatusColors("success").bg} ${getStatusColors("success").text} rounded-md font-medium hover:bg-green-500/20 disabled:opacity-50 transition-colors`}
        title={currentWorkflowId ? `Save "${currentWorkflowName}"` : "Save workflow"}
      >
        {isSaving ? <Loader2 className="w-4 h-4 animate-spin" /> : <Save className="w-4 h-4" />}
      </button>
    </div>
  );
}
