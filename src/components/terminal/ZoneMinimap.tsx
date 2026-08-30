import { X } from "lucide-react";
import { useUIElement } from "@qontinui/ui-bridge";
import type { SessionState } from "./useZoneLayout";
import { useTerminalSession, useZoneMetadata, useUIStateCx } from "./contexts";

const STATE_COLORS: Record<SessionState, string> = {
  idle: "#565f89",
  working: "#7aa2f7",
  "needs-input": "#e0af68",
  completed: "#9ece6a",
  error: "#f7768e",
};

/** Parse CSS grid value like "1", "2", "1 / 3" → [startIndex, span] (0-based start) */
function parseGridValue(value: string, _totalTracks: number): [number, number] {
  const parts = value.split("/").map((s) => s.trim());
  const start = parseInt(parts[0], 10) - 1; // 0-based
  if (parts.length > 1) {
    const end = parseInt(parts[1], 10) - 1;
    return [start, end - start];
  }
  return [start, 1];
}

export function ZoneMinimap() {
  const session = useTerminalSession();
  const zoneLayout = session.zoneLayout;
  const layout = zoneLayout.layout;
  const assignments = zoneLayout.assignments;
  const focusedZone = zoneLayout.focusedZone;
  const maximizedZone = zoneLayout.maximizedZone;
  const onFocusZone = zoneLayout.setFocusedZone;
  const onMaximizeZone = zoneLayout.setMaximizedZone;
  const selectZone = (idx: number) => {
    onFocusZone(idx);
    // When a zone is maximized the grid renders only that zone, so the
    // minimap is the only switcher available — swap the maximized view
    // to the clicked tile instead of leaving the user stuck on the
    // previous one.
    if (maximizedZone !== null) onMaximizeZone(idx);
  };
  const sessionStates = session.sessionStates;
  const { labelsAndTags } = useZoneMetadata();
  const zoneTags = labelsAndTags.zoneTags;
  const labelColorMap = labelsAndTags.labelColorMap;

  /*
   * Visibility is SHARED state, not local `useState`.
   *
   * It used to be local, which made the X button a one-way door: once
   * dismissed there was no control anywhere in the UI that could bring the
   * minimap back, and the choice was forgotten on the next remount — the
   * worst of both, un-undoable AND not remembered. The StatusStrip toggle
   * needs to read and write the same flag, so it lives in `useUIState`
   * (persisted per instance under `zone-minimap`) alongside the other
   * overlay toggles.
   */
  const { state: uiState, toggleMinimap } = useUIStateCx();

  /*
   * Register the minimap itself, not just the things it floats over.
   *
   * A floating widget that is invisible to UI Bridge cannot be named as the
   * cause of an occlusion: an audit walking the registered element graph
   * sees the covered `terminal-zone-header-*` and nothing on top of it, so
   * the only honest verdict it can reach is "no overlap found". Registering
   * the occluder is what makes an autonomous "does anything cover the
   * session name?" check able to answer at all.
   */
  const { ref: minimapRef } = useUIElement({
    id: "terminal-zone-minimap",
    type: "generic",
    label: "Zone minimap",
  });

  if (!uiState.showMinimap || !zoneLayout.isMultiZone) return null;

  const { columns, rows } = layout;
  const mapW = 120;
  const mapH = 80;
  const cellW = mapW / columns;
  const cellH = mapH / rows;

  // Compute center coordinates for each zone
  const zoneCenters: Record<number, { x: number; y: number }> = {};
  layout.zones.forEach((zone, idx) => {
    const [colStart, colSpan] = parseGridValue(zone.col, columns);
    const [rowStart, rowSpan] = parseGridValue(zone.row, rows);
    const x = colStart * cellW + (colSpan * cellW) / 2;
    const y = rowStart * cellH + (rowSpan * cellH) / 2;
    zoneCenters[idx] = { x, y };
  });

  // Compute connections between zones that share tags
  const connections: { from: number; to: number; tag: string; color: string }[] = [];
  if (zoneTags && labelColorMap) {
    const zoneIndices = Object.keys(zoneTags).map(Number);
    for (let i = 0; i < zoneIndices.length; i++) {
      for (let j = i + 1; j < zoneIndices.length; j++) {
        const z1 = zoneIndices[i],
          z2 = zoneIndices[j];
        const shared = zoneTags[z1]?.filter((t) => zoneTags[z2]?.includes(t));
        if (shared && shared.length > 0) {
          connections.push({
            from: z1,
            to: z2,
            tag: shared[0],
            color: labelColorMap[shared[0]] ?? "#bb9af7",
          });
        }
      }
    }
  }

  return (
    /*
     * Top-right, clear of the zone chrome — NOT bottom-right.
     *
     * At `bottom-2 right-2` this 128x88 box floats over whatever the zone
     * grid happens to put in the bottom-right corner, and in flow mode
     * (the synthesized scrolling grid past 9 zones) that is routinely a
     * tile HEADER — so the minimap covered the session name, or truncated
     * it to a fragment. A widget is not allowed to hide the one label that
     * identifies which session a tile belongs to.
     *
     * The `top` offset is the zone title bar's own height: `ZoneLabel`
     * (multi-zone) and the D1 solo session-info strip both occupy a 20px
     * strip at `top-0`, and `ZoneQuickActions` / `ZoneHoverActions` sit at
     * `top-1 right-1` (~22px tall, so their bottom edge lands near 26px).
     * `top-7` (28px) clears both, which is why it is not `top-2`: the
     * minimap must sit BELOW the title bar, not on top of a different
     * piece of chrome.
     *
     * The widget stays registered with UI Bridge (see `useUIElement`
     * above) so an automated audit can see the occluder, not just the
     * things it occludes.
     */
    <div ref={minimapRef} className="absolute top-7 right-2 z-30 group">
      <div
        className="relative bg-[#13141f]/80 border border-[#2a2d3d] rounded-md shadow-lg backdrop-blur-sm"
        style={{ width: mapW + 8, height: mapH + 8, padding: 4 }}
      >
        {/* Dismiss button */}
        <button
          onClick={toggleMinimap}
          title="Hide the zone minimap (restore it from the status strip)"
          aria-label="Hide the zone minimap"
          className="absolute -top-1.5 -right-1.5 w-4 h-4 rounded-full bg-[#2a2d3d] text-[#565f89] hover:text-[#c0caf5] flex items-center justify-center opacity-0 group-hover:opacity-100 transition-opacity z-10"
        >
          <X className="w-2.5 h-2.5" />
        </button>

        <svg width={mapW} height={mapH}>
          {layout.zones.map((zone, idx) => {
            const tabId = assignments[idx];
            const state = tabId ? (sessionStates[tabId] ?? "idle") : "idle";
            const isFocused = idx === focusedZone;
            const [colStart, colSpan] = parseGridValue(zone.col, columns);
            const [rowStart, rowSpan] = parseGridValue(zone.row, rows);
            const x = colStart * cellW;
            const y = rowStart * cellH;
            const w = colSpan * cellW;
            const h = rowSpan * cellH;

            return (
              <g key={`zone-${idx}`} onClick={() => selectZone(idx)} className="cursor-pointer">
                <rect
                  x={x + 1}
                  y={y + 1}
                  width={w - 2}
                  height={h - 2}
                  rx={2}
                  fill={STATE_COLORS[state]}
                  opacity={tabId ? 0.5 : 0.15}
                />
                {isFocused && (
                  <rect
                    x={x + 1}
                    y={y + 1}
                    width={w - 2}
                    height={h - 2}
                    rx={2}
                    fill="none"
                    stroke="#c0caf5"
                    strokeWidth={1.5}
                  />
                )}
                <text
                  x={x + w / 2}
                  y={y + h / 2 + 3}
                  textAnchor="middle"
                  className="text-[8px] font-mono fill-[#c0caf5] pointer-events-none select-none"
                  opacity={0.8}
                >
                  {idx + 1}
                </text>
              </g>
            );
          })}
          {connections.map((conn, i) => {
            const fromCenter = zoneCenters[conn.from];
            const toCenter = zoneCenters[conn.to];
            if (!fromCenter || !toCenter) return null;
            const midX = (fromCenter.x + toCenter.x) / 2;
            const midY = (fromCenter.y + toCenter.y) / 2 - 8;
            return (
              <path
                key={`conn-${i}`}
                d={`M${fromCenter.x},${fromCenter.y} Q${midX},${midY} ${toCenter.x},${toCenter.y}`}
                fill="none"
                stroke={conn.color}
                strokeWidth={0.8}
                opacity={0.3}
              />
            );
          })}
        </svg>
      </div>
    </div>
  );
}
