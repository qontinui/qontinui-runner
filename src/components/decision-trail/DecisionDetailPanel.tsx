import { X, ExternalLink, FileText, Globe, Database, Tag, ArrowRight, Clock } from "lucide-react";
import type { Decision, ConceptSummaryRecord, Alternative, Inspiration } from "@/types/decision-trail";
import { DECISION_CATEGORY_COLORS, DECISION_STATUS_COLORS, parseJsonField } from "@/types/decision-trail";
import type { DecisionCategory, DecisionStatus } from "@/types/decision-trail";

interface DecisionDetailPanelProps {
  decision: Decision | null;
  concept: ConceptSummaryRecord | null;
  onClose: () => void;
}

function Section({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <div className="space-y-1.5">
      <h4 className="text-[10px] font-semibold text-muted-foreground uppercase tracking-wider">{title}</h4>
      {children}
    </div>
  );
}

function DecisionDetail({ decision }: { decision: Decision }) {
  const alternatives: Alternative[] = parseJsonField(decision.alternativesJson, []);
  const tradeoffs: string[] = parseJsonField(decision.tradeoffsJson, []);
  const inspiration: Inspiration | null = parseJsonField<Inspiration | null>(decision.inspirationJson, null);
  const relatedDecisions: string[] = parseJsonField(decision.relatedDecisionsJson, []);
  const affectedFiles: string[] = parseJsonField(decision.affectedFilesJson, []);
  const affectedEndpoints: string[] = parseJsonField(decision.affectedEndpointsJson, []);
  const affectedTables: string[] = parseJsonField(decision.affectedTablesJson, []);
  const tags: string[] = parseJsonField(decision.tagsJson, []);
  const categoryColor = DECISION_CATEGORY_COLORS[decision.category as DecisionCategory] || "#6B7280";
  const statusColor = DECISION_STATUS_COLORS[decision.status as DecisionStatus] || "#6B7280";

  return (
    <div className="space-y-4">
      {/* Header */}
      <div>
        <div className="flex items-center gap-2 mb-2">
          <span
            className="px-2 py-0.5 rounded-full text-[10px] font-medium"
            style={{ backgroundColor: `${categoryColor}20`, color: categoryColor }}
          >
            {decision.category}
          </span>
          <span
            className="px-2 py-0.5 rounded-full text-[10px] font-medium"
            style={{ backgroundColor: `${statusColor}20`, color: statusColor }}
          >
            {decision.status}
          </span>
          <span className={`px-2 py-0.5 rounded-full text-[10px] font-semibold ${
            decision.scale === "strategic" ? "bg-purple-500/20 text-purple-400" : "bg-zinc-500/20 text-zinc-400"
          }`}>
            {decision.scale}
          </span>
        </div>
        <h2 className="text-base font-semibold text-foreground">{decision.title}</h2>
        <div className="flex items-center gap-1 mt-1 text-[10px] text-muted-foreground">
          <Clock className="w-3 h-3" />
          {new Date(decision.timestamp).toLocaleString()}
        </div>
      </div>

      <Section title="Summary">
        <p className="text-xs text-foreground/80 leading-relaxed">{decision.summary}</p>
      </Section>

      <Section title="Rationale">
        <p className="text-xs text-foreground/80 leading-relaxed">{decision.rationale}</p>
      </Section>

      {decision.triggeredBy && (
        <Section title="Triggered By">
          <div className="flex items-center gap-1 text-xs text-foreground/80">
            <ArrowRight className="w-3 h-3" />
            {decision.triggeredBy}
          </div>
        </Section>
      )}

      {alternatives.length > 0 && (
        <Section title="Alternatives Considered">
          <div className="space-y-2">
            {alternatives.map((alt, i) => (
              <div key={i} className="rounded-md bg-muted/50 p-2">
                <div className="text-xs font-medium text-foreground">{alt.name}</div>
                <div className="text-[11px] text-muted-foreground mt-0.5">{alt.reasonRejected}</div>
              </div>
            ))}
          </div>
        </Section>
      )}

      {tradeoffs.length > 0 && (
        <Section title="Accepted Tradeoffs">
          <ul className="space-y-1">
            {tradeoffs.map((t, i) => (
              <li key={i} className="text-xs text-foreground/80 flex items-start gap-1.5">
                <span className="text-amber-400 mt-0.5">-</span>
                {t}
              </li>
            ))}
          </ul>
        </Section>
      )}

      {inspiration && (
        <Section title="Inspiration">
          <div className="rounded-md bg-muted/50 p-2 space-y-1">
            <div className="flex items-center gap-1 text-xs font-medium text-foreground">
              <Globe className="w-3 h-3" />
              {inspiration.source}
              {inspiration.url && (
                <a href={inspiration.url} target="_blank" rel="noopener noreferrer" className="text-primary">
                  <ExternalLink className="w-3 h-3" />
                </a>
              )}
            </div>
            <div className="text-[11px] text-muted-foreground">{inspiration.description}</div>
            {inspiration.whatWeAdapted && (
              <div className="text-[11px] text-foreground/70">
                <span className="font-medium">Adapted:</span> {inspiration.whatWeAdapted}
              </div>
            )}
          </div>
        </Section>
      )}

      {affectedFiles.length > 0 && (
        <Section title="Affected Files">
          <div className="space-y-0.5">
            {affectedFiles.map((f, i) => (
              <div key={i} className="flex items-center gap-1 text-[11px] text-foreground/70 font-mono">
                <FileText className="w-3 h-3 shrink-0" />
                <span className="truncate">{f}</span>
              </div>
            ))}
          </div>
        </Section>
      )}

      {affectedEndpoints.length > 0 && (
        <Section title="Affected Endpoints">
          <div className="space-y-0.5">
            {affectedEndpoints.map((e, i) => (
              <div key={i} className="text-[11px] text-foreground/70 font-mono">{e}</div>
            ))}
          </div>
        </Section>
      )}

      {affectedTables.length > 0 && (
        <Section title="Affected Tables">
          <div className="flex flex-wrap gap-1">
            {affectedTables.map((t, i) => (
              <span key={i} className="flex items-center gap-1 px-1.5 py-0.5 rounded text-[10px] bg-muted text-foreground/70 font-mono">
                <Database className="w-3 h-3" />{t}
              </span>
            ))}
          </div>
        </Section>
      )}

      {tags.length > 0 && (
        <Section title="Tags">
          <div className="flex flex-wrap gap-1">
            {tags.map((t, i) => (
              <span key={i} className="flex items-center gap-1 px-1.5 py-0.5 rounded text-[10px] bg-muted text-muted-foreground">
                <Tag className="w-3 h-3" />{t}
              </span>
            ))}
          </div>
        </Section>
      )}

      {relatedDecisions.length > 0 && (
        <Section title="Related Decisions">
          <div className="space-y-0.5">
            {relatedDecisions.map((id, i) => (
              <div key={i} className="text-[11px] text-primary font-mono">{id}</div>
            ))}
          </div>
        </Section>
      )}
    </div>
  );
}

function ConceptDetail({ concept }: { concept: ConceptSummaryRecord }) {
  const inspiration: Inspiration | null = parseJsonField<Inspiration | null>(concept.inspirationJson, null);
  const benefits: string[] = parseJsonField(concept.benefitsJson, []);
  const components: string[] = parseJsonField(concept.componentsJson, []);
  const relatedDecisions: string[] = parseJsonField(concept.relatedDecisionsJson, []);
  const metrics = parseJsonField<Record<string, number>>(concept.metricsJson, {});

  return (
    <div className="space-y-4">
      <div>
        <h2 className="text-base font-semibold text-foreground">{concept.name}</h2>
        <p className="text-xs text-purple-400 font-medium mt-1">{concept.tagline}</p>
      </div>

      <Section title="Description">
        <p className="text-xs text-foreground/80 leading-relaxed whitespace-pre-wrap">{concept.description}</p>
      </Section>

      {inspiration && (
        <Section title="Inspiration">
          <div className="rounded-md bg-muted/50 p-2 space-y-1">
            <div className="flex items-center gap-1 text-xs font-medium text-foreground">
              <Globe className="w-3 h-3" />
              {inspiration.source}
            </div>
            <div className="text-[11px] text-muted-foreground">{inspiration.description}</div>
            {inspiration.whatWeAdapted && (
              <div className="text-[11px] text-foreground/70">
                <span className="font-medium">Adapted:</span> {inspiration.whatWeAdapted}
              </div>
            )}
            {inspiration.howItBenefitsQontinui && (
              <div className="text-[11px] text-foreground/70">
                <span className="font-medium">Benefits:</span> {inspiration.howItBenefitsQontinui}
              </div>
            )}
          </div>
        </Section>
      )}

      {benefits.length > 0 && (
        <Section title="Benefits">
          <ul className="space-y-1">
            {benefits.map((b, i) => (
              <li key={i} className="text-xs text-foreground/80 flex items-start gap-1.5">
                <span className="text-green-400 mt-0.5">+</span>
                {b}
              </li>
            ))}
          </ul>
        </Section>
      )}

      {components.length > 0 && (
        <Section title="Components">
          <div className="space-y-0.5">
            {components.map((c, i) => (
              <div key={i} className="text-[11px] text-foreground/70 font-mono">
                <FileText className="w-3 h-3 inline mr-1" />{c}
              </div>
            ))}
          </div>
        </Section>
      )}

      {Object.keys(metrics).length > 0 && (
        <Section title="Metrics">
          <div className="flex gap-3">
            {metrics.tablesAdded != null && (
              <div className="text-center">
                <div className="text-lg font-bold text-foreground">{metrics.tablesAdded}</div>
                <div className="text-[10px] text-muted-foreground">Tables</div>
              </div>
            )}
            {metrics.endpointsAdded != null && (
              <div className="text-center">
                <div className="text-lg font-bold text-foreground">{metrics.endpointsAdded}</div>
                <div className="text-[10px] text-muted-foreground">Endpoints</div>
              </div>
            )}
            {metrics.componentsAdded != null && (
              <div className="text-center">
                <div className="text-lg font-bold text-foreground">{metrics.componentsAdded}</div>
                <div className="text-[10px] text-muted-foreground">Components</div>
              </div>
            )}
          </div>
        </Section>
      )}

      {relatedDecisions.length > 0 && (
        <Section title="Related Decisions">
          <div className="space-y-0.5">
            {relatedDecisions.map((id, i) => (
              <div key={i} className="text-[11px] text-primary font-mono">{id}</div>
            ))}
          </div>
        </Section>
      )}
    </div>
  );
}

export function DecisionDetailPanel({ decision, concept, onClose }: DecisionDetailPanelProps) {
  return (
    <div className="w-[380px] border-l border-border bg-card flex flex-col shrink-0">
      <div className="flex items-center justify-between p-3 border-b border-border">
        <h3 className="text-xs font-semibold text-muted-foreground uppercase tracking-wide">
          {decision ? "Decision Details" : "Concept Details"}
        </h3>
        <button onClick={onClose} className="p-1 rounded-md hover:bg-accent text-muted-foreground">
          <X className="w-4 h-4" />
        </button>
      </div>
      <div className="flex-1 overflow-y-auto p-4">
        {decision && <DecisionDetail decision={decision} />}
        {concept && <ConceptDetail concept={concept} />}
      </div>
    </div>
  );
}
