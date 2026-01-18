/**
 * ApiRequestStepEditor.tsx
 *
 * Modal component for editing API Request step configuration.
 * Supports method selection, URL input, headers, body, variable extractions,
 * and assertions.
 */

import React, { useState, useEffect } from "react";
import {
  X,
  Plus,
  Trash2,
  ChevronDown,
  ChevronRight,
  Globe,
  Play,
  AlertCircle,
  Check,
  Copy,
  Library,
  Loader2,
} from "lucide-react";
import type {
  ApiRequestStep,
  HttpMethod,
  ApiContentType,
  ApiVariableExtraction,
  ApiAssertion,
} from "../../types/unified-workflow";

const API_BASE = "http://localhost:9876";

interface ApiRequestStepEditorProps {
  step: ApiRequestStep;
  open: boolean;
  onClose: () => void;
  onSave: (updates: Partial<ApiRequestStep>) => void;
  /** Optional callback to handle save to library */
  onSavedToLibrary?: () => void;
}

const HTTP_METHODS: HttpMethod[] = ["GET", "POST", "PUT", "PATCH", "DELETE"];

const CONTENT_TYPES: { value: ApiContentType; label: string }[] = [
  { value: "application/json", label: "JSON" },
  { value: "application/x-www-form-urlencoded", label: "Form URL Encoded" },
  { value: "text/plain", label: "Plain Text" },
  { value: "none", label: "None" },
];

const ASSERTION_TYPES = [
  { value: "status_code", label: "Status Code" },
  { value: "json_path", label: "JSON Path" },
  { value: "header", label: "Header" },
  { value: "body_contains", label: "Body Contains" },
  { value: "response_time", label: "Response Time" },
];

const OPERATORS = [
  { value: "equals", label: "Equals" },
  { value: "contains", label: "Contains" },
  { value: "matches", label: "Matches (Regex)" },
  { value: "greater_than", label: "Greater Than" },
  { value: "less_than", label: "Less Than" },
];

export function ApiRequestStepEditor({
  step,
  open,
  onClose,
  onSave,
  onSavedToLibrary,
}: ApiRequestStepEditorProps) {
  // Local form state
  const [name, setName] = useState(step.name);
  const [method, setMethod] = useState<HttpMethod>(step.method);
  const [url, setUrl] = useState(step.url);
  const [headers, setHeaders] = useState<{ key: string; value: string }[]>([]);
  const [body, setBody] = useState(step.body || "");
  const [contentType, setContentType] = useState<ApiContentType>(
    step.content_type || "application/json",
  );
  const [timeoutMs, setTimeoutMs] = useState(step.timeout_ms?.toString() || "30000");
  const [followRedirects, setFollowRedirects] = useState(step.follow_redirects ?? true);
  const [extractions, setExtractions] = useState<ApiVariableExtraction[]>(step.extractions || []);
  const [assertions, setAssertions] = useState<ApiAssertion[]>(step.assertions || []);
  const [runOnSubsequent, setRunOnSubsequent] = useState(
    step.run_on_subsequent_iterations ?? false,
  );

  // UI state
  const [showAdvanced, setShowAdvanced] = useState(false);
  const [showExtractions, setShowExtractions] = useState(extractions.length > 0);
  const [showAssertions, setShowAssertions] = useState(assertions.length > 0);
  const [curlImport, setCurlImport] = useState("");
  const [showCurlImport, setShowCurlImport] = useState(false);
  const [testResult, setTestResult] = useState<{ success: boolean; message: string } | null>(null);
  const [isTesting, setIsTesting] = useState(false);
  const [isSavingToLibrary, setIsSavingToLibrary] = useState(false);
  const [saveToLibraryResult, setSaveToLibraryResult] = useState<{
    success: boolean;
    message: string;
  } | null>(null);

  // Initialize headers from step
  useEffect(() => {
    if (step.headers) {
      setHeaders(Object.entries(step.headers).map(([key, value]) => ({ key, value })));
    }
  }, [step.headers]);

  // Handle save
  const handleSave = () => {
    const headerRecord: Record<string, string> = {};
    headers.forEach((h) => {
      if (h.key.trim()) {
        headerRecord[h.key.trim()] = h.value;
      }
    });

    const updates: Partial<ApiRequestStep> = {
      name,
      method,
      url,
      headers: Object.keys(headerRecord).length > 0 ? headerRecord : undefined,
      body: body.trim() || undefined,
      content_type: contentType,
      timeout_ms: parseInt(timeoutMs) || 30000,
      follow_redirects: followRedirects,
      extractions: extractions.length > 0 ? extractions : undefined,
      assertions: assertions.length > 0 ? assertions : undefined,
      run_on_subsequent_iterations: runOnSubsequent,
    };

    onSave(updates);
    onClose();
  };

  // Handle save to library
  const handleSaveToLibrary = async () => {
    if (!url.trim()) {
      setSaveToLibraryResult({ success: false, message: "URL is required" });
      return;
    }

    setIsSavingToLibrary(true);
    setSaveToLibraryResult(null);

    try {
      const headerRecord: Record<string, string> = {};
      headers.forEach((h) => {
        if (h.key.trim()) {
          headerRecord[h.key.trim()] = h.value;
        }
      });

      const response = await fetch(`${API_BASE}/saved-api-requests`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          name: name || "New API Request",
          description: "",
          category: "general",
          tags: [],
          method,
          url,
          headers: headerRecord,
          body: body.trim() || undefined,
          body_content_type: contentType,
          timeout_ms: parseInt(timeoutMs) || 30000,
          follow_redirects: followRedirects,
          variable_extractions: extractions,
          assertions,
        }),
      });

      const result = await response.json();

      if (result.success) {
        setSaveToLibraryResult({ success: true, message: "Saved to library!" });
        onSavedToLibrary?.();
        // Clear the success message after 3 seconds
        setTimeout(() => setSaveToLibraryResult(null), 3000);
      } else {
        setSaveToLibraryResult({ success: false, message: result.error || "Failed to save" });
      }
    } catch (error) {
      setSaveToLibraryResult({
        success: false,
        message: error instanceof Error ? error.message : "Failed to save to library",
      });
    } finally {
      setIsSavingToLibrary(false);
    }
  };

  // Header management
  const addHeader = () => {
    setHeaders([...headers, { key: "", value: "" }]);
  };

  const updateHeader = (index: number, field: "key" | "value", value: string) => {
    const updated = [...headers];
    updated[index][field] = value;
    setHeaders(updated);
  };

  const removeHeader = (index: number) => {
    setHeaders(headers.filter((_, i) => i !== index));
  };

  // Extraction management
  const addExtraction = () => {
    setExtractions([...extractions, { variable_name: "", json_path: "" }]);
    setShowExtractions(true);
  };

  const updateExtraction = (index: number, field: keyof ApiVariableExtraction, value: string) => {
    const updated = [...extractions];
    updated[index] = { ...updated[index], [field]: value };
    setExtractions(updated);
  };

  const removeExtraction = (index: number) => {
    setExtractions(extractions.filter((_, i) => i !== index));
  };

  // Assertion management
  const addAssertion = () => {
    setAssertions([...assertions, { type: "status_code", expected: "200" }]);
    setShowAssertions(true);
  };

  const updateAssertion = (index: number, field: keyof ApiAssertion, value: string | number) => {
    const updated = [...assertions];
    updated[index] = { ...updated[index], [field]: value } as ApiAssertion;
    setAssertions(updated);
  };

  const removeAssertion = (index: number) => {
    setAssertions(assertions.filter((_, i) => i !== index));
  };

  // Import from cURL
  const handleCurlImport = async () => {
    if (!curlImport.trim()) return;

    try {
      const response = await fetch("http://localhost:9876/api-request/import-curl", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ curl_command: curlImport }),
      });

      if (!response.ok) {
        throw new Error("Failed to parse cURL command");
      }

      const result = await response.json();
      if (result.success && result.data) {
        const parsed = result.data;
        setMethod(parsed.method || "GET");
        setUrl(parsed.url || "");
        if (parsed.headers) {
          setHeaders(
            Object.entries(parsed.headers).map(([key, value]) => ({ key, value: value as string })),
          );
        }
        if (parsed.body) {
          setBody(parsed.body);
        }
        if (parsed.content_type) {
          setContentType(parsed.content_type as ApiContentType);
        }
        setShowCurlImport(false);
        setCurlImport("");
      }
    } catch (err) {
      console.error("Failed to import cURL:", err);
    }
  };

  // Test the request
  const handleTestRequest = async () => {
    setIsTesting(true);
    setTestResult(null);

    try {
      const headerRecord: Record<string, string> = {};
      headers.forEach((h) => {
        if (h.key.trim()) {
          headerRecord[h.key.trim()] = h.value;
        }
      });

      const response = await fetch("http://localhost:9876/api-request/test", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          method,
          url,
          headers: Object.keys(headerRecord).length > 0 ? headerRecord : undefined,
          body: body.trim() || undefined,
          content_type: contentType !== "none" ? contentType : undefined,
          timeout_ms: parseInt(timeoutMs) || 30000,
          follow_redirects: followRedirects,
        }),
      });

      const result = await response.json();
      if (result.success && result.data) {
        const data = result.data;
        const isHttpSuccess = data.status_code >= 200 && data.status_code < 400;

        // Build message with status and timing
        let message = `${data.status_code} ${data.status_text} (${data.response_time_ms}ms)`;

        // If HTTP error (4xx/5xx), show the response body for debugging
        if (!isHttpSuccess && data.response_body) {
          // Try to extract error message from JSON response
          try {
            const errorBody = JSON.parse(data.response_body);
            const errorMsg =
              errorBody.detail || errorBody.message || errorBody.error || data.response_body;
            message += `\n${typeof errorMsg === "string" ? errorMsg : JSON.stringify(errorMsg)}`;
          } catch {
            // Not JSON, show raw body (truncated)
            const body =
              data.response_body.length > 200
                ? data.response_body.substring(0, 200) + "..."
                : data.response_body;
            message += `\n${body}`;
          }
        }

        setTestResult({
          success: isHttpSuccess,
          message,
        });
      } else {
        setTestResult({
          success: false,
          message: result.error || "Request failed",
        });
      }
    } catch (err: unknown) {
      setTestResult({
        success: false,
        message: err instanceof Error ? err.message : "Request failed",
      });
    } finally {
      setIsTesting(false);
    }
  };

  // Validation
  const isValid = name.trim() && url.trim();

  if (!open) return null;

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center">
      {/* Backdrop */}
      <div className="absolute inset-0 bg-black/50" onClick={onClose} />

      {/* Dialog */}
      <div className="relative bg-zinc-800 border border-zinc-700 rounded-lg shadow-xl w-full max-w-2xl mx-4 max-h-[90vh] overflow-hidden flex flex-col">
        {/* Header */}
        <div className="flex items-center justify-between p-4 border-b border-zinc-700">
          <div className="flex items-center gap-3">
            <div className="p-2 rounded bg-cyan-500/10">
              <Globe className="w-5 h-5 text-cyan-400" />
            </div>
            <h3 className="text-lg font-semibold text-zinc-100">Edit API Request</h3>
          </div>
          <button
            onClick={onClose}
            className="p-1 text-zinc-400 hover:text-zinc-200 transition-colors"
          >
            <X className="w-5 h-5" />
          </button>
        </div>

        {/* Content */}
        <div className="flex-1 overflow-y-auto p-4 space-y-4">
          {/* Step Name */}
          <div>
            <label className="block text-sm font-medium text-zinc-400 mb-1">Step Name</label>
            <input
              type="text"
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder="API Request"
              className="w-full px-3 py-2 bg-zinc-900 border border-zinc-700 rounded-md text-zinc-200 placeholder-zinc-500 focus:outline-none focus:ring-2 focus:ring-cyan-500/50"
            />
          </div>

          {/* Method & URL */}
          <div className="flex gap-2">
            <div className="w-28">
              <label className="block text-sm font-medium text-zinc-400 mb-1">Method</label>
              <select
                value={method}
                onChange={(e) => setMethod(e.target.value as HttpMethod)}
                className="w-full px-3 py-2 bg-zinc-900 border border-zinc-700 rounded-md text-zinc-200 focus:outline-none focus:ring-2 focus:ring-cyan-500/50"
              >
                {HTTP_METHODS.map((m) => (
                  <option key={m} value={m}>
                    {m}
                  </option>
                ))}
              </select>
            </div>
            <div className="flex-1">
              <label className="block text-sm font-medium text-zinc-400 mb-1">
                URL <span className="text-zinc-500">(supports {"{{variables}}"}</span>
              </label>
              <input
                type="text"
                value={url}
                onChange={(e) => setUrl(e.target.value)}
                placeholder="https://api.example.com/endpoint"
                className="w-full px-3 py-2 bg-zinc-900 border border-zinc-700 rounded-md text-zinc-200 placeholder-zinc-500 focus:outline-none focus:ring-2 focus:ring-cyan-500/50 font-mono text-sm"
              />
            </div>
          </div>

          {/* Quick Actions */}
          <div className="flex gap-2">
            <button
              onClick={() => setShowCurlImport(!showCurlImport)}
              className="flex items-center gap-1.5 px-3 py-1.5 text-sm rounded-md bg-zinc-700 hover:bg-zinc-600 text-zinc-300 transition-colors"
            >
              <Copy className="w-4 h-4" />
              Import cURL
            </button>
            <button
              onClick={handleTestRequest}
              disabled={!url.trim() || isTesting}
              className="flex items-center gap-1.5 px-3 py-1.5 text-sm rounded-md bg-cyan-600 hover:bg-cyan-500 text-white disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
            >
              <Play className="w-4 h-4" />
              {isTesting ? "Testing..." : "Test Request"}
            </button>
          </div>

          {/* Test Result */}
          {testResult && (
            <div
              className={`p-3 rounded-md text-sm ${
                testResult.success ? "bg-green-500/10 text-green-400" : "bg-red-500/10 text-red-400"
              }`}
            >
              <div className="flex items-start gap-2">
                {testResult.success ? (
                  <Check className="w-4 h-4 mt-0.5 flex-shrink-0" />
                ) : (
                  <AlertCircle className="w-4 h-4 mt-0.5 flex-shrink-0" />
                )}
                <div className="whitespace-pre-wrap break-words font-mono">
                  {testResult.message}
                </div>
              </div>
            </div>
          )}

          {/* cURL Import */}
          {showCurlImport && (
            <div className="p-3 border border-zinc-700 rounded-md bg-zinc-900 space-y-3">
              <div className="space-y-2">
                <label className="block text-sm font-medium text-zinc-400">
                  Paste cURL command
                </label>
                <div className="text-xs text-zinc-500 space-y-1">
                  <p className="font-medium text-zinc-400">How to capture from browser:</p>
                  <ol className="list-decimal list-inside space-y-0.5 ml-1">
                    <li>Open DevTools (F12 or right-click → Inspect)</li>
                    <li>
                      Go to the <span className="text-cyan-400">Network</span> tab
                    </li>
                    <li>Perform the action that makes the API request</li>
                    <li>
                      Right-click the request →{" "}
                      <span className="text-cyan-400">Copy as cURL (bash)</span>
                    </li>
                    <li>Paste below</li>
                  </ol>
                  <p className="text-yellow-500/80 mt-1">
                    Use "bash" format, not "cmd" - cmd format is not supported.
                  </p>
                </div>
              </div>
              <textarea
                value={curlImport}
                onChange={(e) => setCurlImport(e.target.value)}
                placeholder="curl -X GET 'https://api.example.com' -H 'Authorization: Bearer token'"
                rows={3}
                className="w-full px-3 py-2 bg-zinc-800 border border-zinc-600 rounded-md text-zinc-200 placeholder-zinc-500 font-mono text-xs resize-none focus:outline-none focus:ring-2 focus:ring-cyan-500/50"
              />
              <div className="flex gap-2">
                <button
                  onClick={handleCurlImport}
                  className="px-3 py-1.5 text-sm rounded-md bg-cyan-600 hover:bg-cyan-500 text-white transition-colors"
                >
                  Import
                </button>
                <button
                  onClick={() => {
                    setShowCurlImport(false);
                    setCurlImport("");
                  }}
                  className="px-3 py-1.5 text-sm rounded-md bg-zinc-700 hover:bg-zinc-600 text-zinc-300 transition-colors"
                >
                  Cancel
                </button>
              </div>
            </div>
          )}

          {/* Headers */}
          <div>
            <div className="flex items-center justify-between mb-2">
              <label className="text-sm font-medium text-zinc-400">Headers</label>
              <button
                onClick={addHeader}
                className="flex items-center gap-1 text-xs text-cyan-400 hover:text-cyan-300"
              >
                <Plus className="w-3 h-3" />
                Add
              </button>
            </div>
            {headers.length === 0 ? (
              <p className="text-xs text-zinc-500 italic">No headers configured</p>
            ) : (
              <div className="space-y-2">
                {headers.map((header, index) => (
                  <div key={index} className="flex gap-2">
                    <input
                      type="text"
                      value={header.key}
                      onChange={(e) => updateHeader(index, "key", e.target.value)}
                      placeholder="Header-Name"
                      className="flex-1 px-2 py-1.5 bg-zinc-900 border border-zinc-700 rounded text-sm text-zinc-200 placeholder-zinc-500 focus:outline-none focus:ring-1 focus:ring-cyan-500/50"
                    />
                    <input
                      type="text"
                      value={header.value}
                      onChange={(e) => updateHeader(index, "value", e.target.value)}
                      placeholder="value"
                      className="flex-1 px-2 py-1.5 bg-zinc-900 border border-zinc-700 rounded text-sm text-zinc-200 placeholder-zinc-500 focus:outline-none focus:ring-1 focus:ring-cyan-500/50"
                    />
                    <button
                      onClick={() => removeHeader(index)}
                      className="p-1.5 text-zinc-500 hover:text-red-400 transition-colors"
                    >
                      <Trash2 className="w-4 h-4" />
                    </button>
                  </div>
                ))}
              </div>
            )}
          </div>

          {/* Body */}
          {(method === "POST" || method === "PUT" || method === "PATCH") && (
            <div>
              <div className="flex items-center justify-between mb-2">
                <label className="text-sm font-medium text-zinc-400">Request Body</label>
                <select
                  value={contentType}
                  onChange={(e) => setContentType(e.target.value as ApiContentType)}
                  className="px-2 py-1 bg-zinc-900 border border-zinc-700 rounded text-xs text-zinc-300 focus:outline-none"
                >
                  {CONTENT_TYPES.map((ct) => (
                    <option key={ct.value} value={ct.value}>
                      {ct.label}
                    </option>
                  ))}
                </select>
              </div>
              <textarea
                value={body}
                onChange={(e) => setBody(e.target.value)}
                placeholder={
                  contentType === "application/json" ? '{"key": "value"}' : "Request body..."
                }
                rows={4}
                className="w-full px-3 py-2 bg-zinc-900 border border-zinc-700 rounded-md text-zinc-200 placeholder-zinc-500 font-mono text-sm resize-none focus:outline-none focus:ring-2 focus:ring-cyan-500/50"
              />
            </div>
          )}

          {/* Variable Extractions */}
          <div className="border border-zinc-700 rounded-md overflow-hidden">
            <button
              onClick={() => setShowExtractions(!showExtractions)}
              className="w-full flex items-center justify-between p-3 bg-zinc-850 hover:bg-zinc-800 transition-colors"
            >
              <span className="text-sm font-medium text-zinc-300">
                Variable Extractions ({extractions.length})
              </span>
              {showExtractions ? (
                <ChevronDown className="w-4 h-4 text-zinc-500" />
              ) : (
                <ChevronRight className="w-4 h-4 text-zinc-500" />
              )}
            </button>
            {showExtractions && (
              <div className="p-3 border-t border-zinc-700 space-y-3">
                <p className="text-xs text-zinc-500">
                  Extract values from the response using JSON paths. Use {"{{variable_name}}"} in
                  subsequent steps.
                </p>
                {extractions.map((ext, index) => (
                  <div key={index} className="flex gap-2 items-start">
                    <div className="flex-1">
                      <input
                        type="text"
                        value={ext.variable_name}
                        onChange={(e) => updateExtraction(index, "variable_name", e.target.value)}
                        placeholder="variable_name"
                        className="w-full px-2 py-1.5 bg-zinc-900 border border-zinc-700 rounded text-sm text-zinc-200 placeholder-zinc-500 focus:outline-none focus:ring-1 focus:ring-cyan-500/50"
                      />
                    </div>
                    <div className="flex-1">
                      <input
                        type="text"
                        value={ext.json_path}
                        onChange={(e) => updateExtraction(index, "json_path", e.target.value)}
                        placeholder="$.data.id"
                        className="w-full px-2 py-1.5 bg-zinc-900 border border-zinc-700 rounded text-sm text-zinc-200 placeholder-zinc-500 font-mono focus:outline-none focus:ring-1 focus:ring-cyan-500/50"
                      />
                    </div>
                    <button
                      onClick={() => removeExtraction(index)}
                      className="p-1.5 text-zinc-500 hover:text-red-400 transition-colors"
                    >
                      <Trash2 className="w-4 h-4" />
                    </button>
                  </div>
                ))}
                <button
                  onClick={addExtraction}
                  className="flex items-center gap-1 text-xs text-cyan-400 hover:text-cyan-300"
                >
                  <Plus className="w-3 h-3" />
                  Add Extraction
                </button>
              </div>
            )}
          </div>

          {/* Assertions */}
          <div className="border border-zinc-700 rounded-md overflow-hidden">
            <button
              onClick={() => setShowAssertions(!showAssertions)}
              className="w-full flex items-center justify-between p-3 bg-zinc-850 hover:bg-zinc-800 transition-colors"
            >
              <span className="text-sm font-medium text-zinc-300">
                Assertions ({assertions.length})
              </span>
              {showAssertions ? (
                <ChevronDown className="w-4 h-4 text-zinc-500" />
              ) : (
                <ChevronRight className="w-4 h-4 text-zinc-500" />
              )}
            </button>
            {showAssertions && (
              <div className="p-3 border-t border-zinc-700 space-y-3">
                <p className="text-xs text-zinc-500">
                  Assert response values. Step fails if any assertion fails.
                </p>
                {assertions.map((assertion, index) => (
                  <div key={index} className="flex gap-2 items-start flex-wrap">
                    <select
                      value={assertion.type}
                      onChange={(e) => updateAssertion(index, "type", e.target.value)}
                      className="px-2 py-1.5 bg-zinc-900 border border-zinc-700 rounded text-sm text-zinc-200 focus:outline-none"
                    >
                      {ASSERTION_TYPES.map((t) => (
                        <option key={t.value} value={t.value}>
                          {t.label}
                        </option>
                      ))}
                    </select>
                    {assertion.type === "json_path" && (
                      <input
                        type="text"
                        value={assertion.json_path || ""}
                        onChange={(e) => updateAssertion(index, "json_path", e.target.value)}
                        placeholder="$.path"
                        className="w-24 px-2 py-1.5 bg-zinc-900 border border-zinc-700 rounded text-sm text-zinc-200 placeholder-zinc-500 font-mono focus:outline-none"
                      />
                    )}
                    {assertion.type === "header" && (
                      <input
                        type="text"
                        value={assertion.header_name || ""}
                        onChange={(e) => updateAssertion(index, "header_name", e.target.value)}
                        placeholder="Header-Name"
                        className="w-28 px-2 py-1.5 bg-zinc-900 border border-zinc-700 rounded text-sm text-zinc-200 placeholder-zinc-500 focus:outline-none"
                      />
                    )}
                    <select
                      value={assertion.operator || "equals"}
                      onChange={(e) => updateAssertion(index, "operator", e.target.value)}
                      className="px-2 py-1.5 bg-zinc-900 border border-zinc-700 rounded text-sm text-zinc-200 focus:outline-none"
                    >
                      {OPERATORS.map((op) => (
                        <option key={op.value} value={op.value}>
                          {op.label}
                        </option>
                      ))}
                    </select>
                    <input
                      type="text"
                      value={String(assertion.expected)}
                      onChange={(e) => updateAssertion(index, "expected", e.target.value)}
                      placeholder="expected value"
                      className="flex-1 min-w-[100px] px-2 py-1.5 bg-zinc-900 border border-zinc-700 rounded text-sm text-zinc-200 placeholder-zinc-500 focus:outline-none"
                    />
                    <button
                      onClick={() => removeAssertion(index)}
                      className="p-1.5 text-zinc-500 hover:text-red-400 transition-colors"
                    >
                      <Trash2 className="w-4 h-4" />
                    </button>
                  </div>
                ))}
                <button
                  onClick={addAssertion}
                  className="flex items-center gap-1 text-xs text-cyan-400 hover:text-cyan-300"
                >
                  <Plus className="w-3 h-3" />
                  Add Assertion
                </button>
              </div>
            )}
          </div>

          {/* Advanced Settings */}
          <div className="border border-zinc-700 rounded-md overflow-hidden">
            <button
              onClick={() => setShowAdvanced(!showAdvanced)}
              className="w-full flex items-center justify-between p-3 bg-zinc-850 hover:bg-zinc-800 transition-colors"
            >
              <span className="text-sm font-medium text-zinc-300">Advanced Settings</span>
              {showAdvanced ? (
                <ChevronDown className="w-4 h-4 text-zinc-500" />
              ) : (
                <ChevronRight className="w-4 h-4 text-zinc-500" />
              )}
            </button>
            {showAdvanced && (
              <div className="p-3 border-t border-zinc-700 space-y-3">
                <div className="grid grid-cols-2 gap-3">
                  <div>
                    <label className="block text-xs text-zinc-500 mb-1">Timeout (ms)</label>
                    <input
                      type="number"
                      value={timeoutMs}
                      onChange={(e) => setTimeoutMs(e.target.value)}
                      min={1000}
                      max={300000}
                      className="w-full px-2 py-1.5 bg-zinc-900 border border-zinc-700 rounded text-sm text-zinc-200 focus:outline-none focus:ring-1 focus:ring-cyan-500/50"
                    />
                  </div>
                  <div className="flex items-center gap-2">
                    <input
                      type="checkbox"
                      id="followRedirects"
                      checked={followRedirects}
                      onChange={(e) => setFollowRedirects(e.target.checked)}
                      className="w-4 h-4 rounded border-zinc-600 bg-zinc-900 text-cyan-500 focus:ring-cyan-500/50"
                    />
                    <label htmlFor="followRedirects" className="text-sm text-zinc-300">
                      Follow redirects
                    </label>
                  </div>
                </div>
                <div className="flex items-center gap-2">
                  <input
                    type="checkbox"
                    id="runOnSubsequent"
                    checked={runOnSubsequent}
                    onChange={(e) => setRunOnSubsequent(e.target.checked)}
                    className="w-4 h-4 rounded border-zinc-600 bg-zinc-900 text-cyan-500 focus:ring-cyan-500/50"
                  />
                  <label htmlFor="runOnSubsequent" className="text-sm text-zinc-300">
                    Run on subsequent iterations
                  </label>
                </div>
              </div>
            )}
          </div>
        </div>

        {/* Footer */}
        <div className="flex flex-col gap-2 p-4 border-t border-zinc-700">
          {/* Save to Library result message */}
          {saveToLibraryResult && (
            <div
              className={`flex items-center gap-2 px-3 py-2 rounded-md text-sm ${
                saveToLibraryResult.success
                  ? "bg-green-500/10 text-green-400"
                  : "bg-red-500/10 text-red-400"
              }`}
            >
              {saveToLibraryResult.success ? (
                <Check className="w-4 h-4" />
              ) : (
                <AlertCircle className="w-4 h-4" />
              )}
              {saveToLibraryResult.message}
            </div>
          )}

          <div className="flex gap-3">
            <button
              onClick={onClose}
              className="flex-1 px-4 py-2 bg-zinc-700 text-zinc-200 rounded-md font-medium hover:bg-zinc-600 transition-colors"
            >
              Cancel
            </button>
            <button
              onClick={handleSaveToLibrary}
              disabled={isSavingToLibrary || !isValid}
              className="flex items-center justify-center gap-2 px-4 py-2 bg-zinc-700 text-zinc-200 rounded-md font-medium hover:bg-zinc-600 disabled:opacity-50 disabled:cursor-not-allowed transition-colors border border-zinc-600"
              title="Save this request to the Library for reuse"
            >
              {isSavingToLibrary ? (
                <Loader2 className="w-4 h-4 animate-spin" />
              ) : (
                <Library className="w-4 h-4" />
              )}
              Save to Library
            </button>
            <button
              onClick={handleSave}
              disabled={!isValid}
              className="flex-1 px-4 py-2 bg-cyan-600 text-white rounded-md font-medium hover:bg-cyan-500 disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
            >
              Save Changes
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
