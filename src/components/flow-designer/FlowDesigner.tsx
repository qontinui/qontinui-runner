/**
 * FlowDesigner.tsx
 *
 * Main Flow Designer page with React Flow canvas.
 * Allows creating, editing, and running deterministic workflows.
 */

import { useState, useCallback, useEffect } from "react";
import {
  ReactFlow,
  MiniMap,
  Controls,
  Background,
  useNodesState,
  useEdgesState,
  addEdge,
  type Node,
  type Edge,
  type OnConnect,
  BackgroundVariant,
} from "@xyflow/react";
import "@xyflow/react/dist/style.css";
import {
  GitBranch,
  Save,
  Play,
  AlertTriangle,
  CheckCircle,
  Plus,
  FlaskConical,
  FolderOpen,
  X,
  Undo2,
  Redo2,
  Cloud,
  CloudOff,
  Loader2,
  Download,
  Upload,
  CheckCircle2,
  XCircle,
} from "lucide-react";
import { save, open } from "@tauri-apps/plugin-dialog";
import { writeTextFile, readTextFile } from "@tauri-apps/plugin-fs";
import { useFlowList, flowActions, useUndoRedo, useUndoRedoKeyboard, useAutosave } from "./useFlowDesigner";
import { FlowStepNode } from "./FlowStepNode";
import { FlowSidebar } from "./FlowSidebar";
import { FlowExecutionProvider, useFlowExecution } from "./FlowExecutionContext";
import type { Flow, FlowStep, StepType } from "../../types/flow";
import { STEP_TYPE_COLORS, getStepTypeName } from "../../types/flow";

// Custom node types - use 'as const' to fix typing issues with React Flow
const nodeTypes = {
  flowStep: FlowStepNode,
} as const;

// Helper to convert Flow to React Flow nodes and edges
function flowToNodesAndEdges(flow: Flow): { nodes: Node[]; edges: Edge[] } {
  const nodes: Node[] = [];
  const edges: Edge[] = [];
  const stepIds = Object.keys(flow.steps);

  // Calculate positions in a simple grid layout
  let y = 0;
  const processed = new Set<string>();
  const positionMap: Record<string, { x: number; y: number }> = {};

  // BFS to layout nodes
  const startStep = flow.start_step || stepIds[0];
  const queue = [startStep];

  while (queue.length > 0 || processed.size < stepIds.length) {
    const stepId = queue.shift();

    if (!stepId) {
      // Find an unprocessed node
      const unprocessed = stepIds.find((id) => !processed.has(id));
      if (unprocessed) queue.push(unprocessed);
      continue;
    }

    if (processed.has(stepId)) continue;
    processed.add(stepId);

    const step = flow.steps[stepId];
    if (!step) continue;

    // Position calculation
    const x = 200;
    positionMap[stepId] = { x, y };
    y += 150;

    // Create node
    nodes.push({
      id: stepId,
      type: "flowStep",
      position: positionMap[stepId],
      data: {
        step,
        isStart: stepId === flow.start_step,
        isSelected: false,
      },
    });

    // Get next steps and create edges
    const stepType = getStepTypeName(step.step_type);

    if (
      stepType === "conditional" &&
      "then_step" in step.step_type &&
      "else_step" in step.step_type
    ) {
      const thenStep = step.step_type.then_step;
      const elseStep = step.step_type.else_step;

      if (thenStep !== "end" && thenStep !== "fail") {
        edges.push({
          id: `${stepId}-then-${thenStep}`,
          source: stepId,
          target: thenStep,
          sourceHandle: "then",
          label: "Yes",
          style: { stroke: "#22c55e" },
        });
        if (!processed.has(thenStep)) queue.push(thenStep);
      }

      if (elseStep !== "end" && elseStep !== "fail") {
        edges.push({
          id: `${stepId}-else-${elseStep}`,
          source: stepId,
          target: elseStep,
          sourceHandle: "else",
          label: "No",
          style: { stroke: "#ef4444" },
        });
        if (!processed.has(elseStep)) queue.push(elseStep);
      }
    } else if ("next" in step.step_type) {
      const nextStep = step.step_type.next;
      if (nextStep && nextStep !== "end" && nextStep !== "fail") {
        edges.push({
          id: `${stepId}-${nextStep}`,
          source: stepId,
          target: nextStep,
        });
        if (!processed.has(nextStep)) queue.push(nextStep);
      }
    }
  }

  return { nodes, edges };
}

// Helper to create a new step of a given type
function createNewStep(type: string, id: string): FlowStep {
  const base = {
    id,
    name: `${type.charAt(0).toUpperCase() + type.slice(1)} Step`,
    description: null,
    timeout_secs: null,
    continue_on_error: false,
    retry_count: 0,
    input_mappings: {},
    condition: null,
  };

  let step_type: StepType;

  switch (type) {
    case "agent":
      step_type = {
        type: "agent",
        role: "assistant",
        prompt: "Describe the task",
        max_iterations: null,
        next: "end",
      };
      break;
    case "tool":
      step_type = { type: "tool", tool_id: "example_tool", inputs: {}, next: "end" };
      break;
    case "conditional":
      step_type = {
        type: "conditional",
        condition: { type: "expression", expr: "true" },
        then_step: "end",
        else_step: "end",
      };
      break;
    case "parallel":
      step_type = { type: "parallel", branches: [], merge_strategy: "wait_all", next: "end" };
      break;
    case "human_input":
      step_type = { type: "human_input", prompt: "Please provide input", options: [], next: "end" };
      break;
    case "transform":
      step_type = { type: "transform", expression: "data", output_key: "result", next: "end" };
      break;
    case "wait":
      step_type = { type: "wait", seconds: 5, next: "end" };
      break;
    case "loop":
      step_type = { type: "loop", over: "items", body_step: "end", next: "end" };
      break;
    case "end":
      step_type = { type: "end" };
      break;
    case "fail":
      step_type = { type: "fail", error: "Flow failed" };
      break;
    default:
      step_type = { type: "end" };
  }

  return { ...base, step_type };
}

// Create empty flow
function createEmptyFlow(): Flow {
  return {
    id: `flow-${Date.now()}`,
    name: "New Flow",
    description: null,
    steps: {},
    start_step: null,
    timeout_secs: null,
    inputs: [],
    outputs: [],
    tags: [],
    version: "1.0.0",
  };
}

/**
 * Execution Status Indicator Component
 * Shows current flow execution status in the toolbar
 */
function ExecutionStatusIndicator() {
  const { isExecuting, flowSuccess, flowError, stepStatuses, reset } = useFlowExecution();

  // Count completed and failed steps
  const completedCount = Array.from(stepStatuses.values()).filter(s => s.status === "completed").length;
  const failedCount = Array.from(stepStatuses.values()).filter(s => s.status === "failed").length;
  const totalProcessed = completedCount + failedCount;

  if (!isExecuting && flowSuccess === null && totalProcessed === 0) {
    return null; // No execution in progress or completed
  }

  return (
    <div className="flex items-center gap-2 px-3 py-1 bg-gray-800 rounded-lg">
      {isExecuting ? (
        <>
          <Loader2 className="w-4 h-4 text-emerald-400 animate-spin" />
          <span className="text-sm text-emerald-400">
            Running... ({completedCount} steps)
          </span>
        </>
      ) : flowSuccess === true ? (
        <>
          <CheckCircle2 className="w-4 h-4 text-emerald-400" />
          <span className="text-sm text-emerald-400">
            Complete ({completedCount} steps)
          </span>
          <button
            onClick={reset}
            className="ml-2 text-xs text-gray-400 hover:text-white"
            title="Clear execution status"
          >
            <X className="w-3 h-3" />
          </button>
        </>
      ) : flowSuccess === false ? (
        <>
          <XCircle className="w-4 h-4 text-red-400" />
          <span className="text-sm text-red-400">
            Failed {flowError ? `: ${flowError.slice(0, 30)}...` : ""}
          </span>
          <button
            onClick={reset}
            className="ml-2 text-xs text-gray-400 hover:text-white"
            title="Clear execution status"
          >
            <X className="w-3 h-3" />
          </button>
        </>
      ) : null}
    </div>
  );
}

function FlowDesignerInner() {
  const { flows, isLoading: _flowsLoading, refetch: refetchFlows } = useFlowList();
  const [_currentFlowId, setCurrentFlowId] = useState<string | null>(null);

  // Use undo/redo hook for flow state
  const {
    value: flow,
    setValue: setFlow,
    undo,
    redo,
    canUndo,
    canRedo,
    reset: resetFlow,
    undoCount,
    redoCount,
  } = useUndoRedo<Flow>(createEmptyFlow());

  // Set up keyboard shortcuts for undo/redo
  useUndoRedoKeyboard(undo, redo, canUndo, canRedo);

  const [selectedStepId, setSelectedStepId] = useState<string | null>(null);
  const [validationErrors, setValidationErrors] = useState<string[]>([]);
  const [isSaving, setIsSaving] = useState(false);
  const [showFlowList, setShowFlowList] = useState(false);
  const [actionInProgress, setActionInProgress] = useState(false);
  const [autosaveEnabled, setAutosaveEnabled] = useState(true);

  // Autosave functionality
  const {
    isPending: autosavePending,
    isSaving: autosaveInProgress,
    lastSaved,
    hasUnsavedChanges,
    markSaved,
  } = useAutosave(
    flow,
    async (flowToSave) => {
      // Only autosave if flow has been saved before (has a valid ID and steps)
      if (flowToSave.id && Object.keys(flowToSave.steps).length > 0) {
        const errors = await flowActions.validateFlow(flowToSave);
        // Only autosave valid flows
        if (errors.length === 0) {
          await flowActions.saveFlow(flowToSave);
          await refetchFlows();
        }
      }
    },
    {
      enabled: autosaveEnabled && _currentFlowId !== null,
      delayMs: 5000,
      onSaveStart: () => console.log("Autosaving..."),
      onSaveSuccess: () => console.log("Autosaved successfully"),
      onSaveError: (err) => console.error("Autosave failed:", err),
    }
  );

  // React Flow state
  const [nodes, setNodes, onNodesChange] = useNodesState([]);
  const [edges, setEdges, onEdgesChange] = useEdgesState([]);

  // Convert flow to nodes/edges when flow changes
  useEffect(() => {
    const { nodes: newNodes, edges: newEdges } = flowToNodesAndEdges(flow);
    setNodes(newNodes);
    setEdges(newEdges);
  }, [flow, setNodes, setEdges]);

  // Update node selection
  useEffect(() => {
    setNodes((nds) =>
      nds.map((node) => ({
        ...node,
        data: {
          ...node.data,
          isSelected: node.id === selectedStepId,
        },
      })),
    );
  }, [selectedStepId, setNodes]);

  const selectedStep = selectedStepId ? flow.steps[selectedStepId] : null;

  const onConnect: OnConnect = useCallback(
    (params) => {
      const { source, target, sourceHandle } = params;
      if (!source || !target) return;

      // Update the flow step's next reference
      setFlow((f) => {
        const step = f.steps[source];
        if (!step) return f;

        let updatedStepType: StepType;

        if (step.step_type.type === "conditional") {
          const cond = step.step_type;
          if (sourceHandle === "then") {
            updatedStepType = { ...cond, then_step: target };
          } else {
            updatedStepType = { ...cond, else_step: target };
          }
        } else if ("next" in step.step_type) {
          updatedStepType = { ...step.step_type, next: target };
        } else {
          // end/fail types don't have next, no update needed
          return f;
        }

        return {
          ...f,
          steps: {
            ...f.steps,
            [source]: { ...step, step_type: updatedStepType },
          },
        };
      });

      setEdges((eds) => addEdge(params, eds));
    },
    [setEdges, setFlow],
  );

  const handleNodeClick = useCallback((event: React.MouseEvent, node: Node) => {
    setSelectedStepId(node.id);
  }, []);

  const handlePaneClick = useCallback(() => {
    setSelectedStepId(null);
  }, []);

  const handleAddStep = useCallback((type: string) => {
    const id = `${type}-${Date.now()}`;
    const newStep = createNewStep(type, id);

    setFlow((f) => {
      const updated = {
        ...f,
        steps: { ...f.steps, [id]: newStep },
        start_step: f.start_step || id,
      };
      return updated;
    });

    setSelectedStepId(id);
  }, [setFlow]);

  const handleUpdateStep = useCallback((updatedStep: FlowStep) => {
    setFlow((f) => ({
      ...f,
      steps: { ...f.steps, [updatedStep.id]: updatedStep },
    }));
  }, [setFlow]);

  const handleDeleteStep = useCallback((stepId: string) => {
    setFlow((f) => {
      const { [stepId]: _, ...remainingSteps } = f.steps;
      return {
        ...f,
        steps: remainingSteps,
        start_step: f.start_step === stepId ? Object.keys(remainingSteps)[0] || null : f.start_step,
      };
    });
    setSelectedStepId(null);
  }, [setFlow]);

  const handleSave = async () => {
    setIsSaving(true);
    try {
      const errors = await flowActions.validateFlow(flow);
      setValidationErrors(errors);

      if (errors.length === 0) {
        await flowActions.saveFlow(flow);
        setCurrentFlowId(flow.id);
        markSaved(); // Mark as saved to reset autosave timer
        await refetchFlows();
      }
    } catch (err) {
      console.error("Save failed:", err);
    } finally {
      setIsSaving(false);
    }
  };

  // Format last saved time
  const formatLastSaved = (date: Date | null): string => {
    if (!date) return "";
    const now = new Date();
    const diff = now.getTime() - date.getTime();
    if (diff < 60000) return "just now";
    if (diff < 3600000) return `${Math.floor(diff / 60000)}m ago`;
    return date.toLocaleTimeString();
  };

  const handleLoadFlow = async (flowId: string) => {
    try {
      const loadedFlow = await (
        await import("../../services/flow-service")
      ).flowService.getFlow(flowId);
      if (loadedFlow) {
        // Reset history when loading a new flow
        resetFlow(loadedFlow);
        setCurrentFlowId(flowId);
        setSelectedStepId(null);
        setValidationErrors([]);
      }
    } catch (err) {
      console.error("Load failed:", err);
    }
    setShowFlowList(false);
  };

  const handleNewFlow = () => {
    // Reset history when creating a new flow
    resetFlow(createEmptyFlow());
    setCurrentFlowId(null);
    setSelectedStepId(null);
    setValidationErrors([]);
  };

  const handleAddSampleFlow = async () => {
    setActionInProgress(true);
    try {
      await flowActions.addSampleFlow();
      await refetchFlows();
    } finally {
      setActionInProgress(false);
    }
  };

  const handleRun = async () => {
    if (!flow.id) return;
    try {
      const instanceId = await flowActions.startExecution(flow.id);
      alert(`Flow execution started: ${instanceId}`);
    } catch (err) {
      console.error("Run failed:", err);
      alert("Failed to start flow execution");
    }
  };

  const handleExport = async () => {
    try {
      // Get the flow service
      const { flowService } = await import("../../services/flow-service");

      // Ask user to pick export format
      const filePath = await save({
        defaultPath: `${flow.name.replace(/[^a-zA-Z0-9]/g, "_")}.json`,
        filters: [
          { name: "JSON", extensions: ["json"] },
          { name: "YAML", extensions: ["yaml", "yml"] },
        ],
      });

      if (!filePath) return;

      // Determine format from extension
      const isYaml = filePath.toLowerCase().endsWith(".yaml") || filePath.toLowerCase().endsWith(".yml");

      // Export based on format
      let content: string;
      if (flow.id && _currentFlowId) {
        // Export saved flow from database
        content = isYaml
          ? await flowService.exportFlowYaml(flow.id)
          : await flowService.exportFlowJson(flow.id);
      } else {
        // Export current unsaved flow
        const exportData = {
          format_version: "1.0.0",
          exported_at: new Date().toISOString(),
          flow: flow,
        };
        content = isYaml
          ? JSON.stringify(exportData, null, 2) // Fallback to JSON for unsaved
          : JSON.stringify(exportData, null, 2);
      }

      // Write to file
      await writeTextFile(filePath, content);
      console.log(`Flow exported to ${filePath}`);
    } catch (err) {
      console.error("Export failed:", err);
      alert("Failed to export flow");
    }
  };

  const handleImport = async () => {
    try {
      const { flowService } = await import("../../services/flow-service");

      // Ask user to pick a file
      const filePath = await open({
        multiple: false,
        filters: [
          { name: "Flow Files", extensions: ["json", "yaml", "yml"] },
        ],
      });

      if (!filePath || Array.isArray(filePath)) return;

      // Read the file
      const content = await readTextFile(filePath);

      // Determine format from extension
      const isYaml = filePath.toLowerCase().endsWith(".yaml") || filePath.toLowerCase().endsWith(".yml");

      // Import based on format
      const importedFlow = isYaml
        ? await flowService.importFlowYaml(content, true)
        : await flowService.importFlowJson(content, true);

      // Reset history with the imported flow
      resetFlow(importedFlow);
      setCurrentFlowId(importedFlow.id);
      setSelectedStepId(null);
      setValidationErrors([]);
      await refetchFlows();

      console.log(`Flow imported: ${importedFlow.name}`);
    } catch (err) {
      console.error("Import failed:", err);
      alert("Failed to import flow. Make sure the file is a valid flow export.");
    }
  };

  return (
    <div className="h-full flex flex-col">
      {/* Toolbar */}
      <div className="flex items-center justify-between p-3 border-b border-gray-700 bg-gray-900">
        <div className="flex items-center gap-4">
          <div className="flex items-center gap-2">
            <GitBranch className="w-6 h-6 text-green-500" />
            <h1 className="text-xl font-bold text-white">Flow Designer</h1>
          </div>

          {/* Flow Name Input */}
          <input
            type="text"
            value={flow.name}
            onChange={(e) => setFlow((f) => ({ ...f, name: e.target.value }))}
            className="bg-gray-800 border border-gray-700 rounded px-3 py-1.5 text-sm text-white focus:outline-none focus:ring-2 focus:ring-green-500 w-48"
          />

          {/* Validation Status */}
          {validationErrors.length > 0 ? (
            <div className="flex items-center gap-2 text-red-400">
              <AlertTriangle className="w-4 h-4" />
              <span className="text-sm">{validationErrors.length} errors</span>
            </div>
          ) : Object.keys(flow.steps).length > 0 ? (
            <div className="flex items-center gap-2 text-green-400">
              <CheckCircle className="w-4 h-4" />
              <span className="text-sm">Valid</span>
            </div>
          ) : null}

          {/* Execution Status Indicator */}
          <ExecutionStatusIndicator />
        </div>

        <div className="flex items-center gap-2">
          {/* Undo/Redo buttons */}
          <div className="flex items-center gap-1 mr-2">
            <button
              onClick={undo}
              disabled={!canUndo}
              title={`Undo (Ctrl+Z)${canUndo ? ` - ${undoCount} step${undoCount !== 1 ? "s" : ""}` : ""}`}
              className="p-1.5 text-gray-400 hover:text-white hover:bg-gray-700 rounded disabled:opacity-30 disabled:cursor-not-allowed transition-colors"
            >
              <Undo2 className="w-4 h-4" />
            </button>
            <button
              onClick={redo}
              disabled={!canRedo}
              title={`Redo (Ctrl+Shift+Z)${canRedo ? ` - ${redoCount} step${redoCount !== 1 ? "s" : ""}` : ""}`}
              className="p-1.5 text-gray-400 hover:text-white hover:bg-gray-700 rounded disabled:opacity-30 disabled:cursor-not-allowed transition-colors"
            >
              <Redo2 className="w-4 h-4" />
            </button>
          </div>

          <button
            onClick={handleNewFlow}
            className="flex items-center gap-2 px-3 py-1.5 bg-gray-700 text-white rounded hover:bg-gray-600 transition-colors"
          >
            <Plus className="w-4 h-4" />
            New
          </button>

          <div className="relative">
            <button
              onClick={() => setShowFlowList(!showFlowList)}
              className="flex items-center gap-2 px-3 py-1.5 bg-gray-700 text-white rounded hover:bg-gray-600 transition-colors"
            >
              <FolderOpen className="w-4 h-4" />
              Open
            </button>

            {showFlowList && (
              <div className="absolute right-0 top-full mt-1 w-64 bg-gray-800 border border-gray-700 rounded-lg shadow-lg z-50">
                <div className="flex items-center justify-between px-3 py-2 border-b border-gray-700">
                  <span className="text-sm font-medium text-gray-300">Saved Flows</span>
                  <button onClick={() => setShowFlowList(false)}>
                    <X className="w-4 h-4 text-gray-400" />
                  </button>
                </div>
                <div className="max-h-64 overflow-auto">
                  {flows.length === 0 ? (
                    <div className="px-3 py-4 text-center text-gray-500 text-sm">
                      No saved flows
                    </div>
                  ) : (
                    flows.map((f) => (
                      <button
                        key={f.id}
                        onClick={() => handleLoadFlow(f.id)}
                        className="w-full px-3 py-2 text-left hover:bg-gray-700 transition-colors"
                      >
                        <div className="font-medium text-white text-sm">{f.name}</div>
                        <div className="text-xs text-gray-400">{f.step_count} steps</div>
                      </button>
                    ))
                  )}
                </div>
                <div className="border-t border-gray-700 p-2">
                  <button
                    onClick={handleAddSampleFlow}
                    disabled={actionInProgress}
                    className="w-full flex items-center justify-center gap-2 px-3 py-1.5 bg-gray-700 text-gray-300 rounded hover:bg-gray-600 transition-colors text-sm"
                  >
                    <FlaskConical className="w-4 h-4" />
                    Add Sample Flow
                  </button>
                </div>
              </div>
            )}
          </div>

          {/* Import/Export buttons */}
          <button
            onClick={handleImport}
            title="Import flow from file"
            className="flex items-center gap-2 px-3 py-1.5 bg-gray-700 text-white rounded hover:bg-gray-600 transition-colors"
          >
            <Upload className="w-4 h-4" />
            Import
          </button>

          <button
            onClick={handleExport}
            disabled={Object.keys(flow.steps).length === 0}
            title="Export flow to file"
            className="flex items-center gap-2 px-3 py-1.5 bg-gray-700 text-white rounded hover:bg-gray-600 transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
          >
            <Download className="w-4 h-4" />
            Export
          </button>

          {/* Save status indicator */}
          <div className="flex items-center gap-2 text-xs text-gray-400 mr-2">
            {autosaveInProgress ? (
              <>
                <Loader2 className="w-3 h-3 animate-spin" />
                <span>Saving...</span>
              </>
            ) : autosavePending ? (
              <>
                <Cloud className="w-3 h-3 text-yellow-400" />
                <span>Unsaved</span>
              </>
            ) : lastSaved ? (
              <>
                <Cloud className="w-3 h-3 text-green-400" />
                <span>Saved {formatLastSaved(lastSaved)}</span>
              </>
            ) : hasUnsavedChanges ? (
              <>
                <Cloud className="w-3 h-3 text-yellow-400" />
                <span>Unsaved</span>
              </>
            ) : null}

            {/* Autosave toggle */}
            <button
              onClick={() => setAutosaveEnabled(!autosaveEnabled)}
              title={autosaveEnabled ? "Disable autosave" : "Enable autosave"}
              className={`p-1 rounded transition-colors ${
                autosaveEnabled
                  ? "text-green-400 hover:text-green-300"
                  : "text-gray-500 hover:text-gray-400"
              }`}
            >
              {autosaveEnabled ? (
                <Cloud className="w-3 h-3" />
              ) : (
                <CloudOff className="w-3 h-3" />
              )}
            </button>
          </div>

          <button
            onClick={handleSave}
            disabled={isSaving || autosaveInProgress}
            className="flex items-center gap-2 px-3 py-1.5 bg-blue-600 text-white rounded hover:bg-blue-500 transition-colors disabled:opacity-50"
          >
            <Save className="w-4 h-4" />
            {isSaving ? "Saving..." : "Save"}
          </button>

          <button
            onClick={handleRun}
            disabled={Object.keys(flow.steps).length === 0 || validationErrors.length > 0}
            className="flex items-center gap-2 px-3 py-1.5 bg-green-600 text-white rounded hover:bg-green-500 transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
          >
            <Play className="w-4 h-4" />
            Run
          </button>
        </div>
      </div>

      {/* Main Content */}
      <div className="flex-1 flex overflow-hidden">
        {/* Canvas */}
        <div className="flex-1 bg-gray-950">
          <ReactFlow
            nodes={nodes}
            edges={edges}
            onNodesChange={onNodesChange}
            onEdgesChange={onEdgesChange}
            onConnect={onConnect}
            onNodeClick={handleNodeClick}
            onPaneClick={handlePaneClick}
            nodeTypes={nodeTypes}
            fitView
            fitViewOptions={{ padding: 0.2 }}
            defaultEdgeOptions={{
              style: { strokeWidth: 2, stroke: "#6b7280" },
              animated: true,
            }}
          >
            <Controls className="!bg-gray-800 !border-gray-700" />
            <MiniMap
              className="!bg-gray-800"
              nodeColor={(node) => {
                const data = node.data as { step?: { step_type?: { type?: string } } } | undefined;
                const stepType = data?.step?.step_type?.type;
                return stepType ? STEP_TYPE_COLORS[stepType] || "#6b7280" : "#6b7280";
              }}
            />
            <Background variant={BackgroundVariant.Dots} gap={16} size={1} color="#374151" />
          </ReactFlow>
        </div>

        {/* Sidebar */}
        <FlowSidebar
          selectedStep={selectedStep}
          onAddStep={handleAddStep}
          onUpdateStep={handleUpdateStep}
          onDeleteStep={handleDeleteStep}
        />
      </div>
    </div>
  );
}

/**
 * FlowDesigner - Main exported component
 * Wraps the inner component with the FlowExecutionProvider for execution state tracking.
 */
export function FlowDesigner() {
  return (
    <FlowExecutionProvider>
      <FlowDesignerInner />
    </FlowExecutionProvider>
  );
}
