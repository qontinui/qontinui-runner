/**
 * generateFromBrief
 *
 * Unified spec → AI workflow generation entry point. Serializes a
 * GenerationBrief into a prompt-ready context block and delegates to the
 * standalone Tauri workflow generator (the Builder-Verifier-Fixer pipeline).
 *
 * Callers are responsible for interpreting the returned CommandResponse
 * (shape: { success, message?, data? }) — this module deliberately does
 * not transform it, to stay consistent with `useWorkflowGeneration`.
 */

import { invoke } from "@tauri-apps/api/core";
import { createLogger } from "@/lib/logger";
import type { GenerationBrief } from "../../types/generation-brief";
import type { CommandResponse } from "../../components/terminal/types";

const log = createLogger("generateFromBrief");

export interface GenerateFromBriefOptions {
  /**
   * Abort signal for cooperative cancellation. Note: Tauri's `invoke` does
   * not currently accept an AbortSignal — the option is accepted for API
   * symmetry with fetch-based callers and is ignored at the transport layer.
   */
  signal?: AbortSignal;
}

const BRIEF_PROMPT_PREFIX =
  "Spec Generation Brief (JSON) — treat deterministicAssertions as snapshot_assert candidates, emit setup/navigation steps for every preconditions[] item before each group's assertions.\n\n";

/**
 * Serialize a brief into the inline context block sent to the AI pipeline.
 * Exported for callers that want to preview the prompt without invoking.
 */
export function serializeBriefToContext(brief: GenerationBrief): string {
  return `${BRIEF_PROMPT_PREFIX}${JSON.stringify(brief, null, 2)}`;
}

/**
 * Invoke the standalone workflow generator with a spec-derived brief.
 * Returns the raw Tauri CommandResponse so callers can surface errors /
 * data as they see fit (matches the shape used by `useWorkflowGeneration`).
 */
export async function generateFromBrief(
  brief: GenerationBrief,
  _options: GenerateFromBriefOptions = {},
): Promise<CommandResponse> {
  const inlineContext = serializeBriefToContext(brief);
  const description = brief.summary;

  const totalDeterministic = brief.groups.reduce(
    (sum, g) => sum + g.deterministicAssertions.length,
    0,
  );
  const totalSemantic = brief.groups.reduce((sum, g) => sum + g.semanticAssertions.length, 0);

  log.info(
    `generateFromBrief: groups=${brief.groups.length}, ` +
      `deterministicAssertions=${totalDeterministic}, ` +
      `semanticAssertions=${totalSemantic}, ` +
      `elementSource=${brief.elementSource}`,
  );

  return invoke<CommandResponse>("generate_workflow_standalone", {
    description,
    inlineContext,
    includeUiBridge: true,
  });
}
