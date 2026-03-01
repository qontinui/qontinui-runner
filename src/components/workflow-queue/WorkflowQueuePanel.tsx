/**
 * WorkflowQueuePanel
 *
 * Right panel showing the ordered queue of workflows to execute.
 * Supports drag-and-drop reordering.
 */

import { useState } from "react";
import { ListOrdered, Inbox } from "lucide-react";
import type { QueuedWorkflow } from "./types";
import { WorkflowQueueItem } from "./WorkflowQueueItem";

interface WorkflowQueuePanelProps {
  items: QueuedWorkflow[];
  onRemove: (queueId: string) => void;
  onReorder: (fromIndex: number, toIndex: number) => void;
}

export function WorkflowQueuePanel({ items, onRemove, onReorder }: WorkflowQueuePanelProps) {
  const [dragIndex, setDragIndex] = useState<number | null>(null);
  const [dragOverIndex, setDragOverIndex] = useState<number | null>(null);

  const handleDragStart = (index: number) => {
    setDragIndex(index);
  };

  const handleDragEnd = () => {
    if (dragIndex !== null && dragOverIndex !== null && dragIndex !== dragOverIndex) {
      onReorder(dragIndex, dragOverIndex);
    }
    setDragIndex(null);
    setDragOverIndex(null);
  };

  const handleDragOver = (index: number) => {
    if (dragIndex !== null && index !== dragIndex) {
      setDragOverIndex(index);
    }
  };

  return (
    <div className="w-1/2 flex flex-col">
      {/* Header */}
      <div className="px-4 py-3 border-b border-white/10">
        <div className="flex items-center gap-2">
          <ListOrdered className="w-5 h-5 text-cyan-400" />
          <h2 className="text-h3 text-foreground">Execution Queue</h2>
          {items.length > 0 && (
            <span className="ml-auto bg-cyan-500/20 text-cyan-400 px-2 py-0.5 rounded-full text-label-sm">
              {items.length}
            </span>
          )}
        </div>
      </div>

      {/* Queue list */}
      <div className="flex-1 overflow-y-auto p-4">
        {items.length === 0 ? (
          <div
            className="flex flex-col items-center justify-center h-full border-2 border-dashed border-white/10 rounded-lg text-muted-foreground"
            onDragOver={(e) => e.preventDefault()}
          >
            <Inbox className="w-10 h-10 mb-3 text-muted-foreground/50" />
            <p className="text-body font-medium text-muted-foreground">Queue is empty</p>
            <p className="text-body-sm mt-1">Add workflows from the library</p>
          </div>
        ) : (
          <div className="space-y-2">
            {items.map((item, index) => (
              <WorkflowQueueItem
                key={item.queueId}
                item={item}
                index={index}
                onRemove={() => onRemove(item.queueId)}
                onDragStart={() => handleDragStart(index)}
                onDragEnd={handleDragEnd}
                onDragOver={() => handleDragOver(index)}
                isDragging={dragIndex === index}
                isDragOver={dragOverIndex === index}
              />
            ))}
          </div>
        )}
      </div>
    </div>
  );
}
