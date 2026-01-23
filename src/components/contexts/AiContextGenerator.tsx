/**
 * AI Context Generator Component
 *
 * Generates knowledge base context entries from natural language descriptions.
 * Uses AI to create well-structured documentation for AI task automation.
 */

import { useState, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Sparkles, Loader2, RefreshCw, Check, X, BookOpen, Wand2 } from "lucide-react";

interface GeneratedContext {
  name: string;
  content: string;
  category?: string;
  tags: string[];
  taskMentions: string[];
}

interface GenerateContextResponse {
  success: boolean;
  data?: GeneratedContext;
  error?: string;
}

interface AiContextGeneratorProps {
  /** Called when a context is generated and accepted */
  onContextGenerated: (context: GeneratedContext) => void;
  /** Called when the generator is closed */
  onCancel?: () => void;
}

// Template prompts for common context topics
const PROMPT_TEMPLATES = [
  {
    label: "Error Handling",
    prompt: "Create documentation for handling common errors and exceptions in automation workflows",
  },
  {
    label: "Playwright Tips",
    prompt: "Best practices and tips for working with Playwright selectors and waits",
  },
  {
    label: "Network Timeouts",
    prompt: "Guidance for handling network timeouts and retry strategies in automation",
  },
  {
    label: "Form Validation",
    prompt: "Common patterns for form validation testing and error state verification",
  },
  {
    label: "Authentication",
    prompt: "Best practices for handling login flows and session management in tests",
  },
  {
    label: "API Testing",
    prompt: "Guidelines for API request testing including headers, body, and response validation",
  },
];

export function AiContextGenerator({
  onContextGenerated,
  onCancel,
}: AiContextGeneratorProps) {
  const [prompt, setPrompt] = useState("");
  const [isGenerating, setIsGenerating] = useState(false);
  const [generatedContext, setGeneratedContext] = useState<GeneratedContext | null>(null);
  const [error, setError] = useState<string | null>(null);

  // Generate context using AI
  const handleGenerate = useCallback(async () => {
    if (!prompt.trim()) {
      setError("Please enter a topic description");
      return;
    }

    setIsGenerating(true);
    setError(null);
    setGeneratedContext(null);

    try {
      const response = await invoke<{
        success: boolean;
        message?: string;
        data?: GenerateContextResponse;
      }>("generate_context_with_ai", {
        input: {
          user_prompt: prompt,
        },
      });

      if (response.success && response.data?.data) {
        setGeneratedContext(response.data.data);
      } else {
        setError(response.data?.error || response.message || "Failed to generate context");
      }
    } catch (err) {
      setError(String(err));
    } finally {
      setIsGenerating(false);
    }
  }, [prompt]);

  // Accept generated context
  const handleAccept = useCallback(() => {
    if (generatedContext) {
      onContextGenerated(generatedContext);
    }
  }, [generatedContext, onContextGenerated]);

  // Apply template
  const handleApplyTemplate = useCallback((template: { prompt: string }) => {
    setPrompt(template.prompt);
  }, []);

  return (
    <div className="flex flex-col h-full bg-neutral-900 rounded-lg border border-neutral-700 overflow-hidden">
      {/* Header */}
      <div className="flex items-center justify-between p-3 border-b border-neutral-700">
        <div className="flex items-center gap-2">
          <Sparkles className="w-4 h-4 text-blue-400" />
          <span className="text-sm font-medium text-neutral-200">AI Context Generator</span>
        </div>
        {onCancel && (
          <button
            onClick={onCancel}
            className="p-1 text-neutral-500 hover:text-neutral-300 transition-colors"
          >
            <X className="w-4 h-4" />
          </button>
        )}
      </div>

      {/* Main content */}
      <div className="flex-1 flex flex-col min-h-0 overflow-hidden p-4">
        {/* Template quick picks */}
        {!generatedContext && (
          <div className="mb-4">
            <label className="block text-xs text-neutral-400 mb-2">Quick Templates</label>
            <div className="flex flex-wrap gap-2">
              {PROMPT_TEMPLATES.map((template) => (
                <button
                  key={template.label}
                  onClick={() => handleApplyTemplate(template)}
                  className="px-2 py-1 text-xs bg-neutral-800 text-neutral-300 rounded hover:bg-neutral-700 transition-colors"
                >
                  {template.label}
                </button>
              ))}
            </div>
          </div>
        )}

        {/* Prompt input or generated context */}
        {!generatedContext ? (
          <>
            {/* Prompt textarea */}
            <div className="flex-1 flex flex-col min-h-0">
              <label className="block text-xs text-neutral-400 mb-2">
                Describe the topic for the knowledge base entry
              </label>
              <textarea
                value={prompt}
                onChange={(e) => setPrompt(e.target.value)}
                placeholder="Example: Best practices for handling network timeouts in Playwright tests, including retry strategies and timeout configuration"
                className="flex-1 px-3 py-2 bg-neutral-800 border border-neutral-700 rounded text-sm text-neutral-200 placeholder-neutral-500 resize-none focus:outline-none focus:border-blue-500 min-h-[100px]"
              />
            </div>

            {/* Error display */}
            {error && (
              <div className="mt-4 p-3 bg-red-900/30 border border-red-700 rounded">
                <p className="text-sm text-red-300">{error}</p>
              </div>
            )}

            {/* Generate button */}
            <button
              onClick={handleGenerate}
              disabled={isGenerating || !prompt.trim()}
              className="mt-4 flex items-center justify-center gap-2 px-4 py-2 bg-blue-600 text-white rounded hover:bg-blue-700 disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
            >
              {isGenerating ? (
                <>
                  <Loader2 className="w-4 h-4 animate-spin" />
                  Generating...
                </>
              ) : (
                <>
                  <Wand2 className="w-4 h-4" />
                  Generate Context
                </>
              )}
            </button>
          </>
        ) : (
          <>
            {/* Generated context preview */}
            <div className="flex-1 flex flex-col min-h-0 overflow-auto">
              {/* Name */}
              <div className="mb-4">
                <label className="block text-xs text-neutral-400 mb-1">Name</label>
                <div className="p-2 bg-neutral-800 rounded border border-neutral-700">
                  <span className="text-sm font-medium text-neutral-200">{generatedContext.name}</span>
                </div>
              </div>

              {/* Content */}
              <div className="mb-4 flex-1 min-h-0">
                <label className="block text-xs text-neutral-400 mb-1">Content (Markdown)</label>
                <div className="h-full max-h-[200px] overflow-auto p-3 bg-neutral-950 rounded border border-neutral-700">
                  <pre className="text-sm font-mono text-neutral-300 whitespace-pre-wrap">
                    {generatedContext.content}
                  </pre>
                </div>
              </div>

              {/* Category */}
              {generatedContext.category && (
                <div className="mb-4">
                  <label className="block text-xs text-neutral-400 mb-1">Category</label>
                  <span className="px-2 py-1 text-xs bg-neutral-800 text-neutral-300 rounded">
                    {generatedContext.category}
                  </span>
                </div>
              )}

              {/* Tags */}
              {generatedContext.tags.length > 0 && (
                <div className="mb-4">
                  <label className="block text-xs text-neutral-400 mb-1">Tags</label>
                  <div className="flex flex-wrap gap-1">
                    {generatedContext.tags.map((tag) => (
                      <span key={tag} className="px-2 py-0.5 text-xs bg-blue-900/30 text-blue-300 rounded">
                        {tag}
                      </span>
                    ))}
                  </div>
                </div>
              )}

              {/* Task mentions */}
              {generatedContext.taskMentions.length > 0 && (
                <div className="mb-4">
                  <label className="block text-xs text-neutral-400 mb-1">Auto-include Keywords</label>
                  <div className="flex flex-wrap gap-1">
                    {generatedContext.taskMentions.map((mention) => (
                      <span key={mention} className="px-2 py-0.5 text-xs bg-green-900/30 text-green-300 rounded">
                        {mention}
                      </span>
                    ))}
                  </div>
                </div>
              )}
            </div>

            {/* Action buttons */}
            <div className="mt-4 flex gap-2">
              <button
                onClick={() => {
                  setGeneratedContext(null);
                }}
                className="flex-1 flex items-center justify-center gap-2 px-4 py-2 bg-neutral-700 text-white rounded hover:bg-neutral-600 transition-colors"
              >
                <RefreshCw className="w-4 h-4" />
                Try Again
              </button>
              <button
                onClick={handleAccept}
                className="flex-1 flex items-center justify-center gap-2 px-4 py-2 bg-green-600 text-white rounded hover:bg-green-700 transition-colors"
              >
                <Check className="w-4 h-4" />
                Use Context
              </button>
            </div>
          </>
        )}
      </div>

      {/* Footer */}
      <div className="p-2 border-t border-neutral-700 text-xs text-neutral-500">
        AI-generated content should be reviewed before saving
      </div>
    </div>
  );
}

export default AiContextGenerator;
