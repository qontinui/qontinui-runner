import React, { useState, useEffect, useCallback } from "react";
import { readTextFile, watch } from "@tauri-apps/plugin-fs";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import rehypeRaw from "rehype-raw";
import { FileText, RefreshCw, Loader2 } from "lucide-react";
import { SHARED_MARKDOWN_COMPONENTS } from "../markdown/shared-components";

interface PlanViewerProps {
  filePath: string;
  visible: boolean;
}

function extractFilename(filePath: string): string {
  const parts = filePath.replace(/\\/g, "/").split("/");
  return parts[parts.length - 1] || filePath;
}

export function PlanViewer({ filePath, visible }: PlanViewerProps) {
  const [content, setContent] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);

  const loadFile = useCallback(async () => {
    try {
      const text = await readTextFile(filePath);
      setContent(text);
      setError(null);
      setLoading(false);
    } catch (err) {
      setError(String(err));
      setLoading(false);
    }
  }, [filePath]);

  useEffect(() => {
    let unwatchFn: (() => void) | null = null;
    let mounted = true;

    const loadFileGuarded = async () => {
      try {
        const text = await readTextFile(filePath);
        if (mounted) {
          setContent(text);
          setError(null);
          setLoading(false);
        }
      } catch (err) {
        if (mounted) {
          setError(String(err));
          setLoading(false);
        }
      }
    };

    setLoading(true);
    setError(null);
    setContent(null);
    loadFileGuarded();

    watch(
      filePath,
      () => {
        loadFileGuarded();
      },
      { delayMs: 500 },
    )
      .then((fn) => {
        unwatchFn = fn;
      })
      .catch(() => {});

    return () => {
      mounted = false;
      unwatchFn?.();
    };
  }, [filePath]);

  const filename = extractFilename(filePath);

  return (
    <div className={`h-full flex flex-col ${!visible ? "pointer-events-none" : ""}`}>
      {/* Header */}
      <div className="h-7 flex items-center justify-between px-3 bg-[#13141f] border-b border-[#2a2d3d] shrink-0">
        <div className="flex items-center gap-2 text-xs text-[#a9b1d6]">
          <FileText className="w-3.5 h-3.5 text-[#7aa2f7]" />
          <span className="font-medium truncate">{filename}</span>
        </div>
        <button
          onClick={loadFile}
          className="p-0.5 rounded hover:bg-[#24283b] text-[#565f89] hover:text-[#a9b1d6] transition-colors"
          title="Refresh"
        >
          <RefreshCw className="w-3.5 h-3.5" />
        </button>
      </div>

      {/* Content */}
      <div className="flex-1 overflow-y-auto bg-[#1a1b26] scrollbar-dark">
        {loading && (
          <div className="flex items-center justify-center h-full">
            <Loader2 className="w-5 h-5 text-[#565f89] animate-spin" />
          </div>
        )}

        {!loading && error && (
          <div className="flex flex-col items-center justify-center h-full gap-3 px-6">
            <p className="text-sm text-red-400">Failed to load file</p>
            <p className="text-xs text-[#565f89] font-mono break-all text-center">{filePath}</p>
            <p className="text-xs text-red-400/70">{error}</p>
            <button
              onClick={loadFile}
              className="mt-2 px-3 py-1.5 text-xs rounded bg-[#24283b] text-[#a9b1d6] hover:bg-[#2a2d3d] transition-colors"
            >
              Retry
            </button>
          </div>
        )}

        {!loading && !error && content !== null && content.length === 0 && (
          <div className="flex items-center justify-center h-full">
            <p className="text-sm text-[#565f89]">File is empty</p>
          </div>
        )}

        {!loading && !error && content && (
          <div
            className="px-6 py-4 prose prose-invert prose-sm max-w-none
              prose-headings:text-[#c0caf5] prose-headings:border-b prose-headings:border-[#2a2d3d] prose-headings:pb-2
              prose-p:text-[#a9b1d6]
              prose-a:text-[#7aa2f7]
              prose-strong:text-[#c0caf5]
              prose-code:text-[#bb9af7] prose-code:bg-[#24283b] prose-code:px-1.5 prose-code:py-0.5 prose-code:rounded
              prose-li:text-[#a9b1d6]
              prose-hr:border-[#2a2d3d]
              prose-blockquote:border-[#7aa2f7] prose-blockquote:text-[#565f89]
              prose-th:text-[#c0caf5] prose-td:text-[#a9b1d6]"
          >
            <ReactMarkdown
              remarkPlugins={[remarkGfm]}
              rehypePlugins={[rehypeRaw]}
              components={SHARED_MARKDOWN_COMPONENTS}
            >
              {content}
            </ReactMarkdown>
          </div>
        )}
      </div>
    </div>
  );
}
