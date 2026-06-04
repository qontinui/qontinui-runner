import { useEffect, useRef } from "react";
import { X, Keyboard } from "lucide-react";

interface ShortcutDef {
  keys: string;
  description: string;
}

const SHORTCUT_GROUPS: { title: string; shortcuts: ShortcutDef[] }[] = [
  {
    title: "Terminals",
    shortcuts: [
      { keys: "Ctrl+Shift+T", description: "New terminal" },
      { keys: "Ctrl+Shift+W", description: "Close focused terminal" },
    ],
  },
  {
    title: "Navigation",
    shortcuts: [
      { keys: "Ctrl+Tab", description: "Next zone / tab" },
      { keys: "Ctrl+Shift+Tab", description: "Previous zone / tab" },
      { keys: "Ctrl+1-9", description: "Focus zone by number" },
      { keys: "Ctrl+Shift+N", description: "Jump to next session needing input" },
      { keys: "Ctrl+Shift+\u2190", description: "Go back in focus history" },
      { keys: "Ctrl+Shift+\u2192", description: "Go forward in focus history" },
      { keys: "Escape", description: "Restore maximized zone / close panel" },
    ],
  },
  {
    title: "Find (focused terminal)",
    shortcuts: [
      { keys: "Ctrl+F", description: "Find in terminal scrollback" },
      { keys: "Enter / F3", description: "Next match" },
      { keys: "Shift+Enter / Shift+F3", description: "Previous match" },
      { keys: "Escape", description: "Close find (clears highlights)" },
    ],
  },
  {
    title: "Scrolling (focused terminal)",
    shortcuts: [
      { keys: "Shift+Wheel", description: "Scroll scrollback even when an app captures the wheel" },
      { keys: "Shift+PageUp", description: "Scroll up one page" },
      { keys: "Shift+PageDown", description: "Scroll down one page" },
      { keys: "Ctrl+Alt+PageUp", description: "Scroll up one line" },
      { keys: "Ctrl+Alt+PageDown", description: "Scroll down one line" },
      { keys: "Ctrl+Home", description: "Scroll to top" },
      { keys: "Ctrl+End", description: "Scroll to bottom" },
    ],
  },
  {
    title: "Layouts",
    shortcuts: [
      { keys: "Ctrl+Shift+1", description: "Single layout" },
      { keys: "Ctrl+Shift+2", description: "Split layout" },
      { keys: "Ctrl+Shift+3", description: "Triptych layout" },
      { keys: "Ctrl+Shift+4", description: "Quad layout" },
      { keys: "Ctrl+Shift+5", description: "Focus + 4 layout" },
      { keys: "Ctrl+Shift+6", description: "Six Pack layout" },
      { keys: "Ctrl+Shift+7", description: "Command Center layout" },
      { keys: "Ctrl+Shift+8", description: "3x3 Full Grid layout" },
    ],
  },
  {
    title: "View & Actions",
    shortcuts: [
      { keys: "Ctrl+Shift+F", description: "Maximize / restore focused zone" },
      { keys: "Ctrl+Shift+M", description: "Cycle view mode (auto / full / compact)" },
      { keys: "Ctrl+Shift+O", description: "Pin / unpin focused zone" },
      { keys: "Ctrl+Shift+P", description: "Toggle control panel" },
      { keys: "Ctrl+Shift+X", description: "Swap zones (press twice: source → target)" },
      { keys: "Ctrl+Shift+L", description: "Cycle through layout presets" },
      { keys: "Ctrl+Shift+A", description: "Toggle auto-focus on needs-input" },
      { keys: "Ctrl+Shift+S", description: "Toggle sound notification" },
      { keys: "Ctrl+Shift+D", description: "Toggle focus mode (dim non-focused zones)" },
      { keys: "Ctrl+Shift+R", description: "Restart focused zone (completed/error)" },
      { keys: "Ctrl+Shift+Enter", description: "Approve all waiting sessions" },
      { keys: "Ctrl+Shift+/", description: "Search across all session output" },
      { keys: "Ctrl+Shift+K", description: "Open command palette" },
      { keys: "Ctrl+Shift+G", description: "Cycle tag filter" },
      { keys: "Ctrl+Shift+I", description: "Toggle zone timeline" },
      { keys: "Ctrl+Shift+H", description: "Show event history card" },
      { keys: "Ctrl+Shift+?", description: "Toggle this help overlay" },
    ],
  },
];

export function KeyboardShortcutsOverlay({ onClose }: { onClose: () => void }) {
  const overlayRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        onClose();
      }
    };
    window.addEventListener("keydown", handler, { capture: true });
    return () => window.removeEventListener("keydown", handler, { capture: true });
  }, [onClose]);

  // Close on backdrop click
  const handleBackdropClick = (e: React.MouseEvent) => {
    if (e.target === overlayRef.current) {
      onClose();
    }
  };

  return (
    <div
      ref={overlayRef}
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 backdrop-blur-sm"
      role="button"
      tabIndex={0}
      aria-label="Close keyboard shortcuts overlay"
      onClick={handleBackdropClick}
      onKeyDown={(e) => {
        if ((e.key === "Enter" || e.key === " ") && e.target === overlayRef.current) onClose();
      }}
    >
      <div className="bg-[#1a1b26] border border-[#2a2d3d] rounded-lg shadow-2xl w-[520px] max-h-[80vh] overflow-auto">
        {/* Header */}
        <div className="flex items-center gap-2 px-4 py-3 border-b border-[#2a2d3d] sticky top-0 bg-[#1a1b26] z-10">
          <Keyboard className="w-4 h-4 text-[#7aa2f7]" />
          <h2 className="text-sm font-medium text-[#c0caf5] flex-1">Keyboard Shortcuts</h2>
          <button
            onClick={onClose}
            className="p-1 rounded hover:bg-[#2a2d3d] transition-colors text-[#565f89] hover:text-[#c0caf5]"
          >
            <X className="w-4 h-4" />
          </button>
        </div>

        {/* Shortcut groups */}
        <div className="p-4 space-y-4">
          {SHORTCUT_GROUPS.map((group) => (
            <div key={group.title}>
              <h3 className="text-[10px] font-semibold text-[#7aa2f7] uppercase tracking-wider mb-2">
                {group.title}
              </h3>
              <div className="space-y-1">
                {group.shortcuts.map((sc) => (
                  <div
                    key={sc.keys}
                    className="flex items-center justify-between py-1 px-2 rounded hover:bg-[#2a2d3d]/50"
                  >
                    <span className="text-[12px] text-[#a9b1d6]">{sc.description}</span>
                    <kbd className="text-[11px] font-mono text-[#c0caf5] bg-[#2a2d3d] border border-[#3b3d57] rounded px-1.5 py-0.5 ml-4 whitespace-nowrap">
                      {sc.keys}
                    </kbd>
                  </div>
                ))}
              </div>
            </div>
          ))}
        </div>

        {/* Footer */}
        <div className="px-4 py-2 border-t border-[#2a2d3d] text-center">
          <span className="text-[10px] text-[#565f89]">
            Press{" "}
            <kbd className="font-mono bg-[#2a2d3d] rounded px-1 py-0.5 text-[#a9b1d6]">Esc</kbd> to
            close
          </span>
        </div>
      </div>
    </div>
  );
}
