/**
 * DemoVisualOverlay
 *
 * Fullscreen overlay that displays pre-generated images or videos during demo recording.
 * Listens for DEMO_VISUAL_OVERLAY_EVENT custom events dispatched by the script executor.
 *
 * Mount this component once at the app root level so it renders above all other content.
 */

import { useState, useEffect, useCallback, useRef } from "react";
import { DEMO_VISUAL_OVERLAY_EVENT, type DemoVisualOverlayDetail } from "@/lib/demo-video/types";
import { convertFileSrc } from "@tauri-apps/api/core";

// =============================================================================
// Helpers
// =============================================================================

const VIDEO_EXTENSIONS = new Set(["mp4", "webm", "mov", "avi", "mkv"]);

function isVideo(filePath: string): boolean {
  const ext = filePath.split(".").pop()?.toLowerCase() ?? "";
  return VIDEO_EXTENSIONS.has(ext);
}

// =============================================================================
// DemoVisualOverlay
// =============================================================================

export function DemoVisualOverlay() {
  const [visible, setVisible] = useState(false);
  const [filePath, setFilePath] = useState("");
  const [caption, setCaption] = useState<string | undefined>();
  const dismissTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  const hide = useCallback(() => {
    setVisible(false);
    setFilePath("");
    setCaption(undefined);
    if (dismissTimerRef.current) {
      clearTimeout(dismissTimerRef.current);
      dismissTimerRef.current = null;
    }
  }, []);

  useEffect(() => {
    const handler = (e: Event) => {
      const detail = (e as CustomEvent<DemoVisualOverlayDetail>).detail;
      if (!detail) return;

      if (detail.action === "hide") {
        hide();
        return;
      }

      // Show
      if (detail.filePath) {
        setFilePath(detail.filePath);
        setCaption(detail.caption);
        setVisible(true);

        // Auto-dismiss after durationMs if provided
        if (dismissTimerRef.current) clearTimeout(dismissTimerRef.current);
        if (detail.durationMs) {
          dismissTimerRef.current = setTimeout(hide, detail.durationMs);
        }
      }
    };

    window.addEventListener(DEMO_VISUAL_OVERLAY_EVENT, handler);
    return () => {
      window.removeEventListener(DEMO_VISUAL_OVERLAY_EVENT, handler);
      if (dismissTimerRef.current) clearTimeout(dismissTimerRef.current);
    };
  }, [hide]);

  if (!visible || !filePath) return null;

  // Convert local file path to a Tauri-safe asset URL
  const src = filePath.startsWith("http") ? filePath : convertFileSrc(filePath);
  const showVideo = isVideo(filePath);

  return (
    <div
      className="fixed inset-0 z-[9999] flex flex-col items-center justify-center bg-black/95 animate-in fade-in duration-300"
      onClick={hide}
    >
      {showVideo ? (
        <video
          src={src}
          autoPlay
          muted
          className="max-w-[90vw] max-h-[80vh] rounded-lg shadow-2xl"
          onEnded={hide}
        />
      ) : (
        <img
          src={src}
          alt={caption || "Demo visual"}
          className="max-w-[90vw] max-h-[80vh] rounded-lg shadow-2xl object-contain"
        />
      )}

      {caption && (
        <p className="mt-4 text-lg text-white/90 font-medium max-w-2xl text-center">{caption}</p>
      )}
    </div>
  );
}
