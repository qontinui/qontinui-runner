/**
 * StateMachineBuilderPage
 *
 * Full-page state machine builder with config management, state/transition
 * editing, graph visualization, pathfinding, and import/export.
 *
 * Uses shared components from @qontinui/workflow-ui/state-machine and
 * shared logic from @qontinui/workflow-utils.
 */

import { useState, useCallback, useMemo } from "react";
import {
  GitBranch,
  Download,
  Upload,
  Search,
  Table2,
  Route,
  Network,
  X,
  Compass,
  Zap,
  Plus,
} from "lucide-react";
import { open, save } from "@tauri-apps/plugin-dialog";
import { readTextFile, writeTextFile } from "@tauri-apps/plugin-fs";
import type {
  StateMachineTransitionCreate,
  StateMachineStateUpdate,
  PathfindingResult,
} from "@qontinui/shared-types";
import {
  StateViewPanel,
  TransitionsPanel,
  PathfindingPanel,
  StateDetailPanel,
  TransitionEditor,
} from "@qontinui/workflow-ui/state-machine";
import { buildExportConfig } from "@qontinui/workflow-utils";

import { useStateMachineConfig } from "@/hooks/useStateMachineConfig";
import { StateMachineGraph } from "./StateMachineGraph";
import { ConfigHeader } from "./ConfigHeader";
import { DiscoveryPanel } from "./DiscoveryPanel";
import { LiveTestPanel } from "./LiveTestPanel";

// =============================================================================
// Types
// =============================================================================

type BuilderTab =
  | "graph"
  | "states"
  | "transitions"
  | "pathfinding"
  | "discovery"
  | "live-test"
  | "export";

const TABS: { id: BuilderTab; label: string; icon: React.ReactNode }[] = [
  { id: "graph", label: "Graph", icon: <Network className="w-4 h-4" /> },
  { id: "states", label: "States", icon: <Table2 className="w-4 h-4" /> },
  { id: "transitions", label: "Transitions", icon: <Route className="w-4 h-4" /> },
  { id: "pathfinding", label: "Pathfinding", icon: <Search className="w-4 h-4" /> },
  { id: "discovery", label: "Discovery", icon: <Compass className="w-4 h-4" /> },
  { id: "live-test", label: "Live Test", icon: <Zap className="w-4 h-4" /> },
  { id: "export", label: "Export / Import", icon: <Download className="w-4 h-4" /> },
];

// =============================================================================
// Component
// =============================================================================

export function StateMachineBuilderPage() {
  const sm = useStateMachineConfig();
  const {
    updateState,
    deleteState,
    createState,
    createTransition,
    updateTransition,
    deleteTransition,
    importConfig,
    loadConfig,
  } = sm;

  const [activeTab, setActiveTab] = useState<BuilderTab>("graph");
  const [selectedStateId, setSelectedStateId] = useState<string | null>(null);
  const [selectedTransitionId, setSelectedTransitionId] = useState<string | null>(null);
  const [showSidebar, setShowSidebar] = useState(true);
  const [importError, setImportError] = useState<string | null>(null);
  const [showNewTransition, setShowNewTransition] = useState(false);
  const [showNewState, setShowNewState] = useState(false);
  const [newStateName, setNewStateName] = useState("");
  const [newStateDescription, setNewStateDescription] = useState("");
  const [isCreatingState, setIsCreatingState] = useState(false);

  // Derived data
  const states = useMemo(() => sm.activeConfig?.states ?? [], [sm.activeConfig?.states]);
  const transitions = useMemo(
    () => sm.activeConfig?.transitions ?? [],
    [sm.activeConfig?.transitions],
  );

  const selectedState = useMemo(
    () => states.find((s) => s.state_id === selectedStateId) ?? null,
    [states, selectedStateId],
  );

  const selectedTransition = useMemo(
    () => transitions.find((t) => t.id === selectedTransitionId) ?? null,
    [transitions, selectedTransitionId],
  );

  // ---------- State handlers ----------

  const handleSelectState = useCallback(
    (stateId: string | null) => {
      setSelectedStateId(stateId);
      setSelectedTransitionId(null);
      setShowNewTransition(false);
      setShowNewState(false);
      if (stateId && !showSidebar) setShowSidebar(true);
    },
    [showSidebar],
  );

  const handleSaveState = useCallback(
    async (id: string, updates: StateMachineStateUpdate) => {
      await updateState(id, updates);
    },
    [updateState],
  );

  const handleDeleteState = useCallback(
    async (id: string) => {
      // StateDetailPanel passes state.id (database UUID)
      const state = states.find((s) => s.id === id);
      await deleteState(id);
      if (state && selectedStateId === state.state_id) {
        setSelectedStateId(null);
      }
    },
    [deleteState, selectedStateId, states],
  );

  const handleCreateState = useCallback(async () => {
    const trimmed = newStateName.trim();
    if (!trimmed) return;
    setIsCreatingState(true);
    try {
      const created = await createState({
        name: trimmed,
        description: newStateDescription.trim() || undefined,
      });
      setShowNewState(false);
      setNewStateName("");
      setNewStateDescription("");
      setSelectedStateId(created.state_id);
      setSelectedTransitionId(null);
    } finally {
      setIsCreatingState(false);
    }
  }, [createState, newStateName, newStateDescription]);

  const handleOpenNewState = useCallback(() => {
    setShowNewState(true);
    setShowNewTransition(false);
    setSelectedStateId(null);
    setSelectedTransitionId(null);
    setShowSidebar(true);
  }, []);

  const handleOpenNewTransition = useCallback(() => {
    setShowNewTransition(true);
    setShowNewState(false);
    setSelectedStateId(null);
    setSelectedTransitionId(null);
    setShowSidebar(true);
  }, []);

  // ---------- Transition handlers ----------

  const handleSelectTransition = useCallback(
    (transitionId: string | null) => {
      setSelectedTransitionId(transitionId);
      setSelectedStateId(null);
      setShowNewTransition(false);
      setShowNewState(false);
      if (transitionId && !showSidebar) setShowSidebar(true);
    },
    [showSidebar],
  );

  const handleSaveTransition = useCallback(
    async (data: StateMachineTransitionCreate) => {
      await createTransition(data);
      setShowNewTransition(false);
    },
    [createTransition],
  );

  const handleUpdateTransition = useCallback(
    async (id: string, data: Partial<StateMachineTransitionCreate>) => {
      await updateTransition(id, data);
    },
    [updateTransition],
  );

  const handleDeleteTransition = useCallback(
    async (id: string) => {
      await deleteTransition(id);
      if (selectedTransitionId === id) {
        setSelectedTransitionId(null);
      }
    },
    [deleteTransition, selectedTransitionId],
  );

  // ---------- Pathfinding ----------

  const handlePathFound = useCallback((_result: PathfindingResult) => {
    // Could highlight path on graph in the future
  }, []);

  // ---------- Export / Import ----------

  const handleExport = useCallback(async () => {
    if (!sm.activeConfig) return;
    const ac = sm.activeConfig;
    const exportData = buildExportConfig(ac, ac.states, ac.transitions);
    const json = JSON.stringify(exportData, null, 2);

    const filePath = await save({
      defaultPath: `${ac.name.replace(/\s+/g, "_")}.json`,
      filters: [{ name: "JSON", extensions: ["json"] }],
    });
    if (filePath) {
      await writeTextFile(filePath, json);
    }
  }, [sm.activeConfig]);

  const handleImportFile = useCallback(async () => {
    setImportError(null);
    const filePath = await open({
      multiple: false,
      filters: [{ name: "JSON", extensions: ["json"] }],
    });
    if (!filePath || typeof filePath !== "string") return;

    try {
      const content = await readTextFile(filePath);
      const data = JSON.parse(content);

      // Basic validation
      if (!data.states || !data.transitions) {
        throw new Error("Invalid config format: missing states or transitions");
      }

      const name =
        data.name ||
        filePath.toString().split(/[\\/]/).pop()?.replace(".json", "") ||
        "Imported Config";
      await importConfig(name, data);
    } catch (err) {
      setImportError(err instanceof Error ? err.message : String(err));
    }
  }, [importConfig]);

  // ---------- Render ----------

  if (!sm.activeConfig) {
    return (
      <div className="h-full flex flex-col p-4 gap-4">
        <ConfigHeader
          configs={sm.configs}
          activeConfigId={null}
          onSelectConfig={(id) => loadConfig(id)}
          onCreateConfig={sm.createConfig}
          onDeleteConfig={sm.deleteConfig}
          isLoading={sm.isLoading}
        />
        <div className="flex-1 flex items-center justify-center">
          <div className="text-center space-y-3">
            <GitBranch className="w-12 h-12 text-text-muted mx-auto" />
            <p className="text-text-secondary">
              Select a config or create a new one to get started.
            </p>
          </div>
        </div>
      </div>
    );
  }

  return (
    <div className="h-full flex flex-col">
      {/* Header */}
      <div className="shrink-0 px-4 pt-4 pb-2">
        <ConfigHeader
          configs={sm.configs}
          activeConfigId={sm.activeConfig.id}
          onSelectConfig={(id) => loadConfig(id)}
          onCreateConfig={sm.createConfig}
          onDeleteConfig={sm.deleteConfig}
          isLoading={sm.isLoading}
        />
      </div>

      {/* Tab bar */}
      <div className="shrink-0 px-4 flex items-center gap-1 border-b border-border-secondary">
        {TABS.map((tab) => (
          <button
            key={tab.id}
            onClick={() => setActiveTab(tab.id)}
            className={`flex items-center gap-1.5 px-3 py-2 text-sm font-medium border-b-2 transition-colors ${
              activeTab === tab.id
                ? "border-brand-primary text-brand-primary"
                : "border-transparent text-text-secondary hover:text-text-primary"
            }`}
          >
            {tab.icon}
            <span>{tab.label}</span>
          </button>
        ))}
      </div>

      {/* Main content area */}
      <div className="flex-1 flex min-h-0">
        {/* Left panel — tab content */}
        <div className="flex-1 overflow-auto">
          {activeTab === "graph" && (
            <div className="relative h-full">
              {/* Floating action buttons */}
              <div className="absolute top-3 left-3 z-10 flex gap-1.5">
                <button
                  onClick={handleOpenNewState}
                  className="flex items-center gap-1.5 px-2.5 py-1.5 text-xs font-medium text-white bg-brand-primary hover:bg-brand-primary/90 rounded shadow-md"
                >
                  <Plus className="w-3.5 h-3.5" />
                  <span>Add State</span>
                </button>
                <button
                  onClick={handleOpenNewTransition}
                  className="flex items-center gap-1.5 px-2.5 py-1.5 text-xs font-medium text-text-primary border border-border-secondary hover:border-border-primary bg-bg-primary/90 backdrop-blur-sm rounded shadow-md"
                >
                  <Plus className="w-3.5 h-3.5" />
                  <span>Add Transition</span>
                </button>
              </div>
              <StateMachineGraph
                states={states}
                transitions={transitions}
                selectedStateId={selectedStateId}
                selectedTransitionId={selectedTransitionId}
                onSelectState={handleSelectState}
                onSelectTransition={handleSelectTransition}
                onDeleteTransition={handleDeleteTransition}
              />
            </div>
          )}

          {activeTab === "states" && (
            <div className="relative h-full">
              <div className="absolute top-3 right-3 z-10">
                <button
                  onClick={handleOpenNewState}
                  className="flex items-center gap-1.5 px-2.5 py-1.5 text-xs font-medium text-white bg-brand-primary hover:bg-brand-primary/90 rounded shadow-md"
                >
                  <Plus className="w-3.5 h-3.5" />
                  <span>Add State</span>
                </button>
              </div>
              <StateViewPanel
                states={states}
                transitions={transitions}
                selectedStateId={selectedStateId}
                onSelectState={handleSelectState}
              />
            </div>
          )}

          {activeTab === "transitions" && (
            <div className="relative h-full">
              <div className="absolute top-3 right-3 z-10">
                <button
                  onClick={handleOpenNewTransition}
                  className="flex items-center gap-1.5 px-2.5 py-1.5 text-xs font-medium text-white bg-brand-primary hover:bg-brand-primary/90 rounded shadow-md"
                >
                  <Plus className="w-3.5 h-3.5" />
                  <span>Add Transition</span>
                </button>
              </div>
              <TransitionsPanel
                states={states}
                transitions={transitions}
                onSelectTransition={handleSelectTransition}
              />
            </div>
          )}

          {activeTab === "pathfinding" && (
            <PathfindingPanel
              states={states}
              transitions={transitions}
              onPathFound={handlePathFound}
            />
          )}

          {activeTab === "discovery" && (
            <DiscoveryPanel
              configId={sm.activeConfig.id}
              configName={sm.activeConfig.name}
              states={states}
              transitions={transitions}
            />
          )}

          {activeTab === "live-test" && <LiveTestPanel states={states} transitions={transitions} />}

          {activeTab === "export" && (
            <div className="p-4 space-y-4">
              <h3 className="text-sm font-semibold text-text-primary">Export / Import</h3>

              <div className="flex gap-3">
                <button
                  onClick={handleExport}
                  className="flex items-center gap-2 px-4 py-2 text-sm font-medium text-white bg-brand-primary hover:bg-brand-primary/90 rounded"
                >
                  <Download className="w-4 h-4" />
                  Export Config
                </button>

                <button
                  onClick={handleImportFile}
                  className="flex items-center gap-2 px-4 py-2 text-sm font-medium text-text-primary border border-border-secondary hover:border-border-primary rounded"
                >
                  <Upload className="w-4 h-4" />
                  Import Config
                </button>
              </div>

              {importError && (
                <div className="p-3 text-sm text-red-400 bg-red-500/10 border border-red-500/20 rounded">
                  {importError}
                </div>
              )}

              {sm.activeConfig && (
                <div className="mt-4 p-3 bg-bg-tertiary border border-border-secondary rounded">
                  <p className="text-xs text-text-secondary mb-1">Current config</p>
                  <p className="text-sm text-text-primary font-medium">{sm.activeConfig.name}</p>
                  <p className="text-xs text-text-secondary mt-1">
                    {states.length} states, {transitions.length} transitions
                  </p>
                </div>
              )}
            </div>
          )}
        </div>

        {/* Right sidebar */}
        {showSidebar &&
          (selectedState || selectedTransition || showNewTransition || showNewState) && (
            <div className="w-80 border-l border-border-secondary overflow-auto">
              <div className="flex items-center justify-between px-3 py-2 border-b border-border-secondary">
                <span className="text-xs font-medium text-text-secondary uppercase">
                  {showNewState
                    ? "New State"
                    : showNewTransition
                      ? "New Transition"
                      : selectedState
                        ? "State Detail"
                        : "Transition Detail"}
                </span>
                <button
                  onClick={() => {
                    setSelectedStateId(null);
                    setSelectedTransitionId(null);
                    setShowNewTransition(false);
                    setShowNewState(false);
                  }}
                  className="p-1 hover:bg-bg-tertiary rounded"
                  aria-label="Close sidebar"
                >
                  <X className="w-3.5 h-3.5 text-text-muted" />
                </button>
              </div>

              {/* New State form */}
              {showNewState && (
                <div className="flex flex-col gap-4 p-4">
                  <div>
                    <label className="block text-xs text-text-secondary mb-1">Name</label>
                    <input
                      type="text"
                      value={newStateName}
                      onChange={(e) => setNewStateName(e.target.value)}
                      placeholder="e.g., Login Page"
                      aria-label="e.g., Login Page"
                      autoFocus
                      onKeyDown={(e) => {
                        if (e.key === "Enter" && newStateName.trim()) handleCreateState();
                      }}
                      className="w-full px-2 py-1.5 text-sm bg-bg-tertiary border border-border-secondary rounded text-text-primary placeholder:text-text-muted"
                    />
                  </div>
                  <div>
                    <label className="block text-xs text-text-secondary mb-1">
                      <span>Optional description</span>
                    </label>
                    <textarea
                      value={newStateDescription}
                      onChange={(e) => setNewStateDescription(e.target.value)}
                      rows={3}
                      placeholder="Optional description"
                      aria-label="Optional description"
                      className="w-full px-2 py-1.5 text-sm bg-bg-tertiary border border-border-secondary rounded text-text-primary placeholder:text-text-muted resize-y"
                    />
                  </div>
                  <button
                    onClick={handleCreateState}
                    disabled={!newStateName.trim() || isCreatingState}
                    className="w-full px-3 py-1.5 text-sm font-medium text-white bg-brand-primary hover:bg-brand-primary/90 disabled:opacity-50 disabled:cursor-not-allowed rounded"
                  >
                    {isCreatingState ? "Creating..." : "Create State"}
                  </button>
                </div>
              )}

              {/* New Transition editor */}
              {showNewTransition && (
                <TransitionEditor
                  transition={null}
                  states={states}
                  onSave={handleSaveTransition}
                  onUpdate={() => Promise.resolve()}
                  onDelete={() => Promise.resolve()}
                  onClose={() => setShowNewTransition(false)}
                />
              )}

              {/* Existing state detail */}
              {selectedState && !showNewState && !showNewTransition && (
                <StateDetailPanel
                  state={selectedState}
                  onSave={handleSaveState}
                  onDelete={handleDeleteState}
                  onClose={() => setSelectedStateId(null)}
                />
              )}

              {/* Existing transition editor */}
              {selectedTransition && !showNewState && !showNewTransition && (
                <TransitionEditor
                  transition={selectedTransition}
                  states={states}
                  onSave={handleSaveTransition}
                  onUpdate={(id, data) => handleUpdateTransition(id, data)}
                  onDelete={(id) => handleDeleteTransition(id)}
                  onClose={() => setSelectedTransitionId(null)}
                />
              )}
            </div>
          )}
      </div>

      {/* Error banner */}
      {sm.error && (
        <div className="shrink-0 px-4 py-2 bg-red-500/10 border-t border-red-500/20 text-sm text-red-400">
          {sm.error}
        </div>
      )}
    </div>
  );
}
