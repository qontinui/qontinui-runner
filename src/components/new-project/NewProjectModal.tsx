/**
 * NewProjectModal
 *
 * Standalone entry point for the New Project flow, launched from the Terminal
 * page header. Wraps {@link NewProjectFlow} in a Radix Dialog (same pattern as
 * `RAGProjectModal`). The flow is fully self-contained; this shell only owns
 * the dialog chrome and forwards the created-project + close callbacks.
 */

import * as Dialog from "@radix-ui/react-dialog";
import { X, FilePlus } from "lucide-react";
import { NewProjectFlow } from "./NewProjectFlow";
import type { SavedProject } from "@/hooks/useSavedProjects";

export interface NewProjectModalProps {
  isOpen: boolean;
  onClose: () => void;
  /** Forwarded from NewProjectFlow when a project is created + registered. */
  onCreated?: (project: SavedProject) => void;
}

export function NewProjectModal({ isOpen, onClose, onCreated }: NewProjectModalProps) {
  return (
    <Dialog.Root open={isOpen} onOpenChange={(open) => !open && onClose()}>
      <Dialog.Portal>
        <Dialog.Overlay className="fixed inset-0 bg-black/50 backdrop-blur-sm z-40" />
        <Dialog.Content
          className="fixed left-1/2 top-1/2 -translate-x-1/2 -translate-y-1/2 bg-card border border-border rounded-xl shadow-2xl z-40 max-h-[85vh] w-[90vw] max-w-lg overflow-hidden flex flex-col"
          aria-describedby="new-project-modal-description"
        >
          {/* Header */}
          <div className="flex items-center justify-between p-6 border-b border-border">
            <div className="flex items-center gap-3">
              <div className="p-2 rounded-lg bg-primary/10">
                <FilePlus className="w-5 h-5 text-primary" />
              </div>
              <div>
                <Dialog.Title className="text-lg font-semibold">New Project</Dialog.Title>
                <p id="new-project-modal-description" className="text-sm text-muted-foreground">
                  Create a local project and, optionally, a coord-managed GitHub repo
                </p>
              </div>
            </div>
            <Dialog.Close asChild>
              <button
                className="p-2 hover:bg-accent/20 rounded-lg transition-colors"
                aria-label="Close"
              >
                <X className="w-5 h-5" />
              </button>
            </Dialog.Close>
          </div>

          {/* Content */}
          <div className="flex-1 overflow-y-auto p-6">
            <NewProjectFlow onCreated={onCreated} onClose={onClose} />
          </div>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}
