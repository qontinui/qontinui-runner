/**
 * TourCard
 *
 * Card component for displaying a product tour in the catalog.
 */

import { Play, Clock, Layers, User } from "lucide-react";
import type { ProductTour, TourAudience } from "@/types/product-tour";

const AUDIENCE_LABELS: Record<TourAudience, string> = {
  "new-user": "New User",
  "power-user": "Power User",
  evaluator: "Evaluator",
};

const AUDIENCE_COLORS: Record<TourAudience, string> = {
  "new-user": "bg-green-500/15 text-green-400 border-green-500/25",
  "power-user": "bg-blue-500/15 text-blue-400 border-blue-500/25",
  evaluator: "bg-amber-500/15 text-amber-400 border-amber-500/25",
};

interface TourCardProps {
  tour: ProductTour;
  onLaunch: (tour: ProductTour) => void;
  onExport?: (tour: ProductTour, format: "html" | "json" | "markdown") => void;
}

export function TourCard({ tour, onLaunch, onExport }: TourCardProps) {
  return (
    <div className="border border-border rounded-lg p-4 hover:border-primary/40 transition-colors group">
      <div className="space-y-3">
        {/* Header */}
        <div className="flex items-start justify-between gap-2">
          <div className="min-w-0">
            <h3 className="text-sm font-semibold truncate">{tour.title}</h3>
            <p className="text-xs text-muted-foreground mt-0.5 line-clamp-2">
              {tour.subtitle}
            </p>
          </div>
          <span
            className={`shrink-0 text-[10px] font-medium px-2 py-0.5 rounded-full border ${AUDIENCE_COLORS[tour.targetAudience]}`}
          >
            {AUDIENCE_LABELS[tour.targetAudience]}
          </span>
        </div>

        {/* Meta */}
        <div className="flex items-center gap-3 text-xs text-muted-foreground">
          <span className="flex items-center gap-1">
            <Clock className="h-3 w-3" />
            {tour.estimatedMinutes} min
          </span>
          <span className="flex items-center gap-1">
            <Layers className="h-3 w-3" />
            {tour.steps.length} steps
          </span>
          <span className="flex items-center gap-1">
            <User className="h-3 w-3" />
            {tour.pages.length} page{tour.pages.length !== 1 ? "s" : ""}
          </span>
        </div>

        {/* Actions */}
        <div className="flex items-center gap-2 pt-1">
          <button
            onClick={() => onLaunch(tour)}
            className="flex items-center gap-1.5 px-3 py-1.5 rounded-md bg-primary text-primary-foreground text-xs font-medium hover:bg-primary/90"
          >
            <Play className="h-3 w-3" />
            Start Tour
          </button>
          {onExport && (
            <div className="flex items-center gap-1">
              <button
                onClick={() => onExport(tour, "html")}
                className="px-2 py-1.5 rounded-md border border-border text-xs hover:bg-accent"
                title="Export as standalone HTML"
              >
                HTML
              </button>
              <button
                onClick={() => onExport(tour, "json")}
                className="px-2 py-1.5 rounded-md border border-border text-xs hover:bg-accent"
                title="Export as JSON"
              >
                JSON
              </button>
              <button
                onClick={() => onExport(tour, "markdown")}
                className="px-2 py-1.5 rounded-md border border-border text-xs hover:bg-accent"
                title="Export as Markdown"
              >
                MD
              </button>
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
