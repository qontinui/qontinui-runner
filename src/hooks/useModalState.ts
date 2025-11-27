/**
 * useModalState Hook
 *
 * Manages modal state for action and image detail modals.
 * Provides type-safe modal management with selected item tracking.
 */

import { useState, useCallback } from "react";
import type { ActionLogEntry } from "../types/displayProfile";
import type { ImageRecognitionEntry } from "../managers/LogManager";

export interface UseModalStateResult {
  // Action modal state
  selectedAction: ActionLogEntry | null;
  isActionModalOpen: boolean;
  openActionModal: (action: ActionLogEntry) => void;
  closeActionModal: () => void;

  // Image modal state
  selectedImageEntry: ImageRecognitionEntry | null;
  isImageModalOpen: boolean;
  openImageModal: (entry: ImageRecognitionEntry) => void;
  closeImageModal: () => void;
}

/**
 * Hook for managing modal state (action and image detail modals)
 */
export function useModalState(): UseModalStateResult {
  const [selectedAction, setSelectedAction] = useState<ActionLogEntry | null>(null);
  const [isActionModalOpen, setIsActionModalOpen] = useState(false);
  const [selectedImageEntry, setSelectedImageEntry] = useState<ImageRecognitionEntry | null>(null);
  const [isImageModalOpen, setIsImageModalOpen] = useState(false);

  const openActionModal = useCallback((action: ActionLogEntry) => {
    setSelectedAction(action);
    setIsActionModalOpen(true);
  }, []);

  const closeActionModal = useCallback(() => {
    setIsActionModalOpen(false);
  }, []);

  const openImageModal = useCallback((entry: ImageRecognitionEntry) => {
    setSelectedImageEntry(entry);
    setIsImageModalOpen(true);
  }, []);

  const closeImageModal = useCallback(() => {
    setIsImageModalOpen(false);
  }, []);

  return {
    selectedAction,
    isActionModalOpen,
    openActionModal,
    closeActionModal,
    selectedImageEntry,
    isImageModalOpen,
    openImageModal,
    closeImageModal,
  };
}
