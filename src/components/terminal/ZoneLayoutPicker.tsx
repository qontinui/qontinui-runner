import { useState, useRef, useEffect } from "react";
import { LayoutGrid } from "lucide-react";
import { LAYOUT_PRESETS, type LayoutPreset } from "./useZoneLayout";
import { useUIComponent } from "ui-bridge";

interface ZoneLayoutPickerProps {
  currentLayoutId: string;
  onSelectLayout: (id: string) => void;
  tabCount: number;
}

/** Renders a tiny visual preview of a layout preset */
function LayoutThumbnail({ layout, isActive }: { layout: LayoutPreset; isActive: boolean }) {
  const size = 28;
  const gap = 1;
  const cellW = (size - gap * (layout.columns - 1)) / layout.columns;
  const cellH = (size - gap * (layout.rows - 1)) / layout.rows;

  // Parse grid positions to pixel rects
  const rects = layout.zones.map((zone) => {
    const parseSpan = (spec: string, cellSize: number, gapSize: number) => {
      const parts = spec.split("/").map((s) => s.trim());
      const start = parseInt(parts[0], 10) - 1;
      const end = parts[1] ? parseInt(parts[1], 10) - 1 : start + 1;
      const span = end - start;
      return {
        offset: start * (cellSize + gapSize),
        size: span * cellSize + (span - 1) * gapSize,
      };
    };

    const col = parseSpan(zone.col, cellW, gap);
    const row = parseSpan(zone.row, cellH, gap);

    return { x: col.offset, y: row.offset, w: col.size, h: row.size };
  });

  return (
    <svg width={size} height={size} viewBox={`0 0 ${size} ${size}`}>
      {rects.map((r, i) => (
        <rect
          key={`rect-${i}`}
          x={r.x}
          y={r.y}
          width={r.w}
          height={r.h}
          rx={1}
          fill={isActive ? "#7aa2f7" : "#414868"}
          opacity={isActive ? 0.8 : 0.5}
        />
      ))}
    </svg>
  );
}

export function ZoneLayoutPicker({
  currentLayoutId,
  onSelectLayout,
  tabCount,
}: ZoneLayoutPickerProps) {
  const [open, setOpen] = useState(false);
  const dropdownRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    const handleClick = (e: MouseEvent) => {
      if (dropdownRef.current && !dropdownRef.current.contains(e.target as Node)) {
        setOpen(false);
      }
    };
    document.addEventListener("mousedown", handleClick);
    return () => document.removeEventListener("mousedown", handleClick);
  }, [open]);

  // ── UI Bridge registration ──────────────────────────────────────────────
  useUIComponent({
    id: "zone-layout-picker",
    name: "Zone Layout Picker",
    description:
      "Selects the multi-zone terminal layout preset. Use select-layout to switch layouts " +
      "or list-layouts to enumerate available options.",
    actions: [
      {
        id: "open",
        label: "Open dropdown",
        handler: () => {
          setOpen(true);
        },
      },
      {
        id: "close",
        label: "Close dropdown",
        handler: () => {
          setOpen(false);
        },
      },
      {
        id: "select-layout",
        label: "Select Layout",
        description: "Switch the terminal grid to a named layout preset.",
        paramSchema: { layoutId: "string (use list-layouts to enumerate valid IDs)" },
        handler: (params?: unknown) => {
          const { layoutId } = (params ?? {}) as { layoutId?: string };
          if (!layoutId) throw new Error("select-layout requires { layoutId: string }");
          const preset = LAYOUT_PRESETS.find((l) => l.id === layoutId);
          if (!preset) {
            throw new Error(
              `Unknown layoutId: "${layoutId}". Valid options: ${LAYOUT_PRESETS.map((l) => l.id).join(", ")}`,
            );
          }
          onSelectLayout(layoutId);
          setOpen(false);
        },
      },
      {
        id: "list-layouts",
        label: "List Layouts",
        description: "Return [{id, name, zones, shortcutKey}] for every built-in layout preset.",
        handler: () =>
          LAYOUT_PRESETS.map((l) => ({
            id: l.id,
            name: l.name,
            zones: l.zones.length,
            shortcutKey: l.shortcutKey ?? null,
          })),
      },
    ],
  });

  const currentLayout = LAYOUT_PRESETS.find((l) => l.id === currentLayoutId);

  return (
    <div className="relative" ref={dropdownRef}>
      <button
        onClick={() => setOpen(!open)}
        className={`flex items-center gap-1.5 px-2 py-1 rounded text-xs transition-colors ${
          open
            ? "text-[#7aa2f7] bg-[#7aa2f7]/10"
            : "text-[#565f89] hover:text-[#a9b1d6] hover:bg-[#1a1b26]/50"
        }`}
        title="Change layout (Ctrl+Shift+L)"
      >
        <LayoutGrid className="w-3.5 h-3.5" />
        {currentLayout && <span className="hidden sm:inline">{currentLayout.name}</span>}
      </button>

      {open && (
        <div className="absolute left-0 top-full mt-1 w-56 bg-[#1a1b26] border border-[#2a2d3d] rounded-lg shadow-xl z-50 overflow-hidden">
          <div className="px-3 py-2 text-[10px] uppercase tracking-wider text-[#565f89] border-b border-[#2a2d3d]">
            Zone Layouts
          </div>
          <div className="py-1">
            {LAYOUT_PRESETS.map((preset) => {
              const isActive = preset.id === currentLayoutId;
              const zoneCount = preset.zones.length;
              const hasEnoughTabs = tabCount >= zoneCount;

              return (
                <button
                  key={preset.id}
                  onClick={() => {
                    onSelectLayout(preset.id);
                    setOpen(false);
                  }}
                  className={`w-full flex items-center gap-3 px-3 py-1.5 text-left transition-colors ${
                    isActive
                      ? "bg-[#7aa2f7]/10 text-[#7aa2f7]"
                      : "text-[#a9b1d6] hover:bg-[#2a2d3d]/50"
                  }`}
                >
                  <LayoutThumbnail layout={preset} isActive={isActive} />
                  <div className="flex-1 min-w-0">
                    <div className="flex items-center gap-2">
                      <span className="text-xs font-medium">{preset.name}</span>
                      {preset.shortcutKey && (
                        <kbd className="text-[9px] px-1 py-0.5 rounded bg-[#2a2d3d] text-[#565f89] font-mono">
                          {preset.shortcutKey}
                        </kbd>
                      )}
                    </div>
                    <span
                      className={`text-[10px] ${
                        hasEnoughTabs ? "text-[#565f89]" : "text-[#e0af68]"
                      }`}
                    >
                      {zoneCount} zone{zoneCount !== 1 ? "s" : ""}
                      {!hasEnoughTabs && ` (${tabCount} tab${tabCount !== 1 ? "s" : ""})`}
                    </span>
                  </div>
                </button>
              );
            })}
          </div>
        </div>
      )}
    </div>
  );
}
