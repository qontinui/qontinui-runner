/**
 * AI API Request Generator Component
 *
 * Generates API request templates from natural language descriptions.
 * Creates complete HTTP requests with method, URL, headers, and body.
 */

import { useState, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Sparkles, Loader2, RefreshCw, Check, X, Wand2 } from "lucide-react";
import type { HttpMethod, ApiContentType } from "../../types";

interface GeneratedApiRequest {
  name: string;
  description: string;
  method: HttpMethod;
  url: string;
  headers: Record<string, string>;
  body?: string;
  body_content_type: ApiContentType;
  timeout_ms: number;
}

interface GenerateApiRequestResponse {
  success: boolean;
  data?: GeneratedApiRequest;
  error?: string;
}

interface AiApiRequestGeneratorProps {
  /** Called when an API request is generated and accepted */
  onRequestGenerated: (request: GeneratedApiRequest) => void;
  /** Called when the generator is closed */
  onCancel?: () => void;
}

const METHOD_COLORS: Record<HttpMethod, string> = {
  GET: "bg-green-900/50 text-green-300",
  POST: "bg-blue-900/50 text-blue-300",
  PUT: "bg-amber-900/50 text-amber-300",
  PATCH: "bg-orange-900/50 text-orange-300",
  DELETE: "bg-red-900/50 text-red-300",
};

// Template prompts for common API request patterns
const PROMPT_TEMPLATES = [
  {
    label: "Create User",
    prompt: "POST request to create a new user with name, email, and password fields",
  },
  {
    label: "Get User List",
    prompt: "GET request to fetch a paginated list of users with optional search filter",
  },
  {
    label: "Update Resource",
    prompt: "PUT request to update a resource by ID with JSON body",
  },
  {
    label: "Delete Item",
    prompt: "DELETE request to remove an item by ID with confirmation header",
  },
  {
    label: "Auth Login",
    prompt:
      "POST request for user authentication with email and password, expecting JWT token response",
  },
  {
    label: "File Upload",
    prompt: "POST request for multipart file upload with metadata",
  },
];

export function AiApiRequestGenerator({
  onRequestGenerated,
  onCancel,
}: AiApiRequestGeneratorProps) {
  const [prompt, setPrompt] = useState("");
  const [baseUrl, setBaseUrl] = useState("");
  const [isGenerating, setIsGenerating] = useState(false);
  const [generatedRequest, setGeneratedRequest] = useState<GeneratedApiRequest | null>(null);
  const [error, setError] = useState<string | null>(null);

  // Generate API request using AI
  const handleGenerate = useCallback(async () => {
    if (!prompt.trim()) {
      setError("Please enter a description of the API request");
      return;
    }

    setIsGenerating(true);
    setError(null);
    setGeneratedRequest(null);

    try {
      const response = await invoke<{
        success: boolean;
        message?: string;
        data?: GenerateApiRequestResponse;
      }>("generate_api_request_with_ai", {
        input: {
          user_prompt: prompt,
          base_url: baseUrl || undefined,
        },
      });

      if (response.success && response.data?.data) {
        setGeneratedRequest(response.data.data);
      } else {
        setError(response.data?.error || response.message || "Failed to generate API request");
      }
    } catch (err) {
      setError(String(err));
    } finally {
      setIsGenerating(false);
    }
  }, [prompt, baseUrl]);

  // Accept generated request
  const handleAccept = useCallback(() => {
    if (generatedRequest) {
      onRequestGenerated(generatedRequest);
    }
  }, [generatedRequest, onRequestGenerated]);

  // Apply template
  const handleApplyTemplate = useCallback((template: { prompt: string }) => {
    setPrompt(template.prompt);
  }, []);

  return (
    <div className="flex flex-col h-full bg-neutral-900 rounded-lg border border-neutral-700 overflow-hidden">
      {/* Header */}
      <div className="flex items-center justify-between p-3 border-b border-neutral-700">
        <div className="flex items-center gap-2">
          <Sparkles className="w-4 h-4 text-indigo-400" />
          <span className="text-sm font-medium text-neutral-200">AI API Request Generator</span>
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
        {!generatedRequest && (
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

        {/* Prompt input or generated request */}
        {!generatedRequest ? (
          <>
            {/* Base URL (optional) */}
            <div className="mb-4">
              <label className="block text-xs text-neutral-400 mb-2">Base URL (optional)</label>
              <input
                type="text"
                value={baseUrl}
                onChange={(e) => setBaseUrl(e.target.value)}
                placeholder="https://api.example.com"
                className="w-full px-3 py-2 bg-neutral-800 border border-neutral-700 rounded text-sm text-neutral-200 placeholder-neutral-500 focus:outline-none focus:border-indigo-500"
              />
            </div>

            {/* Prompt textarea */}
            <div className="flex-1 flex flex-col min-h-0">
              <label className="block text-xs text-neutral-400 mb-2">
                Describe the API request you need
              </label>
              <textarea
                value={prompt}
                onChange={(e) => setPrompt(e.target.value)}
                placeholder="Example: POST request to create a new user with name, email, and password fields, including proper Content-Type header"
                className="flex-1 px-3 py-2 bg-neutral-800 border border-neutral-700 rounded text-sm text-neutral-200 placeholder-neutral-500 resize-none focus:outline-none focus:border-indigo-500 min-h-[100px]"
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
              className="mt-4 flex items-center justify-center gap-2 px-4 py-2 bg-indigo-600 text-white rounded hover:bg-indigo-700 disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
            >
              {isGenerating ? (
                <>
                  <Loader2 className="w-4 h-4 animate-spin" />
                  Generating...
                </>
              ) : (
                <>
                  <Wand2 className="w-4 h-4" />
                  Generate Request
                </>
              )}
            </button>
          </>
        ) : (
          <>
            {/* Generated request preview */}
            <div className="flex-1 flex flex-col min-h-0 overflow-auto">
              {/* Name & Method */}
              <div className="mb-4 flex items-center gap-3">
                <span
                  className={`text-xs px-2 py-1 rounded font-mono ${METHOD_COLORS[generatedRequest.method]}`}
                >
                  {generatedRequest.method}
                </span>
                <span className="text-sm font-medium text-neutral-200">
                  {generatedRequest.name}
                </span>
              </div>

              {/* Description */}
              {generatedRequest.description && (
                <div className="mb-4">
                  <label className="block text-xs text-neutral-400 mb-1">Description</label>
                  <p className="text-sm text-neutral-300">{generatedRequest.description}</p>
                </div>
              )}

              {/* URL */}
              <div className="mb-4">
                <label className="block text-xs text-neutral-400 mb-1">URL</label>
                <div className="p-2 bg-neutral-950 rounded border border-neutral-700">
                  <code className="text-sm font-mono text-neutral-300">{generatedRequest.url}</code>
                </div>
              </div>

              {/* Headers */}
              {Object.keys(generatedRequest.headers).length > 0 && (
                <div className="mb-4">
                  <label className="block text-xs text-neutral-400 mb-1">Headers</label>
                  <div className="p-2 bg-neutral-950 rounded border border-neutral-700 space-y-1">
                    {Object.entries(generatedRequest.headers).map(([key, value]) => (
                      <div key={key} className="flex gap-2 text-xs font-mono">
                        <span className="text-indigo-400">{key}:</span>
                        <span className="text-neutral-300">{value}</span>
                      </div>
                    ))}
                  </div>
                </div>
              )}

              {/* Body */}
              {generatedRequest.body && (
                <div className="mb-4 flex-1 min-h-0">
                  <label className="block text-xs text-neutral-400 mb-1">
                    Body ({generatedRequest.body_content_type})
                  </label>
                  <div className="max-h-[150px] overflow-auto p-3 bg-neutral-950 rounded border border-neutral-700">
                    <pre className="text-sm font-mono text-neutral-300 whitespace-pre-wrap">
                      {generatedRequest.body}
                    </pre>
                  </div>
                </div>
              )}

              {/* Timeout */}
              <div className="mb-4">
                <label className="block text-xs text-neutral-400 mb-1">Timeout</label>
                <span className="text-sm text-neutral-300">{generatedRequest.timeout_ms}ms</span>
              </div>
            </div>

            {/* Action buttons */}
            <div className="mt-4 flex gap-2">
              <button
                onClick={() => {
                  setGeneratedRequest(null);
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
                Use Request
              </button>
            </div>
          </>
        )}
      </div>

      {/* Footer */}
      <div className="p-2 border-t border-neutral-700 text-xs text-neutral-500">
        AI-generated requests should be reviewed before saving
      </div>
    </div>
  );
}

export default AiApiRequestGenerator;
