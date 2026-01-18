/**
 * ModeSwitcher.tsx
 *
 * A segmented control for switching between Automation and Developer modes.
 * Persists the selection to localStorage and updates the navigation state.
 */

import { Wrench, Code } from "lucide-react";
import type { AppMode } from "qontinui-navigation";

// ============================================================================
// Types
// ============================================================================

export interface ModeSwitcherProps {
  mode: AppMode;
  onModeChange: (mode: AppMode) => void;
  collapsed?: boolean;
}

// ============================================================================
// Mode Configuration
// ============================================================================

interface ModeConfig {
  id: AppMode;
  label: string;
  shortLabel: string;
  icon: typeof Wrench;
  description: string;
}

const MODES: ModeConfig[] = [
  {
    id: "automation",
    label: "Auto",
    shortLabel: "Auto",
    icon: Wrench,
    description: "Execute pre-built workflows",
  },
  {
    id: "developer",
    label: "Dev",
    shortLabel: "Dev",
    icon: Code,
    description: "Build and debug workflows",
  },
];

// ============================================================================
// Component
// ============================================================================

export function ModeSwitcher({ mode, onModeChange, collapsed }: ModeSwitcherProps) {
  if (collapsed) {
    // When collapsed, show a simple icon button that toggles modes
    const currentMode = MODES.find((m) => m.id === mode) || MODES[0];
    const nextMode = MODES.find((m) => m.id !== mode) || MODES[1];
    const Icon = currentMode.icon;

    return (
      <div className="px-1.5 py-2">
        <button
          onClick={() => onModeChange(nextMode.id)}
          className="w-full flex items-center justify-center p-2 rounded-md
                     bg-muted/50 hover:bg-muted transition-colors group"
          title={`Switch to ${nextMode.label} mode`}
        >
          <Icon className="w-4 h-4 text-primary" />
        </button>
      </div>
    );
  }

  return (
    <div className="px-3 py-2">
      <div className="flex bg-muted/50 rounded-lg p-1 gap-1">
        {MODES.map((modeConfig) => {
          const Icon = modeConfig.icon;
          const isActive = mode === modeConfig.id;

          return (
            <button
              key={modeConfig.id}
              onClick={() => onModeChange(modeConfig.id)}
              className={`
                flex-1 flex items-center justify-center gap-1.5 py-1.5 px-2 rounded-md
                text-xs font-medium transition-all duration-200
                ${
                  isActive
                    ? "bg-background text-primary shadow-sm"
                    : "text-muted-foreground hover:text-foreground hover:bg-muted/50"
                }
              `}
              title={modeConfig.description}
            >
              <Icon className={`w-3.5 h-3.5 ${isActive ? "text-primary" : ""}`} />
              <span className="truncate">{modeConfig.label}</span>
            </button>
          );
        })}
      </div>
    </div>
  );
}

export default ModeSwitcher;
