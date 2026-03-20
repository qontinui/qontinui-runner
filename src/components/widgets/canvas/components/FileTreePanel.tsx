/**
 * FileTreePanel Component
 *
 * Renders a file/directory tree with optional status indicators.
 */

import { File, Folder } from "lucide-react";
import { cn } from "@/lib/utils";
import type { CanvasPanelComponentProps } from "./types";

interface FileEntry {
  path: string;
  type: "file" | "directory";
  status?: "added" | "modified" | "deleted";
}

const STATUS_CLASSES: Record<string, string> = {
  added: "text-green-400",
  modified: "text-yellow-400",
  deleted: "text-red-400 line-through",
};

const STATUS_PREFIX: Record<string, string> = {
  added: "+",
  modified: "~",
  deleted: "-",
};

export function FileTreePanel({ data, size }: CanvasPanelComponentProps) {
  const root = (data.root as string) ?? "";
  const entries = (data.entries as FileEntry[]) ?? [];
  const isCompact = size === "compact";

  if (entries.length === 0) {
    return <p className="text-sm text-muted-foreground italic">No files</p>;
  }

  return (
    <div className="space-y-1">
      {root && (
        <div className="flex items-center gap-1.5 text-muted-foreground mb-2">
          <Folder className="h-3.5 w-3.5" />
          <span className={cn("font-mono font-medium", isCompact ? "text-[10px]" : "text-xs")}>
            {root}
          </span>
        </div>
      )}
      <div className={cn("font-mono", isCompact ? "text-[10px]" : "text-xs")}>
        {entries.map((entry, idx) => {
          const Icon = entry.type === "directory" ? Folder : File;
          const statusClass = entry.status ? STATUS_CLASSES[entry.status] : "text-foreground";
          const prefix = entry.status ? STATUS_PREFIX[entry.status] : " ";
          const depth = entry.path.split("/").length - 1;

          return (
            <div
              key={entry.path}
              className={cn("flex items-center gap-1.5 py-0.5", statusClass)}
              style={{ paddingLeft: `${depth * 12}px` }}
            >
              {entry.status && (
                <span className="w-3 text-center flex-shrink-0 opacity-70">{prefix}</span>
              )}
              <Icon className="h-3 w-3 flex-shrink-0 opacity-60" />
              <span className="truncate">{entry.path.split("/").pop()}</span>
            </div>
          );
        })}
      </div>
    </div>
  );
}
