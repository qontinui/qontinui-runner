import { useState, useRef, useEffect } from "react";
import { ChevronDown } from "lucide-react";
import type { TerminalTab } from "../useTerminalManager";
import type { SessionState } from "../useZoneLayout";

function ZoneMenuItem({
  label,
  onClick,
  onClose,
  danger,
  disabled,
}: {
  label: string;
  onClick: () => void;
  onClose: () => void;
  danger?: boolean;
  disabled?: boolean;
}) {
  return (
    <button
      onClick={() => {
        onClick();
        onClose();
      }}
      disabled={disabled}
      className={`w-full text-left px-3 py-1.5 text-[11px] transition-colors disabled:opacity-30 ${
        danger ? "text-[#f7768e] hover:bg-[#f7768e]/10" : "text-[#c0caf5] hover:bg-[#7aa2f7]/10"
      }`}
    >
      {label}
    </button>
  );
}

export function ZoneContextMenu({
  x,
  y,
  zoneIndex,
  tab,
  state,
  otherZones,
  onClose,
  onFocus,
  onMaximize,
  onApprove,
  onReject,
  onSwap,
  onUnassign,
  onRestart,
}: {
  x: number;
  y: number;
  zoneIndex: number;
  tab: TerminalTab | undefined;
  state: SessionState;
  otherZones: { index: number; title: string }[];
  onClose: () => void;
  onFocus: () => void;
  onMaximize: () => void;
  onApprove: () => void;
  onReject: () => void;
  onSwap: (targetZone: number) => void;
  onUnassign: () => void;
  onRestart?: () => void;
}) {
  const menuRef = useRef<HTMLDivElement>(null);
  const [showSwapSub, setShowSwapSub] = useState(false);

  useEffect(() => {
    const handleClick = (e: MouseEvent) => {
      if (menuRef.current && !menuRef.current.contains(e.target as Node)) {
        onClose();
      }
    };
    const handleKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    document.addEventListener("mousedown", handleClick);
    document.addEventListener("keydown", handleKey);
    return () => {
      document.removeEventListener("mousedown", handleClick);
      document.removeEventListener("keydown", handleKey);
    };
  }, [onClose]);

  const needsInput = state === "needs-input";

  return (
    <div
      ref={menuRef}
      className="fixed z-50 bg-[#1a1b26] border border-[#2a2d3d] rounded-lg shadow-xl py-1 min-w-[160px] overflow-hidden"
      style={{
        left: x,
        top: y,
      }}
    >
      <div className="px-3 py-1 text-[9px] text-[#565f89] uppercase tracking-wider border-b border-[#2a2d3d] mb-1">
        Zone {zoneIndex + 1}
        {tab ? ` \u2014 ${tab.title}` : ""}
      </div>

      <ZoneMenuItem label="Focus" onClick={onFocus} onClose={onClose} />
      <ZoneMenuItem label="Maximize" onClick={onMaximize} onClose={onClose} disabled={!tab} />

      {needsInput && (
        <>
          <div className="h-px bg-[#2a2d3d] my-1" />
          <ZoneMenuItem label="Approve (y)" onClick={onApprove} onClose={onClose} />
          <ZoneMenuItem label="Reject (n)" onClick={onReject} onClose={onClose} />
        </>
      )}

      {(state === "completed" || state === "error") && onRestart && (
        <>
          <div className="h-px bg-[#2a2d3d] my-1" />
          <ZoneMenuItem label="Restart session" onClick={onRestart} onClose={onClose} />
        </>
      )}

      {otherZones.length > 0 && tab && (
        <>
          <div className="h-px bg-[#2a2d3d] my-1" />
          <div className="relative">
            <button
              className="w-full text-left px-3 py-1.5 text-[11px] text-[#c0caf5] hover:bg-[#7aa2f7]/10 flex items-center justify-between"
              onClick={() => setShowSwapSub(!showSwapSub)}
            >
              Swap with...
              <ChevronDown
                className={`w-3 h-3 transition-transform ${showSwapSub ? "rotate-180" : ""}`}
              />
            </button>
            {showSwapSub && (
              <div className="bg-[#13141f] border-t border-[#2a2d3d]">
                {otherZones.map((z) => (
                  <button
                    key={z.index}
                    className="w-full text-left px-5 py-1 text-[10px] text-[#a9b1d6] hover:bg-[#7aa2f7]/10"
                    onClick={() => {
                      onSwap(z.index);
                      onClose();
                    }}
                  >
                    Zone {z.index + 1}: {z.title}
                  </button>
                ))}
              </div>
            )}
          </div>
        </>
      )}

      {tab && (
        <>
          <div className="h-px bg-[#2a2d3d] my-1" />
          <ZoneMenuItem label="Unassign from zone" onClick={onUnassign} onClose={onClose} danger />
        </>
      )}
    </div>
  );
}
