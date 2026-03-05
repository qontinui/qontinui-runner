import { useState, useCallback, useEffect } from "react";
import type { UIBridgeElement } from "./UIBridgeInspectorPanel";

interface ElementPickerProps {
  elements: UIBridgeElement[];
  onPick: (elementId: string) => void;
  onCancel: () => void;
}

export function ElementPicker({ elements, onPick, onCancel }: ElementPickerProps) {
  const [hoveredId, setHoveredId] = useState<string | null>(null);

  const hitTest = useCallback(
    (x: number, y: number): string | null => {
      let best: UIBridgeElement | null = null;
      let bestArea = Infinity;
      for (const el of elements) {
        const { bounds } = el;
        if (
          x >= bounds.x &&
          x <= bounds.x + bounds.width &&
          y >= bounds.y &&
          y <= bounds.y + bounds.height
        ) {
          const area = bounds.width * bounds.height;
          if (area < bestArea) {
            best = el;
            bestArea = area;
          }
        }
      }
      return best?.id ?? null;
    },
    [elements],
  );

  const handleMouseMove = useCallback(
    (e: React.MouseEvent<HTMLDivElement>) => {
      setHoveredId(hitTest(e.clientX, e.clientY));
    },
    [hitTest],
  );

  const handleClick = useCallback(
    (e: React.MouseEvent<HTMLDivElement>) => {
      e.preventDefault();
      e.stopPropagation();
      const id = hitTest(e.clientX, e.clientY);
      if (id) onPick(id);
      else onCancel();
    },
    [hitTest, onPick, onCancel],
  );

  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (e.key === "Escape") onCancel();
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [onCancel]);

  const hoveredEl = hoveredId ? elements.find((el) => el.id === hoveredId) : null;

  return (
    <>
      <div
        className="fixed inset-0 z-[10000] cursor-crosshair"
        style={{ background: "transparent" }}
        onMouseMove={handleMouseMove}
        onClick={handleClick}
      />
      {hoveredEl && (
        <div
          className="fixed z-[10001] pointer-events-none border-2 border-cyan-400 bg-cyan-400/20 rounded"
          style={{
            left: hoveredEl.bounds.x,
            top: hoveredEl.bounds.y,
            width: hoveredEl.bounds.width,
            height: hoveredEl.bounds.height,
          }}
        />
      )}
      {hoveredEl && (
        <div
          className="fixed z-[10002] pointer-events-none px-2 py-1 text-xs rounded bg-gray-900 border border-cyan-500/50 text-cyan-300"
          style={{
            left: Math.min(hoveredEl.bounds.x, window.innerWidth - 200),
            top: Math.max(hoveredEl.bounds.y - 28, 4),
          }}
        >
          {hoveredEl.id}
        </div>
      )}
      <div className="fixed bottom-4 left-1/2 -translate-x-1/2 z-[10002] px-3 py-1.5 text-xs rounded bg-gray-900 border border-border text-muted-foreground pointer-events-none">
        Click to select element — Esc to cancel
      </div>
    </>
  );
}
