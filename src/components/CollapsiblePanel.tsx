import { useState, useEffect, ReactNode } from "react";
import { ChevronDown, ChevronUp } from "lucide-react";

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
  borderColorClass = "border-primary/50",
  collapsible = true,
  headerExtra,
}: CollapsiblePanelProps) => {
  const [isCollapsed, setIsCollapsed] = useState(() => {
    // If controlled, use controlled value
    if (controlledCollapsed !== undefined) return controlledCollapsed;

    // Otherwise check localStorage
    if (storageKey) {
      const saved = localStorage.getItem(storageKey);
      return saved ? JSON.parse(saved) : defaultCollapsed;
    }

    // Default to defaultCollapsed
    return defaultCollapsed;
  });

  // Sync with controlled prop
  useEffect(() => {
    if (controlledCollapsed !== undefined) {
      setIsCollapsed(controlledCollapsed);
    }
  }, [controlledCollapsed]);

  const handleToggle = () => {
    const newState = !isCollapsed;
    setIsCollapsed(newState);

    // Save to localStorage if key provided
    if (storageKey) {
      localStorage.setItem(storageKey, JSON.stringify(newState));
    }

    // Call onToggle callback if provided
    if (onToggle) {
      onToggle(newState);
    }
  };

  // When not collapsible, always show content
  const showContent = !collapsible || !isCollapsed;

  return (
    <div
      className={`bg-card rounded-lg border border-border/50 hover:${borderColorClass} transition-all duration-300 ${
        !showContent ? "p-0" : "p-6"
      }`}
    >
      {/* Header */}
      {collapsible ? (
        <div className={`w-full flex items-center justify-between ${isCollapsed ? "p-4" : "pb-4"}`}>
          <button
            onClick={handleToggle}
            className="flex items-center gap-2 hover:opacity-80 transition-opacity flex-1"
            aria-expanded={!isCollapsed}
            aria-controls={`panel-content-${title.replace(/\s+/g, "-").toLowerCase()}`}
          >
            <div className={`flex items-center gap-2 ${colorClass}`}>
              {icon}
              <h2 className="text-lg font-semibold">{title}</h2>
            </div>
          </button>
          <div className="flex items-center gap-2">
            {headerExtra}
            <button
              onClick={handleToggle}
              className="flex items-center gap-2 hover:opacity-80 transition-opacity"
            >
              <span className="text-sm text-muted-foreground">
                {isCollapsed ? "Expand" : "Collapse"}
              </span>
              {isCollapsed ? (
                <ChevronDown className="w-5 h-5" />
              ) : (
                <ChevronUp className="w-5 h-5" />
              )}
            </button>
          </div>
        </div>
      ) : (
        <div className={`flex items-center justify-between pb-4`}>
          <div className={`flex items-center gap-2 ${colorClass}`}>
            {icon}
            <h2 className="text-lg font-semibold">{title}</h2>
          </div>
          {headerExtra}
        </div>
      )}

      {/* Content */}
      {showContent && (
        <div
          id={`panel-content-${title.replace(/\s+/g, "-").toLowerCase()}`}
          className="space-y-4 animate-slideDown"
        >
          {children}
        </div>
      )}
    </div>
  );
};

export default CollapsiblePanel;
