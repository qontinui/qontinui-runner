/**
 * ComparisonSpecCard.tsx
 *
 * Displays a single difference found during snapshot comparison.
 * Shows severity, type, what differs, and AI-generated suggestion.
 */

import {
  AlertTriangle,
  AlertCircle,
  Info,
  MinusCircle,
  PlusCircle,
  ArrowLeftRight,
  LayoutTemplate,
  Type,
} from "lucide-react";
import { cn } from "@/lib/utils";

export interface ComparisonSpec {
  id: string;
  name: string;
  description: string;
  type:
    | "missing_element"
    | "extra_element"
    | "different_text"
    | "different_structure"
    | "layout_mismatch";
  severity: "critical" | "major" | "minor" | "info";
  referenceElement?: unknown;
  targetElement?: unknown;
  suggestion?: string;
  confidence: number;
}

interface ComparisonSpecCardProps {
  spec: ComparisonSpec;
  selected?: boolean;
  onToggleSelect?: (id: string) => void;
}

const SEVERITY_CONFIG = {
  critical: {
    icon: AlertCircle,
    color: "text-error",
    bg: "bg-error/10",
    border: "border-error/30",
    label: "Critical",
  },
  major: {
    icon: AlertTriangle,
    color: "text-amber-400",
    bg: "bg-amber-500/10",
    border: "border-amber-500/30",
    label: "Major",
  },
  minor: {
    icon: Info,
    color: "text-blue-400",
    bg: "bg-blue-500/10",
    border: "border-blue-500/30",
    label: "Minor",
  },
  info: {
    icon: Info,
    color: "text-text-muted",
    bg: "bg-surface-hover",
    border: "border-border",
    label: "Info",
  },
};

const TYPE_CONFIG = {
  missing_element: { icon: MinusCircle, label: "Missing Element" },
  extra_element: { icon: PlusCircle, label: "Extra Element" },
  different_text: { icon: Type, label: "Different Text" },
  different_structure: { icon: ArrowLeftRight, label: "Different Structure" },
  layout_mismatch: { icon: LayoutTemplate, label: "Layout Mismatch" },
};

export function ComparisonSpecCard({ spec, selected, onToggleSelect }: ComparisonSpecCardProps) {
  const severity = SEVERITY_CONFIG[spec.severity];
  const typeInfo = TYPE_CONFIG[spec.type];
  const SeverityIcon = severity.icon;
  const TypeIcon = typeInfo.icon;

  return (
    <div
      className={cn(
        "rounded-lg border transition-all",
        severity.border,
        selected && "ring-2 ring-brand-primary/50",
      )}
    >
      {/* Header */}
      <div className={cn("flex items-start gap-3 px-3 py-2.5", severity.bg)}>
        {onToggleSelect && (
          <input
            type="checkbox"
            checked={selected ?? false}
            onChange={() => onToggleSelect(spec.id)}
            className="mt-0.5 rounded border-border"
          />
        )}
        <SeverityIcon className={cn("h-4 w-4 shrink-0 mt-0.5", severity.color)} />
        <div className="flex-1 min-w-0">
          <div className="flex items-center gap-2">
            <span className="text-sm font-medium text-text-primary">{spec.name}</span>
            <span
              className={cn(
                "rounded-full px-1.5 py-0.5 text-[10px] font-semibold uppercase",
                severity.bg,
                severity.color,
              )}
            >
              {severity.label}
            </span>
          </div>
          <div className="flex items-center gap-1.5 mt-0.5">
            <TypeIcon className="h-3 w-3 text-text-muted" />
            <span className="text-[10px] text-text-muted">{typeInfo.label}</span>
            {spec.confidence < 1 && (
              <>
                <span className="text-text-muted/50">·</span>
                <span className="text-[10px] text-text-muted">
                  {Math.round(spec.confidence * 100)}% confidence
                </span>
              </>
            )}
          </div>
        </div>
      </div>

      {/* Body */}
      <div className="px-3 py-2.5 space-y-2">
        <p className="text-xs text-text-muted">{spec.description}</p>

        {/* Suggestion */}
        {spec.suggestion && (
          <div className="rounded-md bg-surface-canvas border border-border px-2.5 py-2">
            <div className="text-[10px] font-medium uppercase tracking-wider text-text-muted mb-1">
              Suggestion
            </div>
            <p className="text-xs text-text-primary">{spec.suggestion}</p>
          </div>
        )}
      </div>
    </div>
  );
}
