// Decision Trail — architectural decision history types
//
// Wire types match the Rust serde(rename_all = "camelCase") output.
// JSON-encoded arrays are transmitted as strings and parsed by components.

export interface Alternative {
  name: string;
  reasonRejected: string;
}

export interface Inspiration {
  source: string;
  url?: string;
  description: string;
  whatWeAdapted: string;
  howItBenefitsQontinui: string;
}

export type DecisionScale = "tactical" | "strategic";
export type DecisionCategory =
  | "architecture"
  | "technology"
  | "design"
  | "integration"
  | "performance"
  | "security"
  | "ux"
  | "data-model";
export type DecisionStatus = "active" | "superseded" | "reversed";

/** Full decision record (wire format from API). */
export interface Decision {
  id: string;
  timestamp: string;
  scale: string;
  category: string;
  status: string;
  title: string;
  summary: string;
  rationale: string;
  alternativesJson: string;
  tradeoffsJson: string;
  triggeredBy?: string;
  inspirationJson?: string;
  relatedDecisionsJson: string;
  affectedFilesJson: string;
  affectedEndpointsJson: string;
  affectedTablesJson: string;
  createdBy?: string;
  supersededBy?: string;
  tagsJson: string;
  isDeleted: boolean;
  createdAt: string;
  updatedAt: string;
}

/** Truncated decision preview for list views (wire format). */
export interface DecisionPreview {
  id: string;
  timestamp: string;
  scale: string;
  category: string;
  status: string;
  title: string;
  summaryPreview: string;
  triggeredBy?: string;
  tagsJson: string;
  createdBy?: string;
  createdAt: string;
  updatedAt: string;
}

export interface DecisionSearchResult extends DecisionPreview {
  rank?: number;
}

/** Full concept summary record (wire format). */
export interface ConceptSummaryRecord {
  id: string;
  name: string;
  tagline: string;
  description: string;
  inspirationJson?: string;
  benefitsJson: string;
  componentsJson: string;
  relatedDecisionsJson: string;
  metricsJson?: string;
  isDeleted: boolean;
  createdAt: string;
  updatedAt: string;
}

/** Truncated concept summary preview (wire format). */
export interface ConceptSummaryPreview {
  id: string;
  name: string;
  tagline: string;
  descriptionPreview: string;
  inspirationJson?: string;
  benefitsJson: string;
  componentsJson: string;
  relatedDecisionsJson: string;
  metricsJson?: string;
  createdAt: string;
  updatedAt: string;
}

/** Input for creating a new decision (frontend → API). */
export interface CreateDecisionInput {
  scale: DecisionScale;
  category: DecisionCategory;
  title: string;
  summary: string;
  rationale: string;
  alternativesJson?: string;
  tradeoffsJson?: string;
  triggeredBy?: string;
  inspirationJson?: string;
  relatedDecisionsJson?: string;
  affectedFilesJson?: string;
  affectedEndpointsJson?: string;
  affectedTablesJson?: string;
  tagsJson?: string;
}

/** Input for creating a concept summary. */
export interface CreateConceptSummaryInput {
  name: string;
  tagline: string;
  description: string;
  inspirationJson?: string;
  benefitsJson?: string;
  componentsJson?: string;
  relatedDecisionsJson?: string;
  metricsJson?: string;
}

// ============================================================================
// Display helpers
// ============================================================================

/** Safe JSON parse with fallback. */
export function parseJsonField<T>(json: string | null | undefined, fallback: T): T {
  if (!json) return fallback;
  try {
    return JSON.parse(json);
  } catch {
    return fallback;
  }
}

export const DECISION_CATEGORY_COLORS: Record<DecisionCategory, string> = {
  architecture: "#8B5CF6",
  technology: "#3B82F6",
  design: "#EC4899",
  integration: "#10B981",
  performance: "#F59E0B",
  security: "#EF4444",
  ux: "#06B6D4",
  "data-model": "#D97706",
};

export const DECISION_SCALE_LABELS: Record<DecisionScale, string> = {
  tactical: "Tactical",
  strategic: "Strategic",
};

export const DECISION_STATUS_COLORS: Record<DecisionStatus, string> = {
  active: "#10B981",
  superseded: "#9CA3AF",
  reversed: "#EF4444",
};
