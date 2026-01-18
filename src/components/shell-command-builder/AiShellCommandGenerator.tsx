/**
 * AI Shell Command Generator Component
 *
 * Generates shell commands from natural language descriptions using AI.
 * Provides templates for common operations and OS-aware command generation.
 */

import { useState, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Sparkles, Loader2, RefreshCw, Check, X, Copy, Wand2, Terminal } from "lucide-react";

interface GenerateShellCommandResponse {
  success: boolean;
  command?: string;
  description?: string;
  error?: string;
}

interface AiShellCommandGeneratorProps {
  /** Called when a command is generated and accepted */
  onCommandGenerated: (command: string, description: string) => void;
  /** Called when the generator is closed */
  onCancel?: () => void;
  /** Initial category for the command */
  initialCategory?: string;
}

// Template prompts for common shell command tasks
const PROMPT_TEMPLATES = [
  {
    label: "Git backup branches",
    prompt: "Create a backup branch with today's date for all git repos in a directory",
    category: "git",
  },
  {
    label: "Git status all repos",
    prompt: "Check git status for all repositories in the current directory",
    category: "git",
  },
  {
    label: "Find large files",
    prompt: "Find all files larger than 100MB in the current directory recursively",
    category: "general",
  },
  {
    label: "Disk usage summary",
    prompt: "Show disk usage summary for subdirectories, sorted by size",
    category: "general",
  },
  {
    label: "Kill process by port",
    prompt: "Kill the process running on a specific port (e.g., port 3000)",
    category: "general",
  },
  {
    label: "npm outdated check",
    prompt: "Check for outdated npm packages in the current project",
    category: "npm",
  },
  {
    label: "Docker cleanup",
    prompt: "Remove all stopped containers and dangling images",
    category: "docker",
  },
  {
    label: "Build and test",
    prompt: "Run build command and then run tests, stopping on first failure",
    category: "build",
  },
];

export function AiShellCommandGenerator({
  onCommandGenerated,
  onCancel,
  initialCategory,
}: AiShellCommandGeneratorProps) {
  const [prompt, setPrompt] = useState("");
  const [isGenerating, setIsGenerating] = useState(false);
  const [generatedCommand, setGeneratedCommand] = useState<string | null>(null);
  const [generatedDescription, setGeneratedDescription] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [targetOS, setTargetOS] = useState<"windows" | "linux" | "macos">("windows");

  // Filter templates by category if provided
  const filteredTemplates = initialCategory
    ? PROMPT_TEMPLATES.filter((t) => t.category === initialCategory || t.category === "general")
    : PROMPT_TEMPLATES;

  // Generate shell command using AI
  const handleGenerate = useCallback(async () => {
    if (!prompt.trim()) {
      setError("Please enter a description of the command you need");
      return;
    }

    setIsGenerating(true);
    setError(null);
    setGeneratedCommand(null);
    setGeneratedDescription(null);

    try {
      const response = await invoke<{
        success: boolean;
        message?: string;
        data?: GenerateShellCommandResponse;
      }>("generate_shell_command_with_ai", {
        input: {
          user_prompt: prompt,
          target_os: targetOS,
          category: initialCategory,
        },
      });

      if (response.success && response.data?.command) {
        setGeneratedCommand(response.data.command);
        setGeneratedDescription(response.data.description || prompt);
      } else {
        setError(response.data?.error || response.message || "Failed to generate command");
      }
    } catch (err) {
      setError(String(err));
    } finally {
      setIsGenerating(false);
    }
  }, [prompt, targetOS, initialCategory]);

  // Accept generated command
  const handleAccept = useCallback(() => {
    if (generatedCommand) {
      onCommandGenerated(generatedCommand, generatedDescription || prompt);
    }
  }, [generatedCommand, generatedDescription, prompt, onCommandGenerated]);

  // Copy to clipboard
  const handleCopy = useCallback(async () => {
    if (generatedCommand) {
      await navigator.clipboard.writeText(generatedCommand);
    }
  }, [generatedCommand]);

  // Apply template
  const handleApplyTemplate = useCallback((template: { prompt: string }) => {
    setPrompt(template.prompt);
  }, []);

  return (
    <div className="flex flex-col h-full bg-neutral-900 rounded-lg border border-neutral-700 overflow-hidden">
      {/* Header */}
      <div className="flex items-center justify-between p-3 border-b border-neutral-700">
        <div className="flex items-center gap-2">
          <Sparkles className="w-4 h-4 text-violet-400" />
          <span className="text-sm font-medium text-neutral-200">AI Command Generator</span>
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
        {/* Target OS selector */}
        <div className="mb-4">
          <label className="block text-xs text-neutral-400 mb-2">Target Operating System</label>
          <div className="flex gap-2">
            {(["windows", "linux", "macos"] as const).map((os) => (
              <button
                key={os}
                onClick={() => setTargetOS(os)}
                className={`px-3 py-1.5 text-xs rounded transition-colors ${
                  targetOS === os
                    ? "bg-violet-600 text-white"
                    : "bg-neutral-800 text-neutral-400 hover:bg-neutral-700"
                }`}
              >
                {os === "windows"
                  ? "Windows (PowerShell)"
                  : os === "linux"
                    ? "Linux (Bash)"
                    : "macOS (Zsh)"}
              </button>
            ))}
          </div>
        </div>

        {/* Template quick picks */}
        {!generatedCommand && (
          <div className="mb-4">
            <label className="block text-xs text-neutral-400 mb-2">Quick Templates</label>
            <div className="flex flex-wrap gap-2">
              {filteredTemplates.slice(0, 6).map((template) => (
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

        {/* Prompt input or generated command */}
        {!generatedCommand ? (
          <>
            {/* Prompt textarea */}
            <div className="flex-1 flex flex-col min-h-0">
              <label className="block text-xs text-neutral-400 mb-2">
                Describe what you want the command to do
              </label>
              <textarea
                value={prompt}
                onChange={(e) => setPrompt(e.target.value)}
                placeholder="Example: Create a backup branch with today's date for all git repos in the parent directory"
                className="flex-1 px-3 py-2 bg-neutral-800 border border-neutral-700 rounded text-sm text-neutral-200 placeholder-neutral-500 resize-none focus:outline-none focus:border-violet-500 min-h-[100px]"
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
              className="mt-4 flex items-center justify-center gap-2 px-4 py-2 bg-violet-600 text-white rounded hover:bg-violet-700 disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
            >
              {isGenerating ? (
                <>
                  <Loader2 className="w-4 h-4 animate-spin" />
                  Generating...
                </>
              ) : (
                <>
                  <Wand2 className="w-4 h-4" />
                  Generate Command
                </>
              )}
            </button>
          </>
        ) : (
          <>
            {/* Generated command preview */}
            <div className="flex-1 flex flex-col min-h-0">
              <div className="flex items-center justify-between mb-2">
                <label className="text-xs text-neutral-400">Generated Command</label>
                <button
                  onClick={handleCopy}
                  className="p-1 text-neutral-500 hover:text-neutral-300 transition-colors"
                  title="Copy to clipboard"
                >
                  <Copy className="w-4 h-4" />
                </button>
              </div>
              <div className="flex-1 overflow-auto p-3 bg-neutral-950 rounded border border-neutral-700">
                <div className="flex items-start gap-2">
                  <Terminal className="w-4 h-4 text-violet-400 flex-shrink-0 mt-0.5" />
                  <pre className="text-sm font-mono text-neutral-300 whitespace-pre-wrap">
                    {generatedCommand}
                  </pre>
                </div>
              </div>
              {generatedDescription && (
                <div className="mt-2 p-2 bg-neutral-800/50 rounded">
                  <p className="text-xs text-neutral-400">{generatedDescription}</p>
                </div>
              )}
            </div>

            {/* Action buttons */}
            <div className="mt-4 flex gap-2">
              <button
                onClick={() => {
                  setGeneratedCommand(null);
                  setGeneratedDescription(null);
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
                Use Command
              </button>
            </div>
          </>
        )}
      </div>

      {/* Footer */}
      <div className="p-2 border-t border-neutral-700 text-xs text-neutral-500">
        Target: {targetOS} | AI-generated commands should be reviewed before execution
      </div>
    </div>
  );
}

export default AiShellCommandGenerator;
