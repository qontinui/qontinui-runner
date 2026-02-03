/**
 * Image Viewer Modal
 *
 * A modal component for displaying full-resolution images.
 * Used to show element thumbnails at larger sizes.
 */

import * as Dialog from "@radix-ui/react-dialog";
import { X, ZoomIn, ZoomOut, RotateCw } from "lucide-react";
import { useState, useCallback } from "react";
import { cn } from "../../lib/utils";

interface ImageViewerModalProps {
  /** Base64 encoded image data (without data URL prefix) */
  imageData: string;
  /** Title for accessibility */
  title: string;
  /** Whether the modal is open */
  isOpen: boolean;
  /** Callback when modal should close */
  onClose: () => void;
  /** Optional image format (default: png) */
  format?: "png" | "jpeg" | "webp";
}

export function ImageViewerModal({
  imageData,
  title,
  isOpen,
  onClose,
  format = "png",
}: ImageViewerModalProps) {
  const [zoom, setZoom] = useState(1);
  const [rotation, setRotation] = useState(0);

  const handleZoomIn = useCallback(() => {
    setZoom((prev) => Math.min(prev * 1.5, 5));
  }, []);

  const handleZoomOut = useCallback(() => {
    setZoom((prev) => Math.max(prev / 1.5, 0.25));
  }, []);

  const handleRotate = useCallback(() => {
    setRotation((prev) => (prev + 90) % 360);
  }, []);

  const handleReset = useCallback(() => {
    setZoom(1);
    setRotation(0);
  }, []);

  // Reset state when modal closes
  const handleOpenChange = useCallback(
    (open: boolean) => {
      if (!open) {
        onClose();
        // Reset state after a short delay to avoid visible reset during close animation
        setTimeout(() => {
          setZoom(1);
          setRotation(0);
        }, 200);
      }
    },
    [onClose],
  );

  const imageSrc = `data:image/${format};base64,${imageData}`;

  return (
    <Dialog.Root open={isOpen} onOpenChange={handleOpenChange}>
      <Dialog.Portal>
        <Dialog.Overlay className="fixed inset-0 bg-black/80 z-50 animate-in fade-in duration-200" />
        <Dialog.Content className="fixed inset-4 z-50 flex flex-col items-center justify-center animate-in fade-in zoom-in-95 duration-200">
          <Dialog.Title className="sr-only">{title}</Dialog.Title>
          <Dialog.Description className="sr-only">
            Full resolution preview of {title}
          </Dialog.Description>

          {/* Toolbar */}
          <div className="absolute top-4 left-1/2 -translate-x-1/2 flex items-center gap-2 bg-black/50 backdrop-blur-sm rounded-lg px-3 py-2 z-10">
            <button
              onClick={handleZoomOut}
              className="p-1.5 text-white/80 hover:text-white hover:bg-white/10 rounded transition-colors"
              title="Zoom out"
            >
              <ZoomOut className="w-5 h-5" />
            </button>
            <span className="text-sm text-white/80 min-w-[60px] text-center">
              {Math.round(zoom * 100)}%
            </span>
            <button
              onClick={handleZoomIn}
              className="p-1.5 text-white/80 hover:text-white hover:bg-white/10 rounded transition-colors"
              title="Zoom in"
            >
              <ZoomIn className="w-5 h-5" />
            </button>
            <div className="w-px h-5 bg-white/20 mx-1" />
            <button
              onClick={handleRotate}
              className="p-1.5 text-white/80 hover:text-white hover:bg-white/10 rounded transition-colors"
              title="Rotate"
            >
              <RotateCw className="w-5 h-5" />
            </button>
            <div className="w-px h-5 bg-white/20 mx-1" />
            <button
              onClick={handleReset}
              className="px-2 py-1 text-xs text-white/80 hover:text-white hover:bg-white/10 rounded transition-colors"
              title="Reset view"
            >
              Reset
            </button>
          </div>

          {/* Close button */}
          <Dialog.Close asChild>
            <button
              className="absolute top-4 right-4 p-2 bg-black/50 backdrop-blur-sm text-white/80 hover:text-white hover:bg-black/70 rounded-full transition-colors z-10"
              aria-label="Close"
            >
              <X className="w-6 h-6" />
            </button>
          </Dialog.Close>

          {/* Image container */}
          <div
            className="relative max-h-[calc(100vh-8rem)] max-w-[calc(100vw-4rem)] overflow-auto"
            onClick={(e) => {
              // Close on background click
              if (e.target === e.currentTarget) {
                onClose();
              }
            }}
          >
            <img
              src={imageSrc}
              alt={title}
              className={cn(
                "object-contain rounded-lg shadow-2xl transition-transform duration-200",
                "max-h-[90vh] max-w-[90vw]",
              )}
              style={{
                transform: `scale(${zoom}) rotate(${rotation}deg)`,
                transformOrigin: "center center",
              }}
              draggable={false}
            />
          </div>

          {/* Caption */}
          <div className="absolute bottom-4 left-1/2 -translate-x-1/2 bg-black/50 backdrop-blur-sm rounded-lg px-4 py-2">
            <p className="text-sm text-white/80">{title}</p>
          </div>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}

export default ImageViewerModal;
