import type { UnifiedStep } from "../../../types/unified-workflow";

export function PromptConfig({
  step,
  onUpdate,
}: {
  step: UnifiedStep & { type: "prompt" };
  onUpdate: (updates: Partial<typeof step>) => void;
}) {
  return (
    <div className="space-y-4">
      <div>
        <label
          htmlFor="prompt-content-textarea"
          className="block text-sm font-medium text-zinc-400 mb-1"
        >
          Prompt Content
        </label>
        <textarea
          id="prompt-content-textarea"
          value={step.content || ""}
          onChange={(e) => onUpdate({ content: e.target.value })}
          placeholder="Enter the prompt for the AI agent..."
          rows={24}
          className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 placeholder-zinc-500 focus:outline-hidden focus:ring-2 focus:ring-blue-500/50 resize-y font-mono text-sm"
        />
        <p className="text-xs text-zinc-500 mt-1">
          This prompt will be sent to the AI agent during the agentic phase
        </p>
      </div>

      <div className="grid grid-cols-2 gap-4">
        <div>
          <label
            htmlFor="prompt-provider-select"
            className="block text-sm font-medium text-zinc-400 mb-1"
          >
            Provider (optional)
          </label>
          <select
            id="prompt-provider-select"
            value={step.provider ?? ""}
            onChange={(e) => onUpdate({ provider: e.target.value || undefined })}
            className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 focus:outline-hidden focus:ring-2 focus:ring-blue-500/50"
          >
            <option value="">Default</option>
            <option value="claude_cli">Claude CLI</option>
            <option value="gemini_api">Gemini API</option>
          </select>
        </div>
        <div>
          <label
            htmlFor="prompt-model-select"
            className="block text-sm font-medium text-zinc-400 mb-1"
          >
            Model (optional)
          </label>
          <select
            id="prompt-model-select"
            value={step.model ?? ""}
            onChange={(e) => onUpdate({ model: e.target.value || undefined })}
            className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 focus:outline-hidden focus:ring-2 focus:ring-blue-500/50"
          >
            <option value="">Default</option>
            <option value="claude-sonnet-4">Claude Sonnet 4</option>
            <option value="claude-opus-4">Claude Opus 4</option>
            <option value="gemini-2.5-pro">Gemini 2.5 Pro</option>
          </select>
        </div>
      </div>
    </div>
  );
}
