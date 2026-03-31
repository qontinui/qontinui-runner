import { useRef, useEffect, useMemo, useCallback } from "react";
import type { TraceTreeNode, WorkflowPhase } from "./types";
import { inferPhase } from "./trace-utils";

interface TraceMinimapProps {
  nodes: TraceTreeNode[];
  traceStartMs: number;
  traceDurationMs: number;
  viewportStart: number; // 0-1 fraction of trace visible start
  viewportEnd: number;   // 0-1 fraction of trace visible end
  onViewportChange?: (start: number, end: number) => void;
  height?: number;
}

const PHASE_HEX: Record<WorkflowPhase, string> = {
  setup: "#3b82f6",
  verification: "#22c55e",
  agentic: "#a855f7",
  completion: "#f59e0b",
  ai: "#ec4899",
  python: "#06b6d4",
  unknown: "#6b7280",
};

export function TraceMinimap({
  nodes,
  traceStartMs,
  traceDurationMs,
  viewportStart,
  viewportEnd,
  onViewportChange,
  height = 40,
}: TraceMinimapProps) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const isDragging = useRef(false);

  // Flatten nodes for rendering
  const spans = useMemo(() => {
    return nodes.map(node => ({
      start: node.offsetMs,
      duration: node.span.duration_ms ?? 0,
      depth: node.depth,
      phase: inferPhase(node.span.name),
      error: node.span.success === false || !!node.span.error,
    }));
  }, [nodes]);

  const maxDepth = useMemo(() => Math.max(1, ...spans.map(s => s.depth + 1)), [spans]);

  // Draw minimap
  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;

    const dpr = window.devicePixelRatio || 1;
    const rect = canvas.getBoundingClientRect();
    canvas.width = rect.width * dpr;
    canvas.height = rect.height * dpr;
    ctx.scale(dpr, dpr);

    const w = rect.width;
    const h = rect.height;

    // Background
    ctx.fillStyle = "#18181b"; // zinc-900
    ctx.fillRect(0, 0, w, h);

    if (traceDurationMs <= 0) return;

    // Draw spans
    const rowHeight = Math.max(1, (h - 4) / maxDepth);
    for (const span of spans) {
      const x = (span.start / traceDurationMs) * w;
      const spanW = Math.max(1, (span.duration / traceDurationMs) * w);
      const y = 2 + span.depth * rowHeight;
      const spanH = Math.max(1, rowHeight - 1);

      ctx.fillStyle = span.error ? "#ef4444" : (PHASE_HEX[span.phase] || "#6b7280");
      ctx.globalAlpha = 0.7;
      ctx.fillRect(x, y, spanW, spanH);
    }

    // Draw viewport indicator
    ctx.globalAlpha = 1;
    const vpX = viewportStart * w;
    const vpW = Math.max(2, (viewportEnd - viewportStart) * w);

    // Dim areas outside viewport
    ctx.fillStyle = "rgba(0, 0, 0, 0.5)";
    ctx.fillRect(0, 0, vpX, h);
    ctx.fillRect(vpX + vpW, 0, w - vpX - vpW, h);

    // Viewport border
    ctx.strokeStyle = "#60a5fa"; // blue-400
    ctx.lineWidth = 1.5;
    ctx.strokeRect(vpX, 0.5, vpW, h - 1);
  }, [spans, maxDepth, traceDurationMs, viewportStart, viewportEnd]);

  // Click/drag handler
  const handlePointerEvent = useCallback((e: React.PointerEvent) => {
    if (!onViewportChange) return;
    const canvas = canvasRef.current;
    if (!canvas) return;

    const rect = canvas.getBoundingClientRect();
    const fraction = Math.max(0, Math.min(1, (e.clientX - rect.left) / rect.width));
    const vpWidth = viewportEnd - viewportStart;
    const newStart = Math.max(0, Math.min(1 - vpWidth, fraction - vpWidth / 2));
    onViewportChange(newStart, newStart + vpWidth);
  }, [onViewportChange, viewportStart, viewportEnd]);

  const onPointerDown = useCallback((e: React.PointerEvent) => {
    isDragging.current = true;
    (e.target as HTMLElement).setPointerCapture(e.pointerId);
    handlePointerEvent(e);
  }, [handlePointerEvent]);

  const onPointerMove = useCallback((e: React.PointerEvent) => {
    if (isDragging.current) handlePointerEvent(e);
  }, [handlePointerEvent]);

  const onPointerUp = useCallback(() => {
    isDragging.current = false;
  }, []);

  return (
    <canvas
      ref={canvasRef}
      className="w-full cursor-pointer border-b border-zinc-700"
      style={{ height }}
      onPointerDown={onPointerDown}
      onPointerMove={onPointerMove}
      onPointerUp={onPointerUp}
    />
  );
}
