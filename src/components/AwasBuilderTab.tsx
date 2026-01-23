/**
 * AwasBuilderTab.tsx
 *
 * Builder for creating and testing AWAS (AI Web Action Standard) configurations.
 * Features:
 * - Discover AWAS manifests from URLs
 * - Browse and select actions from discovered manifests
 * - Configure action parameters
 * - Test action execution with live preview
 * - Save configurations for use in workflows
 */

import { useState, useEffect, useCallback } from "react";
import {
  Wifi,
  Plus,
  Save,
  Trash2,
  Loader2,
  Search,
  Tag,
  FolderOpen,
  Copy,
  X,
  Play,
  RefreshCw,
  Check,
  AlertCircle,
  ChevronDown,
  ChevronRight,
  ExternalLink,
  Zap,
  FileJson,
  List,
} from "lucide-react";
import { getAccentColors } from "@/design-system";
import { BatchDeleteDialog } from "./ui/BatchDeleteDialog";

const API_BASE = "http://localhost:9876";

interface AwasAction {
  id: string;
  name: string;
  method: string;
  endpoint: string;
  intent: string;
  side_effect: boolean;
  parameters: AwasParameter[];
  input_schema?: Record<string, unknown>;
  output_schema?: Record<string, unknown>;
}

interface AwasParameter {
  name: string;
  location: string;
  type: string;
  required: boolean;
  description?: string;
  default?: unknown;
}

interface AwasManifest {
  schema_version: string;
  app_name: string;
  base_url: string;
  conformance_level: string;
  actions: AwasAction[];
  auth?: {
    type: string;
    header_name?: string;
  };
}

interface SavedAwasConfig {
  id: string;
  name: string;
  description: string;
  category: string;
  tags: string[];
  url: string;
  action_id: string;
  action_name: string;
  parameters: Record<string, unknown>;
  created_at: string;
  modified_at: string;
}

interface AwasBuilderTabProps {
  onLog?: (level: string, message: string) => void;
  editConfigId?: string | null;
  onNavigateToLibrary?: () => void;
}

const METHOD_COLORS: Record<string, string> = {
  GET: "bg-green-900/50 text-green-300",
  POST: "bg-blue-900/50 text-blue-300",
  PUT: "bg-amber-900/50 text-amber-300",
  PATCH: "bg-orange-900/50 text-orange-300",
  DELETE: "bg-red-900/50 text-red-300",
};

export function AwasBuilderTab({ onLog, editConfigId, onNavigateToLibrary }: AwasBuilderTabProps) {
  // State
  const [savedConfigs, setSavedConfigs] = useState<SavedAwasConfig[]>([]);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [searchQuery, setSearchQuery] = useState("");
  const [selectedConfig, setSelectedConfig] = useState<SavedAwasConfig | null>(null);
  const [isCreating, setIsCreating] = useState(false);

  // Bulk deletion state
  const [isSelectionMode, setIsSelectionMode] = useState(false);
  const [selectedIds, setSelectedIds] = useState<Set<string>>(new Set());
  const [showBatchDeleteDialog, setShowBatchDeleteDialog] = useState(false);
  const [isDeleting, setIsDeleting] = useState(false);

  // Discovery state
  const [discoveryUrl, setDiscoveryUrl] = useState("");
  const [discovering, setDiscovering] = useState(false);
  const [manifest, setManifest] = useState<AwasManifest | null>(null);
  const [discoveryError, setDiscoveryError] = useState<string | null>(null);

  // Form state
  const [formName, setFormName] = useState("");
  const [formDescription, setFormDescription] = useState("");
  const [formCategory, setFormCategory] = useState("");
  const [formTags, setFormTags] = useState("");
  const [selectedAction, setSelectedAction] = useState<AwasAction | null>(null);
  const [parameterValues, setParameterValues] = useState<Record<string, unknown>>({});

  // Test execution state
  const [testing, setTesting] = useState(false);
  const [testResult, setTestResult] = useState<{
    success: boolean;
    status_code?: number;
    response_body?: unknown;
    error?: string;
  } | null>(null);

  // Expanded actions state
  const [expandedActions, setExpandedActions] = useState<Set<string>>(new Set());

  const accentColors = getAccentColors("teal");

  // Toggle selection for bulk delete
  const toggleSelection = useCallback((id: string) => {
    setSelectedIds((prev) => {
      const next = new Set(prev);
      if (next.has(id)) {
        next.delete(id);
      } else {
        next.add(id);
      }
      return next;
    });
  }, []);

  // Exit selection mode
  const exitSelectionMode = useCallback(() => {
    setIsSelectionMode(false);
    setSelectedIds(new Set());
  }, []);

  // Delete selected configs
  const deleteSelectedConfigs = useCallback(async () => {
    setIsDeleting(true);
    try {
      const updated = savedConfigs.filter((c) => !selectedIds.has(c.id));
      localStorage.setItem("qontinui-awas-configs", JSON.stringify(updated));
      setSavedConfigs(updated);
      exitSelectionMode();
      setShowBatchDeleteDialog(false);
      onLog?.("success", `Deleted ${selectedIds.size} AWAS config(s)`);
    } catch (error) {
      onLog?.("error", `Failed to delete AWAS configs: ${error}`);
    } finally {
      setIsDeleting(false);
    }
  }, [savedConfigs, selectedIds, exitSelectionMode, onLog]);

  // Get names of selected configs for dialog
  const getSelectedNames = useCallback(() => {
    return savedConfigs
      .filter((c) => selectedIds.has(c.id))
      .map((c) => c.name);
  }, [savedConfigs, selectedIds]);

  // Fetch saved configs from storage (simulated - would be from database)
  const fetchSavedConfigs = useCallback(async () => {
    try {
      setLoading(true);
      // For now, we'll use local storage. In production, this would be a database call.
      const stored = localStorage.getItem("qontinui-awas-configs");
      if (stored) {
        setSavedConfigs(JSON.parse(stored));
      }
    } catch (error) {
      console.error("Failed to fetch AWAS configs:", error);
      onLog?.("error", `Failed to fetch AWAS configs: ${error}`);
    } finally {
      setLoading(false);
    }
  }, [onLog]);

  useEffect(() => {
    fetchSavedConfigs();
  }, [fetchSavedConfigs]);

  // Load config for editing
  useEffect(() => {
    if (editConfigId && savedConfigs.length > 0) {
      const config = savedConfigs.find((c) => c.id === editConfigId);
      if (config) {
        selectConfig(config);
      }
    }
  }, [editConfigId, savedConfigs]);

  // Discover AWAS manifest from URL
  const handleDiscover = async () => {
    if (!discoveryUrl.trim()) {
      onLog?.("warning", "Please enter a URL to discover");
      return;
    }

    setDiscovering(true);
    setDiscoveryError(null);
    setManifest(null);
    setSelectedAction(null);

    try {
      const response = await fetch(`${API_BASE}/awas/discover`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ url: discoveryUrl.trim() }),
      });

      const result = await response.json();

      if (result.success && result.manifest) {
        setManifest(result.manifest);
        onLog?.("success", `Discovered AWAS manifest for ${result.manifest.app_name}`);
      } else {
        setDiscoveryError(result.error || "Failed to discover AWAS manifest");
        onLog?.("error", `Discovery failed: ${result.error || "Unknown error"}`);
      }
    } catch (error) {
      setDiscoveryError(`Network error: ${error}`);
      onLog?.("error", `Discovery failed: ${error}`);
    } finally {
      setDiscovering(false);
    }
  };

  // Select an action from the manifest
  const handleSelectAction = (action: AwasAction) => {
    setSelectedAction(action);
    // Initialize parameter values with defaults
    const defaults: Record<string, unknown> = {};
    action.parameters.forEach((param) => {
      if (param.default !== undefined) {
        defaults[param.name] = param.default;
      } else if (param.type === "boolean") {
        defaults[param.name] = false;
      } else if (param.type === "number" || param.type === "integer") {
        defaults[param.name] = 0;
      } else {
        defaults[param.name] = "";
      }
    });
    setParameterValues(defaults);
    setTestResult(null);

    // Auto-fill form name from action
    if (!formName) {
      setFormName(action.name);
    }
    if (!formDescription) {
      setFormDescription(action.intent);
    }
  };

  // Test execute the selected action
  const handleTestExecute = async () => {
    if (!selectedAction || !discoveryUrl) {
      onLog?.("warning", "Please select an action first");
      return;
    }

    setTesting(true);
    setTestResult(null);

    try {
      const response = await fetch(`${API_BASE}/awas/execute`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          url: discoveryUrl.trim(),
          action_id: selectedAction.id,
          params: parameterValues,
        }),
      });

      const result = await response.json();

      if (result.success) {
        setTestResult({
          success: true,
          status_code: result.status_code,
          response_body: result.response_body,
        });
        onLog?.("success", `Action "${selectedAction.name}" executed successfully`);
      } else {
        setTestResult({
          success: false,
          error: result.error || "Execution failed",
        });
        onLog?.("error", `Execution failed: ${result.error || "Unknown error"}`);
      }
    } catch (error) {
      setTestResult({
        success: false,
        error: `Network error: ${error}`,
      });
      onLog?.("error", `Execution failed: ${error}`);
    } finally {
      setTesting(false);
    }
  };

  // Select a saved config for editing
  const selectConfig = (config: SavedAwasConfig) => {
    setSelectedConfig(config);
    setIsCreating(false);
    setFormName(config.name);
    setFormDescription(config.description);
    setFormCategory(config.category);
    setFormTags(config.tags.join(", "));
    setDiscoveryUrl(config.url);
    setParameterValues(config.parameters);
    // Trigger discovery to load the manifest
    setManifest(null);
    setSelectedAction(null);
  };

  // Start creating a new config
  const startCreate = () => {
    setSelectedConfig(null);
    setIsCreating(true);
    setFormName("");
    setFormDescription("");
    setFormCategory("");
    setFormTags("");
    setDiscoveryUrl("");
    setManifest(null);
    setSelectedAction(null);
    setParameterValues({});
    setTestResult(null);
    setDiscoveryError(null);
  };

  // Save config
  const handleSave = async () => {
    if (!formName.trim()) {
      onLog?.("warning", "Name is required");
      return;
    }
    if (!discoveryUrl.trim()) {
      onLog?.("warning", "URL is required");
      return;
    }
    if (!selectedAction) {
      onLog?.("warning", "Please select an action");
      return;
    }

    setSaving(true);
    try {
      const now = new Date().toISOString();
      const config: SavedAwasConfig = {
        id: selectedConfig?.id || crypto.randomUUID(),
        name: formName.trim(),
        description: formDescription.trim(),
        category: formCategory.trim() || "general",
        tags: formTags
          .split(",")
          .map((t) => t.trim())
          .filter(Boolean),
        url: discoveryUrl.trim(),
        action_id: selectedAction.id,
        action_name: selectedAction.name,
        parameters: parameterValues,
        created_at: selectedConfig?.created_at || now,
        modified_at: now,
      };

      // Save to localStorage (in production, this would be a database call)
      const updated = selectedConfig
        ? savedConfigs.map((c) => (c.id === config.id ? config : c))
        : [...savedConfigs, config];

      localStorage.setItem("qontinui-awas-configs", JSON.stringify(updated));
      setSavedConfigs(updated);
      setSelectedConfig(config);
      setIsCreating(false);

      onLog?.("success", `AWAS config "${formName}" saved successfully`);
    } catch (error) {
      onLog?.("error", `Failed to save AWAS config: ${error}`);
    } finally {
      setSaving(false);
    }
  };

  // Delete config
  const handleDelete = async () => {
    if (!selectedConfig) return;

    if (!confirm(`Delete AWAS config "${selectedConfig.name}"?`)) return;

    try {
      const updated = savedConfigs.filter((c) => c.id !== selectedConfig.id);
      localStorage.setItem("qontinui-awas-configs", JSON.stringify(updated));
      setSavedConfigs(updated);
      setSelectedConfig(null);
      setIsCreating(false);
      onLog?.("success", `AWAS config "${selectedConfig.name}" deleted`);
    } catch (error) {
      onLog?.("error", `Failed to delete AWAS config: ${error}`);
    }
  };

  // Duplicate config
  const handleDuplicate = async () => {
    if (!selectedConfig) return;

    const duplicate: SavedAwasConfig = {
      ...selectedConfig,
      id: crypto.randomUUID(),
      name: `${selectedConfig.name} (Copy)`,
      created_at: new Date().toISOString(),
      modified_at: new Date().toISOString(),
    };

    const updated = [...savedConfigs, duplicate];
    localStorage.setItem("qontinui-awas-configs", JSON.stringify(updated));
    setSavedConfigs(updated);
    selectConfig(duplicate);
    onLog?.("success", `AWAS config duplicated as "${duplicate.name}"`);
  };

  // Filter configs
  const filteredConfigs = savedConfigs.filter((config) => {
    if (!searchQuery) return true;
    const query = searchQuery.toLowerCase();
    return (
      config.name.toLowerCase().includes(query) ||
      config.url.toLowerCase().includes(query) ||
      config.description.toLowerCase().includes(query) ||
      config.action_name.toLowerCase().includes(query) ||
      config.tags.some((t) => t.toLowerCase().includes(query))
    );
  });

  // Toggle action expansion
  const toggleActionExpanded = (actionId: string) => {
    setExpandedActions((prev) => {
      const next = new Set(prev);
      if (next.has(actionId)) {
        next.delete(actionId);
      } else {
        next.add(actionId);
      }
      return next;
    });
  };

  return (
    <div className="h-full flex">
      {/* Left Panel - Saved Configs List */}
      <div className="w-80 border-r border-neutral-700 flex flex-col bg-neutral-900">
        {/* Header */}
        <div className="p-4 border-b border-neutral-700">
          <div className="flex items-center justify-between mb-3">
            <h2 className="text-lg font-semibold flex items-center gap-2">
              <Wifi className="w-5 h-5" style={{ color: accentColors.bgSolid }} />
              AWAS Configs
            </h2>
            <div className="flex items-center gap-1">
              <button
                onClick={() => setIsSelectionMode(!isSelectionMode)}
                className={`p-2 rounded-lg transition-colors ${
                  isSelectionMode
                    ? "bg-red-500/20 text-red-400"
                    : "hover:bg-neutral-800 text-neutral-400"
                }`}
                title={isSelectionMode ? "Exit selection mode" : "Select configs to delete"}
              >
                <Trash2 className="w-4 h-4" />
              </button>
              <button
                onClick={startCreate}
                className="p-2 rounded-lg hover:bg-neutral-800 transition-colors"
                title="New AWAS Config"
              >
                <Plus className="w-4 h-4" />
              </button>
            </div>
          </div>

          {/* Search */}
          <div className="relative">
            <Search className="absolute left-3 top-1/2 -translate-y-1/2 w-4 h-4 text-neutral-400" />
            <input
              type="text"
              placeholder="Search AWAS configs..."
              value={searchQuery}
              onChange={(e) => setSearchQuery(e.target.value)}
              className="w-full pl-9 pr-3 py-2 bg-neutral-800 border border-neutral-700 rounded-lg text-sm focus:outline-none focus:border-neutral-600"
            />
          </div>
        </div>

        {/* Selection Mode Header */}
        {isSelectionMode && (
          <div className="flex items-center justify-between px-4 py-2 bg-red-500/10 border-b border-red-500/30">
            <span className="text-sm text-red-400">
              {selectedIds.size} selected
            </span>
            <div className="flex items-center gap-2">
              <button
                onClick={() => setSelectedIds(new Set(filteredConfigs.map(c => c.id)))}
                className="px-2 py-1 text-xs bg-neutral-700 hover:bg-neutral-600 text-neutral-200 rounded"
              >
                Select All
              </button>
              <button
                onClick={() => setSelectedIds(new Set())}
                disabled={selectedIds.size === 0}
                className="px-2 py-1 text-xs bg-neutral-700 hover:bg-neutral-600 text-neutral-200 rounded disabled:opacity-50"
              >
                Clear
              </button>
              <button
                onClick={() => setShowBatchDeleteDialog(true)}
                disabled={selectedIds.size === 0}
                className="px-2 py-1 text-xs bg-red-600 hover:bg-red-500 text-white rounded disabled:opacity-50 disabled:cursor-not-allowed"
              >
                Delete Selected
              </button>
              <button
                onClick={exitSelectionMode}
                className="p-1 text-neutral-400 hover:text-neutral-200"
                title="Cancel selection"
              >
                <X className="w-4 h-4" />
              </button>
            </div>
          </div>
        )}

        {/* Config List */}
        <div className="flex-1 overflow-y-auto p-2">
          {loading ? (
            <div className="flex items-center justify-center py-8">
              <Loader2 className="w-6 h-6 animate-spin text-neutral-400" />
            </div>
          ) : filteredConfigs.length === 0 ? (
            <div className="text-center py-8 text-neutral-400">
              <Wifi className="w-8 h-8 mx-auto mb-2 opacity-50" />
              <p className="text-sm">No AWAS configs found</p>
            </div>
          ) : (
            <div className="space-y-1">
              {filteredConfigs.map((config) => (
                <button
                  key={config.id}
                  onClick={() => isSelectionMode ? toggleSelection(config.id) : selectConfig(config)}
                  className={`w-full text-left p-3 rounded-lg transition-colors ${
                    selectedConfig?.id === config.id ? "bg-neutral-700" : "hover:bg-neutral-800"
                  } ${isSelectionMode && selectedIds.has(config.id) ? "bg-red-500/10" : ""}`}
                >
                  <div className="flex items-center gap-2">
                    {isSelectionMode && (
                      <div
                        className={`w-4 h-4 rounded border flex-shrink-0 flex items-center justify-center ${
                          selectedIds.has(config.id)
                            ? "bg-red-500 border-red-500"
                            : "border-neutral-500 hover:border-red-400"
                        }`}
                      >
                        {selectedIds.has(config.id) && <Check className="w-3 h-3 text-white" />}
                      </div>
                    )}
                    <Wifi className="w-4 h-4 text-teal-400 flex-shrink-0" />
                    <span className="font-medium text-sm truncate flex-1">{config.name}</span>
                  </div>
                  <div className="text-xs text-neutral-400 truncate mt-1">{config.action_name}</div>
                  <div className="text-xs text-neutral-500 truncate mt-0.5 font-mono">
                    {config.url}
                  </div>
                </button>
              ))}
            </div>
          )}
        </div>
      </div>

      {/* Right Panel - Editor */}
      <div className="flex-1 flex flex-col bg-neutral-900/50">
        {!selectedConfig && !isCreating ? (
          <div className="flex-1 flex items-center justify-center">
            <div className="text-center text-neutral-400">
              <Wifi className="w-12 h-12 mx-auto mb-3 opacity-30" />
              <p className="text-lg mb-2">No AWAS config selected</p>
              <p className="text-sm mb-4">Select a config to edit or create a new one</p>
              <button
                onClick={startCreate}
                className="inline-flex items-center gap-2 px-4 py-2 rounded-lg transition-colors"
                style={{ backgroundColor: accentColors.bgSolid, color: "#fff" }}
              >
                <Plus className="w-4 h-4" />
                Create New AWAS Config
              </button>
            </div>
          </div>
        ) : (
          <>
            {/* Toolbar */}
            <div className="flex items-center justify-between p-4 border-b border-neutral-700">
              <h3 className="font-medium">
                {isCreating ? "New AWAS Config" : `Editing: ${selectedConfig?.name}`}
              </h3>
              <div className="flex items-center gap-2">
                {selectedConfig && (
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
                  disabled={saving || !formName.trim() || !selectedAction}
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
              <div className="max-w-4xl space-y-6">
                {/* Basic Info */}
                <div className="space-y-4">
                  <div>
                    <label className="block text-sm font-medium mb-1.5">Name *</label>
                    <input
                      type="text"
                      value={formName}
                      onChange={(e) => setFormName(e.target.value)}
                      placeholder="AWAS config name"
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
                        placeholder="e.g., automation, testing"
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

                {/* Discovery Section */}
                <div className="border border-neutral-700 rounded-lg p-4 space-y-4">
                  <h4 className="font-medium flex items-center gap-2">
                    <Search className="w-4 h-4" style={{ color: accentColors.bgSolid }} />
                    Discover AWAS Manifest
                  </h4>

                  <div className="flex gap-2">
                    <input
                      type="text"
                      value={discoveryUrl}
                      onChange={(e) => setDiscoveryUrl(e.target.value)}
                      placeholder="https://example.com"
                      className="flex-1 px-3 py-2 bg-neutral-800 border border-neutral-700 rounded-lg focus:outline-none focus:border-neutral-600 font-mono text-sm"
                    />
                    <button
                      onClick={handleDiscover}
                      disabled={discovering || !discoveryUrl.trim()}
                      className="flex items-center gap-2 px-4 py-2 rounded-lg disabled:opacity-50 transition-colors bg-neutral-700 hover:bg-neutral-600"
                    >
                      {discovering ? (
                        <Loader2 className="w-4 h-4 animate-spin" />
                      ) : (
                        <RefreshCw className="w-4 h-4" />
                      )}
                      Discover
                    </button>
                  </div>

                  {discoveryError && (
                    <div className="flex items-center gap-2 text-red-400 text-sm">
                      <AlertCircle className="w-4 h-4" />
                      {discoveryError}
                    </div>
                  )}

                  {manifest && (
                    <div className="space-y-3">
                      <div className="flex items-center justify-between">
                        <div className="flex items-center gap-2">
                          <FileJson className="w-4 h-4 text-teal-400" />
                          <span className="font-medium">{manifest.app_name}</span>
                          <span className="text-xs px-2 py-0.5 rounded bg-neutral-700 text-neutral-300">
                            {manifest.conformance_level}
                          </span>
                        </div>
                        <span className="text-xs text-neutral-400">v{manifest.schema_version}</span>
                      </div>

                      <div className="text-xs text-neutral-400">Base URL: {manifest.base_url}</div>

                      {manifest.auth && (
                        <div className="text-xs text-neutral-400">
                          Auth: {manifest.auth.type}
                          {manifest.auth.header_name && ` (${manifest.auth.header_name})`}
                        </div>
                      )}
                    </div>
                  )}
                </div>

                {/* Actions Section */}
                {manifest && (
                  <div className="border border-neutral-700 rounded-lg p-4 space-y-4">
                    <h4 className="font-medium flex items-center gap-2">
                      <List className="w-4 h-4" style={{ color: accentColors.bgSolid }} />
                      Available Actions ({manifest.actions.length})
                    </h4>

                    <div className="space-y-2 max-h-64 overflow-y-auto">
                      {manifest.actions.map((action) => (
                        <div
                          key={action.id}
                          className={`border rounded-lg transition-colors ${
                            selectedAction?.id === action.id
                              ? "border-teal-500 bg-teal-900/20"
                              : "border-neutral-700 hover:border-neutral-600"
                          }`}
                        >
                          <button
                            onClick={() => {
                              handleSelectAction(action);
                              toggleActionExpanded(action.id);
                            }}
                            className="w-full p-3 text-left"
                          >
                            <div className="flex items-center gap-2">
                              {expandedActions.has(action.id) ? (
                                <ChevronDown className="w-4 h-4 text-neutral-400" />
                              ) : (
                                <ChevronRight className="w-4 h-4 text-neutral-400" />
                              )}
                              <span
                                className={`text-xs px-1.5 py-0.5 rounded font-mono ${
                                  METHOD_COLORS[action.method] || "bg-neutral-700 text-neutral-300"
                                }`}
                              >
                                {action.method}
                              </span>
                              <span className="font-medium text-sm flex-1">{action.name}</span>
                              {action.side_effect && (
                                <span className="text-xs px-1.5 py-0.5 rounded bg-amber-900/50 text-amber-300">
                                  Side Effect
                                </span>
                              )}
                              {selectedAction?.id === action.id && (
                                <Check className="w-4 h-4 text-teal-400" />
                              )}
                            </div>
                            <div className="text-xs text-neutral-400 mt-1 ml-6">
                              {action.intent}
                            </div>
                          </button>

                          {expandedActions.has(action.id) && (
                            <div className="px-3 pb-3 pt-0 border-t border-neutral-700 mt-2 ml-6">
                              <div className="text-xs text-neutral-500 font-mono">
                                {action.endpoint}
                              </div>
                              {action.parameters.length > 0 && (
                                <div className="mt-2 space-y-1">
                                  <div className="text-xs text-neutral-400 font-medium">
                                    Parameters:
                                  </div>
                                  {action.parameters.map((param) => (
                                    <div
                                      key={param.name}
                                      className="text-xs text-neutral-500 flex items-center gap-2"
                                    >
                                      <span className="font-mono">{param.name}</span>
                                      <span className="text-neutral-600">({param.location})</span>
                                      <span className="text-neutral-600">{param.type}</span>
                                      {param.required && <span className="text-red-400">*</span>}
                                    </div>
                                  ))}
                                </div>
                              )}
                            </div>
                          )}
                        </div>
                      ))}
                    </div>
                  </div>
                )}

                {/* Parameter Configuration */}
                {selectedAction && selectedAction.parameters.length > 0 && (
                  <div className="border border-neutral-700 rounded-lg p-4 space-y-4">
                    <h4 className="font-medium flex items-center gap-2">
                      <Zap className="w-4 h-4" style={{ color: accentColors.bgSolid }} />
                      Configure Parameters
                    </h4>

                    <div className="space-y-3">
                      {selectedAction.parameters.map((param) => (
                        <div key={param.name}>
                          <label className="block text-sm font-medium mb-1.5">
                            {param.name}
                            {param.required && <span className="text-red-400 ml-1">*</span>}
                            <span className="text-xs text-neutral-500 ml-2">
                              ({param.location}, {param.type})
                            </span>
                          </label>
                          {param.description && (
                            <p className="text-xs text-neutral-400 mb-1.5">{param.description}</p>
                          )}
                          {param.type === "boolean" ? (
                            <label className="flex items-center gap-2 cursor-pointer">
                              <input
                                type="checkbox"
                                checked={(parameterValues[param.name] as boolean) || false}
                                onChange={(e) =>
                                  setParameterValues({
                                    ...parameterValues,
                                    [param.name]: e.target.checked,
                                  })
                                }
                                className="w-4 h-4 rounded"
                              />
                              <span className="text-sm">Enabled</span>
                            </label>
                          ) : param.type === "number" || param.type === "integer" ? (
                            <input
                              type="number"
                              value={(parameterValues[param.name] as number) || 0}
                              onChange={(e) =>
                                setParameterValues({
                                  ...parameterValues,
                                  [param.name]:
                                    param.type === "integer"
                                      ? parseInt(e.target.value)
                                      : parseFloat(e.target.value),
                                })
                              }
                              className="w-full px-3 py-2 bg-neutral-800 border border-neutral-700 rounded-lg focus:outline-none focus:border-neutral-600 text-sm"
                            />
                          ) : (
                            <input
                              type="text"
                              value={(parameterValues[param.name] as string) || ""}
                              onChange={(e) =>
                                setParameterValues({
                                  ...parameterValues,
                                  [param.name]: e.target.value,
                                })
                              }
                              placeholder={param.default?.toString() || ""}
                              className="w-full px-3 py-2 bg-neutral-800 border border-neutral-700 rounded-lg focus:outline-none focus:border-neutral-600 text-sm"
                            />
                          )}
                        </div>
                      ))}
                    </div>
                  </div>
                )}

                {/* Test Execution */}
                {selectedAction && (
                  <div className="border border-neutral-700 rounded-lg p-4 space-y-4">
                    <div className="flex items-center justify-between">
                      <h4 className="font-medium flex items-center gap-2">
                        <Play className="w-4 h-4" style={{ color: accentColors.bgSolid }} />
                        Test Execution
                      </h4>
                      <button
                        onClick={handleTestExecute}
                        disabled={testing}
                        className="flex items-center gap-2 px-3 py-1.5 rounded-lg disabled:opacity-50 transition-colors bg-neutral-700 hover:bg-neutral-600 text-sm"
                      >
                        {testing ? (
                          <Loader2 className="w-4 h-4 animate-spin" />
                        ) : (
                          <Play className="w-4 h-4" />
                        )}
                        Execute
                      </button>
                    </div>

                    {testResult && (
                      <div
                        className={`p-3 rounded-lg ${
                          testResult.success
                            ? "bg-green-900/30 border border-green-700"
                            : "bg-red-900/30 border border-red-700"
                        }`}
                      >
                        <div className="flex items-center gap-2 mb-2">
                          {testResult.success ? (
                            <Check className="w-4 h-4 text-green-400" />
                          ) : (
                            <AlertCircle className="w-4 h-4 text-red-400" />
                          )}
                          <span
                            className={`font-medium ${
                              testResult.success ? "text-green-400" : "text-red-400"
                            }`}
                          >
                            {testResult.success ? "Success" : "Failed"}
                          </span>
                          {testResult.status_code && (
                            <span className="text-xs text-neutral-400">
                              Status: {testResult.status_code}
                            </span>
                          )}
                        </div>
                        {testResult.error && (
                          <p className="text-sm text-red-300">{testResult.error}</p>
                        )}
                        {testResult.response_body && (
                          <pre className="mt-2 p-2 bg-neutral-900 rounded text-xs overflow-auto max-h-48 text-neutral-300">
                            {JSON.stringify(testResult.response_body, null, 2)}
                          </pre>
                        )}
                      </div>
                    )}
                  </div>
                )}
              </div>
            </div>
          </>
        )}
      </div>

      {/* Batch Delete Dialog */}
      <BatchDeleteDialog
        open={showBatchDeleteDialog}
        title="Delete AWAS Configs"
        itemType="config"
        itemNames={getSelectedNames()}
        isDeleting={isDeleting}
        onClose={() => setShowBatchDeleteDialog(false)}
        onConfirm={deleteSelectedConfigs}
      />
    </div>
  );
}
