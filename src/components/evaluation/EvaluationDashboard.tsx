/**
 * EvaluationDashboard
 *
 * Main page for managing evaluation datasets, experiments, and viewing results.
 * Two-panel layout: datasets/detail on the left, experiments/results on the right.
 * Lazy-loaded as a tab in the runner app.
 */

import { useState, lazy, Suspense } from "react";
import { FlaskConical, Plus, RefreshCw, AlertTriangle, Play } from "lucide-react";
import { Button } from "@/components/ui/Button";
import { Card } from "@/components/ui/Card";
import { useEvaluationDatasets, useDatasetItems } from "@/hooks/useEvaluationDatasets";
import { useEvaluationExperiments, useExperimentResults } from "@/hooks/useEvaluationExperiments";
import { DatasetList } from "./DatasetList";
import { DatasetDetail } from "./DatasetDetail";
import { ExperimentList } from "./ExperimentList";
import { ExperimentResults } from "./ExperimentResults";

const LiveEvaluationPanel = lazy(() => import("./LiveEvaluationPanel"));

type RightPanelTab = "experiments" | "evaluate";

export default function EvaluationDashboard() {
  const [selectedDatasetId, setSelectedDatasetId] = useState<string | null>(null);
  const [selectedExperimentId, setSelectedExperimentId] = useState<string | null>(null);
  const [showCreateDataset, setShowCreateDataset] = useState(false);
  const [newDatasetName, setNewDatasetName] = useState("");
  const [newDatasetDescription, setNewDatasetDescription] = useState("");
  const [rightPanelTab, setRightPanelTab] = useState<RightPanelTab>("experiments");
  const [evalPrefillInput, setEvalPrefillInput] = useState("");
  const [evalPrefillOutput, setEvalPrefillOutput] = useState("");

  // Data hooks
  const {
    datasets,
    isLoading: datasetsLoading,
    error: datasetsError,
    refresh: refreshDatasets,
    createDataset,
    deleteDataset,
  } = useEvaluationDatasets();

  const {
    items,
    isLoading: itemsLoading,
    addItems,
    deleteItem,
  } = useDatasetItems(selectedDatasetId);

  const {
    experiments,
    isLoading: experimentsLoading,
    createExperiment,
  } = useEvaluationExperiments(selectedDatasetId);

  const {
    results,
    summary,
    isLoading: resultsLoading,
    error: resultsError,
  } = useExperimentResults(selectedExperimentId);

  const selectedDataset = datasets.find((d) => d.id === selectedDatasetId) ?? null;

  // Handlers
  const handleCreateDataset = () => {
    if (!newDatasetName.trim()) return;
    createDataset.mutate(
      { name: newDatasetName.trim(), description: newDatasetDescription.trim() || undefined },
      {
        onSuccess: (ds) => {
          setSelectedDatasetId(ds.id);
          setShowCreateDataset(false);
          setNewDatasetName("");
          setNewDatasetDescription("");
        },
      },
    );
  };

  const handleDeleteDataset = (id: string) => {
    deleteDataset.mutate(id, {
      onSuccess: () => {
        if (selectedDatasetId === id) {
          setSelectedDatasetId(null);
          setSelectedExperimentId(null);
        }
      },
      onError: (err) => {
        window.alert(
          `Failed to delete dataset: ${err instanceof Error ? err.message : String(err)}`,
        );
      },
    });
  };

  const handleDeleteItem = (itemId: string) => {
    deleteItem.mutate(itemId, {
      onError: (err) => {
        window.alert(`Failed to delete item: ${err instanceof Error ? err.message : String(err)}`);
      },
    });
  };

  const handleCreateExperiment = (name: string, description?: string, promptVariantId?: string) => {
    if (!selectedDatasetId) return;
    createExperiment.mutate(
      { dataset_id: selectedDatasetId, name, description, prompt_variant_id: promptVariantId },
      {
        onSuccess: (exp) => {
          setSelectedExperimentId(exp.id);
        },
      },
    );
  };

  // Error state
  if (datasetsError) {
    // The evaluation dataset REST endpoints (`/api/v1/evaluation/datasets`)
    // are not yet implemented in the runner backend. Distinguish a clean
    // "not implemented" 404 from a transient failure so the page surfaces
    // an informative empty-state rather than a generic "Failed to load"
    // banner that previously cluttered the console with API 404s. When the
    // backend routes land, the existing TanStack Query branch already
    // handles success.
    const isNotImplemented = datasetsError.includes("API 404");
    if (isNotImplemented) {
      return (
        <div className="h-full flex items-center justify-center text-muted-foreground">
          <div className="text-center max-w-md px-6">
            <FlaskConical className="w-12 h-12 mx-auto mb-4 opacity-50" />
            <p className="font-medium">Evaluation Dashboard</p>
            <p className="text-sm mt-2">
              The evaluation backend is not yet enabled in this runner build.
              Datasets and experiments will appear here once the
              <code className="mx-1 px-1 py-0.5 rounded bg-muted text-xs">
                /api/v1/evaluation/datasets
              </code>
              endpoints are available.
            </p>
          </div>
        </div>
      );
    }
    return (
      <div className="h-full flex items-center justify-center text-muted-foreground">
        <div className="text-center">
          <AlertTriangle className="w-12 h-12 mx-auto mb-4 opacity-50" />
          <p>Failed to load evaluation data</p>
          <p className="text-sm mt-2">{datasetsError}</p>
          <button
            onClick={refreshDatasets}
            className="mt-4 px-4 py-2 rounded-md bg-primary text-primary-foreground text-sm hover:bg-primary/90 transition-colors"
          >
            Retry
          </button>
        </div>
      </div>
    );
  }

  return (
    <div className="h-full overflow-hidden flex flex-col" data-tutorial-id="evaluation-dashboard">
      {/* Top bar */}
      <div className="flex items-center justify-between px-4 py-3 border-b border-border/50 shrink-0">
        <div>
          <h2 className="text-lg font-semibold flex items-center gap-2">
            <FlaskConical className="w-5 h-5" />
            Evaluation Dashboard
          </h2>
          <p className="text-sm text-muted-foreground">
            Manage datasets, run experiments, and review results
          </p>
        </div>
        <div className="flex items-center gap-2">
          <Button size="sm" variant="outline" onClick={refreshDatasets}>
            <RefreshCw className="w-3.5 h-3.5" />
            Refresh
          </Button>
          <Button
            size="sm"
            onClick={() => setShowCreateDataset(!showCreateDataset)}
            data-tutorial-id="evaluation-create-dataset"
          >
            <Plus className="w-3.5 h-3.5" />
            New Dataset
          </Button>
        </div>
      </div>

      {/* Create dataset inline form */}
      {showCreateDataset && (
        <div className="px-4 py-3 border-b border-border/50 bg-muted/20 flex items-end gap-3 shrink-0">
          <div className="flex-1 space-y-1">
            <label className="text-xs font-medium text-muted-foreground">Dataset Name</label>
            <input
              aria-label="Dataset Name"
              className="w-full rounded-md bg-background border border-border px-3 py-1.5 text-sm focus:outline-hidden focus:ring-2 focus:ring-primary"
              placeholder="e.g. Customer Support QA"
              value={newDatasetName}
              onChange={(e) => setNewDatasetName(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") handleCreateDataset();
              }}
              autoFocus
            />
          </div>
          <div className="flex-1 space-y-1">
            <label className="text-xs font-medium text-muted-foreground">
              Description (optional)
            </label>
            <input
              aria-label="Description (optional)"
              className="w-full rounded-md bg-background border border-border px-3 py-1.5 text-sm focus:outline-hidden focus:ring-2 focus:ring-primary"
              placeholder="Brief description..."
              value={newDatasetDescription}
              onChange={(e) => setNewDatasetDescription(e.target.value)}
            />
          </div>
          <Button
            size="sm"
            onClick={handleCreateDataset}
            disabled={!newDatasetName.trim() || createDataset.isPending}
          >
            {createDataset.isPending ? "Creating..." : "Create"}
          </Button>
          <Button size="sm" variant="ghost" onClick={() => setShowCreateDataset(false)}>
            Cancel
          </Button>
        </div>
      )}

      {/* Main two-panel layout */}
      <div className="flex-1 flex min-h-0 overflow-hidden">
        {/* Left panel: datasets + detail */}
        <div
          className="w-1/2 flex flex-col border-r border-border/50 min-h-0"
          data-tutorial-id="evaluation-datasets-panel"
        >
          {/* Dataset list (top portion) */}
          <Card className="m-2 mb-1 shrink-0 max-h-[40%] overflow-auto">
            <div className="px-3 py-2 border-b border-border/50">
              <h4 className="text-xs font-semibold uppercase tracking-wider text-muted-foreground">
                Datasets
              </h4>
            </div>
            <div className="p-1">
              <DatasetList
                datasets={datasets}
                isLoading={datasetsLoading}
                selectedId={selectedDatasetId}
                onSelect={(id) => {
                  setSelectedDatasetId(id);
                  setSelectedExperimentId(null);
                }}
                onDelete={handleDeleteDataset}
              />
            </div>
          </Card>

          {/* Dataset detail (bottom portion) */}
          <Card className="m-2 mt-1 flex-1 min-h-0 overflow-hidden flex flex-col">
            {selectedDataset ? (
              <DatasetDetail
                dataset={selectedDataset}
                items={items}
                isLoading={itemsLoading}
                onAddItems={(req) => addItems.mutateAsync(req)}
                isAdding={addItems.isPending}
                onDeleteItem={handleDeleteItem}
                isDeletingItem={deleteItem.isPending}
              />
            ) : (
              <div className="flex-1 flex items-center justify-center text-muted-foreground text-sm">
                Select a dataset to view its items
              </div>
            )}
          </Card>
        </div>

        {/* Right panel: experiments + results / live evaluation */}
        <div
          className="w-1/2 flex flex-col min-h-0"
          data-tutorial-id="evaluation-experiments-panel"
        >
          {/* Tab switcher */}
          <div className="flex items-center gap-1 px-2 pt-2 shrink-0">
            <button
              className={`px-3 py-1.5 rounded-t-md text-xs font-medium transition-colors ${
                rightPanelTab === "experiments"
                  ? "bg-card text-foreground border border-b-0 border-border/50"
                  : "text-muted-foreground hover:text-foreground"
              }`}
              onClick={() => setRightPanelTab("experiments")}
            >
              Experiments
            </button>
            <button
              className={`px-3 py-1.5 rounded-t-md text-xs font-medium transition-colors flex items-center gap-1.5 ${
                rightPanelTab === "evaluate"
                  ? "bg-card text-foreground border border-b-0 border-border/50"
                  : "text-muted-foreground hover:text-foreground"
              }`}
              onClick={() => setRightPanelTab("evaluate")}
            >
              <Play className="w-3 h-3" />
              Run Evaluation
            </button>
          </div>

          {rightPanelTab === "experiments" ? (
            <>
              {/* Experiment list (top portion) */}
              <Card className="m-2 mb-1 shrink-0 max-h-[40%] overflow-hidden flex flex-col">
                <ExperimentList
                  experiments={experiments}
                  isLoading={experimentsLoading}
                  selectedId={selectedExperimentId}
                  onSelect={setSelectedExperimentId}
                  onCreate={handleCreateExperiment}
                  isCreating={createExperiment.isPending}
                  datasetId={selectedDatasetId}
                />
              </Card>

              {/* Experiment results (bottom portion) */}
              <Card className="m-2 mt-1 flex-1 min-h-0 overflow-hidden flex flex-col">
                {selectedExperimentId ? (
                  <ExperimentResults
                    results={results}
                    summary={summary}
                    isLoading={resultsLoading}
                    error={resultsError}
                    onEvaluateRow={(input, output) => {
                      setEvalPrefillInput(input);
                      setEvalPrefillOutput(output);
                      setRightPanelTab("evaluate");
                    }}
                  />
                ) : (
                  <div className="flex-1 flex items-center justify-center text-muted-foreground text-sm">
                    {selectedDatasetId
                      ? "Select an experiment to view results"
                      : "Select a dataset first"}
                  </div>
                )}
              </Card>
            </>
          ) : (
            <Card className="m-2 flex-1 min-h-0 overflow-hidden flex flex-col">
              <Suspense
                fallback={
                  <div className="flex-1 flex items-center justify-center text-muted-foreground">
                    <RefreshCw className="w-5 h-5 animate-spin" />
                  </div>
                }
              >
                <LiveEvaluationPanel
                  prefillInput={evalPrefillInput}
                  prefillOutput={evalPrefillOutput}
                />
              </Suspense>
            </Card>
          )}
        </div>
      </div>
    </div>
  );
}
