/**
 * ResultCardMount
 *
 * Renders the active result card (if any) from `useResultCard()`. Bottom-pinned,
 * positioned above the CommandBar (`absolute bottom-2 ... z-40 w-[420px]`, see
 * CommandBar.tsx:421) but below full-screen modals — DocFinderModal is
 * `fixed inset-0 z-50` (DocFinderModal.tsx:418), so the card uses `z-[45]` to
 * sit above CommandBar (z-40) yet below the modal (z-50).
 *
 * Dark-theme tokens + visual rhythm mirror the popovers in ZoneStatusBar.tsx
 * (MetricsPopover, ~884-997). Dismiss affordances are self-owned (document
 * mousedown for outside-click + document keydown for Escape), mirroring the
 * popover pattern at ZoneStatusBar.tsx:719-726 — deliberately NOT
 * `useKeyboardShortcuts`.
 */

import { useEffect, useRef } from "react";
import { X } from "lucide-react";
import { useResultCard } from "./ResultCardContext";

export function ResultCardMount() {
  const { card, dismissCard } = useResultCard();
  const cardRef = useRef<HTMLDivElement>(null);

  // Outside-click dismiss: any mousedown outside the card ref closes it.
  useEffect(() => {
    if (card === null) return;
    const handler = (e: MouseEvent) => {
      if (cardRef.current && !cardRef.current.contains(e.target as Node)) {
        dismissCard();
      }
    };
    document.addEventListener("mousedown", handler);
    return () => document.removeEventListener("mousedown", handler);
  }, [card, dismissCard]);

  // Esc dismiss.
  useEffect(() => {
    if (card === null) return;
    const handler = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        dismissCard();
      }
    };
    document.addEventListener("keydown", handler);
    return () => document.removeEventListener("keydown", handler);
  }, [card, dismissCard]);

  if (card === null) return null;

  return (
    <div className="absolute bottom-14 left-1/2 -translate-x-1/2 z-[45] w-[420px]">
      <div
        ref={cardRef}
        data-page-element="result-card"
        role="dialog"
        aria-label={card.title}
        className="bg-[#1a1b26] border border-[#2a2d3d] rounded-lg shadow-xl overflow-hidden"
      >
        {/* Header */}
        <div className="flex items-start gap-2 px-3 py-2 border-b border-[#2a2d3d]">
          <div className="flex-1 min-w-0">
            <div className="text-[10px] uppercase tracking-wider text-[#565f89] truncate">
              {card.title}
            </div>
            {card.subtitle && (
              <div className="text-[11px] text-[#a9b1d6] truncate mt-0.5">{card.subtitle}</div>
            )}
          </div>
          <button
            type="button"
            onClick={dismissCard}
            aria-label="Dismiss result card"
            title="Dismiss"
            className="text-[#565f89] hover:text-[#a9b1d6] hover:bg-[#2a2d3d]/50 rounded p-0.5 transition-colors shrink-0"
          >
            <X className="w-3.5 h-3.5" />
          </button>
        </div>

        {/* Body */}
        <div className="max-h-[60vh] overflow-y-auto scrollbar-dark p-3 text-[11px] space-y-2">
          {card.sections?.map((section, si) => (
            <div key={si} className="space-y-1.5">
              {section.heading && (
                <div className="text-[9px] uppercase tracking-wider text-[#565f89] mb-1">
                  {section.heading}
                </div>
              )}
              {section.rows.map((row, ri) => (
                <div key={ri} className="flex justify-between gap-3">
                  <span className="text-[#a9b1d6] truncate">{row.label}</span>
                  <span
                    className="font-mono text-right break-all"
                    style={row.valueColor ? { color: row.valueColor } : { color: "#c0caf5" }}
                  >
                    {row.value}
                  </span>
                </div>
              ))}
            </div>
          ))}
          {card.body}
        </div>

        {/* Footer */}
        {card.footer && (
          <div className="px-3 py-2 border-t border-[#2a2d3d]">
            <button
              type="button"
              onClick={() => {
                void card.footer?.onClick();
              }}
              className="w-full text-[10px] py-1 rounded transition-colors text-[#565f89] hover:text-[#a9b1d6] hover:bg-[#2a2d3d]/50"
            >
              {card.footer.label}
            </button>
          </div>
        )}
      </div>
    </div>
  );
}
