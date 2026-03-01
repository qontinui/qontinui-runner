/**
 * AI Task Generator Component
 *
 * Generates and improves AI task prompts from natural language descriptions.
 * Creates well-structured prompts with clear instructions and expected outputs.
 */

import { useState, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Sparkles, Loader2, RefreshCw, Check, X, Wand2, Pencil } from "lucide-react";

interface GeneratedTask {
  name: string;
  description: string;
  content: string;
  category: string;
  tags: string[];
}

interface GenerateTaskResponse {
  success: boolean;
  data?: GeneratedTask;
  error?: string;
}

interface AiTaskGeneratorProps {
  /** Called when a task is generated and accepted */
  onTaskGenerated: (task: GeneratedTask) => void;
  /** Called when the generator is closed */
  onCancel?: () => void;
  /** Existing prompt content to improve (optional) */
  existingPrompt?: string;
}

// Template prompts for common task types
const PROMPT_TEMPLATES = [
  {
    label: "Summarize Content",
    prompt: "Create a prompt that summarizes webpage content and extracts key points",
  },
  {
    label: "Code Review",
    prompt:
      "Create a prompt that reviews code changes and provides feedback on quality and potential issues",
  },
  {
    label: "Bug Analysis",
    prompt: "Create a prompt that analyzes error logs and suggests potential fixes",
  },
  {
    label: "Test Generation",
    prompt: "Create a prompt that generates test cases from requirements or specifications",
  },
  {
    label: "Documentation",
    prompt: "Create a prompt that generates technical documentation from code",
  },
  {
    label: "Data Extraction",
    prompt: "Create a prompt that extracts structured data from unstructured text",
  },
];

export function AiTaskGenerator({
  onTaskGenerated,
  onCancel,
  existingPrompt,
}: AiTaskGeneratorProps) {
  const [mode, setMode] = useState<"generate" | "improve">(existingPrompt ? "improve" : "generate");
  const [prompt, setPrompt] = useState(existingPrompt || "");
  const [isGenerating, setIsGenerating] = useState(false);
  const [generatedTask, setGeneratedTask] = useState<GeneratedTask | null>(null);
  const [error, setError] = useState<string | null>(null);

  // Generate task using AI
  const handleGenerate = useCallback(async () => {
    if (!prompt.trim()) {
      setError(
        mode === "generate"
          ? "Please describe what you want the task to do"
          : "Please enter the prompt you want to improve",
      );
      return;
    }

    setIsGenerating(true);
    setError(null);
    setGeneratedTask(null);

    try {
      const response = await invoke<{
        success: boolean;
        message?: string;
        data?: GenerateTaskResponse;
      }>("generate_task_prompt_with_ai", {
        input: {
          user_prompt: prompt,
          mode: mode,
        },
      });

      if (response.success && response.data?.data) {
        setGeneratedTask(response.data.data);
      } else {
        setError(response.data?.error || response.message || "Failed to generate task");
      }
    } catch (err) {
      setError(String(err));
    } finally {
      setIsGenerating(false);
    }
  }, [prompt, mode]);

  // Accept generated task
  const handleAccept = useCallback(() => {
    if (generatedTask) {
      onTaskGenerated(generatedTask);
    }
  }, [generatedTask, onTaskGenerated]);

  // Apply template
  const handleApplyTemplate = useCallback((template: { prompt: string }) => {
    setPrompt(template.prompt);
    setMode("generate");
  }, []);

  return (
    <div className="flex flex-col h-full bg-card rounded-lg border border-border overflow-hidden">
      {/* Header */}
      <div className="flex items-center justify-between p-3 border-b border-border">
        <div className="flex items-center gap-2">
          <Sparkles className="w-4 h-4 text-amber-400" />
          <span className="text-sm font-medium text-foreground">AI Task Generator</span>
        </div>
        {onCancel && (
          <button
            onClick={onCancel}
            className="p-1 text-muted-foreground hover:text-foreground transition-colors"
          >
            <X className="w-4 h-4" />
          </button>
        )}
      </div>

      {/* Main content */}
      <div className="flex-1 flex flex-col min-h-0 overflow-hidden p-4">
        {/* Mode selector */}
        <div className="mb-4">
          <label className="block text-xs text-muted-foreground mb-2">Mode</label>
          <div className="flex gap-2">
            <button
              onClick={() => {
                setMode("generate");
                if (existingPrompt && mode === "improve") {
                  setPrompt("");
                }
              }}
              className={`flex-1 flex items-center justify-center gap-2 px-3 py-2 text-xs rounded transition-colors ${
                mode === "generate"
                  ? "bg-amber-600 text-white"
                  : "bg-muted text-muted-foreground hover:bg-muted/80"
              }`}
            >
              <Wand2 className="w-3.5 h-3.5" />
              Generate New
            </button>
            <button
              onClick={() => {
                setMode("improve");
                if (existingPrompt) {
                  setPrompt(existingPrompt);
                }
              }}
              className={`flex-1 flex items-center justify-center gap-2 px-3 py-2 text-xs rounded transition-colors ${
                mode === "improve"
                  ? "bg-amber-600 text-white"
                  : "bg-muted text-muted-foreground hover:bg-muted/80"
              }`}
            >
              <Pencil className="w-3.5 h-3.5" />
              Improve Existing
            </button>
          </div>
        </div>

        {/* Template quick picks (only in generate mode) */}
        {!generatedTask && mode === "generate" && (
          <div className="mb-4">
            <label className="block text-xs text-muted-foreground mb-2">Quick Templates</label>
            <div className="flex flex-wrap gap-2">
              {PROMPT_TEMPLATES.map((template) => (
                <button
                  key={template.label}
                  onClick={() => handleApplyTemplate(template)}
                  className="px-2 py-1 text-xs bg-muted text-foreground rounded hover:bg-muted/80 transition-colors"
                >
                  {template.label}
                </button>
              ))}
            </div>
          </div>
        )}

        {/* Prompt input or generated task */}
        {!generatedTask ? (
          <>
            {/* Prompt textarea */}
            <div className="flex-1 flex flex-col min-h-0">
              <label className="block text-xs text-muted-foreground mb-2">
                {mode === "generate"
                  ? "Describe the task you want to create"
                  : "Paste the prompt you want to improve"}
              </label>
              <textarea
                value={prompt}
                onChange={(e) => setPrompt(e.target.value)}
                placeholder={
                  mode === "generate"
                    ? "Example: Create a task that analyzes error logs and suggests fixes based on common patterns"
                    : "Paste your existing prompt here to improve its structure and clarity..."
                }
                className="flex-1 px-3 py-2 bg-muted border border-border rounded text-sm text-foreground placeholder:text-muted-foreground resize-none focus:outline-none focus:border-amber-500 min-h-[120px] font-mono"
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
              className="mt-4 flex items-center justify-center gap-2 px-4 py-2 bg-amber-600 text-black rounded hover:bg-amber-500 disabled:opacity-50 disabled:cursor-not-allowed transition-colors font-medium"
            >
              {isGenerating ? (
                <>
                  <Loader2 className="w-4 h-4 animate-spin" />
                  {mode === "generate" ? "Generating..." : "Improving..."}
                </>
              ) : (
                <>
                  <Wand2 className="w-4 h-4" />
                  {mode === "generate" ? "Generate Task" : "Improve Prompt"}
                </>
              )}
            </button>
          </>
        ) : (
          <>
            {/* Generated task preview */}
            <div className="flex-1 flex flex-col min-h-0 overflow-auto">
              {/* Name */}
              <div className="mb-4">
                <label className="block text-xs text-muted-foreground mb-1">Name</label>
                <div className="p-2 bg-muted rounded border border-border">
                  <span className="text-sm font-medium text-foreground">{generatedTask.name}</span>
                </div>
              </div>

              {/* Description */}
              {generatedTask.description && (
                <div className="mb-4">
                  <label className="block text-xs text-muted-foreground mb-1">Description</label>
                  <p className="text-sm text-foreground">{generatedTask.description}</p>
                </div>
              )}

              {/* Content (the actual prompt) */}
              <div className="mb-4 flex-1 min-h-0">
                <label className="block text-xs text-muted-foreground mb-1">Prompt Content</label>
                <div className="h-full max-h-[200px] overflow-auto p-3 bg-background rounded border border-border">
                  <pre className="text-sm font-mono text-foreground whitespace-pre-wrap">
                    {generatedTask.content}
                  </pre>
                </div>
              </div>

              {/* Category */}
              {generatedTask.category && (
                <div className="mb-4">
                  <label className="block text-xs text-muted-foreground mb-1">Category</label>
                  <span className="px-2 py-1 text-xs bg-muted text-foreground rounded">
                    {generatedTask.category}
                  </span>
                </div>
              )}

              {/* Tags */}
              {generatedTask.tags.length > 0 && (
                <div className="mb-4">
                  <label className="block text-xs text-muted-foreground mb-1">Tags</label>
                  <div className="flex flex-wrap gap-1">
                    {generatedTask.tags.map((tag) => (
                      <span
                        key={tag}
                        className="px-2 py-0.5 text-xs bg-amber-900/30 text-amber-300 rounded"
                      >
                        {tag}
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
                  setGeneratedTask(null);
                }}
                className="flex-1 flex items-center justify-center gap-2 px-4 py-2 bg-muted/80 text-white rounded hover:bg-muted transition-colors"
              >
                <RefreshCw className="w-4 h-4" />
                Try Again
              </button>
              <button
                onClick={handleAccept}
                className="flex-1 flex items-center justify-center gap-2 px-4 py-2 bg-green-600 text-white rounded hover:bg-green-700 transition-colors"
              >
                <Check className="w-4 h-4" />
                Use Task
              </button>
            </div>
          </>
        )}
      </div>

      {/* Footer */}
      <div className="p-2 border-t border-border text-xs text-muted-foreground">
        AI-generated prompts should be reviewed and tested before use
      </div>
    </div>
  );
}

export default AiTaskGenerator;
