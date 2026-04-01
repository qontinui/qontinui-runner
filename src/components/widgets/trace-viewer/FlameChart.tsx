import React, { useRef, useEffect, useState, useCallback, useMemo } from "react";
import type {
  TraceSpan,
  FlameNode,
  CriticalPathInfo,
  TraceTreeNode,
  TokenOverlayMode,
} from "./types";
import {
  buildFlameData,
  flattenFlameNodes,
  formatDuration,
  getTokenHeat,
  computeTokenInsights,
} from "./trace-utils";

// Canvas color mapping for phases (not Tailwind — raw hex for canvas)
const PHASE_HEX: Record<string, { bg: string; text: string }> = {
  setup: { bg: "#3b82f6", text: "#93c5fd" },
  verification: { bg: "#f59e0b", text: "#fcd34d" },
  agentic: { bg: "#a855f7", text: "#c4b5fd" },
  completion: { bg: "#10b981", text: "#6ee7b7" },
  ai: { bg: "#22c55e", text: "#86efac" },
  python: { bg: "#f97316", text: "#fdba74" },
  unknown: { bg: "#71717a", text: "#a1a1aa" },
};

const ROW_HEIGHT = 22;
const ROW_GAP = 2;
const NAME_MIN_WIDTH = 60;

function blendHeatColor(baseColor: string, heat: number): string {
  if (heat <= 0) return baseColor;
  const r = Math.round(239 * heat + parseInt(baseColor.slice(1, 3), 16) * (1 - heat));
  const g = Math.round(68 * heat + parseInt(baseColor.slice(3, 5), 16) * (1 - heat));
  const b = Math.round(68 * heat + parseInt(baseColor.slice(5, 7), 16) * (1 - heat));
  return `#${r.toString(16).padStart(2, "0")}${g.toString(16).padStart(2, "0")}${b.toString(16).padStart(2, "0")}`;
}

interface FlameChartProps {
  roots: TraceTreeNode[];
  onSelectSpan: (span: TraceSpan) => void;
  selectedSpanId: string | null;
  criticalPath: CriticalPathInfo | null;
  height: number;
  tokenOverlay?: TokenOverlayMode;
  tokenInsights?: ReturnType<typeof computeTokenInsights>;
}

interface ViewState {
  /** Horizontal zoom factor (1 = fit all). */
  zoom: number;
  /** Horizontal pan offset in fraction [0, 1). */
  panX: number;
}

export const FlameChart: React.FC<FlameChartProps> = ({
  roots,
  onSelectSpan,
  selectedSpanId,
  criticalPath,
  height,
  tokenOverlay,
  tokenInsights,
}) => {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const containerRef = useRef<HTMLDivElement>(null);
  const [view, setView] = useState<ViewState>({ zoom: 1, panX: 0 });
  const [maxDepth, setMaxDepth] = useState<number>(20);
  const [focusStack, setFocusStack] = useState<FlameNode[]>([]);
  const [hoveredSpan, setHoveredSpan] = useState<{ span: TraceSpan; x: number; y: number } | null>(
    null,
  );
  const [canvasSize, setCanvasSize] = useState({ width: 800, height: 400 });
  const [isDragging, setIsDragging] = useState(false);
  const dragRef = useRef<{ startX: number; startPanX: number } | null>(null);

  // Build flame data
  const flameRoots = useMemo(() => buildFlameData(roots), [roots]);
  const allFlameNodes = useMemo(() => flattenFlameNodes(flameRoots), [flameRoots]);

  // If focused on a subtree, filter to that subtree
  const displayRoot = focusStack.length > 0 ? focusStack[focusStack.length - 1] : null;
  const displayNodes = useMemo(() => {
    if (!displayRoot) return allFlameNodes;
    return flattenFlameNodes([displayRoot]);
  }, [displayRoot, allFlameNodes]);

  // Compute max depth available
  const actualMaxDepth = useMemo(
    () => displayNodes.reduce((max, n) => Math.max(max, n.depth), 0),
    [displayNodes],
  );

  // Observe container size
  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;
    const observer = new ResizeObserver((entries) => {
      for (const entry of entries) {
        const { width, height: h } = entry.contentRect;
        setCanvasSize({ width: Math.floor(width), height: Math.floor(h) });
      }
    });
    observer.observe(container);
    return () => observer.disconnect();
  }, []);

  // Render canvas
  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;

    const dpr = window.devicePixelRatio || 1;
    canvas.width = canvasSize.width * dpr;
    canvas.height = canvasSize.height * dpr;
    ctx.scale(dpr, dpr);

    // Clear
    ctx.fillStyle = "#18181b"; // zinc-900
    ctx.fillRect(0, 0, canvasSize.width, canvasSize.height);

    const totalWidth = canvasSize.width * view.zoom;
    const offsetX = -view.panX * totalWidth;

    // Base depth offset for focused subtree
    const baseDepth = displayRoot ? displayRoot.depth : 0;

    // Reference x range for focused subtree
    const refXStart = displayRoot ? displayRoot.xStart : 0;
    const refXEnd = displayRoot ? displayRoot.xEnd : 1;
    const refWidth = refXEnd - refXStart || 1;

    for (const node of displayNodes) {
      const relativeDepth = node.depth - baseDepth;
      if (relativeDepth > maxDepth) continue;

      const y = relativeDepth * (ROW_HEIGHT + ROW_GAP);
      if (y > canvasSize.height) continue;

      // Map x positions relative to display root
      const normXStart = (node.xStart - refXStart) / refWidth;
      const normXEnd = (node.xEnd - refXStart) / refWidth;

      const x = offsetX + normXStart * totalWidth;
      const w = Math.max((normXEnd - normXStart) * totalWidth, 2);

      // Skip if out of viewport
      if (x + w < 0 || x > canvasSize.width) continue;

      const phase = node.phase;
      const colors = PHASE_HEX[phase] || PHASE_HEX.unknown;

      // Dimming for non-critical path
      const onCritPath = criticalPath?.spanIds.has(node.span.span_id);
      const alpha = criticalPath && !onCritPath ? 0.35 : 1;

      ctx.globalAlpha = alpha;

      // Background
      const isSelected = node.span.span_id === selectedSpanId;
      let baseBg = colors.bg;
      if (tokenOverlay && tokenOverlay !== "none" && tokenInsights) {
        const maxVal =
          tokenOverlay === "input"
            ? tokenInsights.maxInputTokens
            : tokenOverlay === "output"
              ? tokenInsights.maxOutputTokens
              : tokenInsights.maxCostCents;
        const heat = getTokenHeat(node.span, tokenOverlay, maxVal);
        baseBg = blendHeatColor(colors.bg, heat);
      }
      ctx.fillStyle = isSelected ? "#ffffff20" : baseBg + "99";
      ctx.fillRect(x, y, w, ROW_HEIGHT);

      // Border
      if (isSelected) {
        ctx.strokeStyle = "#ffffff60";
        ctx.lineWidth = 2;
        ctx.strokeRect(x, y, w, ROW_HEIGHT);
      } else if (onCritPath && criticalPath) {
        ctx.strokeStyle = "#ef4444";
        ctx.lineWidth = 1.5;
        ctx.strokeRect(x, y, w, ROW_HEIGHT);
      }

      // Error indicator
      if (!node.span.success) {
        ctx.fillStyle = "#ef4444";
        ctx.fillRect(x, y, 3, ROW_HEIGHT);
      }

      // Text label (only if wide enough)
      if (w > NAME_MIN_WIDTH) {
        const label = node.span.name.replace(/^qontinui\./, "");
        const dur = formatDuration(node.span.duration_ms ?? 0);
        const text = `${label} (${dur})`;

        ctx.fillStyle = colors.text;
        ctx.font = "11px ui-monospace, monospace";
        ctx.textBaseline = "middle";

        // Clip text to rectangle
        ctx.save();
        ctx.beginPath();
        ctx.rect(x + 4, y, w - 8, ROW_HEIGHT);
        ctx.clip();
        ctx.fillText(text, x + 4, y + ROW_HEIGHT / 2);
        ctx.restore();
      }

      ctx.globalAlpha = 1;
    }
  }, [
    displayNodes,
    canvasSize,
    view,
    maxDepth,
    selectedSpanId,
    criticalPath,
    displayRoot,
    tokenOverlay,
    tokenInsights,
  ]);

  // Hit-test helper
  const hitTest = useCallback(
    (clientX: number, clientY: number): FlameNode | null => {
      const canvas = canvasRef.current;
      if (!canvas) return null;
      const rect = canvas.getBoundingClientRect();
      const mx = clientX - rect.left;
      const my = clientY - rect.top;

      const totalWidth = canvasSize.width * view.zoom;
      const offsetX = -view.panX * totalWidth;
      const baseDepth = displayRoot ? displayRoot.depth : 0;
      const refXStart = displayRoot ? displayRoot.xStart : 0;
      const refXEnd = displayRoot ? displayRoot.xEnd : 1;
      const refWidth = refXEnd - refXStart || 1;

      for (const node of displayNodes) {
        const relativeDepth = node.depth - baseDepth;
        if (relativeDepth > maxDepth) continue;

        const y = relativeDepth * (ROW_HEIGHT + ROW_GAP);
        const normXStart = (node.xStart - refXStart) / refWidth;
        const normXEnd = (node.xEnd - refXStart) / refWidth;
        const x = offsetX + normXStart * totalWidth;
        const w = Math.max((normXEnd - normXStart) * totalWidth, 2);

        if (mx >= x && mx <= x + w && my >= y && my <= y + ROW_HEIGHT) {
          return node;
        }
      }
      return null;
    },
    [displayNodes, canvasSize, view, maxDepth, displayRoot],
  );

  // Mouse events
  const handleMouseMove = useCallback(
    (e: React.MouseEvent) => {
      if (dragRef.current) {
        const dx = e.clientX - dragRef.current.startX;
        const panDelta = -dx / (canvasSize.width * view.zoom);
        const newPanX = Math.max(
          0,
          Math.min(1 - 1 / view.zoom, dragRef.current.startPanX + panDelta),
        );
        setView((v) => ({ ...v, panX: newPanX }));
        return;
      }
      const node = hitTest(e.clientX, e.clientY);
      if (node) {
        setHoveredSpan({ span: node.span, x: e.clientX, y: e.clientY });
      } else {
        setHoveredSpan(null);
      }
    },
    [hitTest, canvasSize.width, view.zoom],
  );

  const handleMouseDown = useCallback(
    (e: React.MouseEvent) => {
      dragRef.current = { startX: e.clientX, startPanX: view.panX };
      setIsDragging(true);
    },
    [view.panX],
  );

  const handleMouseUp = useCallback(() => {
    dragRef.current = null;
    setIsDragging(false);
  }, []);

  const handleClick = useCallback(
    (e: React.MouseEvent) => {
      const node = hitTest(e.clientX, e.clientY);
      if (node) onSelectSpan(node.span);
    },
    [hitTest, onSelectSpan],
  );

  const handleDoubleClick = useCallback(
    (e: React.MouseEvent) => {
      const node = hitTest(e.clientX, e.clientY);
      if (node && node.children.length > 0) {
        setFocusStack((prev) => [...prev, node]);
        setView({ zoom: 1, panX: 0 });
      }
    },
    [hitTest],
  );

  const handleWheel = useCallback(
    (e: React.WheelEvent) => {
      e.preventDefault();
      const delta = e.deltaY > 0 ? 0.9 : 1.1;
      setView((v) => {
        const newZoom = Math.max(1, Math.min(50, v.zoom * delta));
        // Keep mouse position stable
        const rect = canvasRef.current?.getBoundingClientRect();
        if (rect) {
          const mouseXFrac = (e.clientX - rect.left) / canvasSize.width;
          const oldWorldX = v.panX + mouseXFrac / v.zoom;
          const newPanX = Math.max(0, Math.min(1 - 1 / newZoom, oldWorldX - mouseXFrac / newZoom));
          return { zoom: newZoom, panX: newPanX };
        }
        return { ...v, zoom: newZoom };
      });
    },
    [canvasSize.width],
  );

  const resetFocus = useCallback(() => {
    setFocusStack([]);
    setView({ zoom: 1, panX: 0 });
  }, []);

  return (
    <div className="flex-1 flex flex-col min-w-0" data-tutorial-id="trace-flamechart">
      {/* Controls bar */}
      <div
        className="flex items-center gap-3 px-3 py-1 bg-zinc-900/50 border-b border-zinc-700 text-[11px]"
        data-tutorial-id="trace-flamechart-controls"
      >
        {/* Breadcrumb */}
        <div className="flex items-center gap-1 text-zinc-400">
          <button
            onClick={resetFocus}
            className={`hover:text-zinc-200 ${focusStack.length === 0 ? "text-zinc-200" : ""}`}
          >
            Root
          </button>
          {focusStack.map((node, i) => (
            <React.Fragment key={node.span.span_id}>
              <span className="text-zinc-600">/</span>
              <button
                onClick={() => {
                  setFocusStack((prev) => prev.slice(0, i + 1));
                  setView({ zoom: 1, panX: 0 });
                }}
                className={`hover:text-zinc-200 truncate max-w-[120px] ${
                  i === focusStack.length - 1 ? "text-zinc-200" : ""
                }`}
              >
                {node.span.name.replace(/^qontinui\./, "")}
              </button>
            </React.Fragment>
          ))}
        </div>

        <div className="ml-auto flex items-center gap-3">
          {/* Depth slider */}
          <label className="flex items-center gap-1 text-zinc-500">
            Depth:
            <input
              type="range"
              min={1}
              max={Math.max(actualMaxDepth, 1)}
              value={Math.min(maxDepth, actualMaxDepth)}
              onChange={(e) => setMaxDepth(Number(e.target.value))}
              className="w-16 h-3"
            />
            <span className="text-zinc-400 w-4 text-right">
              {Math.min(maxDepth, actualMaxDepth)}
            </span>
          </label>

          {/* Reset zoom */}
          {view.zoom > 1 && (
            <button
              onClick={() => setView({ zoom: 1, panX: 0 })}
              className="text-zinc-500 hover:text-zinc-300"
            >
              Reset zoom
            </button>
          )}
        </div>
      </div>

      {/* Canvas container */}
      <div
        ref={containerRef}
        className="flex-1 relative overflow-hidden"
        style={{ minHeight: height - 48 }}
      >
        <canvas
          ref={canvasRef}
          style={{
            width: canvasSize.width,
            height: canvasSize.height,
            cursor: isDragging ? "grabbing" : "default",
          }}
          onMouseMove={handleMouseMove}
          onMouseDown={handleMouseDown}
          onMouseUp={handleMouseUp}
          onMouseLeave={() => {
            handleMouseUp();
            setHoveredSpan(null);
          }}
          onClick={handleClick}
          onDoubleClick={handleDoubleClick}
          onWheel={handleWheel}
        />

        {/* Tooltip */}
        {hoveredSpan && (
          <div
            className="fixed z-50 px-2 py-1 bg-zinc-800 border border-zinc-600 rounded shadow-lg text-[11px] text-zinc-200 pointer-events-none"
            style={{
              left: hoveredSpan.x + 12,
              top: hoveredSpan.y + 12,
            }}
          >
            <div className="font-medium">{hoveredSpan.span.name.replace(/^qontinui\./, "")}</div>
            <div className="text-zinc-400">
              {formatDuration(hoveredSpan.span.duration_ms ?? 0)}
              {roots.length > 0 && roots[0].span.duration_ms
                ? ` (${(((hoveredSpan.span.duration_ms ?? 0) / (roots[0].span.duration_ms ?? 1)) * 100).toFixed(1)}%)`
                : ""}
            </div>
            {!hoveredSpan.span.success && <div className="text-red-400">Error</div>}
            {criticalPath?.spanIds.has(hoveredSpan.span.span_id) && (
              <div className="text-red-300">On critical path</div>
            )}
            {criticalPath && !criticalPath.spanIds.has(hoveredSpan.span.span_id) && (
              <div className="text-zinc-500">
                Slack:{" "}
                {formatDuration(criticalPath.slackBySpanId.get(hoveredSpan.span.span_id) ?? 0)}
              </div>
            )}
            <div className="text-zinc-500 mt-0.5">Double-click to focus subtree</div>
          </div>
        )}
      </div>
    </div>
  );
};
