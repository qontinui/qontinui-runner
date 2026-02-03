/**
 * ApiRequestBuilderTab.tsx
 *
 * Builder for creating and editing saved API request templates.
 * These templates can be reused across workflows for HTTP requests.
 */

import { useState, useEffect, useCallback } from "react";
import {
  Globe,
  Plus,
  Save,
  Trash2,
  Loader2,
  Search,
  Tag,
  FolderOpen,
  Copy,
  X,
  Settings,
  Clock,
  Code,
  Check,
} from "lucide-react";
import type {
  SavedApiRequest,
  CreateSavedApiRequestRequest,
  UpdateSavedApiRequestRequest,
  HttpMethod,
  ApiContentType,
} from "../types";
import { getAccentColors } from "@/design-system";
import { BuilderToolbar, toolbarActions } from "./ui/BuilderToolbar";
import { AiApiRequestGenerator } from "./api-request-builder/AiApiRequestGenerator";
import { BatchDeleteDialog } from "./ui/BatchDeleteDialog";

const API_BASE = "http://localhost:9876";

const HTTP_METHODS: HttpMethod[] = ["GET", "POST", "PUT", "PATCH", "DELETE"];

const CONTENT_TYPES: { value: ApiContentType; label: string }[] = [
  { value: "application/json", label: "JSON" },
  { value: "application/x-www-form-urlencoded", label: "Form URL Encoded" },
  { value: "text/plain", label: "Plain Text" },
  { value: "none", label: "None" },
];

const METHOD_COLORS: Record<HttpMethod, string> = {
  GET: "bg-green-900/50 text-green-300",
  POST: "bg-blue-900/50 text-blue-300",
  PUT: "bg-amber-900/50 text-amber-300",
  PATCH: "bg-orange-900/50 text-orange-300",
  DELETE: "bg-red-900/50 text-red-300",
};

interface ApiRequestBuilderTabProps {
  onLog?: (level: string, message: string) => void;
  editRequestId?: string | null;
  _onNavigateToLibrary?: () => void;
}

export function ApiRequestBuilderTab({
  onLog,
  editRequestId,
  _onNavigateToLibrary,
}: ApiRequestBuilderTabProps) {
  // State
  const [requests, setRequests] = useState<SavedApiRequest[]>([]);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [searchQuery, setSearchQuery] = useState("");
  const [selectedRequest, setSelectedRequest] = useState<SavedApiRequest | null>(null);
  const [isCreating, setIsCreating] = useState(false);
  const [showAiGenerator, setShowAiGenerator] = useState(false);

  // Selection mode state (for batch delete)
  const [isSelectionMode, setIsSelectionMode] = useState(false);
  const [selectedIds, setSelectedIds] = useState<Set<string>>(new Set());
  const [showBatchDeleteDialog, setShowBatchDeleteDialog] = useState(false);
  const [isDeleting, setIsDeleting] = useState(false);

  // Form state
  const [formName, setFormName] = useState("");
  const [formDescription, setFormDescription] = useState("");
  const [formCategory, setFormCategory] = useState("");
  const [formTags, setFormTags] = useState("");
  const [formMethod, setFormMethod] = useState<HttpMethod>("GET");
  const [formUrl, setFormUrl] = useState("");
  const [formHeaders, setFormHeaders] = useState<{ key: string; value: string }[]>([]);
  const [formBody, setFormBody] = useState("");
  const [formContentType, setFormContentType] = useState<ApiContentType>("application/json");
  const [formTimeout, setFormTimeout] = useState(30000);
  const [formFollowRedirects, setFormFollowRedirects] = useState(true);

  const accentColors = getAccentColors("indigo");

  // Fetch requests
  const fetchRequests = useCallback(async () => {
    try {
      setLoading(true);
      const response = await fetch(`${API_BASE}/saved-api-requests`);
      if (response.ok) {
        const result = await response.json();
        if (result.success && result.data) {
          setRequests(result.data);
        } else if (Array.isArray(result)) {
          setRequests(result);
        } else {
          setRequests([]);
        }
      }
    } catch (error) {
      console.error("Failed to fetch API requests:", error);
      onLog?.("error", `Failed to fetch API requests: ${error}`);
    } finally {
      setLoading(false);
    }
  }, [onLog]);

  useEffect(() => {
    fetchRequests();
  }, [fetchRequests]);

  // Load request for editing
  useEffect(() => {
    if (editRequestId && requests.length > 0) {
      const request = requests.find((r) => r.id === editRequestId);
      if (request) {
        selectRequest(request);
      }
    }
  }, [editRequestId, requests]);

  // Select a request for editing
  const selectRequest = (request: SavedApiRequest) => {
    setSelectedRequest(request);
    setIsCreating(false);
    setFormName(request.name);
    setFormDescription(request.description);
    setFormCategory(request.category);
    setFormTags(request.tags.join(", "));
    setFormMethod(request.method);
    setFormUrl(request.url);
    setFormHeaders(Object.entries(request.headers).map(([key, value]) => ({ key, value })));
    setFormBody(request.body || "");
    setFormContentType(request.body_content_type || "application/json");
    setFormTimeout(request.timeout_ms);
    setFormFollowRedirects(request.follow_redirects);
  };

  // Start creating a new request
  const startCreate = () => {
    setSelectedRequest(null);
    setIsCreating(true);
    setFormName("");
    setFormDescription("");
    setFormCategory("");
    setFormTags("");
    setFormMethod("GET");
    setFormUrl("");
    setFormHeaders([]);
    setFormBody("");
    setFormContentType("application/json");
    setFormTimeout(30000);
    setFormFollowRedirects(true);
  };

  // Add header
  const addHeader = () => {
    setFormHeaders([...formHeaders, { key: "", value: "" }]);
  };

  // Update header
  const updateHeader = (index: number, field: "key" | "value", value: string) => {
    const updated = [...formHeaders];
    updated[index][field] = value;
    setFormHeaders(updated);
  };

  // Remove header
  const removeHeader = (index: number) => {
    setFormHeaders(formHeaders.filter((_, i) => i !== index));
  };

  // Save request
  const handleSave = async () => {
    if (!formName.trim() || !formUrl.trim()) {
      onLog?.("warning", "Name and URL are required");
      return;
    }

    setSaving(true);
    try {
      const headers: Record<string, string> = {};
      formHeaders.forEach((h) => {
        if (h.key.trim()) {
          headers[h.key.trim()] = h.value;
        }
      });

      const payload: CreateSavedApiRequestRequest | UpdateSavedApiRequestRequest = {
        name: formName.trim(),
        description: formDescription.trim(),
        category: formCategory.trim() || "general",
        tags: formTags
          .split(",")
          .map((t) => t.trim())
          .filter(Boolean),
        method: formMethod,
        url: formUrl.trim(),
        headers,
        body: formBody.trim() || undefined,
        body_content_type: formBody.trim() ? formContentType : undefined,
        timeout_ms: formTimeout,
        follow_redirects: formFollowRedirects,
      };

      let response;
      if (selectedRequest) {
        response = await fetch(`${API_BASE}/saved-api-requests/${selectedRequest.id}`, {
          method: "PUT",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify(payload),
        });
      } else {
        response = await fetch(`${API_BASE}/saved-api-requests`, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify(payload),
        });
      }

      const result = await response.json();
      if (result.success && result.data) {
        onLog?.("success", `API request "${formName}" saved successfully`);
        await fetchRequests();
        selectRequest(result.data);
      } else if (response.ok && result.id) {
        onLog?.("success", `API request "${formName}" saved successfully`);
        await fetchRequests();
        selectRequest(result);
      } else {
        onLog?.("error", `Failed to save API request: ${result.error || "Unknown error"}`);
      }
    } catch (error) {
      onLog?.("error", `Failed to save API request: ${error}`);
    } finally {
      setSaving(false);
    }
  };

  // Delete request
  const handleDelete = async () => {
    if (!selectedRequest) return;

    if (!confirm(`Delete API request "${selectedRequest.name}"?`)) return;

    try {
      const response = await fetch(`${API_BASE}/saved-api-requests/${selectedRequest.id}`, {
        method: "DELETE",
      });

      const result = await response.json();
      if (result.success || response.ok) {
        onLog?.("success", `API request "${selectedRequest.name}" deleted`);
        setSelectedRequest(null);
        setIsCreating(false);
        await fetchRequests();
      } else {
        onLog?.("error", `Failed to delete API request: ${result.error || "Unknown error"}`);
      }
    } catch (error) {
      onLog?.("error", `Failed to delete API request: ${error}`);
    }
  };

  // Duplicate request
  const handleDuplicate = async () => {
    if (!selectedRequest) return;

    try {
      const payload: CreateSavedApiRequestRequest = {
        name: `${selectedRequest.name} (Copy)`,
        description: selectedRequest.description,
        category: selectedRequest.category,
        tags: selectedRequest.tags,
        method: selectedRequest.method,
        url: selectedRequest.url,
        headers: selectedRequest.headers,
        body: selectedRequest.body,
        body_content_type: selectedRequest.body_content_type,
        timeout_ms: selectedRequest.timeout_ms,
        follow_redirects: selectedRequest.follow_redirects,
      };

      const response = await fetch(`${API_BASE}/saved-api-requests`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(payload),
      });

      const result = await response.json();
      if (result.success && result.data) {
        onLog?.("success", `API request duplicated as "${result.data.name}"`);
        await fetchRequests();
        selectRequest(result.data);
      } else if (response.ok && result.id) {
        onLog?.("success", `API request duplicated as "${result.name}"`);
        await fetchRequests();
        selectRequest(result);
      }
    } catch (error) {
      onLog?.("error", `Failed to duplicate API request: ${error}`);
    }
  };

  // Handle AI-generated request
  const handleAiRequestGenerated = (generated: {
    name: string;
    description: string;
    method: HttpMethod;
    url: string;
    headers: Record<string, string>;
    body?: string;
    body_content_type: ApiContentType;
    timeout_ms: number;
  }) => {
    // Set up form with generated values
    setFormName(generated.name);
    setFormDescription(generated.description);
    setFormMethod(generated.method);
    setFormUrl(generated.url);
    setFormHeaders(Object.entries(generated.headers).map(([key, value]) => ({ key, value })));
    setFormBody(generated.body || "");
    setFormContentType(generated.body_content_type);
    setFormTimeout(generated.timeout_ms);
    setFormCategory("");
    setFormTags("");
    setFormFollowRedirects(true);

    // Switch to create mode
    setSelectedRequest(null);
    setIsCreating(true);
    setShowAiGenerator(false);

    onLog?.("success", "API request generated with AI - review and save");
  };

  // Filter requests
  const filteredRequests = requests.filter((request) => {
    if (!searchQuery) return true;
    const query = searchQuery.toLowerCase();
    return (
      request.name.toLowerCase().includes(query) ||
      request.url.toLowerCase().includes(query) ||
      request.description.toLowerCase().includes(query) ||
      request.category.toLowerCase().includes(query) ||
      request.tags.some((t) => t.toLowerCase().includes(query))
    );
  });

  // Toggle selection for batch delete
  const toggleSelection = useCallback((requestId: string) => {
    setSelectedIds((prev) => {
      const next = new Set(prev);
      if (next.has(requestId)) {
        next.delete(requestId);
      } else {
        next.add(requestId);
      }
      return next;
    });
  }, []);

  // Exit selection mode
  const exitSelectionMode = useCallback(() => {
    setIsSelectionMode(false);
    setSelectedIds(new Set());
  }, []);

  // Delete selected requests
  const deleteSelected = useCallback(async () => {
    if (selectedIds.size === 0) return;

    setIsDeleting(true);
    try {
      // Delete in parallel
      const deletePromises = Array.from(selectedIds).map((id) =>
        fetch(`${API_BASE}/saved-api-requests/${id}`, { method: "DELETE" }),
      );
      await Promise.all(deletePromises);

      // Refresh the list
      await fetchRequests();

      // If currently selected request was deleted, clear it
      if (selectedRequest && selectedIds.has(selectedRequest.id)) {
        setSelectedRequest(null);
        setIsCreating(false);
      }

      // Exit selection mode
      exitSelectionMode();
      setShowBatchDeleteDialog(false);
      onLog?.("success", `Deleted ${selectedIds.size} API request(s)`);
    } catch (error) {
      onLog?.("error", `Failed to delete API requests: ${error}`);
    } finally {
      setIsDeleting(false);
    }
  }, [selectedIds, fetchRequests, selectedRequest, exitSelectionMode, onLog]);

  // Get names of selected requests for the delete dialog
  const getSelectedNames = useCallback((): string[] => {
    return requests.filter((r) => selectedIds.has(r.id)).map((r) => r.name);
  }, [requests, selectedIds]);

  return (
    <div className="h-full flex">
      {/* Left Panel - Request List */}
      <div className="w-80 border-r border-neutral-700 flex flex-col bg-neutral-900">
        {/* Header */}
        <div className="p-4 border-b border-neutral-700">
          <div className="flex items-center justify-between mb-2">
            <h2 className="text-lg font-semibold flex items-center gap-2">
              <Globe className="w-5 h-5" style={{ color: accentColors.bgSolid }} />
              API Requests
            </h2>
          </div>

          {/* Action buttons */}
          <BuilderToolbar
            className="mb-3"
            actions={[
              toolbarActions.ai(() => setShowAiGenerator(true)),
              toolbarActions.new(startCreate, "New"),
              toolbarActions.delete(() => setIsSelectionMode(!isSelectionMode), isSelectionMode),
            ]}
          />

          {/* Search */}
          <div className="relative">
            <Search className="absolute left-3 top-1/2 -translate-y-1/2 w-4 h-4 text-neutral-400" />
            <input
              type="text"
              placeholder="Search API requests..."
              value={searchQuery}
              onChange={(e) => setSearchQuery(e.target.value)}
              className="w-full pl-9 pr-3 py-2 bg-neutral-800 border border-neutral-700 rounded-lg text-sm focus:outline-none focus:border-neutral-600"
            />
          </div>
        </div>

        {/* Selection Mode Header */}
        {isSelectionMode && (
          <div className="flex items-center justify-between px-4 py-2 bg-red-500/10 border-b border-red-500/30">
            <span className="text-sm text-red-400">{selectedIds.size} selected</span>
            <div className="flex items-center gap-2">
              {selectedIds.size > 0 && (
                <button
                  onClick={() => setShowBatchDeleteDialog(true)}
                  className="flex items-center gap-1 px-3 py-1 text-sm font-medium bg-red-600 hover:bg-red-500 text-white rounded-md transition-colors"
                >
                  <Trash2 className="w-3.5 h-3.5" />
                  Delete
                </button>
              )}
              <button
                onClick={exitSelectionMode}
                className="px-3 py-1 text-sm text-neutral-400 hover:text-neutral-200 transition-colors"
              >
                Cancel
              </button>
            </div>
          </div>
        )}

        {/* Request List */}
        <div className="flex-1 overflow-y-auto p-2">
          {loading ? (
            <div className="flex items-center justify-center py-8">
              <Loader2 className="w-6 h-6 animate-spin text-neutral-400" />
            </div>
          ) : filteredRequests.length === 0 ? (
            <div className="text-center py-8 text-neutral-400">
              <Globe className="w-8 h-8 mx-auto mb-2 opacity-50" />
              <p className="text-sm">No API requests found</p>
            </div>
          ) : (
            <div className="space-y-1">
              {filteredRequests.map((request) => (
                <button
                  key={request.id}
                  onClick={() => {
                    if (isSelectionMode) {
                      toggleSelection(request.id);
                    } else {
                      selectRequest(request);
                    }
                  }}
                  className={`w-full text-left p-3 rounded-lg transition-colors flex items-start gap-3 ${
                    isSelectionMode && selectedIds.has(request.id)
                      ? "bg-red-500/20 border border-red-500/50"
                      : selectedRequest?.id === request.id
                        ? "bg-neutral-700"
                        : "hover:bg-neutral-800"
                  } ${isSelectionMode ? "border" : ""} ${
                    isSelectionMode && !selectedIds.has(request.id) ? "border-transparent" : ""
                  }`}
                >
                  {/* Checkbox in selection mode */}
                  {isSelectionMode && (
                    <div
                      className={`flex-shrink-0 w-5 h-5 mt-0.5 rounded border-2 flex items-center justify-center transition-colors ${
                        selectedIds.has(request.id)
                          ? "bg-red-500 border-red-500"
                          : "border-neutral-500"
                      }`}
                    >
                      {selectedIds.has(request.id) && <Check className="w-3 h-3 text-white" />}
                    </div>
                  )}
                  <div className="flex-1 min-w-0">
                    <div className="flex items-center gap-2">
                      <span
                        className={`text-xs px-1.5 py-0.5 rounded font-mono ${
                          METHOD_COLORS[request.method]
                        }`}
                      >
                        {request.method}
                      </span>
                      <span className="font-medium text-sm truncate flex-1">{request.name}</span>
                    </div>
                    <div className="text-xs text-neutral-400 truncate mt-1 font-mono">
                      {request.url}
                    </div>
                  </div>
                </button>
              ))}
            </div>
          )}
        </div>
      </div>

      {/* Right Panel - Editor or AI Generator */}
      <div className="flex-1 flex flex-col bg-neutral-900/50">
        {showAiGenerator ? (
          <div className="flex-1 p-4">
            <AiApiRequestGenerator
              onRequestGenerated={handleAiRequestGenerated}
              onCancel={() => setShowAiGenerator(false)}
            />
          </div>
        ) : !selectedRequest && !isCreating ? (
          <div className="flex-1 flex items-center justify-center">
            <div className="text-center text-neutral-400">
              <Globe className="w-12 h-12 mx-auto mb-3 opacity-30" />
              <p className="text-lg mb-2">No API request selected</p>
              <p className="text-sm mb-4">Select a request to edit or create a new one</p>
              <button
                onClick={startCreate}
                className="inline-flex items-center gap-2 px-4 py-2 rounded-lg transition-colors"
                style={{ backgroundColor: accentColors.bgSolid, color: "#fff" }}
              >
                <Plus className="w-4 h-4" />
                Create New API Request
              </button>
            </div>
          </div>
        ) : (
          <>
            {/* Toolbar */}
            <div className="flex items-center justify-between p-4 border-b border-neutral-700">
              <h3 className="font-medium">
                {isCreating ? "New API Request" : `Editing: ${selectedRequest?.name}`}
              </h3>
              <div className="flex items-center gap-2">
                {selectedRequest && (
                  <>
                    <button
                      onClick={handleDuplicate}
                      className="p-2 rounded-lg hover:bg-neutral-800 transition-colors"
                      title="Duplicate"
                    >
                      <Copy className="w-4 h-4" />
                    </button>
                    <button
                      onClick={handleDelete}
                      className="p-2 rounded-lg hover:bg-neutral-800 text-red-400 transition-colors"
                      title="Delete"
                    >
                      <Trash2 className="w-4 h-4" />
                    </button>
                  </>
                )}
                <button
                  onClick={handleSave}
                  disabled={saving || !formName.trim() || !formUrl.trim()}
                  className="flex items-center gap-2 px-4 py-2 rounded-lg disabled:opacity-50 transition-colors"
                  style={{ backgroundColor: accentColors.bgSolid, color: "#fff" }}
                >
                  {saving ? (
                    <Loader2 className="w-4 h-4 animate-spin" />
                  ) : (
                    <Save className="w-4 h-4" />
                  )}
                  Save
                </button>
              </div>
            </div>

            {/* Form */}
            <div className="flex-1 overflow-y-auto p-4">
              <div className="max-w-3xl space-y-6">
                {/* Basic Info */}
                <div className="space-y-4">
                  <div>
                    <label className="block text-sm font-medium mb-1.5">Name *</label>
                    <input
                      type="text"
                      value={formName}
                      onChange={(e) => setFormName(e.target.value)}
                      placeholder="API request name"
                      className="w-full px-3 py-2 bg-neutral-800 border border-neutral-700 rounded-lg focus:outline-none focus:border-neutral-600"
                    />
                  </div>

                  <div>
                    <label className="block text-sm font-medium mb-1.5">Description</label>
                    <input
                      type="text"
                      value={formDescription}
                      onChange={(e) => setFormDescription(e.target.value)}
                      placeholder="Brief description"
                      className="w-full px-3 py-2 bg-neutral-800 border border-neutral-700 rounded-lg focus:outline-none focus:border-neutral-600"
                    />
                  </div>

                  <div className="grid grid-cols-2 gap-4">
                    <div>
                      <label className="block text-sm font-medium mb-1.5">
                        <FolderOpen className="w-4 h-4 inline mr-1" />
                        Category
                      </label>
                      <input
                        type="text"
                        value={formCategory}
                        onChange={(e) => setFormCategory(e.target.value)}
                        placeholder="e.g., auth, users"
                        className="w-full px-3 py-2 bg-neutral-800 border border-neutral-700 rounded-lg focus:outline-none focus:border-neutral-600"
                      />
                    </div>
                    <div>
                      <label className="block text-sm font-medium mb-1.5">
                        <Tag className="w-4 h-4 inline mr-1" />
                        Tags
                      </label>
                      <input
                        type="text"
                        value={formTags}
                        onChange={(e) => setFormTags(e.target.value)}
                        placeholder="comma, separated, tags"
                        className="w-full px-3 py-2 bg-neutral-800 border border-neutral-700 rounded-lg focus:outline-none focus:border-neutral-600"
                      />
                    </div>
                  </div>
                </div>

                {/* Request */}
                <div className="border border-neutral-700 rounded-lg p-4 space-y-4">
                  <h4 className="font-medium flex items-center gap-2">
                    <Globe className="w-4 h-4" style={{ color: accentColors.bgSolid }} />
                    Request
                  </h4>

                  {/* Method and URL */}
                  <div className="flex gap-2">
                    <select
                      value={formMethod}
                      onChange={(e) => setFormMethod(e.target.value as HttpMethod)}
                      className="px-3 py-2 bg-neutral-800 border border-neutral-700 rounded-lg focus:outline-none focus:border-neutral-600"
                    >
                      {HTTP_METHODS.map((method) => (
                        <option key={method} value={method}>
                          {method}
                        </option>
                      ))}
                    </select>
                    <input
                      type="text"
                      value={formUrl}
                      onChange={(e) => setFormUrl(e.target.value)}
                      placeholder="https://api.example.com/endpoint"
                      className="flex-1 px-3 py-2 bg-neutral-800 border border-neutral-700 rounded-lg focus:outline-none focus:border-neutral-600 font-mono text-sm"
                    />
                  </div>

                  {/* Headers */}
                  <div>
                    <div className="flex items-center justify-between mb-2">
                      <label className="text-sm font-medium">Headers</label>
                      <button
                        onClick={addHeader}
                        className="text-xs px-2 py-1 rounded bg-neutral-800 hover:bg-neutral-700 transition-colors"
                      >
                        <Plus className="w-3 h-3 inline mr-1" />
                        Add Header
                      </button>
                    </div>
                    {formHeaders.length > 0 ? (
                      <div className="space-y-2">
                        {formHeaders.map((header, index) => (
                          <div key={index} className="flex gap-2">
                            <input
                              type="text"
                              value={header.key}
                              onChange={(e) => updateHeader(index, "key", e.target.value)}
                              placeholder="Header name"
                              className="w-1/3 px-3 py-2 bg-neutral-800 border border-neutral-700 rounded-lg focus:outline-none focus:border-neutral-600 text-sm"
                            />
                            <input
                              type="text"
                              value={header.value}
                              onChange={(e) => updateHeader(index, "value", e.target.value)}
                              placeholder="Value"
                              className="flex-1 px-3 py-2 bg-neutral-800 border border-neutral-700 rounded-lg focus:outline-none focus:border-neutral-600 text-sm"
                            />
                            <button
                              onClick={() => removeHeader(index)}
                              className="p-2 text-red-400 hover:bg-neutral-800 rounded-lg transition-colors"
                            >
                              <X className="w-4 h-4" />
                            </button>
                          </div>
                        ))}
                      </div>
                    ) : (
                      <p className="text-xs text-neutral-500">No headers added</p>
                    )}
                  </div>

                  {/* Body */}
                  {formMethod !== "GET" && (
                    <div>
                      <div className="flex items-center justify-between mb-2">
                        <label className="text-sm font-medium">
                          <Code className="w-4 h-4 inline mr-1" />
                          Body
                        </label>
                        <select
                          value={formContentType}
                          onChange={(e) => setFormContentType(e.target.value as ApiContentType)}
                          className="text-xs px-2 py-1 bg-neutral-800 border border-neutral-700 rounded focus:outline-none focus:border-neutral-600"
                        >
                          {CONTENT_TYPES.map((ct) => (
                            <option key={ct.value} value={ct.value}>
                              {ct.label}
                            </option>
                          ))}
                        </select>
                      </div>
                      <textarea
                        value={formBody}
                        onChange={(e) => setFormBody(e.target.value)}
                        placeholder={
                          formContentType === "application/json"
                            ? '{\n  "key": "value"\n}'
                            : "Request body..."
                        }
                        rows={8}
                        className="w-full px-3 py-2 bg-neutral-800 border border-neutral-700 rounded-lg focus:outline-none focus:border-neutral-600 font-mono text-sm resize-y"
                      />
                    </div>
                  )}
                </div>

                {/* Settings */}
                <div className="border border-neutral-700 rounded-lg p-4 space-y-4">
                  <h4 className="font-medium flex items-center gap-2">
                    <Settings className="w-4 h-4" style={{ color: accentColors.bgSolid }} />
                    Settings
                  </h4>

                  <div className="grid grid-cols-2 gap-4">
                    <div>
                      <label className="block text-sm mb-1.5">
                        <Clock className="w-4 h-4 inline mr-1" />
                        Timeout (ms)
                      </label>
                      <input
                        type="number"
                        value={formTimeout}
                        onChange={(e) => setFormTimeout(parseInt(e.target.value) || 30000)}
                        min={1000}
                        step={1000}
                        className="w-full px-3 py-2 bg-neutral-800 border border-neutral-700 rounded-lg focus:outline-none focus:border-neutral-600 text-sm"
                      />
                    </div>
                    <div className="flex items-center">
                      <label className="flex items-center gap-3 cursor-pointer">
                        <input
                          type="checkbox"
                          checked={formFollowRedirects}
                          onChange={(e) => setFormFollowRedirects(e.target.checked)}
                          className="w-4 h-4 rounded"
                        />
                        <span className="text-sm">Follow redirects</span>
                      </label>
                    </div>
                  </div>
                </div>
              </div>
            </div>
          </>
        )}
      </div>

      {/* Batch Delete Dialog */}
      <BatchDeleteDialog
        open={showBatchDeleteDialog}
        title="Delete API Requests"
        itemType="API request"
        itemNames={getSelectedNames()}
        isDeleting={isDeleting}
        onClose={() => setShowBatchDeleteDialog(false)}
        onConfirm={deleteSelected}
      />
    </div>
  );
}
