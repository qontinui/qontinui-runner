/**
 * RAGProjectModal Component
 *
 * Modal dialog for selecting and loading RAG projects.
 * Wraps the ProjectSelector component in a modal dialog.
 */

import * as Dialog from "@radix-ui/react-dialog";
import { X, Sparkles } from "lucide-react";
import { ProjectSelector } from "./ProjectSelector";
import { useState, useEffect } from "react";

export interface RAGProjectModalProps {
  isOpen: boolean;
  onClose: () => void;
  onLog?: (level: "info" | "warning" | "error" | "debug" | "success", message: string) => void;
}

export function RAGProjectModal({ isOpen, onClose, onLog }: RAGProjectModalProps) {
  const [loading, setLoading] = useState(false);

  // Handle successful project load
  const handleProjectLoad = (projectId: string, projectName: string) => {
    setLoading(true);
    onLog?.("success", `Loaded RAG project: ${projectName}`);

    // Close modal after a short delay to show success feedback
    setTimeout(() => {
      setLoading(false);
      onClose();
    }, 500);
  };

  // Reset loading state when modal closes
  useEffect(() => {
    if (!isOpen) {
      // eslint-disable-next-line react-hooks/set-state-in-effect -- reset transient state on modal close
      setLoading(false);
    }
  }, [isOpen]);

  return (
    <Dialog.Root open={isOpen} onOpenChange={onClose}>
      <Dialog.Portal>
        <Dialog.Overlay className="fixed inset-0 bg-black/50 backdrop-blur-sm z-40" />
        <Dialog.Content
          className="fixed left-1/2 top-1/2 -translate-x-1/2 -translate-y-1/2 bg-card border border-border rounded-xl shadow-2xl z-40 max-h-[70vh] w-[90vw] max-w-2xl overflow-hidden flex flex-col"
          aria-describedby="rag-modal-description"
        >
          {/* Header */}
          <div className="flex items-center justify-between p-6 border-b border-border">
            <div className="flex items-center gap-3">
              <div className="p-2 rounded-lg bg-primary/10">
                <Sparkles className="w-5 h-5 text-primary" />
              </div>
              <div>
                <Dialog.Title className="text-lg font-semibold">Load RAG Project</Dialog.Title>
                <p id="rag-modal-description" className="text-sm text-muted-foreground">
                  Select a saved RAG project to load into the executor
                </p>
              </div>
            </div>
            <Dialog.Close asChild>
              <button
                className="p-2 hover:bg-accent/20 rounded-lg transition-colors"
                aria-label="Close"
                disabled={loading}
              >
                <X className="w-5 h-5" />
              </button>
            </Dialog.Close>
          </div>

          {/* Content */}
          <div className="flex-1 overflow-y-auto p-6">
            <ProjectSelector onProjectLoad={handleProjectLoad} onLog={onLog} />
          </div>

          {/* Footer */}
          <div className="border-t border-border p-4 flex justify-end gap-3">
            <Dialog.Close asChild>
              <button className="btn-secondary" disabled={loading}>
                Cancel
              </button>
            </Dialog.Close>
          </div>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}
