/**
 * UIBridgeStateMachinePage
 *
 * Main page for building state machines from UI Bridge SDK apps.
 * Replaces the old visual GUI automation StateMachineBuilderPage.
 *
 * Tabs: Discovery, Graph Editor, State View, Transitions, Pathfinding, Export
 */

import { useState, useCallback, useEffect, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  Network,
  GitBranch,
  Route,
  Download,
  Plus,
  RefreshCw,
  Loader2,
  Search,
  Layers,
  ArrowRightLeft,
} from "lucide-react";
import type {
  StateMachineTransitionCreate,
  StateMachineStateUpdate,
  PathfindingResult,
} from "@qontinui/shared-types";
import {
  StateViewPanel,
  TransitionsPanel,
  TransitionEditor,
  StateDetailPanel,
  PathfindingPanel,
} from "@qontinui/workflow-ui/state-machine";
import type { CaptureScreenshotMeta } from "@qontinui/workflow-ui/state-machine";

import { useStateMachineConfig } from "@/hooks/useStateMachineConfig";
import { useElementDrag } from "@/hooks/useElementDrag";
import { useUIBridgeDiscovery } from "@/hooks/useUIBridgeDiscovery";
import { useToast } from "@/hooks/useToast";
import { UIBridgeStateGraph } from "./UIBridgeStateGraph";
import { UIBridgeDiscoveryPanel } from "./UIBridgeDiscoveryPanel";
import { ExportPanel } from "./ExportPanel";
import { ToastContainer } from "@/components/ToastContainer";
import { StateExplorationResultsViewer } from "@/components/state-explorer/StateExplorationResultsViewer";

const TABS = [
  { value: "discovery", label: "Discovery", icon: Search },
  { value: "graph", label: "Graph Editor", icon: GitBranch },
  { value: "states", label: "State View", icon: Layers },
  { value: "transitions", label: "Transitions", icon: ArrowRightLeft },
  { value: "pathfinding", label: "Pathfinding", icon: Route },
  { value: "exploration", label: "Exploration Results", icon: Network },
  { value: "export", label: "Export", icon: Download },
] as const;

type TabValue = (typeof TABS)[number]["value"];

export function UIBridgeStateMachinePage() {
  const sm = useStateMachineConfig();
  const { toasts, showToast, dismissToast } = useToast();
  const discovery = useUIBridgeDiscovery(sm, showToast);

  const [activeTab, setActiveTab] = useState<TabValue>(() => {
    // Default to exploration tab when navigated via API (e.g., test verification)
    const params = new URLSearchParams(window.location.search);
    return (params.get("tab") as TabValue) || "discovery";
  });
  const [selectedStateId, setSelectedStateId] = useState<string | null>(null);
  const [selectedTransitionId, setSelectedTransitionId] = useState<string | null>(null);
  const [showNewTransition, setShowNewTransition] = useState(false);
  const [highlightedPath, setHighlightedPath] = useState<PathfindingResult["steps"] | undefined>();
  // Thumbnails loaded from database when config changes, with localStorage fallback
  const [elementThumbnails, setElementThumbnails] = useState<Record<string, string> | undefined>(
    () => {
      try {
        const stored = localStorage.getItem("qontinui-runner-sm-thumbnails");
        if (stored) {
          const parsed = JSON.parse(stored);
          if (parsed && Object.keys(parsed).length > 0) return parsed;
        }
      } catch {
        /* */
      }
      return undefined;
    },
  );
  const activeConfigId = sm.activeConfig?.id;
  useEffect(() => {
    if (!activeConfigId) {
      // eslint-disable-next-line react-hooks/set-state-in-effect -- data fetching reset
      setElementThumbnails(undefined);
      return;
    }
    let isCancelled = false;
    invoke<Record<string, string>>("sm_get_thumbnails", { configId: activeConfigId })
      .then((thumbs) => {
        if (isCancelled) return;
        if (Object.keys(thumbs).length > 0) {
          setElementThumbnails(thumbs);
        }
      })
      .catch(() => {
        // Command may not be available in older runner builds
      });
    return () => {
      isCancelled = true;
    };
  }, [activeConfigId]);

  // Auto-switch to exploration tab when navigated via test-navigation API
  useEffect(() => {
    const handler = () => setActiveTab("exploration");
    window.addEventListener("sm-show-exploration", handler);
    return () => window.removeEventListener("sm-show-exploration", handler);
  }, []);

  // Capture screenshots for screenshot state view
  const [captureScreenshots, setCaptureScreenshots] = useState<
    CaptureScreenshotMeta[] | undefined
  >();
  const screenshotImageCache = useRef<Map<string, string>>(new Map());
  useEffect(() => {
    if (!activeConfigId) {
      // eslint-disable-next-line react-hooks/set-state-in-effect -- data fetching reset
      setCaptureScreenshots(undefined);
      screenshotImageCache.current.clear();
      return;
    }
    screenshotImageCache.current.clear();
    setCaptureScreenshots(undefined);
    let isCancelled = false;
    invoke<CaptureScreenshotMeta[]>("sm_get_capture_screenshots", { configId: activeConfigId })
      .then((screenshots) => {
        if (isCancelled) return;
        setCaptureScreenshots(screenshots.length > 0 ? screenshots : undefined);
      })
      .catch(() => {
        // Command may not be available in older runner builds
      });
    return () => {
      isCancelled = true;
    };
  }, [activeConfigId]);

  // Load screenshot image on demand (returns data URL, cached in component)
  const loadScreenshotImage = useCallback(async (screenshotId: string): Promise<string> => {
    const cached = screenshotImageCache.current.get(screenshotId);
    if (cached) return cached;
    const dataUrl = await invoke<string>("sm_get_capture_screenshot_image", { screenshotId });
    screenshotImageCache.current.set(screenshotId, dataUrl);
    return dataUrl;
  }, []);

  // Also sync from discovery co-occurrence data (available during exploration before save)
  const coocThumbs = discovery.cooccurrenceData?.elementThumbnails;
  useEffect(() => {
    if (coocThumbs && Object.keys(coocThumbs).length > 0) {
      // eslint-disable-next-line react-hooks/set-state-in-effect -- syncing from external discovery data
      setElementThumbnails(coocThumbs);
    }
  }, [coocThumbs]);

  // Derived state
  const states = sm.activeConfig?.states ?? [];
  const transitions = sm.activeConfig?.transitions ?? [];
  const selectedState =
    states.find((s) => s.id === selectedStateId || s.state_id === selectedStateId) ?? null;
  const selectedTransition = transitions.find((t) => t.id === selectedTransitionId) ?? null;

  const showStatePanel = selectedState && !showNewTransition;
  const showTransitionPanel = (selectedTransition || showNewTransition) && !selectedState;
  const noConfig = !sm.activeConfig;
  const hasConfig = !!sm.activeConfig;

  // Transition operations
  const handleCreateTransition = useCallback(
    async (data: StateMachineTransitionCreate) => {
      await sm.createTransition(data);
      setShowNewTransition(false);
    },
    [sm],
  );

  const handleUpdateTransition = useCallback(
    async (id: string, data: Partial<StateMachineTransitionCreate>) => {
      await sm.updateTransition(id, data);
    },
    [sm],
  );

  const handleDeleteTransition = useCallback(
    async (id: string) => {
      await sm.deleteTransition(id);
      setSelectedTransitionId(null);
    },
    [sm],
  );

  // State update for drag-and-drop
  const handleSaveState = useCallback(
    async (stateId: string, updates: StateMachineStateUpdate) => {
      await sm.updateState(stateId, updates);
    },
    [sm],
  );

  const handleUpdateStateElements = useCallback(
    async (stateId: string, elementIds: string[]) => {
      await handleSaveState(stateId, { element_ids: elementIds });
    },
    [handleSaveState],
  );

  // Pathfinding
  const handlePathFound = useCallback((result: PathfindingResult) => {
    setHighlightedPath(result.found ? result.steps : undefined);
  }, []);

  // Drag-and-drop
  const elementDrag = useElementDrag({
    states,
    transitions,
    onCreateTransition: handleCreateTransition,
    onUpdateState: handleUpdateStateElements,
    showToast,
  });

  // Handle static analysis import event from discovery panel
  useEffect(() => {
    const handleStaticImport = async (e: Event) => {
      const detail = (e as CustomEvent).detail as {
        name: string;
        config: unknown;
        stateCount: number;
        transitionCount: number;
      };
      try {
        await sm.importConfig(detail.name, detail.config as any);
        showToast(
          `Imported static state machine: ${detail.stateCount} states, ${detail.transitionCount} transitions`,
          "success",
        );
        setActiveTab("graph");
      } catch (err) {
        showToast(err instanceof Error ? err.message : "Import failed", "error");
      }
    };
    window.addEventListener("sm-import-static", handleStaticImport);
    return () => window.removeEventListener("sm-import-static", handleStaticImport);
  }, [sm, showToast]);

  // When discovery creates a config, select it and switch to graph
  const handleConfigCreated = useCallback(
    async (configId: string) => {
      await sm.loadConfigs();
      await sm.loadConfig(configId);
      setActiveTab("graph");
    },
    [sm],
  );

  return (
    <div className="flex flex-col h-full">
      {/* Header */}
      <div className="flex items-center justify-between px-6 py-4 border-b border-border-primary bg-surface-primary">
        <div className="flex items-center gap-3">
          <Network className="size-5 text-brand-primary" />
          <h1 className="text-lg font-semibold text-text-primary">
            {sm.activeConfig ? sm.activeConfig.name : "UI Bridge States"}
          </h1>
        </div>

        <div className="flex items-center gap-3">
          {/* Config selector */}
          <select
            value={sm.activeConfig?.id ?? ""}
            onChange={(e) => {
              const id = e.target.value;
              if (id) {
                sm.loadConfig(id);
                discovery.reset();
              } else {
                sm.setActiveConfig(null);
              }
            }}
            className="w-[220px] h-9 rounded-md border border-border-primary bg-surface-primary px-3 text-sm text-text-primary focus:outline-hidden focus:ring-2 focus:ring-brand-primary [&>option]:text-black [&>option]:bg-white"
            style={{ colorScheme: "dark" }}
          >
            <option value="">Select configuration...</option>
            {sm.configs.map((c) => (
              <option key={c.id} value={c.id}>
                {c.name}
              </option>
            ))}
          </select>

          {/* Refresh */}
          <button
            onClick={() => {
              sm.loadConfigs();
              if (sm.activeConfig) sm.loadConfig(sm.activeConfig.id);
            }}
            disabled={sm.isLoading}
            className="inline-flex items-center gap-1.5 rounded-md px-3 py-1.5 text-sm font-medium border border-border-primary text-text-primary hover:bg-surface-secondary disabled:opacity-50"
          >
            <RefreshCw className={`size-3.5 ${sm.isLoading ? "animate-spin" : ""}`} />
            Refresh
          </button>
        </div>
      </div>

      {/* Tab Navigation */}
      <div className="border-b border-border-primary bg-surface-primary px-6">
        <div className="inline-flex h-9 w-fit items-center justify-center rounded-lg p-[3px]">
          {TABS.map(({ value, label, icon: Icon }) => (
            <button
              key={value}
              onClick={() => setActiveTab(value)}
              aria-label={
                value === "exploration" ? "exploration results navigation link or tab" : undefined
              }
              className={`inline-flex items-center justify-center gap-1.5 rounded-md px-2 py-1 text-sm font-medium whitespace-nowrap transition-[color,box-shadow] ${
                activeTab === value
                  ? "bg-background shadow-xs text-foreground"
                  : "text-muted-foreground hover:text-foreground"
              }`}
            >
              <Icon className="size-3.5" />
              {label}
            </button>
          ))}
        </div>
      </div>

      {/* Tab Content — all panels stay in DOM (hidden when inactive) */}

      {/* Discovery Tab */}
      <div className={`flex-1 overflow-y-auto ${activeTab !== "discovery" ? "hidden" : ""}`}>
        <UIBridgeDiscoveryPanel discovery={discovery} onConfigCreated={handleConfigCreated} />
      </div>

      {/* Graph Editor Tab */}
      <div className={`flex-1 flex min-h-0 ${activeTab !== "graph" ? "hidden" : ""}`}>
        <div className={sm.isLoading ? "flex-1 flex items-center justify-center" : "hidden"}>
          <Loader2 className="size-8 animate-spin text-brand-primary" />
        </div>
        <div
          className={
            noConfig && !sm.isLoading
              ? "flex-1 flex items-center justify-center text-text-muted"
              : "hidden"
          }
        >
          Select a configuration to view the state graph
        </div>
        <div className={hasConfig && !sm.isLoading ? "flex-1 flex" : "hidden"}>
          {/* Graph */}
          <div className="flex-1 relative">
            {/* Add Transition FAB */}
            <div className="absolute top-4 left-4 z-10">
              <button
                onClick={() => {
                  setSelectedStateId(null);
                  setSelectedTransitionId(null);
                  setShowNewTransition(true);
                }}
                className="inline-flex items-center gap-1.5 rounded-md px-3 py-1.5 text-sm font-medium bg-brand-primary text-white hover:bg-brand-primary/90"
              >
                <Plus className="size-3.5" />
                Add Transition
              </button>
            </div>

            <UIBridgeStateGraph
              states={states}
              transitions={transitions}
              selectedStateId={selectedStateId}
              selectedTransitionId={selectedTransitionId}
              elementThumbnails={elementThumbnails}
              onSelectState={(id) => {
                setSelectedStateId(id);
                setSelectedTransitionId(null);
                setShowNewTransition(false);
              }}
              onSelectTransition={(id) => {
                setSelectedTransitionId(id);
                setSelectedStateId(null);
                setShowNewTransition(false);
              }}
              highlightedPath={highlightedPath}
              onStartElementDrag={elementDrag.handleStartElementDrag}
              onDragOver={elementDrag.handleDragOver}
              onDrop={elementDrag.handleDrop}
              isDragging={elementDrag.isDragging}
              dropTargetStateId={elementDrag.dropTargetStateId}
              onDeleteTransition={handleDeleteTransition}
            />
          </div>

          {/* Side Panel — State */}
          <div
            className={
              showStatePanel && selectedState
                ? "w-80 border-l border-border-primary bg-surface-primary overflow-y-auto"
                : "hidden"
            }
          >
            {selectedState && (
              <StateDetailPanel
                state={selectedState}
                onSave={handleSaveState}
                onClose={() => setSelectedStateId(null)}
              />
            )}
          </div>

          {/* Side Panel — Transition Editor */}
          <div
            className={
              showTransitionPanel
                ? "w-80 border-l border-border-primary bg-surface-primary overflow-y-auto"
                : "hidden"
            }
          >
            <TransitionEditor
              transition={selectedTransition}
              states={states}
              onSave={handleCreateTransition}
              onUpdate={handleUpdateTransition}
              onDelete={handleDeleteTransition}
              onClose={() => {
                setSelectedTransitionId(null);
                setShowNewTransition(false);
              }}
            />
          </div>
        </div>
      </div>

      {/* State View Tab */}
      <div className={`flex-1 flex min-h-0 ${activeTab !== "states" ? "hidden" : ""}`}>
        <div
          className={
            noConfig && !sm.isLoading
              ? "flex-1 flex items-center justify-center text-text-muted"
              : "hidden"
          }
        >
          Select a configuration to view states
        </div>
        <div className={hasConfig ? "flex-1 flex" : "hidden"}>
          <StateViewPanel
            states={states}
            transitions={transitions}
            selectedStateId={selectedStateId}
            onSelectState={(id) => {
              setSelectedStateId(id);
              setSelectedTransitionId(null);
            }}
            elementThumbnails={elementThumbnails}
            captureScreenshots={captureScreenshots}
            onLoadScreenshotImage={loadScreenshotImage}
          />
        </div>
      </div>

      {/* Transitions Tab */}
      <div className={`flex-1 flex min-h-0 ${activeTab !== "transitions" ? "hidden" : ""}`}>
        <div
          className={
            noConfig && !sm.isLoading
              ? "flex-1 flex items-center justify-center text-text-muted"
              : "hidden"
          }
        >
          Select a configuration to view transitions
        </div>
        <div className={hasConfig ? "flex-1 flex" : "hidden"}>
          <TransitionsPanel
            states={states}
            transitions={transitions}
            onSelectTransition={(id) => {
              setSelectedTransitionId(id);
              setSelectedStateId(null);
            }}
          />
        </div>
      </div>

      {/* Pathfinding Tab */}
      <div className={`flex-1 overflow-y-auto ${activeTab !== "pathfinding" ? "hidden" : ""}`}>
        <div
          className={
            noConfig && !sm.isLoading
              ? "flex items-center justify-center h-full text-text-muted"
              : "hidden"
          }
        >
          Select a configuration to use pathfinding
        </div>
        <div className={hasConfig ? "" : "hidden"}>
          <PathfindingPanel
            states={states}
            transitions={transitions}
            onPathFound={handlePathFound}
          />
        </div>
      </div>

      {/* Exploration Results Tab — uses off-screen positioning instead of display:none
           so that UI Bridge SDK can still scan severity elements when tab is inactive */}
      <div
        className="flex-1 overflow-y-auto"
        style={
          activeTab !== "exploration"
            ? {
                position: "absolute",
                left: "-9999px",
                width: "1px",
                height: "1px",
                overflow: "hidden",
              }
            : undefined
        }
      >
        <StateExplorationResultsViewer />
      </div>

      {/* Export Tab */}
      <div className={`flex-1 overflow-y-auto ${activeTab !== "export" ? "hidden" : ""}`}>
        <div
          className={
            noConfig && !sm.isLoading
              ? "flex items-center justify-center h-full text-text-muted"
              : "hidden"
          }
        >
          Select a configuration to export
        </div>
        <div className={hasConfig ? "" : "hidden"}>
          <ExportPanel config={sm.activeConfig} showToast={showToast} />
        </div>
      </div>

      <ToastContainer toasts={toasts} onDismiss={dismissToast} />
    </div>
  );
}
