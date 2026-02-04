/**
 * AgenticPromptEditor
 *
 * Editor for configuring the agentic prompt that runs when
 * verification steps fail.
 */

import { Bot } from "lucide-react";

interface AgenticPromptEditorProps {
  prompt: string;
  onPromptChange: (prompt: string) => void;
  maxIterations: number;
  onMaxIterationsChange: (max: number) => void;
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
}: AgenticPromptEditorProps) {
  return (
    <div className="p-4 space-y-4">
      <div className="flex items-center gap-2">
        <Bot className="w-4 h-4 text-purple-400" />
        <h3 className="text-sm font-medium text-neutral-200">Agentic Configuration</h3>
      </div>

      <p className="text-xs text-neutral-400">
        This prompt runs when a verification step fails. The AI agent will attempt to fix the
        application state so verification passes.
      </p>

      <div>
        <label className="block text-xs text-neutral-400 mb-1">Agentic Prompt</label>
        <textarea
          value={prompt || DEFAULT_PROMPT}
          onChange={(e) => onPromptChange(e.target.value)}
          rows={8}
          className="w-full px-3 py-2 text-sm bg-neutral-900 border border-neutral-600 rounded-md text-neutral-200 placeholder:text-neutral-500 focus:outline-none focus:border-purple-500 font-mono resize-y"
        />
      </div>

      <div className="flex items-center gap-3">
        <label className="text-xs text-neutral-400">Max Iterations</label>
        <input
          type="number"
          value={maxIterations}
          onChange={(e) => onMaxIterationsChange(Number(e.target.value))}
          min={1}
          max={20}
          className="w-20 px-2 py-1 text-sm bg-neutral-900 border border-neutral-600 rounded text-neutral-200 focus:outline-none focus:border-purple-500"
        />
        <span className="text-xs text-neutral-500">
          verification-agentic loops before stopping
        </span>
      </div>
    </div>
  );
}
