/**
 * Pathfinding Step Output Handler
 *
 * Handles outputs from pathfinding steps (state-machine path search),
 * providing:
 * - Parsing of the computed path + per-edge metadata
 * - AI summary generation for verification
 * - Test config auto-population
 * - Assertable field extraction
 *
 * Input schema (edge): `qontinui-schemas/.../per_type/pathfinding_step.py`
 * (`PathfindingStep`).
 * Result envelope: `.../per_type/pathfinding_result.py` (`PathfindingResult`).
 */

import {
  type PathfindingPathEntry,
  type PathfindingStepOutput,
  type StepOutputType,
  generateStepOutputId,
} from "../../types/step-output";
import type {
  StepOutputHandler,
  StepOutputMetadata,
  TestAutoPopulateConfig,
  AssertableField,
} from "./types";

// ============================================================================
// Raw Input Types
// ============================================================================

interface RawPathfindingStep {
  transition_id?: string;
  transition_name?: string;
  from_states?: string[];
  activate_states?: string[];
  exit_states?: string[];
  path_cost?: number;
}

interface RawPathfindingOutput {
  found?: boolean;
  steps?: RawPathfindingStep[];
  path?: RawPathfindingStep[]; // Alternative field name
  total_cost?: number;
  error?: string;
  from_states?: string[];
  to_states?: string[];
}

interface PathfindingStepConfig {
  from_states?: string[];
  to_states?: string[];
  target_states?: string[]; // Alternative field name
}

// ============================================================================
// Handler Implementation
// ============================================================================

export class PathfindingHandler implements StepOutputHandler<PathfindingStepOutput> {
  readonly stepType: StepOutputType = "pathfinding";
  readonly displayName = "Pathfinding";
  readonly description = "State-machine path search with ordered transition edges and total cost";

  parseOutput(
    rawOutput: unknown,
    stepConfig?: unknown,
    metadata?: StepOutputMetadata,
  ): PathfindingStepOutput {
    const raw = (rawOutput || {}) as RawPathfindingOutput;
    const config = (stepConfig || {}) as PathfindingStepConfig;

    const rawPath = raw.steps ?? raw.path ?? [];
    const path: PathfindingPathEntry[] = rawPath.map((entry) => ({
      transition_id: entry.transition_id ?? "",
      transition_name: entry.transition_name ?? "",
      from_states: entry.from_states ?? [],
      activate_states: entry.activate_states ?? [],
      exit_states: entry.exit_states ?? [],
      path_cost: entry.path_cost ?? 0,
    }));

    const totalCost =
      raw.total_cost ?? path.reduce((sum, entry) => sum + (entry.path_cost || 0), 0);
    const found = raw.found ?? path.length > 0;

    return {
      id: generateStepOutputId(),
      step_type: "pathfinding",
      step_id: metadata?.step_id,
      step_name: metadata?.step_name || this.deriveName(config, raw),
      executed_at: metadata?.started_at || new Date().toISOString(),
      duration_ms: metadata?.duration_ms ?? 0,
      success: metadata?.success ?? (found && !raw.error),
      error: metadata?.error ?? raw.error,
      found,
      path,
      total_cost: totalCost,
      from_states: raw.from_states ?? config.from_states,
      to_states: raw.to_states ?? config.to_states ?? config.target_states,
      pathfinding_error: raw.error,
    };
  }

  summarizeForAI(output: PathfindingStepOutput): string {
    const lines: string[] = [];

    lines.push(`### Pathfinding: ${output.step_name}`);

    const statusEmoji = output.found ? "✅" : "❌";
    lines.push(`- **Status:** ${statusEmoji} ${output.found ? "Path Found" : "No Path Found"}`);

    if (output.from_states && output.from_states.length > 0) {
      lines.push(`- **From:** ${output.from_states.join(", ")}`);
    }

    if (output.to_states && output.to_states.length > 0) {
      lines.push(`- **To:** ${output.to_states.join(", ")}`);
    }

    lines.push(`- **Edges:** ${output.path.length}`);
    lines.push(`- **Total Cost:** ${this.formatCost(output.total_cost)}`);

    if (output.duration_ms !== undefined) {
      lines.push(`- **Duration:** ${output.duration_ms}ms`);
    }

    if (output.pathfinding_error) {
      lines.push(`- **Pathfinding Error:** ${output.pathfinding_error}`);
    }

    if (output.error && output.error !== output.pathfinding_error) {
      lines.push(`- **Error:** ${output.error}`);
    }

    // Ordered path
    if (output.path.length > 0) {
      lines.push("");
      lines.push("**Computed Path:**");
      lines.push("");
      lines.push("| # | Transition | From | Activates | Exits | Cost |");
      lines.push("|---|------------|------|-----------|-------|------|");
      const maxRows = 30;
      const renderable = output.path.slice(0, maxRows);
      for (let i = 0; i < renderable.length; i++) {
        const e = renderable[i];
        const name = e.transition_name || e.transition_id || "-";
        const from = e.from_states.length > 0 ? e.from_states.join(", ") : "-";
        const act = e.activate_states.length > 0 ? e.activate_states.join(", ") : "-";
        const ex = e.exit_states.length > 0 ? e.exit_states.join(", ") : "-";
        lines.push(
          `| ${i + 1} | ${this.truncateValue(name)} | ${this.truncateValue(from)} | ${this.truncateValue(act)} | ${this.truncateValue(ex)} | ${this.formatCost(e.path_cost)} |`,
        );
      }
      if (output.path.length > maxRows) {
        lines.push(`*... and ${output.path.length - maxRows} more edges*`);
      }

      // Truncate the whole block if it's getting too long.
      const rendered = lines.join("\n");
      if (rendered.length > 4000) {
        return rendered.substring(0, 4000) + "\n... (truncated)";
      }
    }

    return lines.join("\n");
  }

  toTestConfig(output: PathfindingStepOutput): TestAutoPopulateConfig | null {
    // Pathfinding is a pure search: it has no config that a test can replay
    // independently of the rest of the state-machine fixture. The path
    // itself is a result, not a config. Return null so the test draft
    // surface doesn't try to "pin" a non-deterministic search result.
    if (output.path.length === 0 && !output.from_states && !output.to_states) {
      return null;
    }

    const config: Record<string, unknown> = {
      found: output.found,
      edge_count: output.path.length,
      expected_total_cost: output.total_cost,
    };

    if (output.from_states && output.from_states.length > 0) {
      config.from_states = output.from_states;
    }
    if (output.to_states && output.to_states.length > 0) {
      config.to_states = output.to_states;
    }

    if (output.path.length > 0) {
      config.expected_transitions = output.path.map((e) => e.transition_id);
    }

    return {
      config_key: "pathfinding_config",
      config_value: config,
      description: `Pathfinding config (${output.path.length} edges, cost=${this.formatCost(output.total_cost)})`,
    };
  }

  getAssertableFields(output: PathfindingStepOutput): AssertableField[] {
    const fields: AssertableField[] = [];

    fields.push({
      path: "found",
      name: "Path Found",
      description: "Whether the search found a valid path",
      type: "boolean",
      example_value: output.found,
      suggested_assertions: ["Assert a path was found", "Assert no path was found"],
    });

    fields.push({
      path: "path.length",
      name: "Edge Count",
      description: "Number of edges on the computed path",
      type: "number",
      example_value: output.path.length,
      suggested_assertions: [
        "Assert edge count equals expected",
        "Assert edge count is within bounds",
      ],
    });

    fields.push({
      path: "total_cost",
      name: "Total Path Cost",
      description: "Sum of per-edge costs",
      type: "number",
      example_value: output.total_cost,
      suggested_assertions: [
        "Assert total cost is under threshold",
        "Assert total cost equals expected",
      ],
    });

    if (output.from_states && output.from_states.length > 0) {
      fields.push({
        path: "from_states",
        name: "From States",
        description: "Starting state set used by the search",
        type: "array",
        example_value: output.from_states,
        suggested_assertions: ["Assert search started from expected states"],
      });
    }

    if (output.to_states && output.to_states.length > 0) {
      fields.push({
        path: "to_states",
        name: "To States",
        description: "Target state set the search aimed at",
        type: "array",
        example_value: output.to_states,
        suggested_assertions: ["Assert search targeted expected states"],
      });
    }

    // Per-edge assertables for the first few edges
    const maxEdges = Math.min(output.path.length, 3);
    for (let i = 0; i < maxEdges; i++) {
      const edge = output.path[i];
      fields.push({
        path: `path[${i}].transition_id`,
        name: `Edge ${i + 1} Transition`,
        description: `Transition taken at position ${i + 1}: ${edge.transition_name || edge.transition_id}`,
        type: "string",
        example_value: edge.transition_id,
        suggested_assertions: [`Assert edge ${i + 1} uses expected transition`],
      });
    }

    if (output.duration_ms !== undefined) {
      fields.push({
        path: "duration_ms",
        name: "Search Duration",
        description: "Time taken to run pathfinding in milliseconds",
        type: "number",
        example_value: output.duration_ms,
        suggested_assertions: [
          "Assert search completes within time limit",
          "Record as performance metric",
        ],
      });
    }

    return fields;
  }

  /**
   * Derive a compact step name from the endpoints of the search.
   */
  private deriveName(config: PathfindingStepConfig, raw: RawPathfindingOutput): string {
    const from = (raw.from_states ?? config.from_states ?? [])[0];
    const to = (raw.to_states ?? config.to_states ?? config.target_states ?? [])[0];
    if (from && to) return `${from} → ${to}`;
    if (to) return `→ ${to}`;
    if (from) return `${from} →`;
    return "pathfinding";
  }

  /**
   * Render a path_cost with up to 3 decimal places (pathfinding costs are floats).
   */
  private formatCost(cost: number): string {
    if (!Number.isFinite(cost)) return String(cost);
    if (Number.isInteger(cost)) return cost.toString();
    return cost.toFixed(3).replace(/\.?0+$/, "");
  }

  /**
   * Truncate an inline value for display in a table cell.
   */
  private truncateValue(value: string): string {
    if (value.length <= 40) return value;
    return value.substring(0, 37) + "...";
  }
}

// ============================================================================
// Export singleton instance
// ============================================================================

export const pathfindingHandler = new PathfindingHandler();
