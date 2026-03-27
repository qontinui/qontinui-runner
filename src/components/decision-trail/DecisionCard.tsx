import { Clock, Tag, ArrowRight } from "lucide-react";
import type { DecisionPreview, DecisionCategory } from "@/types/decision-trail";
import { DECISION_CATEGORY_COLORS, DECISION_STATUS_COLORS, parseJsonField } from "@/types/decision-trail";

interface DecisionCardProps {
  decision: DecisionPreview;
  onClick?: (id: string) => void;
  compact?: boolean;
}

function CategoryBadge({ category }: { category: string }) {
  const color = DECISION_CATEGORY_COLORS[category as DecisionCategory] || "#6B7280";
  return (
    <span
      className="inline-flex items-center gap-1 px-2 py-0.5 rounded-full text-[10px] font-medium"
      style={{ backgroundColor: `${color}20`, color }}
    >
      {category}
    </span>
  );
}

function ScaleBadge({ scale }: { scale: string }) {
  const isStrategic = scale === "strategic";
  return (
    <span
      className={`inline-flex items-center px-2 py-0.5 rounded-full text-[10px] font-semibold ${
        isStrategic
          ? "bg-purple-500/20 text-purple-400"
          : "bg-zinc-500/20 text-zinc-400"
      }`}
    >
      {isStrategic ? "Strategic" : "Tactical"}
    </span>
  );
}

function StatusDot({ status }: { status: string }) {
  const color = DECISION_STATUS_COLORS[status as keyof typeof DECISION_STATUS_COLORS] || "#6B7280";
  return (
    <span
      className="inline-block w-2 h-2 rounded-full"
      style={{ backgroundColor: color }}
      title={status}
    />
  );
}

export function DecisionCard({ decision, onClick, compact }: DecisionCardProps) {
  const tags = parseJsonField<string[]>(decision.tagsJson, []);

  const timestamp = new Date(decision.timestamp);
  const timeStr = timestamp.toLocaleDateString("en-US", {
    month: "short",
    day: "numeric",
    year: "numeric",
  });

  return (
    <button
      onClick={() => onClick?.(decision.id)}
      className={`w-full text-left rounded-lg border border-border/50 bg-card hover:bg-accent/50 transition-colors ${
        compact ? "p-3" : "p-4"
      }`}
    >
      <div className="flex items-start justify-between gap-2 mb-2">
        <div className="flex items-center gap-2 flex-wrap">
          <StatusDot status={decision.status} />
          <ScaleBadge scale={decision.scale} />
          <CategoryBadge category={decision.category} />
        </div>
        <span className="text-[10px] text-muted-foreground flex items-center gap-1 shrink-0">
          <Clock className="w-3 h-3" />
          {timeStr}
        </span>
      </div>

      <h3 className={`font-medium text-foreground leading-snug ${compact ? "text-xs" : "text-sm"}`}>
        {decision.title}
      </h3>

      {!compact && decision.summaryPreview && (
        <p className="mt-1.5 text-xs text-muted-foreground line-clamp-2">
          {decision.summaryPreview}
        </p>
      )}

      {decision.triggeredBy && (
        <div className="mt-2 flex items-center gap-1 text-[10px] text-muted-foreground">
          <ArrowRight className="w-3 h-3" />
          <span className="truncate">{decision.triggeredBy}</span>
        </div>
      )}

      {tags.length > 0 && (
        <div className="mt-2 flex items-center gap-1 flex-wrap">
          <Tag className="w-3 h-3 text-muted-foreground" />
          {tags.slice(0, 4).map((tag) => (
            <span
              key={tag}
              className="px-1.5 py-0.5 rounded text-[10px] bg-muted text-muted-foreground"
            >
              {tag}
            </span>
          ))}
          {tags.length > 4 && (
            <span className="text-[10px] text-muted-foreground">+{tags.length - 4}</span>
          )}
        </div>
      )}
    </button>
  );
}
