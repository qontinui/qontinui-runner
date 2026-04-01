import { Lightbulb, ExternalLink, ChevronRight } from "lucide-react";
import type { ConceptSummaryPreview, Inspiration } from "@/types/decision-trail";
import { parseJsonField } from "@/types/decision-trail";

interface ConceptCardProps {
  concept: ConceptSummaryPreview;
  onClick?: (id: string) => void;
}

export function ConceptCard({ concept, onClick }: ConceptCardProps) {
  const benefits = parseJsonField<string[]>(concept.benefitsJson, []);
  const inspiration = parseJsonField<Inspiration | null>(concept.inspirationJson, null);
  const relatedDecisions = parseJsonField<string[]>(concept.relatedDecisionsJson, []);

  return (
    <button
      onClick={() => onClick?.(concept.id)}
      className="w-full text-left rounded-lg border border-border/50 bg-card hover:bg-accent/50 transition-colors p-4"
    >
      <div className="flex items-start justify-between mb-2">
        <div className="flex items-center gap-2">
          <div className="p-1.5 rounded-md bg-purple-500/15">
            <Lightbulb className="w-4 h-4 text-purple-400" />
          </div>
          <h3 className="text-sm font-semibold text-foreground">{concept.name}</h3>
        </div>
        <ChevronRight className="w-4 h-4 text-muted-foreground shrink-0" />
      </div>

      <p className="text-xs text-purple-400 font-medium mb-2">{concept.tagline}</p>

      {concept.descriptionPreview && (
        <p className="text-xs text-muted-foreground line-clamp-3 mb-3">
          {concept.descriptionPreview}
        </p>
      )}

      {inspiration && (
        <div className="flex items-center gap-1 text-[10px] text-muted-foreground mb-2">
          <ExternalLink className="w-3 h-3" />
          <span>Inspired by: {inspiration.source}</span>
        </div>
      )}

      <div className="flex items-center gap-3 text-[10px] text-muted-foreground">
        {benefits.length > 0 && (
          <span>
            {benefits.length} benefit{benefits.length !== 1 ? "s" : ""}
          </span>
        )}
        {relatedDecisions.length > 0 && (
          <span>
            {relatedDecisions.length} decision{relatedDecisions.length !== 1 ? "s" : ""}
          </span>
        )}
      </div>
    </button>
  );
}
