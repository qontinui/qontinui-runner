import { ConceptCard } from "./ConceptCard";
import type { ConceptSummaryPreview } from "@/types/decision-trail";

interface ConceptViewProps {
  concepts: ConceptSummaryPreview[];
  onSelectConcept: (id: string) => void;
}

export function ConceptView({ concepts, onSelectConcept }: ConceptViewProps) {
  if (concepts.length === 0) {
    return (
      <div className="flex items-center justify-center h-full text-muted-foreground text-sm">
        No concept summaries yet. Create one to document a major feature or architectural pattern.
      </div>
    );
  }

  return (
    <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 gap-3">
      {concepts.map((c) => (
        <ConceptCard key={c.id} concept={c} onClick={onSelectConcept} />
      ))}
    </div>
  );
}
