/**
 * DatasetList
 *
 * Displays evaluation datasets in a selectable list with name, item count,
 * version, and last-updated time. Includes a delete button with confirmation.
 */

import { useState } from "react";
import { Database, Trash2, RefreshCw } from "lucide-react";
import { Badge } from "@/components/ui/Badge";
import { Button } from "@/components/ui/Button";
import type { EvaluationDataset } from "@/hooks/useEvaluationDatasets";

interface DatasetListProps {
  datasets: EvaluationDataset[];
  isLoading: boolean;
  selectedId: string | null;
  onSelect: (id: string) => void;
  onDelete: (id: string) => void;
}

function formatDate(iso: string): string {
  const d = new Date(iso);
  return d.toLocaleDateString(undefined, { month: "short", day: "numeric", year: "numeric" });
}

export function DatasetList({ datasets, isLoading, selectedId, onSelect, onDelete }: DatasetListProps) {
  const [confirmDeleteId, setConfirmDeleteId] = useState<string | null>(null);

  if (isLoading) {
    return (
      <div className="flex items-center justify-center py-12 text-muted-foreground">
        <RefreshCw className="w-5 h-5 animate-spin" />
      </div>
    );
  }

  if (datasets.length === 0) {
    return (
      <div className="flex flex-col items-center justify-center py-12 text-muted-foreground gap-2">
        <Database className="w-8 h-8 opacity-50" />
        <p className="text-sm">No datasets yet</p>
      </div>
    );
  }

  return (
    <div className="space-y-1">
      {datasets.map((ds) => {
        const isSelected = ds.id === selectedId;
        return (
          <div
            key={ds.id}
            role="button"
            tabIndex={0}
            onClick={() => onSelect(ds.id)}
            onKeyDown={(e) => {
              if (e.key === "Enter" || e.key === " ") onSelect(ds.id);
            }}
            className={`group flex items-center justify-between px-3 py-2.5 rounded-md cursor-pointer transition-colors ${
              isSelected
                ? "bg-primary/10 border border-primary/30"
                : "hover:bg-muted/50 border border-transparent"
            }`}
          >
            <div className="min-w-0 flex-1">
              <div className="flex items-center gap-2">
                <span className="text-sm font-medium truncate">{ds.name}</span>
                <Badge variant="muted" size="sm">v{ds.version}</Badge>
              </div>
              <div className="flex items-center gap-3 mt-0.5 text-xs text-muted-foreground">
                <span>{ds.item_count} items</span>
                <span>{formatDate(ds.updated_at)}</span>
              </div>
            </div>

            {confirmDeleteId === ds.id ? (
              <div className="flex items-center gap-1 shrink-0 ml-2">
                <Button
                  size="sm"
                  variant="danger"
                  onClick={(e) => {
                    e.stopPropagation();
                    onDelete(ds.id);
                    setConfirmDeleteId(null);
                  }}
                >
                  Confirm
                </Button>
                <Button
                  size="sm"
                  variant="ghost"
                  onClick={(e) => {
                    e.stopPropagation();
                    setConfirmDeleteId(null);
                  }}
                >
                  Cancel
                </Button>
              </div>
            ) : (
              <button
                className="shrink-0 ml-2 p-1 rounded opacity-0 group-hover:opacity-100 hover:bg-destructive/10 hover:text-destructive transition-all"
                onClick={(e) => {
                  e.stopPropagation();
                  setConfirmDeleteId(ds.id);
                }}
                title="Delete dataset"
              >
                <Trash2 className="w-3.5 h-3.5" />
              </button>
            )}
          </div>
        );
      })}
    </div>
  );
}
