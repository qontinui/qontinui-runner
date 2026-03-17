/**
 * ElementDetailsPanel
 *
 * Right-panel component for displaying element details in the screenshot state view.
 * Shows element label, tag/role, bounding box coordinates, states membership with
 * confidence scores, screenshot presence checklist, element thumbnail, and fingerprint details.
 *
 * Used within the three-panel screenshot mode layout alongside StateDetailsPanel.
 */

import { Info, Layers } from "lucide-react";
import type { StateMachineState } from "@qontinui/shared-types";
import type { FingerprintDetail, CaptureScreenshotMeta } from "@qontinui/workflow-ui/state-machine";
import { STATE_COLORS } from "@qontinui/workflow-utils";

export interface ElementDetailsPanelProps {
  /** Currently selected element hash, or null if none selected */
  selectedElementHash: string | null;
  /** Fingerprint detail for the selected element */
  fingerprintDetail: FingerprintDetail | null;
  /** Element bounding box coordinates from the current capture */
  elementBounds: Record<string, { x: number; y: number; width: number; height: number }>;
  /** Map from element hash → state IDs the element belongs to */
  hashToStates: Map<string, string[]>;
  /** Map from element hash → capture indices where element appears */
  hashToCaptures: Map<string, number[]>;
  /** All states in the config */
  states: StateMachineState[];
  /** All capture screenshots */
  captureScreenshots: CaptureScreenshotMeta[];
  /** Current screenshot index */
  currentScreenshotIndex: number;
  /** Optional element thumbnails keyed by hash */
  elementThumbnails?: Record<string, string>;
  /** Optional fingerprint details for resolving labels */
  fingerprintDetails?: Record<string, FingerprintDetail>;
  /** Callback when user clicks a state to navigate */
  onSelectState: (stateId: string) => void;
  /** Callback when user clicks a screenshot index to navigate */
  onNavigateScreenshot: (index: number) => void;
}

export function ElementDetailsPanel({
  selectedElementHash,
  fingerprintDetail,
  elementBounds,
  hashToStates,
  hashToCaptures,
  states,
  captureScreenshots,
  currentScreenshotIndex,
  elementThumbnails,
  onSelectState,
  onNavigateScreenshot,
}: ElementDetailsPanelProps) {
  if (!selectedElementHash) {
    return (
      <div
        className="text-center text-text-muted py-8"
        aria-label="element details panel with tag name role bounding box coordinates"
      >
        <Info className="size-8 mx-auto mb-2 opacity-30" />
        <p className="text-xs">Click an element on the canvas to inspect it</p>
        <p className="text-[10px] mt-1 opacity-60">Ctrl+Click for multi-select</p>
      </div>
    );
  }

  const bounds = elementBounds[selectedElementHash];
  const stateIds = hashToStates.get(selectedElementHash) ?? [];
  const captureIndices = hashToCaptures.get(selectedElementHash) ?? [];
  const thumbnail = elementThumbnails?.[selectedElementHash];

  return (
    <div
      className="space-y-3"
      aria-label="element details panel with tag name role bounding box coordinates"
      data-testid="element-details-panel"
    >
      {/* Header: label + tag/role */}
      <div>
        <h4 className="text-sm font-semibold text-text-primary truncate">
          {fingerprintDetail?.accessibleName || selectedElementHash}
        </h4>
        {fingerprintDetail && (
          <div className="text-[10px] text-text-muted mt-0.5">
            &lt;{fingerprintDetail.tagName}&gt;
            {fingerprintDetail.role ? ` role="${fingerprintDetail.role}"` : ""}
          </div>
        )}
      </div>

      {/* Element thumbnail */}
      {thumbnail && (
        <div>
          <img
            src={thumbnail.startsWith("data:") ? thumbnail : `data:image/png;base64,${thumbnail}`}
            alt="Element thumbnail"
            className="w-full rounded border border-border-secondary"
          />
        </div>
      )}

      {/* Bounding box coordinates */}
      {bounds && (
        <div>
          <h5 className="text-[10px] font-medium text-text-muted uppercase tracking-wider mb-1">
            Bounding Box
          </h5>
          <div className="grid grid-cols-2 gap-1 text-[10px]">
            <div className="bg-bg-secondary rounded px-2 py-1">
              <span className="text-text-muted">x:</span>{" "}
              <span className="text-text-primary">{Math.round(bounds.x)}</span>
            </div>
            <div className="bg-bg-secondary rounded px-2 py-1">
              <span className="text-text-muted">y:</span>{" "}
              <span className="text-text-primary">{Math.round(bounds.y)}</span>
            </div>
            <div className="bg-bg-secondary rounded px-2 py-1">
              <span className="text-text-muted">w:</span>{" "}
              <span className="text-text-primary">{Math.round(bounds.width)}</span>
            </div>
            <div className="bg-bg-secondary rounded px-2 py-1">
              <span className="text-text-muted">h:</span>{" "}
              <span className="text-text-primary">{Math.round(bounds.height)}</span>
            </div>
          </div>
        </div>
      )}

      {/* Fingerprint details */}
      {fingerprintDetail && (
        <div>
          <h5 className="text-[10px] font-medium text-text-muted uppercase tracking-wider mb-1">
            Fingerprint
          </h5>
          <div className="space-y-1 text-[10px]">
            <div className="bg-bg-secondary rounded px-2 py-1">
              <span className="text-text-muted">Hash:</span>{" "}
              <code className="text-text-primary font-mono text-[9px]">
                {selectedElementHash.slice(0, 16)}...
              </code>
            </div>
            {fingerprintDetail.positionZone && (
              <div className="bg-bg-secondary rounded px-2 py-1">
                <span className="text-text-muted">Zone:</span>{" "}
                <span className="text-text-primary">{fingerprintDetail.positionZone}</span>
              </div>
            )}
            {fingerprintDetail.sizeCategory && (
              <div className="bg-bg-secondary rounded px-2 py-1">
                <span className="text-text-muted">Size:</span>{" "}
                <span className="text-text-primary">{fingerprintDetail.sizeCategory}</span>
              </div>
            )}
          </div>
        </div>
      )}

      {/* States membership with confidence scores */}
      {stateIds.length > 0 && (
        <div>
          <h5 className="text-[10px] font-medium text-text-muted uppercase tracking-wider mb-1">
            States Membership
          </h5>
          <div className="space-y-1">
            {stateIds.map((sid) => {
              const s = states.find((st) => st.state_id === sid);
              if (!s) return null;
              const colorIdx = states.indexOf(s);
              const color = STATE_COLORS[colorIdx % STATE_COLORS.length]!;
              return (
                <button
                  key={sid}
                  onClick={() => onSelectState(sid)}
                  className="w-full flex items-center gap-2 text-[10px] px-2 py-1 rounded bg-bg-secondary hover:bg-bg-tertiary text-left"
                >
                  <div
                    className="w-2 h-2 rounded-full shrink-0"
                    style={{ backgroundColor: color.border }}
                  />
                  <span className="text-text-primary truncate">{s.name}</span>
                  <span
                    className={`ml-auto shrink-0 ${
                      Math.round(s.confidence * 100) >= 80
                        ? "text-green-400"
                        : "text-amber-400"
                    }`}
                  >
                    {Math.round(s.confidence * 100)}%
                  </span>
                </button>
              );
            })}
          </div>
        </div>
      )}

      {/* Screenshot presence checklist */}
      {captureIndices.length > 0 && (
        <div>
          <h5 className="text-[10px] font-medium text-text-muted uppercase tracking-wider mb-1">
            Screenshot Presence
          </h5>
          <div className="flex flex-wrap gap-1">
            {captureScreenshots.map((cap, idx) => {
              const present = captureIndices.includes(idx);
              return (
                <button
                  key={cap.id}
                  onClick={() => onNavigateScreenshot(idx)}
                  className={`text-[9px] px-1.5 py-0.5 rounded ${
                    present
                      ? idx === currentScreenshotIndex
                        ? "bg-brand-primary/20 text-brand-primary border border-brand-primary/30"
                        : "bg-green-500/10 text-green-400 border border-green-500/20"
                      : "bg-bg-secondary text-text-muted border border-border-secondary opacity-50"
                  }`}
                >
                  #{idx + 1} {present ? "\u2713" : ""}
                </button>
              );
            })}
          </div>
        </div>
      )}
    </div>
  );
}
