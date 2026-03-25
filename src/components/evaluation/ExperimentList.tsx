/**
 * ExperimentList
 *
 * Lists experiments for the selected dataset. Shows name, status badge,
 * completed/total count, and created date. Includes a "New Experiment" button.
 */

import { useState } from "react";
import { FlaskConical, Plus, RefreshCw } from "lucide-react";
import { Badge } from "@/components/ui/Badge";
import { Button } from "@/components/ui/Button";
import type { EvaluationExperiment, ExperimentStatus } from "@/hooks/useEvaluationExperiments";

interface ExperimentListProps {
  experiments: EvaluationExperiment[];
  isLoading: boolean;
  selectedId: string | null;
  onSelect: (id: string) => void;
  onCreate: (name: string, description?: string, promptVariantId?: string) => void;
  isCreating: boolean;
  datasetId: string | null;
}

const STATUS_VARIANT: Record<ExperimentStatus, "muted" | "info" | "success" | "danger"> = {
  pending: "muted",
  running: "info",
  completed: "success",
  failed: "danger",
};

function formatDate(iso: string): string {
  const d = new Date(iso);
  return d.toLocaleDateString(undefined, { month: "short", day: "numeric", year: "numeric" });
}

export function ExperimentList({
  experiments,
  isLoading,
  selectedId,
  onSelect,
  onCreate,
  isCreating,
  datasetId,
}: ExperimentListProps) {
  const [showCreate, setShowCreate] = useState(false);
  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  const [promptVariantId, setPromptVariantId] = useState("");

  const handleCreate = () => {
    if (!name.trim()) return;
    onCreate(name.trim(), description.trim() || undefined, promptVariantId.trim() || undefined);
    setName("");
    setDescription("");
    setPromptVariantId("");
    setShowCreate(false);
  };

  if (!datasetId) {
    return (
      <div className="flex flex-col items-center justify-center py-12 text-muted-foreground gap-2">
        <FlaskConical className="w-8 h-8 opacity-50" />
        <p className="text-sm">Select a dataset to view experiments</p>
      </div>
    );
  }

  return (
    <div className="flex flex-col h-full">
      {/* Header */}
      <div className="flex items-center justify-between px-4 py-3 border-b border-border/50">
        <h3 className="text-sm font-semibold">Experiments</h3>
        <Button
          size="sm"
          variant={showCreate ? "ghost" : "outline"}
          onClick={() => setShowCreate(!showCreate)}
        >
          <Plus className="w-3.5 h-3.5" />
          {showCreate ? "Cancel" : "New Experiment"}
        </Button>
      </div>

      {/* Create form */}
      {showCreate && (
        <div className="px-4 py-3 border-b border-border/50 bg-muted/20 space-y-2">
          <input
            className="w-full rounded-md bg-background border border-border px-3 py-1.5 text-xs focus:outline-hidden focus:ring-2 focus:ring-primary"
            placeholder="Experiment name"
            value={name}
            onChange={(e) => setName(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") handleCreate();
            }}
          />
          <input
            className="w-full rounded-md bg-background border border-border px-3 py-1.5 text-xs focus:outline-hidden focus:ring-2 focus:ring-primary"
            placeholder="Description (optional)"
            value={description}
            onChange={(e) => setDescription(e.target.value)}
          />
          <input
            className="w-full rounded-md bg-background border border-border px-3 py-1.5 text-xs focus:outline-hidden focus:ring-2 focus:ring-primary"
            placeholder="Prompt Variant ID (optional)"
            value={promptVariantId}
            onChange={(e) => setPromptVariantId(e.target.value)}
          />
          <div className="flex justify-end">
            <Button size="sm" onClick={handleCreate} disabled={!name.trim() || isCreating}>
              {isCreating ? "Creating..." : "Create"}
            </Button>
          </div>
        </div>
      )}

      {/* List */}
      <div className="flex-1 overflow-auto px-2 py-1">
        {isLoading ? (
          <div className="flex items-center justify-center py-12 text-muted-foreground">
            <RefreshCw className="w-5 h-5 animate-spin" />
          </div>
        ) : experiments.length === 0 ? (
          <div className="flex flex-col items-center justify-center py-12 text-muted-foreground gap-2">
            <FlaskConical className="w-8 h-8 opacity-50" />
            <p className="text-sm">No experiments yet</p>
          </div>
        ) : (
          <div className="space-y-1">
            {experiments.map((exp) => {
              const isSelected = exp.id === selectedId;
              return (
                <div
                  key={exp.id}
                  role="button"
                  tabIndex={0}
                  onClick={() => onSelect(exp.id)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter" || e.key === " ") onSelect(exp.id);
                  }}
                  className={`flex items-center justify-between px-3 py-2.5 rounded-md cursor-pointer transition-colors ${
                    isSelected
                      ? "bg-primary/10 border border-primary/30"
                      : "hover:bg-muted/50 border border-transparent"
                  }`}
                >
                  <div className="min-w-0 flex-1">
                    <div className="flex items-center gap-2">
                      <span className="text-sm font-medium truncate">{exp.name}</span>
                      <Badge variant={STATUS_VARIANT[exp.status]} size="sm">
                        {exp.status}
                      </Badge>
                    </div>
                    <div className="flex items-center gap-3 mt-0.5 text-xs text-muted-foreground">
                      <span>{exp.completed_count}/{exp.total_count} completed</span>
                      <span>{formatDate(exp.created_at)}</span>
                    </div>
                  </div>
                </div>
              );
            })}
          </div>
        )}
      </div>
    </div>
  );
}
