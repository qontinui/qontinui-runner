// Architecture model types — mirrors Rust types from reflection::architecture

export interface ComponentNode {
  id: string;
  component_path: string;
  component_type: string; // 'file' | 'module' | 'service'
  fix_count: number;
  error_count: number;
  causal_involvement_count: number;
  effective_fix_count: number;
  ineffective_fix_count: number;
  health_score: number;
  change_velocity: number;
  last_activity_at: string | null;
}

export interface ComponentEdge {
  source: string;
  target: string;
  relationship_type: string; // 'impacts' | 'co_changes_with'
  strength: number;
}

export interface GraphStats {
  total_components: number;
  total_relationships: number;
  avg_health_score: number;
  most_volatile: string[];
}

export interface ComponentGraph {
  nodes: ComponentNode[];
  edges: ComponentEdge[];
  stats: GraphStats;
}

export interface RebuildResult {
  components_count: number;
  relationships_count: number;
  workflow_name: string;
}

export interface FixSummary {
  id: string;
  fix_type: string;
  fix_description: string;
  effectiveness: string | null;
  applied_at: string | null;
}

export interface ImpactEntry {
  component_path: string;
  relationship_type: string;
  strength: number;
}

export interface ComponentDetails {
  node: ComponentNode;
  recent_fixes: FixSummary[];
  impacted_by: ImpactEntry[];
  impacts: ImpactEntry[];
}

export interface ImpactAnalysis {
  component: string;
  direct_impacts: ImpactEntry[];
  transitive_impacts: ImpactEntry[];
  total_impact_radius: number;
  highest_risk_path: string[];
}

// Temporal trend types (v103)

export interface TrendPoint {
  timestamp: string;
  value: number;
  count?: number;
}

export interface WorkflowTrends {
  workflow_name: string;
  convergence: TrendPoint[];
  fix_rate: TrendPoint[];
  velocity: TrendPoint[];
  total_fixes: TrendPoint[];
  snapshot_count: number;
}

export interface ComponentTrend {
  component_path: string;
  health_scores: TrendPoint[];
  fix_counts: TrendPoint[];
}

// Effectiveness over time types (v106)

export interface EffectivenessBucket {
  bucket: string;
  total: number;
  effective: number;
  ineffective: number;
  regression: number;
  effectiveness_rate: number;
}

export interface EffectivenessOverTime {
  workflow_name: string;
  bucket_type: string;
  buckets: EffectivenessBucket[];
}
