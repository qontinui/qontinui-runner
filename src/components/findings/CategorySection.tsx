/**
 * CategorySection.tsx
 *
 * Component for displaying a section of findings grouped by category.
 * Shows category header with icon, count, and collapsible finding list.
 */

import { useState } from "react";
import {
  Bug,
  CheckSquare,
  Shield,
  Settings,
  CheckCircle,
  Info,
  Database,
  Activity,
  TestTube,
  Sparkles,
  FileText,
  Zap,
  AlertTriangle,
  ChevronDown,
  ChevronRight,
} from "lucide-react";
import { getAccentColors } from "@/design-system";
import type { Finding, FindingCategory } from "../../types/findings";
import { getCategoryColorClasses } from "../../services/FindingCategories";
import { FindingCard } from "./FindingCard";

// Map icon names to Lucide components
const iconMap: Record<string, React.ComponentType<{ className?: string }>> = {
  Bug,
  CheckSquare,
  Shield,
  Settings,
  CheckCircle,
  Info,
  Database,
  Activity,
  TestTube,
  Sparkles,
  FileText,
  Zap,
  AlertTriangle,
};

interface CategorySectionProps {
  category: FindingCategory;
  findings: Finding[];
  onAnalyze?: (finding: Finding) => void;
  onResolve?: (finding: Finding, resolution: string) => void;
  onProvideInput?: (finding: Finding, response: string) => void;
  onDismiss?: (finding: Finding) => void;
  processingFindingId?: string | null;
  defaultExpanded?: boolean;
}

export function CategorySection({
  category,
  findings,
  onAnalyze,
  onResolve,
  onProvideInput,
  onDismiss,
  processingFindingId,
  defaultExpanded = true,
}: CategorySectionProps) {
  const [isExpanded, setIsExpanded] = useState(defaultExpanded);

  const categoryColors = getCategoryColorClasses(category.color);
  const IconComponent = iconMap[category.icon] || Info;

  // Count statistics
  const resolvedCount = findings.filter((f) => f.status === "resolved").length;
  const actionableCount = findings.filter(
    (f) => f.actionable && f.status !== "resolved" && f.status !== "wont_fix",
  ).length;
  const needsInputCount = findings.filter((f) => f.status === "needs_input").length;

  if (findings.length === 0) {
    return null;
  }

  return (
    <div data-ui-id={`findings-category-section-${category.id}`} className="rounded-lg border border-border overflow-hidden">
      {/* Category Header */}
      <button
        data-ui-id="findings-category-toggle-btn"
        onClick={() => setIsExpanded(!isExpanded)}
        className={`w-full flex items-center gap-3 p-3 ${categoryColors.bg} hover:bg-muted/50 transition-colors`}
      >
        {/* Expand/Collapse Icon */}
        <div className="flex-shrink-0 text-muted-foreground">
          {isExpanded ? <ChevronDown className="w-4 h-4" /> : <ChevronRight className="w-4 h-4" />}
        </div>

        {/* Category Icon */}
        <div className={`flex-shrink-0 p-2 rounded-lg ${categoryColors.bg}`}>
          <IconComponent className={`w-5 h-5 ${categoryColors.text}`} />
        </div>

        {/* Category Info */}
        <div className="flex-1 text-left">
          <div className="flex items-center gap-2">
            <h3 className="font-medium text-sm text-foreground">{category.name}</h3>
            <span
              className={`px-2 py-0.5 rounded-full text-xs font-medium ${categoryColors.badgeBg} text-white`}
            >
              {findings.length}
            </span>
          </div>
          <p className="text-xs text-muted-foreground">{category.description}</p>
        </div>

        {/* Stats */}
        <div className="flex-shrink-0 flex items-center gap-3 text-xs">
          {actionableCount > 0 && (
            <span className={getAccentColors("amber").text}>{actionableCount} actionable</span>
          )}
          {needsInputCount > 0 && (
            <span className={getAccentColors("purple").text}>{needsInputCount} needs input</span>
          )}
          {resolvedCount > 0 && (
            <span className={getAccentColors("green").text}>{resolvedCount} resolved</span>
          )}
        </div>
      </button>

      {/* Findings List */}
      {isExpanded && (
        <div data-ui-id="findings-section-list" className="p-3 space-y-2 bg-background/50">
          {findings.map((finding) => (
            <FindingCard
              key={finding.id}
              finding={finding}
              onAnalyze={onAnalyze}
              onResolve={onResolve}
              onProvideInput={onProvideInput}
              onDismiss={onDismiss}
              isProcessing={processingFindingId === finding.id}
            />
          ))}
        </div>
      )}
    </div>
  );
}
