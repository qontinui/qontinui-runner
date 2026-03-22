/**
 * Spec Runner
 *
 * Core function that runs SpecExecutor against a set of elements.
 * Used by the IPC handler to execute spec groups from Rust requests.
 */

import { SpecExecutor } from "@qontinui/ui-bridge/specs";
import type { SpecGroup, SpecGroupResult, SpecExecutionOptions } from "@qontinui/ui-bridge/specs";
import type { DiscoveredElement } from "@qontinui/ui-bridge/control";
import type { ArtifactStore } from "@qontinui/ui-bridge/artifacts";
import { createArtifact, captureEnvironment } from "@qontinui/ui-bridge/artifacts";

export interface SpecRunnerOptions extends SpecExecutionOptions {
  /** Optional artifact store for persisting immutable result records. */
  artifactStore?: ArtifactStore;
  /** Spec ID for artifact source tracing. */
  specId?: string;
}

/**
 * Execute a single SpecGroup against a set of discovered elements.
 *
 * Creates a fresh SpecExecutor, loads elements, and runs assertions.
 * Optionally wraps the result in an immutable artifact.
 */
export async function executeSpecGroup(
  group: SpecGroup,
  elements: DiscoveredElement[],
  options?: SpecRunnerOptions,
): Promise<SpecGroupResult> {
  const executor = new SpecExecutor();
  executor.updateElements(elements);
  const result = await executor.executeGroup(group, options);

  // Persist as immutable artifact if a store is provided
  if (options?.artifactStore) {
    try {
      const artifact = await createArtifact(
        result,
        {
          specId: options.specId,
          triggeredBy: "workflow-step",
        },
        captureEnvironment(),
      );
      await options.artifactStore.save(artifact);
    } catch {
      // Artifact emission is best-effort — don't break spec execution
    }
  }

  return result;
}
