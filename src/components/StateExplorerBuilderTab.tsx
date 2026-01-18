/**
 * StateExplorerBuilderTab.tsx
 *
 * Builder for creating and editing saved exploration configurations.
 * These configurations define how the state explorer should test the application.
 */

import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import {
  Compass,
  Plus,
  Save,
  Trash2,
  Loader2,
  Search,
  Tag,
  Copy,
  Settings,
  Clock,
  Target,
  Camera,
  AlertTriangle,
  FolderOpen,
  X,
  Check,
} from "lucide-react";
import type {
  SavedExploration,
  CreateSavedExplorationRequest,
  UpdateSavedExplorationRequest,
  ExplorationConfig,
} from "../types";
import { getDefaultSavedExploration } from "../types";
import { getAccentColors } from "@/design-system";

const API_BASE = "http://localhost:9876";

const STRATEGIES = [
  {
    id: "smoke_test",
    name: "Smoke Test",
    description: "Quick verification of critical paths only",
  },
  {
    id: "exhaustive",
    name: "Exhaustive",
    description: "Verify all states and transitions",
  },
  {
    id: "regression",
    name: "Regression",
    description: "Focus on previously failed areas",
  },
  {
    id: "random_walk",
    name: "Random Walk",
    description: "Random exploration of the state machine",
  },
  {
    id: "targeted",
    name: "Targeted",
    description: "Verify specific states/transitions",
  },
];

interface ConfigStateInfo {
  id: string;
  name: string;
  description?: string;
  is_initial: boolean;
  is_final: boolean;
}

interface ConfigTransitionInfo {
  id: string;
  name: string;
  from_state?: string;
  to_state?: string;
}

interface StateExplorerBuilderTabProps {
  onLog?: (level: string, message: string) => void;
  editExplorationId?: string | null;
  onNavigateToLibrary?: () => void;
}

export function StateExplorerBuilderTab({
  onLog,
  editExplorationId,
  onNavigateToLibrary,
}: StateExplorerBuilderTabProps) {
  // State
  const [explorations, setExplorations] = useState<SavedExploration[]>([]);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [searchQuery, setSearchQuery] = useState("");
  const [selectedExploration, setSelectedExploration] = useState<SavedExploration | null>(null);
  const [isCreating, setIsCreating] = useState(false);

  // Config file state
  const [configPath, setConfigPath] = useState("");
  const [loadingConfig, setLoadingConfig] = useState(false);
  const [availableStates, setAvailableStates] = useState<ConfigStateInfo[]>([]);
  const [availableTransitions, setAvailableTransitions] = useState<ConfigTransitionInfo[]>([]);
  const [runnerConfigPath, setRunnerConfigPath] = useState<string | null>(null);

  // Form state
  const [formName, setFormName] = useState("");
  const [formDescription, setFormDescription] = useState("");
  const [formTags, setFormTags] = useState("");
  const [formStrategy, setFormStrategy] = useState<
    "exhaustive" | "smoke_test" | "regression" | "random_walk" | "targeted"
  >("smoke_test");
  const [formMaxStates, setFormMaxStates] = useState(0);
  const [formMaxDuration, setFormMaxDuration] = useState(0);
  const [formStateDelay, setFormStateDelay] = useState(500);
  const [formCaptureScreenshots, setFormCaptureScreenshots] = useState(true);
  const [formCaptureTransitionScreenshots, setFormCaptureTransitionScreenshots] = useState(false);
  const [formStopOnFirstFailure, setFormStopOnFirstFailure] = useState(false);
  const [formTargetStateIds, setFormTargetStateIds] = useState<string[]>([]);
  const [formTargetTransitionIds, setFormTargetTransitionIds] = useState<string[]>([]);

  const accentColors = getAccentColors("emerald");

  // Load config file and parse states/transitions
  const loadConfigFile = useCallback(
    async (path: string) => {
      if (!path) {
        setAvailableStates([]);
        setAvailableTransitions([]);
        return;
      }

      setLoadingConfig(true);
      try {
        const response = await fetch(`${API_BASE}/configs/parse`, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ path }),
        });
        const result = await response.json();
        if (result.success && result.data) {
          setAvailableStates(result.data.states || []);
          setAvailableTransitions(result.data.transitions || []);
          onLog?.(
            "success",
            `Loaded ${result.data.states?.length || 0} states and ${result.data.transitions?.length || 0} transitions`,
          );
        } else {
          onLog?.("error", `Failed to parse config: ${result.error || "Unknown error"}`);
          setAvailableStates([]);
          setAvailableTransitions([]);
        }
      } catch (error) {
        onLog?.("error", `Failed to load config: ${error}`);
        setAvailableStates([]);
        setAvailableTransitions([]);
      } finally {
        setLoadingConfig(false);
      }
    },
    [onLog],
  );

  // Browse for config file
  const browseConfigFile = async () => {
    try {
      const selected = await open({
        filters: [{ name: "Config Files", extensions: ["json", "yaml", "yml"] }],
        multiple: false,
      });
      if (selected && typeof selected === "string") {
        setConfigPath(selected);
        await loadConfigFile(selected);
      }
    } catch (error) {
      onLog?.("error", `Failed to open file dialog: ${error}`);
    }
  };

  // Fetch explorations
  const fetchExplorations = useCallback(async () => {
    try {
      setLoading(true);
      const response = await fetch(`${API_BASE}/saved-explorations`);
      if (response.ok) {
        const result = await response.json();
        if (result.success && result.data) {
          setExplorations(result.data);
        } else if (Array.isArray(result)) {
          setExplorations(result);
        } else {
          setExplorations([]);
        }
      }
    } catch (error) {
      console.error("Failed to fetch explorations:", error);
      onLog?.("error", `Failed to fetch explorations: ${error}`);
    } finally {
      setLoading(false);
    }
  }, [onLog]);

  useEffect(() => {
    fetchExplorations();
  }, [fetchExplorations]);

  // Check runner status for loaded config on mount
  useEffect(() => {
    let cancelled = false;
    const checkRunnerConfig = async () => {
      try {
        const response = await fetch(`${API_BASE}/status`);
        if (response.ok && !cancelled) {
          const result = await response.json();
          const path = result.data?.config_path || result.config_path;
          if (path) {
            setRunnerConfigPath(path);
            // Auto-load states/transitions from runner's config
            setConfigPath(path);
            // Parse the config to get states/transitions
            const parseResponse = await fetch(`${API_BASE}/configs/parse`, {
              method: "POST",
              headers: { "Content-Type": "application/json" },
              body: JSON.stringify({ path }),
            });
            if (parseResponse.ok && !cancelled) {
              const parseResult = await parseResponse.json();
              if (parseResult.success && parseResult.data) {
                setAvailableStates(parseResult.data.states || []);
                setAvailableTransitions(parseResult.data.transitions || []);
              }
            }
          }
        }
      } catch (error) {
        console.error("Failed to check runner status:", error);
      }
    };
    checkRunnerConfig();
    return () => {
      cancelled = true;
    };
  }, []); // Run only once on mount

  // Load exploration for editing
  useEffect(() => {
    if (editExplorationId && explorations.length > 0) {
      const exploration = explorations.find((v) => v.id === editExplorationId);
      if (exploration) {
        selectExploration(exploration);
      }
    }
  }, [editExplorationId, explorations]);

  // Select an exploration for editing
  const selectExploration = async (exploration: SavedExploration) => {
    setSelectedExploration(exploration);
    setIsCreating(false);
    setFormName(exploration.name);
    setFormDescription(exploration.description || "");
    setFormTags(exploration.tags.join(", "));
    setFormStrategy(exploration.config.strategy);
    setFormMaxStates(exploration.config.max_states);
    setFormMaxDuration(exploration.config.max_duration_seconds);
    setFormStateDelay(exploration.config.state_delay_ms);
    setFormCaptureScreenshots(exploration.config.capture_screenshots);
    setFormCaptureTransitionScreenshots(exploration.config.capture_transition_screenshots);
    setFormStopOnFirstFailure(exploration.config.stop_on_first_failure);
    setFormTargetStateIds(exploration.config.target_state_ids || []);
    setFormTargetTransitionIds(exploration.config.target_transition_ids || []);

    // Load config file if specified
    if (exploration.config_path) {
      setConfigPath(exploration.config_path);
      await loadConfigFile(exploration.config_path);
    } else {
      setConfigPath("");
      setAvailableStates([]);
      setAvailableTransitions([]);
    }
  };

  // Start creating a new exploration
  const startCreate = () => {
    const defaults = getDefaultSavedExploration();
    setSelectedExploration(null);
    setIsCreating(true);
    setFormName("");
    setFormDescription("");
    setFormTags("");
    setFormStrategy(defaults.config.strategy);
    setFormMaxStates(defaults.config.max_states);
    setFormMaxDuration(defaults.config.max_duration_seconds);
    setFormStateDelay(defaults.config.state_delay_ms);
    setFormCaptureScreenshots(defaults.config.capture_screenshots);
    setFormCaptureTransitionScreenshots(defaults.config.capture_transition_screenshots);
    setFormStopOnFirstFailure(defaults.config.stop_on_first_failure);
    setFormTargetStateIds([]);
    setFormTargetTransitionIds([]);
    setConfigPath("");
    setAvailableStates([]);
    setAvailableTransitions([]);
  };

  // Build config object (without config_path, which is stored separately)
  const buildConfig = (): Omit<ExplorationConfig, "config_path"> => ({
    strategy: formStrategy,
    max_states: formMaxStates,
    max_duration_seconds: formMaxDuration,
    target_state_ids: formTargetStateIds,
    target_transition_ids: formTargetTransitionIds,
    capture_screenshots: formCaptureScreenshots,
    capture_transition_screenshots: formCaptureTransitionScreenshots,
    state_delay_ms: formStateDelay,
    stop_on_first_failure: formStopOnFirstFailure,
  });

  // Save exploration
  const handleSave = async () => {
    if (!formName.trim()) {
      onLog?.("warning", "Name is required");
      return;
    }

    setSaving(true);
    try {
      const payload: CreateSavedExplorationRequest | UpdateSavedExplorationRequest = {
        name: formName.trim(),
        description: formDescription.trim() || undefined,
        config_path: configPath || undefined,
        config: buildConfig(),
        tags: formTags
          .split(",")
          .map((t) => t.trim())
          .filter(Boolean),
      };

      let response;
      if (selectedExploration) {
        response = await fetch(`${API_BASE}/saved-explorations/${selectedExploration.id}`, {
          method: "PUT",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify(payload),
        });
      } else {
        response = await fetch(`${API_BASE}/saved-explorations`, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify(payload),
        });
      }

      const result = await response.json();
      if (result.success && result.data) {
        onLog?.("success", `Exploration "${formName}" saved successfully`);
        await fetchExplorations();
        selectExploration(result.data);
      } else if (response.ok && result.id) {
        onLog?.("success", `Exploration "${formName}" saved successfully`);
        await fetchExplorations();
        selectExploration(result);
      } else {
        onLog?.("error", `Failed to save exploration: ${result.error || "Unknown error"}`);
      }
    } catch (error) {
      onLog?.("error", `Failed to save exploration: ${error}`);
    } finally {
      setSaving(false);
    }
  };

  // Delete exploration
  const handleDelete = async () => {
    if (!selectedExploration) return;

    if (!confirm(`Delete exploration "${selectedExploration.name}"?`)) return;

    try {
      const response = await fetch(`${API_BASE}/saved-explorations/${selectedExploration.id}`, {
        method: "DELETE",
      });

      const result = await response.json();
      if (result.success || response.ok) {
        onLog?.("success", `Exploration "${selectedExploration.name}" deleted`);
        setSelectedExploration(null);
        setIsCreating(false);
        await fetchExplorations();
      } else {
        onLog?.("error", `Failed to delete exploration: ${result.error || "Unknown error"}`);
      }
    } catch (error) {
      onLog?.("error", `Failed to delete exploration: ${error}`);
    }
  };

  // Duplicate exploration
  const handleDuplicate = async () => {
    if (!selectedExploration) return;

    try {
      const payload: CreateSavedExplorationRequest = {
        name: `${selectedExploration.name} (Copy)`,
        description: selectedExploration.description,
        config: selectedExploration.config,
        tags: selectedExploration.tags,
      };

      const response = await fetch(`${API_BASE}/saved-explorations`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(payload),
      });

      const result = await response.json();
      if (result.success && result.data) {
        onLog?.("success", `Exploration duplicated as "${result.data.name}"`);
        await fetchExplorations();
        selectExploration(result.data);
      } else if (response.ok && result.id) {
        onLog?.("success", `Exploration duplicated as "${result.name}"`);
        await fetchExplorations();
        selectExploration(result);
      }
    } catch (error) {
      onLog?.("error", `Failed to duplicate exploration: ${error}`);
    }
  };

  // Toggle state selection
  const toggleState = (stateId: string) => {
    setFormTargetStateIds((prev) =>
      prev.includes(stateId) ? prev.filter((id) => id !== stateId) : [...prev, stateId],
    );
  };

  // Toggle transition selection
  const toggleTransition = (transitionId: string) => {
    setFormTargetTransitionIds((prev) =>
      prev.includes(transitionId)
        ? prev.filter((id) => id !== transitionId)
        : [...prev, transitionId],
    );
  };

  // Filter explorations
  const filteredExplorations = explorations.filter((exploration) => {
    if (!searchQuery) return true;
    const query = searchQuery.toLowerCase();
    return (
      exploration.name.toLowerCase().includes(query) ||
      (exploration.description && exploration.description.toLowerCase().includes(query)) ||
      exploration.tags.some((t) => t.toLowerCase().includes(query))
    );
  });

  return (
    <div className="h-full flex">
      {/* Left Panel - Exploration List */}
      <div className="w-80 border-r border-neutral-700 flex flex-col bg-neutral-900">
        {/* Header */}
        <div className="p-4 border-b border-neutral-700">
          <div className="flex items-center justify-between mb-3">
            <h2 className="text-lg font-semibold flex items-center gap-2">
              <Compass className="w-5 h-5" style={{ color: accentColors.bgSolid }} />
              State Explorer
            </h2>
            <button
              onClick={startCreate}
              className="p-2 rounded-lg hover:bg-neutral-800 transition-colors"
              title="New Exploration"
            >
              <Plus className="w-4 h-4" />
            </button>
          </div>

          {/* Search */}
          <div className="relative">
            <Search className="absolute left-3 top-1/2 -translate-y-1/2 w-4 h-4 text-neutral-400" />
            <input
              type="text"
              placeholder="Search explorations..."
              value={searchQuery}
              onChange={(e) => setSearchQuery(e.target.value)}
              className="w-full pl-9 pr-3 py-2 bg-neutral-800 border border-neutral-700 rounded-lg text-sm focus:outline-none focus:border-neutral-600"
            />
          </div>
        </div>

        {/* Exploration List */}
        <div className="flex-1 overflow-y-auto p-2">
          {loading ? (
            <div className="flex items-center justify-center py-8">
              <Loader2 className="w-6 h-6 animate-spin text-neutral-400" />
            </div>
          ) : filteredExplorations.length === 0 ? (
            <div className="text-center py-8 text-neutral-400">
              <Compass className="w-8 h-8 mx-auto mb-2 opacity-50" />
              <p className="text-sm">No explorations found</p>
            </div>
          ) : (
            <div className="space-y-1">
              {filteredExplorations.map((exploration) => (
                <button
                  key={exploration.id}
                  onClick={() => selectExploration(exploration)}
                  className={`w-full text-left p-3 rounded-lg transition-colors ${
                    selectedExploration?.id === exploration.id
                      ? "bg-neutral-700"
                      : "hover:bg-neutral-800"
                  }`}
                >
                  <div className="font-medium text-sm truncate">{exploration.name}</div>
                  <div className="flex items-center gap-2 mt-1">
                    <span className="text-xs px-1.5 py-0.5 rounded bg-neutral-800 text-neutral-400">
                      {exploration.config.strategy}
                    </span>
                    {exploration.run_count > 0 && (
                      <span className="text-xs text-neutral-500">{exploration.run_count} runs</span>
                    )}
                  </div>
                </button>
              ))}
            </div>
          )}
        </div>
      </div>

      {/* Right Panel - Editor */}
      <div className="flex-1 flex flex-col bg-neutral-900/50">
        {!selectedExploration && !isCreating ? (
          <div className="flex-1 flex items-center justify-center">
            <div className="text-center text-neutral-400">
              <Compass className="w-12 h-12 mx-auto mb-3 opacity-30" />
              <p className="text-lg mb-2">No exploration selected</p>
              <p className="text-sm mb-4">Select an exploration to edit or create a new one</p>
              <button
                onClick={startCreate}
                className="inline-flex items-center gap-2 px-4 py-2 rounded-lg transition-colors"
                style={{ backgroundColor: accentColors.bgSolid, color: "#000" }}
              >
                <Plus className="w-4 h-4" />
                Create New Exploration
              </button>
            </div>
          </div>
        ) : (
          <>
            {/* Toolbar */}
            <div className="flex items-center justify-between p-4 border-b border-neutral-700">
              <h3 className="font-medium">
                {isCreating ? "New Exploration" : `Editing: ${selectedExploration?.name}`}
              </h3>
              <div className="flex items-center gap-2">
                {selectedExploration && (
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
                  disabled={saving || !formName.trim()}
                  className="flex items-center gap-2 px-4 py-2 rounded-lg disabled:opacity-50 transition-colors"
                  style={{ backgroundColor: accentColors.bgSolid, color: "#000" }}
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
                      placeholder="Exploration name"
                      className="w-full px-3 py-2 bg-neutral-800 border border-neutral-700 rounded-lg focus:outline-none focus:border-neutral-600"
                    />
                  </div>

                  <div>
                    <label className="block text-sm font-medium mb-1.5">Description</label>
                    <textarea
                      value={formDescription}
                      onChange={(e) => setFormDescription(e.target.value)}
                      placeholder="Describe what this exploration tests..."
                      rows={2}
                      className="w-full px-3 py-2 bg-neutral-800 border border-neutral-700 rounded-lg focus:outline-none focus:border-neutral-600 resize-y"
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

                {/* Config File */}
                <div className="border border-neutral-700 rounded-lg p-4 space-y-4">
                  <div className="flex items-center gap-2">
                    <FolderOpen className="w-4 h-4" style={{ color: accentColors.bgSolid }} />
                    <h4 className="font-medium">Workflow Configuration</h4>
                  </div>

                  {/* Show runner's loaded config if available */}
                  {runnerConfigPath && configPath !== runnerConfigPath && (
                    <div className="flex items-center gap-2 p-2 bg-blue-900/20 border border-blue-800 rounded-lg">
                      <span className="text-xs text-blue-400">
                        Runner has config loaded: {runnerConfigPath.split(/[/\\]/).pop()}
                      </span>
                      <button
                        onClick={async () => {
                          setConfigPath(runnerConfigPath);
                          await loadConfigFile(runnerConfigPath);
                        }}
                        className="text-xs px-2 py-1 bg-blue-800 hover:bg-blue-700 rounded transition-colors"
                      >
                        Use This
                      </button>
                    </div>
                  )}

                  <div>
                    <label className="block text-sm mb-1.5">Config File Path</label>
                    <div className="flex gap-2">
                      <input
                        type="text"
                        value={configPath}
                        onChange={(e) => setConfigPath(e.target.value)}
                        placeholder="Select a workflow config file..."
                        className="flex-1 px-3 py-2 bg-neutral-800 border border-neutral-700 rounded-lg focus:outline-none focus:border-neutral-600 text-sm"
                      />
                      <button
                        onClick={browseConfigFile}
                        disabled={loadingConfig}
                        className="px-3 py-2 bg-neutral-800 border border-neutral-700 rounded-lg hover:bg-neutral-700 transition-colors"
                      >
                        {loadingConfig ? (
                          <Loader2 className="w-4 h-4 animate-spin" />
                        ) : (
                          <FolderOpen className="w-4 h-4" />
                        )}
                      </button>
                      {configPath && (
                        <button
                          onClick={() => loadConfigFile(configPath)}
                          disabled={loadingConfig}
                          className="px-3 py-2 bg-neutral-800 border border-neutral-700 rounded-lg hover:bg-neutral-700 transition-colors text-sm"
                        >
                          Reload
                        </button>
                      )}
                    </div>
                    {availableStates.length > 0 && (
                      <p className="text-xs text-neutral-500 mt-1">
                        Loaded {availableStates.length} states and {availableTransitions.length}{" "}
                        transitions
                      </p>
                    )}
                    {configPath === runnerConfigPath && runnerConfigPath && (
                      <p className="text-xs text-emerald-500 mt-1">
                        Using runner's loaded configuration
                      </p>
                    )}
                  </div>
                </div>

                {/* Strategy */}
                <div className="border border-neutral-700 rounded-lg p-4 space-y-4">
                  <div className="flex items-center gap-2">
                    <Target className="w-4 h-4" style={{ color: accentColors.bgSolid }} />
                    <h4 className="font-medium">Exploration Strategy</h4>
                  </div>

                  <div className="grid grid-cols-1 gap-2">
                    {STRATEGIES.map((strategy) => (
                      <label
                        key={strategy.id}
                        className={`flex items-start gap-3 p-3 rounded-lg border cursor-pointer transition-colors ${
                          formStrategy === strategy.id
                            ? "border-emerald-600 bg-emerald-900/20"
                            : "border-neutral-700 hover:border-neutral-600"
                        }`}
                      >
                        <input
                          type="radio"
                          name="strategy"
                          value={strategy.id}
                          checked={formStrategy === strategy.id}
                          onChange={(e) => setFormStrategy(e.target.value as typeof formStrategy)}
                          className="mt-1"
                        />
                        <div>
                          <div className="font-medium text-sm">{strategy.name}</div>
                          <div className="text-xs text-neutral-400">{strategy.description}</div>
                        </div>
                      </label>
                    ))}
                  </div>

                  {formStrategy === "targeted" && (
                    <div className="space-y-4 pt-2 border-t border-neutral-700">
                      {/* Target States */}
                      <div>
                        <label className="block text-sm font-medium mb-2">Target States</label>
                        {availableStates.length === 0 ? (
                          <p className="text-sm text-neutral-500 italic">
                            Load a config file to select states
                          </p>
                        ) : (
                          <div className="max-h-48 overflow-y-auto border border-neutral-700 rounded-lg">
                            {availableStates.map((state) => (
                              <label
                                key={state.id}
                                className={`flex items-center gap-3 p-2 cursor-pointer hover:bg-neutral-800 transition-colors border-b border-neutral-700 last:border-b-0 ${
                                  formTargetStateIds.includes(state.id) ? "bg-emerald-900/20" : ""
                                }`}
                              >
                                <input
                                  type="checkbox"
                                  checked={formTargetStateIds.includes(state.id)}
                                  onChange={() => toggleState(state.id)}
                                  className="w-4 h-4 rounded"
                                />
                                <div className="flex-1 min-w-0">
                                  <div className="text-sm font-medium truncate">{state.name}</div>
                                  {state.description && (
                                    <div className="text-xs text-neutral-500 truncate">
                                      {state.description}
                                    </div>
                                  )}
                                </div>
                                <div className="flex gap-1">
                                  {state.is_initial && (
                                    <span className="text-xs px-1.5 py-0.5 rounded bg-blue-900/50 text-blue-400">
                                      Initial
                                    </span>
                                  )}
                                  {state.is_final && (
                                    <span className="text-xs px-1.5 py-0.5 rounded bg-green-900/50 text-green-400">
                                      Final
                                    </span>
                                  )}
                                </div>
                              </label>
                            ))}
                          </div>
                        )}
                        {formTargetStateIds.length > 0 && (
                          <p className="text-xs text-neutral-500 mt-1">
                            {formTargetStateIds.length} state(s) selected
                          </p>
                        )}
                      </div>

                      {/* Target Transitions */}
                      <div>
                        <label className="block text-sm font-medium mb-2">Target Transitions</label>
                        {availableTransitions.length === 0 ? (
                          <p className="text-sm text-neutral-500 italic">
                            Load a config file to select transitions
                          </p>
                        ) : (
                          <div className="max-h-48 overflow-y-auto border border-neutral-700 rounded-lg">
                            {availableTransitions.map((transition) => (
                              <label
                                key={transition.id}
                                className={`flex items-center gap-3 p-2 cursor-pointer hover:bg-neutral-800 transition-colors border-b border-neutral-700 last:border-b-0 ${
                                  formTargetTransitionIds.includes(transition.id)
                                    ? "bg-emerald-900/20"
                                    : ""
                                }`}
                              >
                                <input
                                  type="checkbox"
                                  checked={formTargetTransitionIds.includes(transition.id)}
                                  onChange={() => toggleTransition(transition.id)}
                                  className="w-4 h-4 rounded"
                                />
                                <div className="flex-1 min-w-0">
                                  <div className="text-sm font-medium truncate">
                                    {transition.name}
                                  </div>
                                  {(transition.from_state || transition.to_state) && (
                                    <div className="text-xs text-neutral-500 truncate">
                                      {transition.from_state || "?"} → {transition.to_state || "?"}
                                    </div>
                                  )}
                                </div>
                              </label>
                            ))}
                          </div>
                        )}
                        {formTargetTransitionIds.length > 0 && (
                          <p className="text-xs text-neutral-500 mt-1">
                            {formTargetTransitionIds.length} transition(s) selected
                          </p>
                        )}
                      </div>
                    </div>
                  )}
                </div>

                {/* Limits */}
                <div className="border border-neutral-700 rounded-lg p-4 space-y-4">
                  <div className="flex items-center gap-2">
                    <Clock className="w-4 h-4" style={{ color: accentColors.bgSolid }} />
                    <h4 className="font-medium">Limits</h4>
                  </div>

                  <div className="grid grid-cols-3 gap-4">
                    <div>
                      <label className="block text-sm mb-1.5">Max States</label>
                      <input
                        type="number"
                        value={formMaxStates}
                        onChange={(e) => setFormMaxStates(parseInt(e.target.value) || 0)}
                        min={0}
                        placeholder="0 = unlimited"
                        className="w-full px-3 py-2 bg-neutral-800 border border-neutral-700 rounded-lg focus:outline-none focus:border-neutral-600 text-sm"
                      />
                      <p className="text-xs text-neutral-500 mt-1">0 = unlimited</p>
                    </div>
                    <div>
                      <label className="block text-sm mb-1.5">Max Duration (s)</label>
                      <input
                        type="number"
                        value={formMaxDuration}
                        onChange={(e) => setFormMaxDuration(parseInt(e.target.value) || 0)}
                        min={0}
                        placeholder="0 = unlimited"
                        className="w-full px-3 py-2 bg-neutral-800 border border-neutral-700 rounded-lg focus:outline-none focus:border-neutral-600 text-sm"
                      />
                      <p className="text-xs text-neutral-500 mt-1">0 = unlimited</p>
                    </div>
                    <div>
                      <label className="block text-sm mb-1.5">State Delay (ms)</label>
                      <input
                        type="number"
                        value={formStateDelay}
                        onChange={(e) => setFormStateDelay(parseInt(e.target.value) || 500)}
                        min={0}
                        className="w-full px-3 py-2 bg-neutral-800 border border-neutral-700 rounded-lg focus:outline-none focus:border-neutral-600 text-sm"
                      />
                    </div>
                  </div>
                </div>

                {/* Options */}
                <div className="border border-neutral-700 rounded-lg p-4 space-y-4">
                  <div className="flex items-center gap-2">
                    <Settings className="w-4 h-4" style={{ color: accentColors.bgSolid }} />
                    <h4 className="font-medium">Options</h4>
                  </div>

                  <div className="space-y-3">
                    <label className="flex items-center gap-3 cursor-pointer">
                      <input
                        type="checkbox"
                        checked={formCaptureScreenshots}
                        onChange={(e) => setFormCaptureScreenshots(e.target.checked)}
                        className="w-4 h-4 rounded"
                      />
                      <div>
                        <div className="text-sm flex items-center gap-2">
                          <Camera className="w-4 h-4" />
                          Capture screenshots at each state
                        </div>
                      </div>
                    </label>

                    <label className="flex items-center gap-3 cursor-pointer">
                      <input
                        type="checkbox"
                        checked={formCaptureTransitionScreenshots}
                        onChange={(e) => setFormCaptureTransitionScreenshots(e.target.checked)}
                        className="w-4 h-4 rounded"
                      />
                      <div>
                        <div className="text-sm">Capture screenshots during transitions</div>
                      </div>
                    </label>

                    <label className="flex items-center gap-3 cursor-pointer">
                      <input
                        type="checkbox"
                        checked={formStopOnFirstFailure}
                        onChange={(e) => setFormStopOnFirstFailure(e.target.checked)}
                        className="w-4 h-4 rounded"
                      />
                      <div>
                        <div className="text-sm flex items-center gap-2">
                          <AlertTriangle className="w-4 h-4" />
                          Stop on first failure
                        </div>
                      </div>
                    </label>
                  </div>
                </div>
              </div>
            </div>
          </>
        )}
      </div>
    </div>
  );
}
