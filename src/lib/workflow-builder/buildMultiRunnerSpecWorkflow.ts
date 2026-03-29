/**
 * buildMultiRunnerSpecWorkflow
 *
 * Given all discovered specs for an application and a set of target runners,
 * partitions the specs across runners, generates a workflow for each partition,
 * and returns a MultiLoopConfig ready to launch.
 */

import type { DiscoveredSpec } from "../spec-prompt-builder";
import { partitionSpecs, type PartitionStrategy } from "./partitionSpecs";
import { buildSpecWorkflow, type BuildSpecWorkflowInput } from "./buildSpecWorkflow";
import type { UnifiedWorkflow } from "../../types/unified-workflow";

export interface RunnerTarget {
  /** Runner instance ID (used by supervisor for restarts). */
  runnerId: string;
  /** Port the target runner is listening on. */
  port: number;
  /** Human-readable name for display. */
  name: string;
}

export interface MultiRunnerSpecWorkflowInput {
  /** All discovered specs to distribute. */
  specs: DiscoveredSpec[];
  /** Available target runners. */
  runners: RunnerTarget[];
  /** How to split specs across runners. */
  partitionStrategy?: PartitionStrategy;
  /** Shared loop settings applied to every loop entry. */
  loopSettings?: {
    maxIterations?: number;
    supervisorPort?: number;
    exitStrategy?: { type: "reflection" } | { type: "workflow_verification" } | { type: "fixed_iterations" };
    betweenIterations?: { type: "restart_runner"; rebuild: boolean } | { type: "restart_on_signal"; rebuild: boolean } | { type: "wait_healthy" } | { type: "none" };
    retryOnFailure?: boolean;
  };
  /** Custom agentic prompt for failure recovery in each workflow. */
  agenticPrompt?: string;
  /** Element source for UI Bridge steps. */
  elementSource?: "control" | "external";
  /** Max spec workflow iterations (inner loop per-workflow). */
  specMaxIterations?: number;
  /** Stop all loops if any single loop errors out. */
  stopAllOnError?: boolean;
}

export interface MultiRunnerSpecWorkflowResult {
  /** MultiLoopConfig ready to pass to start_multi_orchestration_loop. */
  multiLoopConfig: MultiLoopConfig;
  /** Generated workflows that need to be saved before launching. */
  workflows: Array<{ loopId: string; workflow: UnifiedWorkflow }>;
  /** Partition details for display. */
  partitions: Array<{
    loopId: string;
    label: string;
    runner: RunnerTarget;
    specCount: number;
    assertionCount: number;
  }>;
}

/** Matches the Rust MultiLoopConfig / MultiLoopEntry types. */
export interface MultiLoopConfig {
  loops: MultiLoopEntry[];
  stop_all_on_error: boolean;
}

export interface MultiLoopEntry {
  loop_id: string;
  label: string | null;
  config: {
    target_runner_port: number | null;
    target_runner_id: string | null;
    supervisor_port: number;
    workflow_id: string;
    max_iterations: number;
    exit_strategy: { type: string };
    between_iterations: { type: string; rebuild?: boolean };
    retry_on_failure: boolean;
    wait_for_fixer: boolean;
    pipeline: null;
  };
}

/**
 * Merge multiple SpecConfigs into one by concatenating their groups.
 * Each group gets a prefix to avoid ID collisions.
 * Uses BuildSpecWorkflowInput's specConfig which accepts the broader type.
 */
function mergeSpecConfigs(specs: DiscoveredSpec[]): BuildSpecWorkflowInput["specConfig"] {
  const allGroups = specs.flatMap((spec) =>
    spec.config.groups.map((group) => ({
      ...group,
      // Prefix group ID with spec ID to avoid collisions
      id: `${spec.specId}__${group.id}`,
      name: `[${spec.specId}] ${group.name}`,
    })),
  );

  return {
    version: "1.0.0",
    description: `Merged spec: ${specs.map((s) => s.specId).join(", ")}`,
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    groups: allGroups as any,
    metadata: {
      specType: "semantic",
      elementSource: specs[0]?.config.metadata?.elementSource ?? "control",
    },
  };
}

/** Count total enabled assertions in a spec. */
function countAssertions(spec: DiscoveredSpec): number {
  return spec.config.groups.reduce(
    (sum, g) => sum + g.assertions.filter((a) => a.enabled !== false).length,
    0,
  );
}

/**
 * Build a multi-runner spec workflow configuration.
 *
 * Returns workflows that need to be saved and a MultiLoopConfig that can be
 * passed directly to the `start_multi_orchestration_loop` Tauri command.
 *
 * Caller is responsible for saving each workflow via the unified-workflows API
 * before launching the multi-loop.
 */
export function buildMultiRunnerSpecWorkflow(
  input: MultiRunnerSpecWorkflowInput,
): MultiRunnerSpecWorkflowResult {
  const {
    specs,
    runners,
    partitionStrategy = "balanced",
    loopSettings = {},
    agenticPrompt,
    elementSource,
    specMaxIterations = 3,
    stopAllOnError = false,
  } = input;

  const partitions = partitionSpecs(specs, runners.length, partitionStrategy);

  const workflows: MultiRunnerSpecWorkflowResult["workflows"] = [];
  const partitionDetails: MultiRunnerSpecWorkflowResult["partitions"] = [];
  const loopEntries: MultiLoopEntry[] = [];

  for (let i = 0; i < partitions.length; i++) {
    const partition = partitions[i];
    const runner = runners[i];
    const loopId = `spec-${runner.runnerId}`;

    // Merge all specs in this partition into a single SpecConfig
    const mergedConfig = mergeSpecConfigs(partition.specs);

    // Build workflow from merged spec
    const buildInput: BuildSpecWorkflowInput = {
      specConfig: mergedConfig,
      maxIterations: specMaxIterations,
      elementSource: elementSource ?? "control",
      workflowName: `Spec Check: ${partition.label} (${runner.name})`,
      agenticPrompt,
    };

    const workflow = buildSpecWorkflow(buildInput);

    workflows.push({ loopId, workflow });
    partitionDetails.push({
      loopId,
      label: partition.label,
      runner,
      specCount: partition.specs.length,
      assertionCount: partition.specs.reduce((sum, s) => sum + countAssertions(s), 0),
    });

    loopEntries.push({
      loop_id: loopId,
      label: `${partition.label} → ${runner.name}`,
      config: {
        target_runner_port: runner.port,
        target_runner_id: runner.runnerId,
        supervisor_port: loopSettings.supervisorPort ?? 9875,
        workflow_id: workflow.id ?? "",
        max_iterations: loopSettings.maxIterations ?? 5,
        exit_strategy: loopSettings.exitStrategy ?? { type: "reflection" },
        between_iterations: loopSettings.betweenIterations ?? { type: "none" },
        retry_on_failure: loopSettings.retryOnFailure ?? false,
        wait_for_fixer: true,
        pipeline: null,
      },
    });
  }

  return {
    multiLoopConfig: {
      loops: loopEntries,
      stop_all_on_error: stopAllOnError,
    },
    workflows,
    partitions: partitionDetails,
  };
}
