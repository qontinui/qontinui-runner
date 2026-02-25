/**
 * EmptyState Component
 *
 * Standardized empty state display for dashboard widgets.
 * Provides a consistent visual pattern when a widget has no data to show.
 */

import { type LucideIcon } from "lucide-react";
import { cn } from "../../../../lib/utils";

interface EmptyStateProps {
  icon: LucideIcon;
  title: string;
  description?: string;
  className?: string;
}

export function EmptyState({ icon: Icon, title, description, className }: EmptyStateProps) {
  return (
    <div className={cn("flex flex-col items-center justify-center py-12 px-4", className)}>
      <div className="rounded-full bg-muted/50 p-3 mb-3">
        <Icon className="h-6 w-6 text-muted-foreground" />
      </div>
      <p className="text-sm font-medium text-foreground">{title}</p>
      {description && (
        <p className="text-xs text-muted-foreground mt-1 text-center max-w-[200px]">
          {description}
        </p>
      )}
    </div>
  );
}
