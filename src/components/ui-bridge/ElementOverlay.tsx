import { memo } from "react";
import type { UIBridgeElement } from "./inspector-types";

interface ElementOverlayProps {
  elements: UIBridgeElement[];
  selectedElementId: string | null;
  onSelectElement: (id: string) => void;
}

export const ElementOverlay = memo(function ElementOverlay({
  elements,
  selectedElementId,
  onSelectElement,
}: ElementOverlayProps) {
  return (
    <div className="pointer-events-none fixed inset-0 z-[9999]" style={{ isolation: "isolate" }}>
      {elements
        .filter((el) => el.visible && el.bounds.width > 0 && el.bounds.height > 0)
        .map((el) => (
          <div
            key={el.id}
            className="pointer-events-auto absolute cursor-pointer"
            style={{
              left: el.bounds.x,
              top: el.bounds.y,
              width: el.bounds.width,
              height: el.bounds.height,
            }}
            onClick={(e) => {
              e.stopPropagation();
              onSelectElement(el.id);
            }}
            onKeyDown={(e) => {
              if (e.key === "Enter" || e.key === " ") {
                e.preventDefault();
                onSelectElement(el.id);
              }
            }}
            role="button"
            tabIndex={0}
          >
            <div
              className={`w-full h-full border-2 rounded transition-colors ${
                el.id === selectedElementId
                  ? "border-cyan-400 bg-cyan-400/20"
                  : "border-cyan-400/40 bg-cyan-400/5 hover:border-cyan-400 hover:bg-cyan-400/10"
              }`}
            />
          </div>
        ))}
    </div>
  );
});
