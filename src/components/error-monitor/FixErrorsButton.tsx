/**
 * FixErrorsButton.tsx
 *
 * Button component that generates and triggers a fix workflow for detected errors.
 */

import { useState } from "react";
import { Wrench, Loader2, AlertCircle, CheckCircle, ChevronDown } from "lucide-react";
import { cn } from "../../lib/utils";
import { useFixWorkflow } from "../../hooks/useErrorMonitor";

const API_BASE = "http://localhost:9876";

interface FixErrorsButtonProps {
  /** Task run ID to scope errors to */
  taskRunId?: string;
  /** Callback when workflow is generated */
  onWorkflowGenerated?: (workflow: Record<string, unknown>) => void;
  /** Optional className */
  className?: string;
  /** Whether to show as compact variant */
  compact?: boolean;
}

export function FixErrorsButton({
  taskRunId,
  onWorkflowGenerated,
  className,
  compact = false,
}: FixErrorsButtonProps) {
  const [showDropdown, setShowDropdown] = useState(false);
  const [generating, setGenerating] = useState(false);
  const [success, setSuccess] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const { summary, loading: checkingErrors, check, generateWorkflow } = useFixWorkflow();

  const handleClick = async () => {
    setError(null);
    setSuccess(false);

    // First check if there are fixable errors
    const checkResult = await check(taskRunId);
    if (!checkResult.canGenerateWorkflow) {
      setError("No fixable errors found");
      setTimeout(() => setError(null), 3000);
      return;
    }

    // Show dropdown with options
    setShowDropdown(true);
  };

  const handleGenerateWorkflow = async () => {
    try {
      setGenerating(true);
      setError(null);
      setShowDropdown(false);

      const workflow = await generateWorkflow();

      if (onWorkflowGenerated) {
        onWorkflowGenerated(workflow);
        setSuccess(true);
        setTimeout(() => setSuccess(false), 3000);
      } else {
        // Save the workflow to the database
        const savedWorkflow = await saveWorkflowToDatabase(workflow);
        setSuccess(true);
        setTimeout(() => setSuccess(false), 3000);

        // Navigate to the workflow builder with the new workflow
        // The workflow is now saved and can be edited/run from the Workflows tab
        window.dispatchEvent(
          new CustomEvent("navigate-to-workflow", {
            detail: { workflowId: savedWorkflow.id },
          }),
        );
      }
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to generate workflow");
      setTimeout(() => setError(null), 5000);
    } finally {
      setGenerating(false);
    }
  };

  const handleQuickFix = async () => {
    try {
      setGenerating(true);
      setError(null);
      setShowDropdown(false);

      const workflow = await generateWorkflow();

      // Execute the workflow inline without saving to the library
      // This prevents cluttering the workflow library with auto-generated fix workflows
      const runResponse = await fetch(`${API_BASE}/unified-workflows/execute-inline`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          name: (workflow.name as string) || "Fix Application Errors",
          description: (workflow.description as string) || "",
          setup_steps: workflow.setup_steps || [],
          verification_steps: workflow.verification_steps || [],
          agentic_steps: workflow.agentic_steps || [],
          completion_steps: workflow.completion_steps || [],
          max_iterations:
            (workflow.settings as Record<string, unknown>)?.max_agentic_iterations || 10,
          targeted_error_ids: workflow.targeted_error_ids || [],
        }),
      });

      if (!runResponse.ok) {
        const errorText = await runResponse.text();
        let errorMessage = "Failed to run workflow";
        try {
          const errorData = JSON.parse(errorText);
          errorMessage = errorData.error || errorMessage;
        } catch {
          errorMessage = errorText || errorMessage;
        }
        // Handle conflict (duplicate workflow) error specially
        if (runResponse.status === 409) {
          throw new Error(errorMessage);
        }
        throw new Error(errorMessage);
      }

      setSuccess(true);
      setTimeout(() => setSuccess(false), 3000);

      // Navigate to the Active page to show the running workflow
      window.dispatchEvent(new CustomEvent("navigate-to-active"));
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to start fix workflow");
      setTimeout(() => setError(null), 5000);
    } finally {
      setGenerating(false);
    }
  };

  /**
   * Save the generated workflow JSON to the database.
   */
  const saveWorkflowToDatabase = async (workflowJson: Record<string, unknown>) => {
    // Extract fields from the generated workflow JSON
    const createRequest = {
      name: (workflowJson.name as string) || "Fix Application Errors",
      description: (workflowJson.description as string) || "",
      category: "error-fix",
      tags: ["auto-generated", "error-fix"],
      setup_steps: workflowJson.setup_steps || [],
      verification_steps: workflowJson.verification_steps || [],
      agentic_steps: workflowJson.agentic_steps || [],
      completion_steps: workflowJson.completion_steps || [],
      max_iterations:
        (workflowJson.settings as Record<string, unknown>)?.max_agentic_iterations || 10,
    };

    const response = await fetch(`${API_BASE}/unified-workflows`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(createRequest),
    });

    if (!response.ok) {
      const errorText = await response.text();
      let errorMessage = "Failed to save workflow";
      try {
        const errorData = JSON.parse(errorText);
        errorMessage = errorData.error || errorMessage;
      } catch {
        errorMessage = errorText || errorMessage;
      }
      throw new Error(errorMessage);
    }

    const result = await response.json();
    return result.data;
  };

  // Determine button state and content
  const isLoading = checkingErrors || generating;
  const hasErrors = summary && summary.total > 0;
  const errorCount = summary?.total || 0;
  const criticalCount = summary?.criticalCount || 0;

  // Color based on severity
  const buttonColors =
    criticalCount > 0
      ? "bg-red-500 hover:bg-red-600 text-white"
      : hasErrors
        ? "bg-amber-500 hover:bg-amber-600 text-white"
        : "bg-muted hover:bg-muted/80 text-muted-foreground";

  if (compact) {
    return (
      <button
        onClick={handleClick}
        disabled={isLoading}
        className={cn(
          "relative flex items-center gap-1.5 px-2 py-1 text-xs rounded transition-colors",
          buttonColors,
          isLoading && "opacity-70 cursor-not-allowed",
          className,
        )}
        title={summary?.recommendedAction || "Fix application errors"}
      >
        {isLoading ? (
          <Loader2 className="w-3 h-3 animate-spin" />
        ) : success ? (
          <CheckCircle className="w-3 h-3" />
        ) : error ? (
          <AlertCircle className="w-3 h-3" />
        ) : (
          <Wrench className="w-3 h-3" />
        )}
        {hasErrors && <span>{errorCount}</span>}
      </button>
    );
  }

  return (
    <div className="relative">
      <button
        onClick={handleClick}
        disabled={isLoading}
        className={cn(
          "flex items-center gap-2 px-4 py-2 rounded-lg transition-colors font-medium",
          buttonColors,
          isLoading && "opacity-70 cursor-not-allowed",
          className,
        )}
      >
        {isLoading ? (
          <Loader2 className="w-4 h-4 animate-spin" />
        ) : success ? (
          <CheckCircle className="w-4 h-4" />
        ) : error ? (
          <AlertCircle className="w-4 h-4" />
        ) : (
          <Wrench className="w-4 h-4" />
        )}
        <span>
          {generating
            ? "Generating..."
            : success
              ? "Workflow Ready"
              : error
                ? "Error"
                : hasErrors
                  ? `Fix ${errorCount} Error${errorCount !== 1 ? "s" : ""}`
                  : "Fix Errors"}
        </span>
        {hasErrors && !isLoading && !success && !error && <ChevronDown className="w-4 h-4" />}
      </button>

      {/* Error message */}
      {error && (
        <div className="absolute top-full left-0 mt-2 w-64 p-2 bg-destructive/10 border border-destructive/30 rounded text-sm text-destructive">
          {error}
        </div>
      )}

      {/* Dropdown */}
      {showDropdown && summary && (
        <div className="absolute top-full right-0 mt-2 w-80 bg-background border border-border rounded-lg shadow-xl z-50">
          {/* Summary */}
          <div className="p-4 border-b border-border">
            <h3 className="font-medium mb-2">Fixable Errors Summary</h3>
            <div className="grid grid-cols-3 gap-2 text-center">
              <div className="p-2 bg-red-500/10 rounded">
                <div className="text-lg font-bold text-red-500">{summary.criticalCount}</div>
                <div className="text-xs text-muted-foreground">Critical</div>
              </div>
              <div className="p-2 bg-orange-500/10 rounded">
                <div className="text-lg font-bold text-orange-500">{summary.errorCount}</div>
                <div className="text-xs text-muted-foreground">Errors</div>
              </div>
              <div className="p-2 bg-yellow-500/10 rounded">
                <div className="text-lg font-bold text-yellow-500">{summary.warningCount}</div>
                <div className="text-xs text-muted-foreground">Warnings</div>
              </div>
            </div>
            <p className="text-sm text-muted-foreground mt-3">{summary.recommendedAction}</p>
          </div>

          {/* Actions */}
          <div className="p-2">
            <button
              onClick={handleQuickFix}
              className="w-full flex items-center gap-3 px-4 py-3 hover:bg-muted rounded transition-colors text-left"
            >
              <div className="w-8 h-8 bg-primary/20 rounded-full flex items-center justify-center">
                <Wrench className="w-4 h-4 text-primary" />
              </div>
              <div>
                <div className="font-medium">Quick Fix</div>
                <div className="text-xs text-muted-foreground">
                  Generate workflow and start immediately
                </div>
              </div>
            </button>
            <button
              onClick={handleGenerateWorkflow}
              className="w-full flex items-center gap-3 px-4 py-3 hover:bg-muted rounded transition-colors text-left"
            >
              <div className="w-8 h-8 bg-muted rounded-full flex items-center justify-center">
                <Wrench className="w-4 h-4 text-muted-foreground" />
              </div>
              <div>
                <div className="font-medium">Generate Workflow</div>
                <div className="text-xs text-muted-foreground">
                  Create workflow for review before running
                </div>
              </div>
            </button>
          </div>

          {/* Close */}
          <div className="p-2 border-t border-border">
            <button
              onClick={() => setShowDropdown(false)}
              className="w-full py-2 text-sm text-muted-foreground hover:text-foreground transition-colors"
            >
              Cancel
            </button>
          </div>
        </div>
      )}

      {/* Backdrop to close dropdown */}
      {showDropdown && (
        <div className="fixed inset-0 z-40" onClick={() => setShowDropdown(false)} />
      )}
    </div>
  );
}

export default FixErrorsButton;
