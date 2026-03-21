interface SpecTriageViewProps {
  specId: string;
  specConfig: {
    description?: string;
    groups: Array<{
      id: string;
      name: string;
      category?: string;
      assertions: Array<{
        id: string;
        description: string;
        severity: string;
        assertionType: string;
        enabled?: boolean;
      }>;
    }>;
  };
  onUpdateSpec?: (specId: string, config: unknown) => void;
  onFixCode?: (specId: string, assertionId: string) => void;
}

export function SpecTriageView({ specId, specConfig }: SpecTriageViewProps) {
  return (
    <div className="p-4 text-sm text-muted-foreground">
      Triage view for {specId} ({specConfig.groups.length} groups)
    </div>
  );
}
