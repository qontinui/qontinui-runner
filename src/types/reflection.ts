/**
 * Types for the reflection workflow system.
 *
 * Mirrors the Rust types in src-tauri/src/reflection/types.rs.
 */

/** Fix types that a reflection workflow can apply */
export type FixType =
  | "knowledge_base_update"
  | "workflow_step_rewrite"
  | "selector_fix"
  | "tool_config_update"
  | "context_addition"
  | "instruction_clarification";

/** Status of a reflection fix */
export type FixStatus = "applied" | "reverted" | "superseded";

/** Effectiveness evaluation result */
export type FixEffectiveness = "effective" | "ineffective" | "caused_regression" | "inconclusive";

/** Confidence level for a fix */
export type FixConfidence = "high" | "medium" | "low";

/** A fix applied by a reflection workflow */
export interface ReflectionFix {
  id: string;
  source_task_run_id: string;
  reflection_task_run_id: string;
  source_finding_id?: string;
  source_knowledge_id?: string;
  fix_type: FixType;
  fix_description: string;
  file_changed?: string;
  old_value?: string;
  new_value?: string;
  confidence: FixConfidence;
  status: FixStatus;
  effectiveness?: FixEffectiveness;
  effectiveness_evidence?: string;
  applied_at: string;
  evaluated_at?: string;
  created_at: string;
}

/** Input for creating a new reflection fix */
export interface CreateReflectionFixInput {
  source_task_run_id: string;
  reflection_task_run_id: string;
  source_finding_id?: string;
  source_knowledge_id?: string;
  fix_type: FixType;
  fix_description: string;
  file_changed?: string;
  old_value?: string;
  new_value?: string;
  confidence?: FixConfidence;
}

/** Input for updating fix effectiveness */
export interface UpdateEffectivenessInput {
  effectiveness: FixEffectiveness;
  effectiveness_evidence?: string;
}

/** Aggregated effectiveness report for a workflow */
export interface EffectivenessReport {
  workflow_name: string;
  total_fixes: number;
  effective_count: number;
  ineffective_count: number;
  regression_count: number;
  inconclusive_count: number;
  unevaluated_count: number;
  effectiveness_rate: number;
  fixes: ReflectionFix[];
}

/** Summary of a reflection run */
export interface ReflectionRunSummary {
  task_run_id: string;
  source_task_run_id?: string;
  status: string;
  created_at: string;
  completed_at?: string;
  fix_count: number;
}

/** Response from the trigger endpoint */
export interface TriggerResponse {
  reflection_task_run_id: string;
  status: "launched" | "skipped";
}

/** Response from the evaluate endpoint */
export interface EvaluateResponse {
  evaluated_count: number;
  effective_count: number;
}
