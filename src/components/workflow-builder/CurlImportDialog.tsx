/**
 * CurlImportDialog.tsx
 *
 * Modal dialog for importing a cURL command and converting it to an API request step.
 */

import { useState } from "react";
import { Upload, X, Loader2, AlertCircle, Check } from "lucide-react";
import type { WorkflowPhase, HttpMethod } from "../../types";
import { getAccentColors, getStatusColors } from "@/design-system";
import { getApiBase, tracedFetch } from "@/lib/runner-api";

// Parsed cURL response from the backend
interface ParsedCurl {
  method: string;
  url: string;
  headers: Record<string, string>;
  body?: string;
  content_type?: string;
}

interface CurlImportDialogProps {
  /** Whether the dialog is open */
  isOpen: boolean;
  /** Called when the dialog is closed */
  onClose: () => void;
  /** Called when importing is complete with the parsed data */
  onImport: (
    parsed: {
      method: HttpMethod;
      url: string;
      headers: Record<string, string>;
      body?: string;
      bodyContentType?: string;
    },
    phase: WorkflowPhase,
  ) => void;
  /** The phase for the new step */
  phase: WorkflowPhase;
}

export function CurlImportDialog({ isOpen, onClose, onImport, phase }: CurlImportDialogProps) {
  const [curlCommand, setCurlCommand] = useState("");
  const [isLoading, setIsLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [parsedResult, setParsedResult] = useState<ParsedCurl | null>(null);

  // Parse the cURL command
  const handleParse = async () => {
    if (!curlCommand.trim()) {
      setError("Please enter a cURL command");
      return;
    }

    setIsLoading(true);
    setError(null);
    setParsedResult(null);

    try {
      const response = await tracedFetch(`${getApiBase()}/api-request/import-curl`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ curl_command: curlCommand }),
      });

      const data = await response.json();

      if (data.success && data.data) {
        setParsedResult(data.data);
      } else {
        setError(data.error || "Failed to parse cURL command");
      }
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to parse cURL command");
    } finally {
      setIsLoading(false);
    }
  };

  // Import the parsed result
  const handleImport = () => {
    if (!parsedResult) return;

    onImport(
      {
        method: parsedResult.method as HttpMethod,
        url: parsedResult.url,
        headers: parsedResult.headers,
        body: parsedResult.body,
        bodyContentType: parsedResult.content_type,
      },
      phase,
    );
    onClose();
    // Reset state for next use
    setCurlCommand("");
    setParsedResult(null);
    setError(null);
  };

  // Reset on close
  const handleClose = () => {
    onClose();
    setCurlCommand("");
    setParsedResult(null);
    setError(null);
  };

  if (!isOpen) return null;

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center">
      {/* Backdrop */}
      <div className="absolute inset-0 bg-black/50" onClick={handleClose} />

      {/* Dialog */}
      <div className="relative bg-card border border-border rounded-lg shadow-xl w-full max-w-2xl mx-4 max-h-[80vh] flex flex-col overflow-hidden">
        {/* Header */}
        <div className="flex items-center justify-between px-6 py-4 border-b border-border">
          <div className="flex items-center gap-2">
            <Upload className={`w-5 h-5 ${getAccentColors("cyan").text}`} />
            <h3 className="text-lg font-semibold">Import cURL Command</h3>
          </div>
          <button
            onClick={handleClose}
            className="p-1 text-muted-foreground hover:text-foreground transition-colors"
          >
            <X className="w-5 h-5" />
          </button>
        </div>

        {/* Content */}
        <div className="flex-1 overflow-y-auto p-6 space-y-4">
          {/* cURL Input */}
          <div>
            <label className="block text-sm font-medium mb-2">cURL Command</label>
            <textarea
              value={curlCommand}
              onChange={(e) => {
                setCurlCommand(e.target.value);
                setParsedResult(null);
                setError(null);
              }}
              placeholder={`curl 'https://api.example.com/endpoint' \\
  -H 'Content-Type: application/json' \\
  -H 'Authorization: Bearer token' \\
  --data-raw '{"key": "value"}'`}
              className="w-full px-3 py-2 bg-background border border-border rounded-md text-sm font-mono resize-none h-40 focus:outline-none focus:ring-2 focus:ring-primary"
              autoFocus
            />
            <p className="text-xs text-muted-foreground mt-1">
              Paste a cURL command copied from browser DevTools or other sources
            </p>
          </div>

          {/* Error Message */}
          {error && (
            <div
              className={`flex items-start gap-2 p-3 rounded-md ${getStatusColors("error").bg} ${getStatusColors("error").text}`}
            >
              <AlertCircle className="w-4 h-4 mt-0.5 flex-shrink-0" />
              <span className="text-sm">{error}</span>
            </div>
          )}

          {/* Parse Button */}
          {!parsedResult && (
            <button
              onClick={handleParse}
              disabled={isLoading || !curlCommand.trim()}
              className={`w-full flex items-center justify-center gap-2 px-4 py-2 ${getAccentColors("cyan").bgSolid} text-white rounded-md font-medium hover:opacity-90 disabled:opacity-50 disabled:cursor-not-allowed transition-colors`}
            >
              {isLoading ? (
                <>
                  <Loader2 className="w-4 h-4 animate-spin" />
                  Parsing...
                </>
              ) : (
                "Parse cURL"
              )}
            </button>
          )}

          {/* Parsed Result Preview */}
          {parsedResult && (
            <div className="space-y-3 p-4 bg-muted rounded-lg">
              <div
                data-content-role="status"
                className="flex items-center gap-2 text-sm font-medium text-green-500"
              >
                <Check className="w-4 h-4" />
                Successfully parsed
              </div>

              <div className="space-y-2">
                <div className="flex items-center gap-2">
                  <span
                    data-content-role="label"
                    className="text-xs font-medium text-muted-foreground uppercase"
                  >
                    Method:
                  </span>
                  <span
                    data-content-role="badge"
                    className={`px-2 py-0.5 text-xs font-medium rounded ${
                      parsedResult.method === "GET"
                        ? "bg-green-500/20 text-green-400"
                        : parsedResult.method === "POST"
                          ? "bg-blue-500/20 text-blue-400"
                          : parsedResult.method === "PUT"
                            ? "bg-amber-500/20 text-amber-400"
                            : parsedResult.method === "DELETE"
                              ? "bg-red-500/20 text-red-400"
                              : "bg-purple-500/20 text-purple-400"
                    }`}
                  >
                    {parsedResult.method}
                  </span>
                </div>

                <div>
                  <span
                    data-content-role="label"
                    className="text-xs font-medium text-muted-foreground uppercase"
                  >
                    URL:
                  </span>
                  <div
                    data-content-role="body-text"
                    data-content-label="parsed URL"
                    className="text-sm font-mono mt-1 break-all"
                  >
                    {parsedResult.url}
                  </div>
                </div>

                {Object.keys(parsedResult.headers).length > 0 && (
                  <div>
                    <span
                      data-content-role="label"
                      className="text-xs font-medium text-muted-foreground uppercase"
                    >
                      Headers: ({Object.keys(parsedResult.headers).length})
                    </span>
                    <div className="mt-1 space-y-1">
                      {Object.entries(parsedResult.headers)
                        .slice(0, 3)
                        .map(([key, value]) => (
                          <div key={key} className="text-xs font-mono truncate">
                            <span className="text-muted-foreground">{key}:</span> {value}
                          </div>
                        ))}
                      {Object.keys(parsedResult.headers).length > 3 && (
                        <div className="text-xs text-muted-foreground">
                          +{Object.keys(parsedResult.headers).length - 3} more headers
                        </div>
                      )}
                    </div>
                  </div>
                )}

                {parsedResult.body && (
                  <div>
                    <span
                      data-content-role="label"
                      className="text-xs font-medium text-muted-foreground uppercase"
                    >
                      Body:
                    </span>
                    <pre className="text-xs font-mono mt-1 p-2 bg-background rounded max-h-24 overflow-auto">
                      {parsedResult.body.length > 200
                        ? parsedResult.body.substring(0, 200) + "..."
                        : parsedResult.body}
                    </pre>
                  </div>
                )}
              </div>
            </div>
          )}
        </div>

        {/* Footer */}
        <div className="flex gap-3 px-6 py-4 border-t border-border">
          <button
            onClick={handleClose}
            className="flex-1 px-4 py-2 bg-muted text-foreground rounded-md font-medium hover:bg-muted/80 transition-colors"
          >
            Cancel
          </button>
          {parsedResult && (
            <button
              onClick={handleImport}
              className={`flex-1 flex items-center justify-center gap-2 px-4 py-2 ${getAccentColors("cyan").bgSolid} text-white rounded-md font-medium hover:opacity-90 transition-colors`}
            >
              <Check className="w-4 h-4" />
              Import
            </button>
          )}
        </div>
      </div>
    </div>
  );
}

export default CurlImportDialog;
