/**
 * Clipboard sync and file sharing components for mobile devices.
 *
 * Provides:
 * - useShareToMobile() hook — invokes the Tauri share_to_mobile command
 * - ShareToMobileButton — small icon button to place next to copy buttons
 * - ShareFileButton — button that opens file dialog and uploads to mobile
 */

import React, { useState, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { Smartphone, Check, AlertCircle, Upload } from "lucide-react";

// ---------------------------------------------------------------------------
// Hook
// ---------------------------------------------------------------------------

export function useShareToMobile() {
  const [sharing, setSharing] = useState(false);
  const [shared, setShared] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const shareToMobile = useCallback(async (text: string) => {
    if (!text.trim()) return;
    setSharing(true);
    setError(null);
    try {
      await invoke("share_to_mobile", { text });
      setShared(true);
      setTimeout(() => setShared(false), 2000);
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      setError(msg);
      console.error("Share to mobile failed:", msg);
    } finally {
      setSharing(false);
    }
  }, []);

  return { shareToMobile, sharing, shared, error };
}

// ---------------------------------------------------------------------------
// Button
// ---------------------------------------------------------------------------

interface ShareToMobileButtonProps {
  /** Text content to share */
  getText: () => string;
  /** Additional CSS classes */
  className?: string;
  /** Size of the icon in pixels */
  size?: number;
}

/**
 * Small icon button that shares text to mobile via the clipboard relay.
 * Designed to sit next to existing copy-to-clipboard buttons.
 */
export function ShareToMobileButton({
  getText,
  className = "",
  size = 14,
}: ShareToMobileButtonProps) {
  const { shareToMobile, sharing, shared, error } = useShareToMobile();

  const handleClick = async (e: React.MouseEvent) => {
    e.stopPropagation();
    const text = getText();
    if (text) {
      await shareToMobile(text);
    }
  };

  const icon = shared ? (
    <Check className="text-green-500" style={{ width: size, height: size }} />
  ) : error ? (
    <AlertCircle className="text-red-400" style={{ width: size, height: size }} />
  ) : (
    <Smartphone
      className={`text-muted-foreground ${sharing ? "animate-pulse" : ""}`}
      style={{ width: size, height: size }}
    />
  );

  const title = shared ? "Shared to mobile!" : error ? `Share failed: ${error}` : "Share to mobile";

  return (
    <button
      onClick={handleClick}
      disabled={sharing}
      className={`p-1.5 rounded hover:bg-muted transition-opacity ${className}`}
      title={title}
    >
      {icon}
    </button>
  );
}

// ---------------------------------------------------------------------------
// File sharing
// ---------------------------------------------------------------------------

interface ShareFileButtonProps {
  /** Optional: pre-set file path to share (skips dialog) */
  filePath?: string;
  /** Additional CSS classes */
  className?: string;
  /** Size of the icon in pixels */
  size?: number;
}

/**
 * Button that opens a file picker and uploads the selected file to the
 * backend for mobile access.
 */
export function ShareFileButton({ filePath, className = "", size = 14 }: ShareFileButtonProps) {
  const [uploading, setUploading] = useState(false);
  const [uploaded, setUploaded] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const handleClick = async (e: React.MouseEvent) => {
    e.stopPropagation();
    setError(null);

    let path = filePath;

    if (!path) {
      const selected = await open({
        multiple: false,
        title: "Select file to share with mobile",
      });
      if (!selected) return; // user cancelled
      path = selected as string;
    }

    setUploading(true);
    try {
      await invoke("share_file_to_mobile", { filePath: path });
      setUploaded(true);
      setTimeout(() => setUploaded(false), 2000);
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      setError(msg);
      console.error("Share file to mobile failed:", msg);
    } finally {
      setUploading(false);
    }
  };

  const icon = uploaded ? (
    <Check className="text-green-500" style={{ width: size, height: size }} />
  ) : error ? (
    <AlertCircle className="text-red-400" style={{ width: size, height: size }} />
  ) : (
    <Upload
      className={`text-muted-foreground ${uploading ? "animate-pulse" : ""}`}
      style={{ width: size, height: size }}
    />
  );

  const title = uploaded
    ? "File shared to mobile!"
    : error
      ? `Share failed: ${error}`
      : "Share file to mobile";

  return (
    <button
      onClick={handleClick}
      disabled={uploading}
      className={`p-1.5 rounded hover:bg-muted transition-opacity ${className}`}
      title={title}
    >
      {icon}
    </button>
  );
}
