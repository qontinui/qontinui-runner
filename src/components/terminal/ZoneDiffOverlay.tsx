import { useState, useEffect, useRef } from "react";
import { X, Copy, Check } from "lucide-react";

interface ZoneDiffOverlayProps {
  leftLabel: string;
  rightLabel: string;
  leftLines: string[];
  rightLines: string[];
  onClose: () => void;
}

export function ZoneDiffOverlay({
  leftLabel,
  rightLabel,
  leftLines,
  rightLines,
  onClose,
}: ZoneDiffOverlayProps) {
  const [copied, setCopied] = useState(false);
  const overlayRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        onClose();
      }
    };
    window.addEventListener("keydown", handler, { capture: true });
    return () => window.removeEventListener("keydown", handler, { capture: true });
  }, [onClose]);

  // Simple line-by-line diff: compare by index, highlight differences
  const maxLen = Math.max(leftLines.length, rightLines.length);
  const diffRows: { left: string; right: string; same: boolean }[] = [];
  for (let i = 0; i < maxLen; i++) {
    const l = leftLines[i] ?? "";
    const r = rightLines[i] ?? "";
    diffRows.push({ left: l, right: r, same: l === r });
  }

  const diffText = diffRows
    .map((row) => {
      if (row.same) return `  ${row.left}`;
      return `- ${row.left}\n+ ${row.right}`;
    })
    .join("\n");

  const diffCount = diffRows.filter((r) => !r.same).length;

  const handleCopy = () => {
    navigator.clipboard.writeText(diffText).then(() => {
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    });
  };

  return (
    <div
      ref={overlayRef}
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 backdrop-blur-sm"
      role="button"
      tabIndex={0}
      aria-label="Close diff overlay"
      onClick={(e) => {
        if (e.target === overlayRef.current) onClose();
      }}
      onKeyDown={(e) => {
        if ((e.key === "Enter" || e.key === " ") && e.target === overlayRef.current) onClose();
      }}
    >
      <div className="bg-[#1a1b26] border border-[#2a2d3d] rounded-lg shadow-2xl w-[700px] max-h-[80vh] flex flex-col overflow-hidden">
        {/* Header */}
        <div className="flex items-center gap-2 px-4 py-3 border-b border-[#2a2d3d] shrink-0">
          <h2 className="text-sm font-medium text-[#c0caf5] flex-1">Output Diff</h2>
          <span className="text-[10px] text-[#565f89]">
            {diffCount} difference{diffCount !== 1 ? "s" : ""} / {maxLen} line
            {maxLen !== 1 ? "s" : ""}
          </span>
          <button
            onClick={handleCopy}
            className={`p-1 rounded transition-colors ${
              copied ? "text-[#9ece6a]" : "text-[#565f89] hover:text-[#a9b1d6] hover:bg-[#2a2d3d]"
            }`}
            title={copied ? "Copied!" : "Copy diff to clipboard"}
          >
            {copied ? <Check className="w-4 h-4" /> : <Copy className="w-4 h-4" />}
          </button>
          <button
            onClick={onClose}
            className="p-1 rounded hover:bg-[#2a2d3d] transition-colors text-[#565f89] hover:text-[#c0caf5]"
          >
            <X className="w-4 h-4" />
          </button>
        </div>

        {/* Column headers */}
        <div className="flex border-b border-[#2a2d3d] shrink-0">
          <div className="flex-1 px-3 py-1.5 text-[10px] font-medium text-[#f7768e] uppercase tracking-wider border-r border-[#2a2d3d]">
            {leftLabel}
          </div>
          <div className="flex-1 px-3 py-1.5 text-[10px] font-medium text-[#9ece6a] uppercase tracking-wider">
            {rightLabel}
          </div>
        </div>

        {/* Diff content */}
        <div className="flex-1 overflow-y-auto scrollbar-dark">
          {diffRows.length === 0 ? (
            <div className="px-4 py-8 text-center text-[12px] text-[#565f89]">
              No output to compare
            </div>
          ) : (
            diffRows.map((row, i) => (
              <div
                key={`diff-row-${i}`}
                className={`flex border-b border-[#2a2d3d]/30 ${row.same ? "" : "bg-[#2a2d3d]/20"}`}
              >
                <div
                  className={`flex-1 px-3 py-0.5 font-mono text-[10px] truncate border-r border-[#2a2d3d]/30 ${
                    row.same
                      ? "text-[#565f89]"
                      : row.left
                        ? "text-[#f7768e] bg-[#f7768e]/5"
                        : "text-[#565f89]/30 italic"
                  }`}
                  title={row.left}
                >
                  {row.left || "\u00A0"}
                </div>
                <div
                  className={`flex-1 px-3 py-0.5 font-mono text-[10px] truncate ${
                    row.same
                      ? "text-[#565f89]"
                      : row.right
                        ? "text-[#9ece6a] bg-[#9ece6a]/5"
                        : "text-[#565f89]/30 italic"
                  }`}
                  title={row.right}
                >
                  {row.right || "\u00A0"}
                </div>
              </div>
            ))
          )}
        </div>

        {/* Footer */}
        <div className="px-4 py-2 border-t border-[#2a2d3d] text-center shrink-0">
          <span className="text-[10px] text-[#565f89]">
            Press{" "}
            <kbd className="font-mono bg-[#2a2d3d] rounded px-1 py-0.5 text-[#a9b1d6]">Esc</kbd> to
            close
          </span>
        </div>
      </div>
    </div>
  );
}
