/**
 * Spec Runner
 *
 * Core function that runs SpecExecutor against a set of elements.
 * Used by the IPC handler to execute spec groups from Rust requests.
 */

import { SpecExecutor } from "@qontinui/ui-bridge/specs";
import type { SpecGroup, SpecGroupResult, SpecExecutionOptions } from "@qontinui/ui-bridge/specs";
import type { DiscoveredElement } from "@qontinui/ui-bridge/control";

/**
 * Execute a single SpecGroup against a set of discovered elements.
 *
 * Creates a fresh SpecExecutor, loads elements, and runs assertions.
 */
export async function executeSpecGroup(
  group: SpecGroup,
  elements: DiscoveredElement[],
  options?: SpecExecutionOptions,
): Promise<SpecGroupResult> {
  const executor = new SpecExecutor();
  executor.updateElements(elements);
  return executor.executeGroup(group, options);
}
