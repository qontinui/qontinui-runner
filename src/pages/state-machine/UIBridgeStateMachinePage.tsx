/**
 * UIBridgeStateMachinePage
 *
 * Main page for building state machines from UI Bridge SDK apps.
 * Replaces the old visual GUI automation StateMachineBuilderPage.
 *
 * Tabs: Discovery, Graph Editor, State View, Transitions, Pathfinding, Export
 */

import { useState, useCallback, useEffect, useRef, type ComponentType } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useUIElement, useUIComponent } from "@qontinui/ui-bridge";
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
  Workflow,
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
  DiagramTab,
} from "@qontinui/workflow-ui/state-machine";
import type { CaptureScreenshotMeta } from "@qontinui/workflow-ui/state-machine";

import type { PermittedTrigger, BlockedTrigger } from "@qontinui/workflow-ui";

import { useStateMachineConfig } from "@/hooks/useStateMachineConfig";
import { useElementDrag } from "@/hooks/useElementDrag";
import { useUIBridgeDiscovery } from "@/hooks/useUIBridgeDiscovery";
import { useToast } from "@/hooks/useToast";
import { UIBridgeStateGraph } from "./UIBridgeStateGraph";
import { UIBridgeDiscoveryPanel } from "./UIBridgeDiscoveryPanel";
import { ExportPanel } from "./ExportPanel";
import { ProvenanceBadge } from "./ProvenanceBadge";
import { ObservationsPanel } from "./ObservationsPanel";
import { getApiBase } from "@/lib/runner-api";
import type { StateProvenance, StateProvenanceMeta } from "@/lib/compile-state-machine";
import { ToastContainer } from "@/components/ToastContainer";
import { StateExplorationResultsViewer } from "@/components/state-explorer/StateExplorationResultsViewer";
import { createRunnerDataAdapter } from "@/lib/workflow-builder/runner-data-adapter";

// Singleton adapter reused across introspection fetches.
const dataAdapter = createRunnerDataAdapter();

const TABS = [
  { value: "discovery", label: "Discovery", icon: Search },
  { value: "graph", label: "Graph Editor", icon: GitBranch },
  { value: "states", label: "State View", icon: Layers },
  { value: "transitions", label: "Transitions", icon: ArrowRightLeft },
  { value: "diagram", label: "Diagram", icon: Workflow },
  { value: "pathfinding", label: "Pathfinding", icon: Route },
  { value: "exploration", label: "Exploration Results", icon: Network },
  { value: "export", label: "Export", icon: Download },
] as const;

type TabValue = (typeof TABS)[number]["value"];

/** Tab button with UI Bridge element registration. */
function SMTabButton({
  value,
  label,
  icon: Icon,
  active,
  onClick,
}: {
  value: string;
  label: string;
  icon: ComponentType<{ className?: string }>;
  active: boolean;
  onClick: () => void;
}) {
  const { ref } = useUIElement({
    id: `sm-tab-${value}`,
    type: "button",
    label: `${label} tab`,
    actions: ["click"],
  });
  return (
    <button
      ref={ref}
      onClick={onClick}
      aria-label={
        value === "exploration" ? "exploration results navigation link or tab" : undefined
      }
      className={`inline-flex items-center justify-center gap-1.5 rounded-md px-2 py-1 text-sm font-medium whitespace-nowrap transition-[color,box-shadow] ${
        active
          ? "bg-background shadow-xs text-foreground"
          : "text-muted-foreground hover:text-foreground"
      }`}
    >
      <Icon className="size-3.5" />
      {label}
    </button>
  );
}

export function UIBridgeStateMachinePage() {
  const sm = useStateMachineConfig();
  const { toasts, showToast, dismissToast } = useToast();
  const discovery = useUIBridgeDiscovery(sm, showToast);

  useUIComponent({
    id: "state-machine-page",
    name: "State Machine Builder",
    description: "Build and explore state machines from UI Bridge SDK apps",
  });

  const [activeTab, setActiveTab] = useState<TabValue>(() => {
    // Default to exploration tab when navigated via API (e.g., test verification)
    const params = new URLSearchParams(window.location.search);
    return (params.get("tab") as TabValue) || "discovery";
  });
  const [selectedStateId, setSelectedStateId] = useState<string | null>(null);
  const [selectedTransitionId, setSelectedTransitionId] = useState<string | null>(null);
  const [showNewTransition, setShowNewTransition] = useState(false);
  const [highlightedPath, setHighlightedPath] = useState<PathfindingResult["steps"] | undefined>();
  // Thumbnails are fetched from the database keyed by configId. Storing the
  // configId alongside the thumbs lets us reset-on-config-change as a pure
  // derivation (see `elementThumbnails` below) instead of a useEffect with a
  // sync setState in its body.
  //
  // `fetchedConfigId: null` on initial mount signals the localStorage fallback
  // — show those thumbs as a hint until the fresh fetch lands, regardless of
  // the current active config.
  const [fetchedThumbs, setFetchedThumbs] = useState<{
    configId: string | null;
    thumbs: Record<string, string> | undefined;
  }>(() => {
    try {
      const stored = localStorage.getItem("qontinui-runner-sm-thumbnails");
      if (stored) {
        const parsed = JSON.parse(stored);
        if (parsed && Object.keys(parsed).length > 0) {
          return { configId: null, thumbs: parsed };
        }
      }
    } catch {
      /* */
    }
    return { configId: null, thumbs: undefined };
  });
  const activeConfigId = sm.activeConfig?.id;
  useEffect(() => {
    if (!activeConfigId) return;
    let isCancelled = false;
    invoke<Record<string, string>>("sm_get_thumbnails", { configId: activeConfigId })
      .then((thumbs) => {
        if (isCancelled) return;
        setFetchedThumbs({
          configId: activeConfigId,
          thumbs: Object.keys(thumbs).length > 0 ? thumbs : undefined,
        });
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

  // Capture screenshots for screenshot state view. Same config-keyed pattern
  // as thumbnails above so the reset-on-config-change is a pure derivation.
  const [fetchedScreenshots, setFetchedScreenshots] = useState<{
    configId: string | null;
    screenshots: CaptureScreenshotMeta[] | undefined;
  }>({ configId: null, screenshots: undefined });
  const screenshotImageCache = useRef<Map<string, string>>(new Map());
  useEffect(() => {
    // Invalidate the in-memory image cache whenever we switch configs — the
    // cached data-URLs belong to the previous config's screenshots.
    screenshotImageCache.current.clear();
    if (!activeConfigId) return;
    let isCancelled = false;
    invoke<CaptureScreenshotMeta[]>("sm_get_capture_screenshots", { configId: activeConfigId })
      .then((screenshots) => {
        if (isCancelled) return;
        setFetchedScreenshots({
          configId: activeConfigId,
          screenshots: screenshots.length > 0 ? screenshots : undefined,
        });
      })
      .catch(() => {
        // Command may not be available in older runner builds
      });
    return () => {
      isCancelled = true;
    };
  }, [activeConfigId]);

  const captureScreenshots =
    fetchedScreenshots.configId === activeConfigId ? fetchedScreenshots.screenshots : undefined;

  // Load screenshot image on demand (returns data URL, cached in component)
  const loadScreenshotImage = useCallback(async (screenshotId: string): Promise<string> => {
    const cached = screenshotImageCache.current.get(screenshotId);
    if (cached) return cached;
    const dataUrl = await invoke<string>("sm_get_capture_screenshot_image", { screenshotId });
    screenshotImageCache.current.set(screenshotId, dataUrl);
    return dataUrl;
  }, []);

  // Live thumbnails from the discovery co-occurrence stream take precedence
  // over the fetched-from-DB ones during exploration. When `configId` is null
  // on fetchedThumbs, it's the localStorage-fallback at mount — show it as a
  // hint regardless of the active config, until the fresh fetch lands.
  const coocThumbs = discovery.cooccurrenceData?.elementThumbnails;
  const elementThumbnails =
    coocThumbs && Object.keys(coocThumbs).length > 0
      ? coocThumbs
      : fetchedThumbs.configId === null || fetchedThumbs.configId === activeConfigId
        ? fetchedThumbs.thumbs
        : undefined;

  // Derived state
  const states = sm.activeConfig?.states ?? [];
  const transitions = sm.activeConfig?.transitions ?? [];

  // ======================================================================
  // Trigger introspection: fetch permitted/blocked when on Transitions tab
  // ======================================================================
  const [permittedTriggers, setPermittedTriggers] = useState<PermittedTrigger[]>([]);
  const [blockedTriggers, setBlockedTriggers] = useState<BlockedTrigger[]>([]);
  // Hypothetical active state override. Empty means "use runtime's current
  // active states". Future work: surface a picker; Phase 2 just wires the
  // data flow end-to-end.
  const [introspectionActiveIds] = useState<string[]>([]);
  const selectedState =
    states.find((s) => s.id === selectedStateId || s.state_id === selectedStateId) ?? null;
  const selectedTransition = transitions.find((t) => t.id === selectedTransitionId) ?? null;

  const showStatePanel = selectedState && !showNewTransition;
  const showTransitionPanel = (selectedTransition || showNewTransition) && !selectedState;
  const noConfig = !sm.activeConfig;
  const hasConfig = !!sm.activeConfig;

  // ======================================================================
  // Observation-pipeline provenance — read compiled state's provenance +
  // meta from extra_metadata (populated by save-compiled after the TS
  // compiler assigned them). Absent fields fall back to ai-generated in the
  // ProvenanceBadge so older compiled SMs don't require a re-compile to
  // render cleanly.
  // ======================================================================
  const selectedProvenance: StateProvenance | undefined = (() => {
    const raw = selectedState?.extra_metadata?.provenance;
    if (raw === "observed" || raw === "ai-generated" || raw === "ai-fallback") {
      return raw;
    }
    return undefined;
  })();
  const selectedProvenanceMeta: StateProvenanceMeta | undefined = (() => {
    const raw = selectedState?.extra_metadata?.provenanceMeta;
    if (raw && typeof raw === "object") {
      return raw as StateProvenanceMeta;
    }
    return undefined;
  })();

  // `StateMachineConfig` currently has no `spec_id` column (the plan notes
  // this — add it in a later migration). Until then, pass undefined to
  // ObservationsPanel, which maps to "default (NULL) spec_id" on the
  // backend — matching the drift detector's own NULL-scope behaviour.
  const activeSpecId: string | undefined = undefined;

  const handleInvalidateState = useCallback(async () => {
    // Invalidation is spec-scoped per the plan; without a spec_id we don't
    // fire — the endpoint rejects filter-less calls to prevent nuking the
    // entire observations table.
    if (!activeSpecId) {
      showToast("Invalidation needs a spec_id on this SM config. Add one and re-compile.", "error");
      return;
    }
    try {
      const resp = await fetch(`${getApiBase()}/co-occurrence/invalidate`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          spec_id: activeSpecId,
          reason: `manual invalidation of state '${selectedState?.name ?? selectedState?.state_id}' from State Machine page`,
          invalidated_by: "state-machine-page",
        }),
      });
      const json = (await resp.json()) as {
        success: boolean;
        data?: { count: number; undo_token: string };
        error?: string;
      };
      if (!resp.ok || !json.success || !json.data) {
        throw new Error(json.error ?? `HTTP ${resp.status}`);
      }
      showToast(
        `Invalidated ${json.data.count} observation(s) for this spec. Undo (valid for 24 h) — token: ${json.data.undo_token.slice(0, 8)}…`,
        "success",
      );
    } catch (err) {
      showToast(
        `Invalidation failed: ${err instanceof Error ? err.message : String(err)}`,
        "error",
      );
    }
  }, [activeSpecId, selectedState, showToast]);

  // Refetch permitted/blocked triggers when the Transitions tab opens or
  // the active config changes. When no config is loaded, clear both lists.
  useEffect(() => {
    if (activeTab !== "transitions" || !hasConfig) {
      return;
    }
    let cancelled = false;
    (async () => {
      try {
        const [permitted, blocked] = await Promise.all([
          dataAdapter.getPermittedTriggers
            ? dataAdapter.getPermittedTriggers(introspectionActiveIds)
            : Promise.resolve([] as PermittedTrigger[]),
          dataAdapter.getBlockedTriggers
            ? dataAdapter.getBlockedTriggers(introspectionActiveIds)
            : Promise.resolve([] as BlockedTrigger[]),
        ]);
        if (cancelled) return;
        setPermittedTriggers(permitted);
        setBlockedTriggers(blocked);
      } catch {
        if (!cancelled) {
          setPermittedTriggers([]);
          setBlockedTriggers([]);
        }
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [activeTab, hasConfig, activeConfigId, introspectionActiveIds]);

  // ======================================================================
  // Mermaid diagram: fetch when the Diagram tab is selected or the active
  // state set / config changes.
  // ======================================================================
  const [diagramSource, setDiagramSource] = useState<string>("");
  const [diagramLoading, setDiagramLoading] = useState(false);
  const [diagramRefreshNonce, setDiagramRefreshNonce] = useState(0);

  // Mermaid can't render state graphs of arbitrary size — above ~50 nodes
  // its layout engine bogs down or crashes WebView2. Skip the fetch entirely
  // past this threshold and show a message via DiagramTab.unavailableReason.
  const DIAGRAM_MAX_NODES = 50;
  const diagramUnavailableReason =
    states.length > DIAGRAM_MAX_NODES
      ? `Diagram is too large to render (${states.length} states). Mermaid chokes above ~${DIAGRAM_MAX_NODES} nodes — use the State View or Graph Editor tabs instead.`
      : undefined;

  useEffect(() => {
    if (activeTab !== "diagram" || !hasConfig) return;
    if (diagramUnavailableReason) return;
    if (!dataAdapter.getMermaidDiagram) return;
    let cancelled = false;

    (async () => {
      setDiagramLoading(true);
      try {
        const src = await dataAdapter.getMermaidDiagram!(introspectionActiveIds);
        if (cancelled) return;
        setDiagramSource(src);
      } catch {
        if (!cancelled) setDiagramSource("");
      } finally {
        if (!cancelled) setDiagramLoading(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [
    activeTab,
    hasConfig,
    activeConfigId,
    introspectionActiveIds,
    diagramRefreshNonce,
    diagramUnavailableReason,
  ]);

  const handleRefreshDiagram = useCallback(() => {
    setDiagramRefreshNonce((n) => n + 1);
  }, []);

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
        // eslint-disable-next-line @typescript-eslint/no-explicit-any
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

      {/* Observations panel — drift score, re-derive, invalidation log */}
      {hasConfig && <ObservationsPanel specId={activeSpecId} showToast={showToast} />}

      {/* Tab Navigation */}
      <div className="border-b border-border-primary bg-surface-primary px-6">
        <div className="inline-flex h-9 w-fit items-center justify-center rounded-lg p-[3px]">
          {TABS.map(({ value, label, icon }) => (
            <SMTabButton
              key={value}
              value={value}
              label={label}
              icon={icon}
              active={activeTab === value}
              onClick={() => setActiveTab(value)}
            />
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
              // Hidden-tab gate: ReactFlow's onSelectionChange fires when its
              // tab becomes hidden, which would clobber selections made in
              // other tabs. Only accept graph-origin selection changes while
              // the graph is actually the active tab.
              onSelectState={(id) => {
                if (activeTab !== "graph") return;
                setSelectedStateId(id);
                setSelectedTransitionId(null);
                setShowNewTransition(false);
              }}
              onSelectTransition={(id) => {
                if (activeTab !== "graph") return;
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
              <>
                {/* Provenance header — observation-pipeline signal above the
                    stock StateDetailPanel so user sees the state's lifecycle
                    status + can invalidate its observations with one click. */}
                <div className="flex items-center justify-between gap-2 px-4 pt-4 pb-2 border-b border-border-secondary">
                  <ProvenanceBadge
                    provenance={selectedProvenance}
                    provenanceMeta={selectedProvenanceMeta}
                  />
                  <button
                    onClick={() => void handleInvalidateState()}
                    disabled={!activeSpecId}
                    className="text-[10px] px-2 py-0.5 rounded border border-border-primary text-text-secondary hover:text-text-primary hover:bg-surface-secondary disabled:opacity-50"
                    title={
                      activeSpecId
                        ? "Soft-delete all observations for this spec (24 h undo)"
                        : "Spec has no spec_id — invalidation disabled"
                    }
                  >
                    Invalidate observations
                  </button>
                </div>
                <StateDetailPanel
                  state={selectedState}
                  onSave={handleSaveState}
                  onClose={() => setSelectedStateId(null)}
                />
              </>
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
            selectedTransitionId={selectedTransitionId}
            onSelectTransition={(id) => {
              setSelectedTransitionId(id);
              setSelectedStateId(null);
            }}
            activeStateIds={introspectionActiveIds}
            permittedTriggers={permittedTriggers}
            blockedTriggers={blockedTriggers}
          />
        </div>
      </div>

      {/* Diagram Tab */}
      <div className={`flex-1 flex min-h-0 ${activeTab !== "diagram" ? "hidden" : ""}`}>
        <div
          className={
            noConfig && !sm.isLoading
              ? "flex-1 flex items-center justify-center text-text-muted"
              : "hidden"
          }
        >
          Select a configuration to view the diagram
        </div>
        <div className={hasConfig ? "flex-1 flex" : "hidden"}>
          <DiagramTab
            activeStateIds={introspectionActiveIds}
            diagramSource={diagramSource}
            isLoading={diagramLoading}
            onRefresh={handleRefreshDiagram}
            unavailableReason={diagramUnavailableReason}
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
