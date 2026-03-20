/**
 * StepOutputPanel Component
 *
 * Displays output/result content with optional syntax highlighting.
 * Used for shell command output, API responses, script output, etc.
 */

import { useState } from "react";
import { Copy, Check, ChevronDown, ChevronUp } from "lucide-react";
import { cn } from "@/lib/utils";
import { Button, ScrollArea, Badge } from "@/components/ui";
import { getAccentColors } from "@/design-system";

interface StepOutputPanelProps {
  /** Title for the panel */
  title: string;
  /** Output content to display */
  content: string;
  /** Content type for optional styling */
  contentType?: "text" | "json" | "html" | "error";
  /** Maximum height before collapsing */
  maxHeight?: string;
  /** Whether the panel is collapsible */
  collapsible?: boolean;
  /** Default collapsed state */
  defaultCollapsed?: boolean;
  /** Additional CSS classes */
  className?: string;
  /** Accent color for the panel */
  accentColor?: string;
  /** Show copy button */
  showCopy?: boolean;
  /** Show line numbers */
  showLineNumbers?: boolean;
}

/**
 * Format JSON content with indentation.
 */
function formatJson(content: string): string {
  try {
    const parsed = JSON.parse(content);
    return JSON.stringify(parsed, null, 2);
  } catch {
    return content;
  }
}

/**
 * Output panel component.
 */
export function StepOutputPanel({
  title,
  content,
  contentType = "text",
  maxHeight = "max-h-[200px]",
  collapsible = false,
  defaultCollapsed = false,
  className,
  accentColor = "slate",
  showCopy = true,
  showLineNumbers = false,
}: StepOutputPanelProps) {
  const [isCollapsed, setIsCollapsed] = useState(defaultCollapsed);
  const [copied, setCopied] = useState(false);
  const colors = getAccentColors(accentColor as "slate" | "blue" | "green" | "red");

  const displayContent = contentType === "json" ? formatJson(content) : content;
  const lines = displayContent.split("\n");

  const handleCopy = async () => {
    try {
      await navigator.clipboard.writeText(content);
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    } catch {
      // Ignore copy errors
    }
  };

  const headerContent = (
    <div className="flex items-center justify-between">
      <div className="flex items-center gap-2">
        <h4 className="text-sm font-medium text-foreground">{title}</h4>
        {contentType !== "text" && (
          <Badge variant="muted" className="text-[10px] uppercase">
            {contentType}
          </Badge>
        )}
      </div>
      <div className="flex items-center gap-1">
        {showCopy && (
          <Button
            size="sm"
            variant="ghost"
            className="h-6 w-6 p-0 focus-visible:ring-2 focus-visible:ring-primary/50"
            onClick={(e) => {
              e.stopPropagation();
              handleCopy();
            }}
            aria-label="Copy output to clipboard"
            title="Copy to clipboard"
          >
            {copied ? (
              <Check className="h-3.5 w-3.5 text-green-500" />
            ) : (
              <Copy className="h-3.5 w-3.5 text-muted-foreground" />
            )}
          </Button>
        )}
        {collapsible && (
          <Button
            size="sm"
            variant="ghost"
            className="h-6 w-6 p-0 focus-visible:ring-2 focus-visible:ring-primary/50"
            onClick={() => setIsCollapsed(!isCollapsed)}
            aria-label={isCollapsed ? "Expand output panel" : "Collapse output panel"}
            aria-expanded={!isCollapsed}
          >
            {isCollapsed ? (
              <ChevronDown className="h-4 w-4 text-muted-foreground" />
            ) : (
              <ChevronUp className="h-4 w-4 text-muted-foreground" />
            )}
          </Button>
        )}
      </div>
    </div>
  );

  return (
    <div className={cn("border rounded-md overflow-hidden", colors.border, className)}>
      {/* Header */}
      <div
        className={cn(
          "px-3 py-2 border-b",
          colors.bg,
          colors.border,
          collapsible && "cursor-pointer hover:bg-muted/50",
        )}
        onClick={collapsible ? () => setIsCollapsed(!isCollapsed) : undefined}
        onKeyDown={
          collapsible
            ? (e) => {
                if (e.key === "Enter" || e.key === " ") {
                  e.preventDefault();
                  setIsCollapsed(!isCollapsed);
                }
              }
            : undefined
        }
        role={collapsible ? "button" : undefined}
        tabIndex={collapsible ? 0 : undefined}
      >
        {headerContent}
      </div>

      {/* Content */}
      {!isCollapsed && (
        <ScrollArea className={cn(maxHeight, "bg-muted/20")}>
          <div className="p-3">
            <pre
              className={cn(
                "text-xs font-mono whitespace-pre-wrap break-all",
                contentType === "error" && "text-red-400",
                contentType === "json" && "text-foreground",
              )}
            >
              {showLineNumbers ? (
                <div className="flex">
                  {/* Line numbers */}
                  <div className="pr-3 mr-3 border-r border-border/50 text-muted-foreground select-none flex-shrink-0">
                    {lines.map((_, i) => (
                      <div key={`ln-${i}`}>{i + 1}</div>
                    ))}
                  </div>
                  {/* Content */}
                  <div className="flex-1 min-w-0 overflow-hidden">
                    {lines.map((line, i) => (
                      <div key={`line-${i}`} className="break-all whitespace-pre-wrap">
                        {line || " "}
                      </div>
                    ))}
                  </div>
                </div>
              ) : (
                displayContent
              )}
            </pre>
          </div>
        </ScrollArea>
      )}
    </div>
  );
}

export default StepOutputPanel;
