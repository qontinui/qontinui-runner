/**
 * AgenticPromptEditor
 *
 * Editor for configuring the agentic prompt that runs when
 * verification steps fail.
 */

import { Bot, Monitor, Globe } from "lucide-react";

interface AgenticPromptEditorProps {
  prompt: string;
  onPromptChange: (prompt: string) => void;
  maxIterations: number;
  onMaxIterationsChange: (max: number) => void;
  elementSource?: "control" | "external";
  onElementSourceChange?: (source: "control" | "external") => void;
}

const DEFAULT_PROMPT = `You are a QA assistant. The verification step above has failed.

Analyze the failure and take corrective action:
1. Read the error details to understand what failed
2. Check the current page state
3. Fix the issue if possible (e.g., navigate to the correct page, fill in missing data)
4. If the issue is a genuine bug, document it clearly

Do not modify the test specifications - fix the application state instead.`;

export function AgenticPromptEditor({
  prompt,
  onPromptChange,
  maxIterations,
  onMaxIterationsChange,
  elementSource,
  onElementSourceChange,
}: AgenticPromptEditorProps) {
  return (
    <div className="p-4 space-y-4">
      <div className="flex items-center gap-2">
        <Bot className="w-4 h-4 text-purple-400" />
        <h3 className="text-sm font-medium text-foreground">Agentic Configuration</h3>
      </div>

      <p className="text-xs text-muted-foreground">
        This prompt runs when a verification step fails. The AI agent will attempt to fix the
        application state so verification passes.
      </p>

      {/* Element source selector */}
      {elementSource && onElementSourceChange && (
        <div>
          <label className="block text-xs text-muted-foreground mb-2">Element Source</label>
          <div className="flex gap-2">
            <button
              onClick={() => onElementSourceChange("control")}
              className={`flex items-center gap-2 px-3 py-2 text-xs rounded-md border transition-colors ${
                elementSource === "control"
                  ? "bg-blue-500/20 border-blue-500/40 text-blue-300"
                  : "bg-muted border-border text-muted-foreground hover:text-foreground"
              }`}
            >
              <Monitor className="w-3.5 h-3.5" />
              Runner UI
            </button>
            <button
              onClick={() => onElementSourceChange("external")}
              className={`flex items-center gap-2 px-3 py-2 text-xs rounded-md border transition-colors ${
                elementSource === "external"
                  ? "bg-emerald-500/20 border-emerald-500/40 text-emerald-300"
                  : "bg-muted border-border text-muted-foreground hover:text-foreground"
              }`}
            >
              <Globe className="w-3.5 h-3.5" />
              Browser Tab
            </button>
          </div>
          <p className="text-[10px] text-muted-foreground mt-1">
            {elementSource === "control"
              ? "Specs run against the runner's own UI elements"
              : "Specs run against an external browser tab via SDK connection"}
          </p>
        </div>
      )}

      <div>
        <label className="block text-xs text-muted-foreground mb-1">Agentic Prompt</label>
        <textarea
          value={prompt || DEFAULT_PROMPT}
          onChange={(e) => onPromptChange(e.target.value)}
          rows={8}
          className="w-full px-3 py-2 text-sm bg-card border border-border rounded-md text-foreground placeholder:text-muted-foreground focus:outline-hidden focus:border-purple-500 font-mono resize-y"
        />
      </div>

      <div className="flex items-center gap-3">
        <label className="text-xs text-muted-foreground">Max Iterations</label>
        <input
          type="number"
          value={maxIterations}
          onChange={(e) => onMaxIterationsChange(Number(e.target.value))}
          min={1}
          max={20}
          className="w-20 px-2 py-1 text-sm bg-card border border-border rounded text-foreground focus:outline-hidden focus:border-purple-500"
        />
        <span className="text-xs text-muted-foreground">
          verification-agentic loops before stopping
        </span>
      </div>
    </div>
  );
}
