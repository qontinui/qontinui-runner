/**
 * HtmlViewerModal.tsx
 *
 * Modal for viewing captured HTML content with syntax highlighting.
 */

import { useState, useEffect, useRef } from "react";
import { X, Copy, Download, Check, Search, Globe, FileCode, Clock, RefreshCw } from "lucide-react";
import { useDomCaptureHtml } from "../../hooks/useDomCaptures";
import type { DomCapture } from "../../types/domCapture";

/** Format bytes to human readable size */
function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

/** Format timestamp to readable date/time */
function formatDateTime(timestamp: string): string {
  return new Date(timestamp).toLocaleString();
}

/** Escape HTML for display */
function escapeHtml(html: string): string {
  return html
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&#039;");
}

/** Basic HTML syntax highlighting */
function highlightHtml(html: string): string {
  // Escape first
  let highlighted = escapeHtml(html);

  // Highlight tags
  highlighted = highlighted.replace(
    /(&lt;\/?)([\w-]+)/g,
    '$1<span class="text-blue-400">$2</span>',
  );

  // Highlight attributes
  highlighted = highlighted.replace(/\s([\w-]+)=/g, ' <span class="text-yellow-400">$1</span>=');

  // Highlight attribute values
  highlighted = highlighted.replace(
    /(&quot;[^&]*&quot;)/g,
    '<span class="text-green-400">$1</span>',
  );

  // Highlight comments
  highlighted = highlighted.replace(
    /(&lt;!--[\s\S]*?--&gt;)/g,
    '<span class="text-gray-500">$1</span>',
  );

  return highlighted;
}

interface Props {
  capture: DomCapture;
  onClose: () => void;
}

export function HtmlViewerModal({ capture, onClose }: Props) {
  const { data: html, isLoading, error } = useDomCaptureHtml(capture.id);
  const [copied, setCopied] = useState(false);
  const [searchQuery, setSearchQuery] = useState("");
  const [showLineNumbers, setShowLineNumbers] = useState(true);
  const contentRef = useRef<HTMLPreElement>(null);

  // Handle escape key to close
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        onClose();
      }
      // Ctrl+F to focus search
      if (e.ctrlKey && e.key === "f") {
        e.preventDefault();
        document.getElementById("html-search")?.focus();
      }
    };
    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, [onClose]);

  const handleCopy = async () => {
    if (!html) return;
    try {
      await navigator.clipboard.writeText(html);
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    } catch (err) {
      console.error("Failed to copy:", err);
    }
  };

  const handleDownload = () => {
    if (!html) return;
    const blob = new Blob([html], { type: "text/html" });
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = `${capture.id}.html`;
    document.body.appendChild(a);
    a.click();
    document.body.removeChild(a);
    URL.revokeObjectURL(url);
  };

  // Split HTML into lines for display
  const lines = html?.split("\n") || [];
  const filteredLines = searchQuery
    ? lines.filter((line) => line.toLowerCase().includes(searchQuery.toLowerCase()))
    : lines;

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50 backdrop-blur-sm">
      <div className="bg-background border border-border rounded-lg shadow-xl w-[90vw] h-[90vh] flex flex-col overflow-hidden">
        {/* Header */}
        <div className="flex items-center justify-between p-4 border-b border-border flex-shrink-0">
          <div className="flex items-center gap-3 min-w-0">
            <FileCode className="w-5 h-5 text-primary flex-shrink-0" />
            <div className="min-w-0">
              <h2 className="font-medium truncate">{capture.pageTitle}</h2>
              <div className="flex items-center gap-2 text-xs text-muted-foreground">
                <Globe className="w-3 h-3" />
                <span className="truncate">{capture.url}</span>
                <span className="text-muted-foreground/50">|</span>
                <Clock className="w-3 h-3" />
                <span>{formatDateTime(capture.timestamp)}</span>
                <span className="text-muted-foreground/50">|</span>
                <span>{formatSize(capture.htmlSizeBytes)}</span>
              </div>
            </div>
          </div>

          <div className="flex items-center gap-2">
            {/* Search */}
            <div className="relative">
              <Search className="absolute left-3 top-1/2 -translate-y-1/2 w-4 h-4 text-muted-foreground" />
              <input
                id="html-search"
                type="text"
                placeholder="Search (Ctrl+F)"
                value={searchQuery}
                onChange={(e) => setSearchQuery(e.target.value)}
                className="pl-9 pr-3 py-1.5 text-sm bg-muted border border-border rounded-md w-48 focus:outline-none focus:ring-2 focus:ring-primary/50"
              />
            </div>

            {/* Actions */}
            <button
              onClick={handleCopy}
              disabled={!html}
              className="p-2 rounded-md hover:bg-muted transition-colors disabled:opacity-50"
              title="Copy to clipboard"
            >
              {copied ? <Check className="w-4 h-4 text-green-500" /> : <Copy className="w-4 h-4" />}
            </button>
            <button
              onClick={handleDownload}
              disabled={!html}
              className="p-2 rounded-md hover:bg-muted transition-colors disabled:opacity-50"
              title="Download as HTML"
            >
              <Download className="w-4 h-4" />
            </button>
            <button
              onClick={onClose}
              className="p-2 rounded-md hover:bg-muted transition-colors"
              title="Close (Escape)"
            >
              <X className="w-4 h-4" />
            </button>
          </div>
        </div>

        {/* Content */}
        <div className="flex-1 overflow-auto bg-[#1e1e1e]">
          {isLoading ? (
            <div className="flex items-center justify-center h-full">
              <RefreshCw className="w-6 h-6 animate-spin text-muted-foreground" />
            </div>
          ) : error ? (
            <div className="flex flex-col items-center justify-center h-full text-red-500">
              <p>Failed to load HTML content</p>
              <p className="text-sm text-muted-foreground mt-1">{String(error)}</p>
            </div>
          ) : (
            <div className="flex">
              {/* Line numbers */}
              {showLineNumbers && (
                <div className="flex-shrink-0 select-none text-right pr-4 py-4 text-xs text-muted-foreground bg-[#1a1a1a] border-r border-border">
                  {filteredLines.map((_, i) => (
                    <div key={`ln-${i}`} className="px-2 leading-5">
                      {searchQuery ? i + 1 : i + 1}
                    </div>
                  ))}
                </div>
              )}

              {/* Code content */}
              <pre
                ref={contentRef}
                className="flex-1 p-4 text-xs font-mono text-foreground overflow-x-auto leading-5"
              >
                {filteredLines.map((line, i) => (
                  <div
                    key={`code-${i}`}
                    dangerouslySetInnerHTML={{ __html: highlightHtml(line) }}
                    className={
                      searchQuery && line.toLowerCase().includes(searchQuery.toLowerCase())
                        ? "bg-yellow-500/20"
                        : ""
                    }
                  />
                ))}
              </pre>
            </div>
          )}
        </div>

        {/* Footer */}
        <div className="flex items-center justify-between px-4 py-2 border-t border-border text-xs text-muted-foreground flex-shrink-0">
          <div className="flex items-center gap-4">
            <label className="flex items-center gap-1.5 cursor-pointer">
              <input
                type="checkbox"
                checked={showLineNumbers}
                onChange={(e) => setShowLineNumbers(e.target.checked)}
                className="rounded"
              />
              Line numbers
            </label>
            {searchQuery && (
              <span>
                {filteredLines.length} of {lines.length} lines match
              </span>
            )}
          </div>
          <div>
            {lines.length} lines | {capture.captureType === "selector" ? "Partial" : "Full"} capture
          </div>
        </div>
      </div>
    </div>
  );
}
