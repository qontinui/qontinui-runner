import { getApiBase } from "@/lib/runner-api";
import { extractJsonBlock } from "../parse";
import type { StepHandler } from "./types";

/**
 * `architecture-spec`: extract the spec JSON block and add it as a generated
 * file. Tries immediately, then retries once via the task-run output API if
 * text events are still in transit.
 */
export const architectureSpecStep: StepHandler = (ctx) => {
  // Extract JSON — try immediately, then retry via API if text events are still in transit
  const handleSpecError = () => {
    if (ctx.controller.signal.aborted) return;
    ctx.setStepStatuses((prev) =>
      prev.map((s) => (s.label.includes("Architecture") ? { ...s, state: "error" } : s)),
    );
    ctx.setError(
      "Architecture spec generation did not produce valid JSON. You can still apply the hook files.",
    );
    ctx.pendingStepRef.current = null;
    ctx.setPhase("preview");
  };

  const tryExtractSpec = (content: string) => {
    const jsonBlock = extractJsonBlock(content);
    if (jsonBlock) {
      let specFileName = "project.architecture.uibridge.json";
      try {
        const parsed = JSON.parse(jsonBlock);
        if (typeof parsed.projectName === "string" && parsed.projectName) {
          specFileName =
            parsed.projectName
              .toLowerCase()
              .replace(/[^a-z0-9]+/g, "-")
              .replace(/^-|-$/g, "") + ".architecture.uibridge.json";
        }
      } catch {
        // Use default name
      }

      ctx.setGeneratedFiles((prev) => [...prev, { filePath: specFileName, content: jsonBlock }]);
      ctx.setStepStatuses((prev) =>
        prev.map((s) => (s.label.includes("Architecture") ? { ...s, state: "done" } : s)),
      );
      ctx.pendingStepRef.current = null;
      ctx.setExpandedFiles(new Set(ctx.generatedFiles.map((f) => f.filePath).concat(["spec"])));
      ctx.setPhase("preview");
      return true;
    }
    return false;
  };

  // Try extracting from current content
  const specContent = ctx.messages
    .filter((m) => m.role === "ai")
    .map((m) => m.content)
    .join("\n\n");
  const fullSpecContent = ctx.streamingContent
    ? specContent + "\n\n" + ctx.streamingContent
    : specContent;

  if (!tryExtractSpec(fullSpecContent)) {
    // Race condition: text events still in transit. Retry via API.
    if (ctx.specRetryTimerRef.current) clearTimeout(ctx.specRetryTimerRef.current);
    const timer = setTimeout(async () => {
      ctx.specRetryTimerRef.current = null;
      ctx.setRetryTimer(null);
      if (ctx.controller.signal.aborted || !ctx.taskRunId) {
        if (!ctx.controller.signal.aborted) handleSpecError();
        return;
      }

      try {
        const resp = await fetch(
          `${getApiBase()}/task-runs/${ctx.taskRunId}/output?tail_chars=200000`,
          { signal: ctx.controller.signal },
        );
        if (ctx.controller.signal.aborted) return;
        const data = await resp.json();
        const apiOutput: string = data?.output || "";
        if (!tryExtractSpec(apiOutput)) {
          handleSpecError();
        }
      } catch {
        if (!ctx.controller.signal.aborted) {
          handleSpecError();
        }
      }
    }, 1000);
    ctx.specRetryTimerRef.current = timer;
    ctx.setRetryTimer(timer);
  }
};
