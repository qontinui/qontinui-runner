import { useState, useEffect, ReactNode } from "react";
import { ChevronDown } from "lucide-react";
import { instanceStorage } from "@/lib/instance-storage";

interface CollapsiblePanelProps {
  title: string;
  icon?: ReactNode;
  children: ReactNode;
  defaultCollapsed?: boolean;
  collapsed?: boolean;
  onToggle?: (collapsed: boolean) => void;
  storageKey?: string;
  colorClass?: string;
  borderColorClass?: string;
  /** If false, the panel is always expanded and has no collapse controls */
  collapsible?: boolean;
  /** Extra content to show in the header (e.g., action buttons) */
  headerExtra?: ReactNode;
}

const CollapsiblePanel = ({
  title,
  icon,
  children,
  defaultCollapsed = false,
  collapsed: controlledCollapsed,
  onToggle,
  storageKey,
  colorClass = "text-primary",
  borderColorClass: _borderColorClass = "border-primary/50",
  collapsible = true,
  headerExtra,
}: CollapsiblePanelProps) => {
  const [isCollapsed, setIsCollapsed] = useState(() => {
    // If controlled, use controlled value
    if (controlledCollapsed !== undefined) return controlledCollapsed;

    // Otherwise check instanceStorage
    if (storageKey) {
      return instanceStorage.getJSON(storageKey, defaultCollapsed);
    }

    // Default to defaultCollapsed
    return defaultCollapsed;
  });

  // Sync with controlled prop
  useEffect(() => {
    if (controlledCollapsed !== undefined) {
      // eslint-disable-next-line react-hooks/set-state-in-effect -- sync controlled prop to internal state
      setIsCollapsed(controlledCollapsed);
    }
  }, [controlledCollapsed]);

  const handleToggle = () => {
    const newState = !isCollapsed;
    setIsCollapsed(newState);

    // Save to instanceStorage if key provided
    if (storageKey) {
      instanceStorage.setJSON(storageKey, newState);
    }

    // Call onToggle callback if provided
    if (onToggle) {
      onToggle(newState);
    }
  };

  // When not collapsible, always show content
  const showContent = !collapsible || !isCollapsed;

  return (
    <div className="panel">
      {/* Header */}
      {collapsible ? (
        <button
          onClick={handleToggle}
          className="panel-header-btn"
          aria-expanded={!isCollapsed}
          aria-controls={`panel-content-${title.replace(/\s+/g, "-").toLowerCase()}`}
        >
          <div className="panel-title">
            <span className={colorClass}>{icon}</span>
            <span>{title}</span>
          </div>
          <div className="flex items-center gap-2">
            {headerExtra}
            <ChevronDown
              className={`w-4 h-4 text-muted-foreground transition-transform ${
                !isCollapsed ? "rotate-180" : ""
              }`}
            />
          </div>
        </button>
      ) : (
        <div className="panel-header">
          <div className="panel-title">
            <span className={colorClass}>{icon}</span>
            <span>{title}</span>
          </div>
          {headerExtra}
        </div>
      )}

      {/* Content */}
      {showContent && (
        <div
          id={`panel-content-${title.replace(/\s+/g, "-").toLowerCase()}`}
          className="panel-content space-y-3"
        >
          {children}
        </div>
      )}
    </div>
  );
};

export default CollapsiblePanel;
