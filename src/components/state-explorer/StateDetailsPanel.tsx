/**
 * StateDetailsPanel
 *
 * Right-panel component for displaying state details in the screenshot state view.
 * Shows state name with confidence badge, element count + screenshot count stats,
 * scrollable element list with thumbnails, screenshot list, acceptance_criteria
 * as checklist, and domain_knowledge as collapsible cards.
 *
 * Used within the three-panel screenshot mode layout alongside ElementDetailsPanel.
 */

import { useState } from "react";
import { CheckCircle, ChevronDown, ChevronRight, Layers } from "lucide-react";
import type { StateMachineState } from "@qontinui/shared-types";
import type { FingerprintDetail, CaptureScreenshotMeta } from "@qontinui/workflow-ui/state-machine";
import { STATE_COLORS, getElementLabel } from "@qontinui/workflow-utils";

export interface StateDetailsPanelProps {
  /** Currently selected state, or null if none */
  selectedState: StateMachineState | null;
  /** All states for color indexing */
  states: StateMachineState[];
  /** All capture screenshots */
  captureScreenshots: CaptureScreenshotMeta[];
  /** Set of element hashes belonging to selected state */
  selectedStateHashes: Set<string>;
  /** Current screenshot index */
  currentScreenshotIndex: number;
  /** Optional element thumbnails keyed by hash */
  elementThumbnails?: Record<string, string>;
  /** Optional fingerprint details for resolving labels */
  fingerprintDetails?: Record<string, FingerprintDetail>;
  /** Callback when user clicks element to select it */
  onSelectElement: (hash: string) => void;
  /** Callback when user clicks screenshot to navigate */
  onNavigateScreenshot: (index: number) => void;
}

function getFingerprintHash(elementId: string): string {
  const idx = elementId.indexOf(":");
  return idx > 0 ? elementId.slice(idx + 1) : elementId;
}

function resolveElementLabel(
  elementId: string,
  fingerprintDetails?: Record<string, FingerprintDetail>,
  state?: StateMachineState,
): string {
  const hash = getFingerprintHash(elementId);
  const fp = fingerprintDetails?.[hash] ?? fingerprintDetails?.[elementId];
  if (fp) {
    if (fp.accessibleName) return fp.accessibleName;
    const parts = [fp.tagName, fp.role].filter(Boolean);
    if (parts.length > 0) return parts.join(" ");
  }
  const labels = state?.extra_metadata?.elementLabels as Record<string, string> | undefined;
  if (labels?.[elementId]) return labels[elementId];
  return getElementLabel(elementId);
}

export function StateDetailsPanel({
  selectedState,
  states,
  captureScreenshots,
  selectedStateHashes,
  currentScreenshotIndex,
  elementThumbnails,
  fingerprintDetails,
  onSelectElement,
  onNavigateScreenshot,
}: StateDetailsPanelProps) {
  const [expandedDk, setExpandedDk] = useState<Set<string>>(new Set());

  if (!selectedState) {
    return (
      <div
        className="text-center text-text-muted py-8"
        aria-label="state details panel with confidence element count screenshot count"
      >
        <Layers className="size-8 mx-auto mb-2 opacity-30" />
        <p className="text-xs">Select a state to view details</p>
      </div>
    );
  }

  const colorIdx = states.indexOf(selectedState);
  const stateColor = STATE_COLORS[colorIdx >= 0 ? colorIdx % STATE_COLORS.length : 0];

  // Count screenshots containing this state's elements
  const screenshotCount = captureScreenshots.filter((cap) => {
    try {
      const hashes = JSON.parse(cap.fingerprintHashesJson) as string[];
      return hashes.some((h) => selectedStateHashes.has(h));
    } catch {
      return false;
    }
  }).length;

  return (
    <div
      className="space-y-3"
      aria-label="state details panel with confidence element count screenshot count"
      data-testid="state-details-panel"
    >
      {/* Header: name + confidence badge */}
      <div>
        <div className="flex items-center gap-2">
          <div
            className="w-2.5 h-2.5 rounded-full shrink-0"
            style={{ backgroundColor: stateColor?.border }}
          />
          <h4 className="text-sm font-semibold text-text-primary truncate">{selectedState.name}</h4>
          <span
            className={`text-[10px] px-1.5 py-0.5 rounded-full shrink-0 ${
              Math.round(selectedState.confidence * 100) >= 80
                ? "bg-green-500/10 text-green-400 border border-green-500/30"
                : "bg-amber-500/10 text-amber-400 border border-amber-500/30"
            }`}
          >
            {Math.round(selectedState.confidence * 100)}%
          </span>
        </div>
        {selectedState.description && (
          <p className="text-[10px] text-text-muted mt-1">{selectedState.description}</p>
        )}
        {/* Stats */}
        <div className="flex items-center gap-2 mt-1.5 text-[10px] text-text-muted">
          <span>{selectedState.element_ids.length} elements</span>
          <span>&middot;</span>
          <span>{screenshotCount} screenshots</span>
        </div>
      </div>

      {/* Scrollable element list with thumbnails */}
      <div>
        <h5 className="text-[10px] font-medium text-text-muted uppercase tracking-wider mb-1">
          Elements
        </h5>
        <div className="space-y-0.5 max-h-60 overflow-y-auto">
          {selectedState.element_ids.map((eid) => {
            const hash = getFingerprintHash(eid);
            const label = resolveElementLabel(eid, fingerprintDetails, selectedState);
            const thumb = elementThumbnails?.[hash] ?? elementThumbnails?.[eid];
            return (
              <button
                key={eid}
                onClick={() => onSelectElement(hash)}
                className="w-full flex items-center gap-1.5 text-[10px] px-2 py-1 rounded text-left hover:bg-bg-secondary text-text-primary"
              >
                {thumb ? (
                  <img
                    src={thumb.startsWith("data:") ? thumb : `data:image/png;base64,${thumb}`}
                    alt={label}
                    className="w-5 h-5 object-cover rounded shrink-0"
                  />
                ) : (
                  <Layers className="size-3 text-text-muted shrink-0" />
                )}
                <span className="truncate">{label}</span>
              </button>
            );
          })}
        </div>
      </div>

      {/* Screenshot list - click to navigate */}
      <div>
        <h5 className="text-[10px] font-medium text-text-muted uppercase tracking-wider mb-1">
          Screenshots
        </h5>
        <div className="flex flex-wrap gap-1">
          {captureScreenshots.map((cap, idx) => {
            let hasElements = false;
            try {
              const hashes = JSON.parse(cap.fingerprintHashesJson) as string[];
              hasElements = hashes.some((h) => selectedStateHashes.has(h));
            } catch {
              /* skip */
            }
            if (!hasElements) return null;
            return (
              <button
                key={cap.id}
                onClick={() => onNavigateScreenshot(idx)}
                className={`text-[9px] px-1.5 py-0.5 rounded border ${
                  idx === currentScreenshotIndex
                    ? "bg-brand-primary/20 text-brand-primary border-brand-primary/30"
                    : "bg-bg-secondary text-text-muted border-border-secondary hover:border-text-muted"
                }`}
              >
                Capture #{idx + 1}
              </button>
            );
          })}
        </div>
      </div>

      {/* Acceptance criteria as checklist */}
      {selectedState.acceptance_criteria.length > 0 && (
        <div>
          <h5 className="text-[10px] font-medium text-text-muted uppercase tracking-wider mb-1">
            Acceptance Criteria
          </h5>
          <ul className="space-y-0.5">
            {selectedState.acceptance_criteria.map((ac, i) => (
              <li key={i} className="flex items-start gap-1 text-[10px] text-text-muted">
                <CheckCircle className="size-3 text-green-500 mt-0.5 shrink-0" />
                <span>{ac}</span>
              </li>
            ))}
          </ul>
        </div>
      )}

      {/* Domain knowledge as collapsible cards */}
      {selectedState.domain_knowledge.length > 0 && (
        <div>
          <h5 className="text-[10px] font-medium text-text-muted uppercase tracking-wider mb-1">
            Domain Knowledge
          </h5>
          <div className="space-y-1.5">
            {selectedState.domain_knowledge.map((dk) => {
              const isExpanded = expandedDk.has(dk.id);
              return (
                <div
                  key={dk.id}
                  className="p-2 rounded bg-bg-secondary border border-border-secondary"
                >
                  <button
                    onClick={() => {
                      setExpandedDk((prev) => {
                        const next = new Set(prev);
                        if (next.has(dk.id)) next.delete(dk.id);
                        else next.add(dk.id);
                        return next;
                      });
                    }}
                    className="w-full flex items-center gap-1 text-left"
                  >
                    {isExpanded ? (
                      <ChevronDown className="size-3 text-text-muted shrink-0" />
                    ) : (
                      <ChevronRight className="size-3 text-text-muted shrink-0" />
                    )}
                    <span className="text-[10px] font-medium text-text-primary truncate">
                      {dk.title}
                    </span>
                  </button>
                  {isExpanded && (
                    <div className="mt-1 pl-4">
                      <div className="text-[9px] text-text-muted">{dk.content}</div>
                      {dk.tags.length > 0 && (
                        <div className="flex flex-wrap gap-0.5 mt-1">
                          {dk.tags.map((tag) => (
                            <span
                              key={tag}
                              className="text-[8px] px-1 py-0.5 rounded-full bg-brand-primary/10 text-brand-primary"
                            >
                              {tag}
                            </span>
                          ))}
                        </div>
                      )}
                    </div>
                  )}
                </div>
              );
            })}
          </div>
        </div>
      )}
    </div>
  );
}
