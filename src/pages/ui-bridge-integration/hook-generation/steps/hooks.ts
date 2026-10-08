import {
  buildArchitectureSpecPrompt,
  buildArchitectureSpecRegenPrompt,
} from "@/lib/hook-gen-prompt-builder";
import { readFile } from "../../integrationApi";
import { extractGeneratedFiles } from "../parse";
import type { StepHandler } from "./types";

// Async helper (kept outside the handler to avoid fetch-in-useEffect lint)
async function fetchAndSendSpecPrompt(params: {
  signal: AbortSignal;
  isRegenSpec: boolean;
  projectPath: string;
  analysis: { framework: string; project_path: string };
  /** `useAiSession().sendMessage`; its outcome is not consulted here. */
  sendMessage: (msg: string) => Promise<unknown>;
}): Promise<void> {
  const { signal, isRegenSpec, projectPath, analysis, sendMessage } = params;
  let specPrompt: string;
  if (isRegenSpec) {
    let existingSpec = "";
    try {
      const data = await readFile(projectPath, "project.architecture.uibridge.json", signal);
      if (signal.aborted) return;
      if (data.success && data.data) existingSpec = data.data;
    } catch (_e) {
      if (signal.aborted) return;
      // Fall through to fresh generation
    }
    specPrompt = existingSpec
      ? buildArchitectureSpecRegenPrompt(analysis, existingSpec)
      : buildArchitectureSpecPrompt(analysis);
  } else {
    specPrompt = buildArchitectureSpecPrompt(analysis);
  }
  if (signal.aborted) return;
  await sendMessage(
    "Now generate an architecture spec for this project. You already have context from the hook generation step — use what you learned.\n\n" +
      specPrompt,
  );
}

/** `hooks`: extract the `// FILE:` hook files, then chain to the architecture spec or go to preview. */
export const hooksStep: StepHandler = (ctx, { fullContent }) => {
  const files = extractGeneratedFiles(fullContent);
  if (files.length > 0) {
    ctx.setGeneratedFiles(files);
    ctx.setStepStatuses((prev) =>
      prev.map((s) => (s.label.includes("Hook") ? { ...s, state: "done" } : s)),
    );

    // If architecture spec is included, proceed to that step
    if (ctx.includeArchSpec) {
      ctx.pendingStepRef.current = "architecture-spec";
      ctx.setPhase("generating-spec");
      ctx.setStepStatuses((prev) =>
        prev.map((s) => (s.label.includes("Architecture") ? { ...s, state: "active" } : s)),
      );
      // Send the architecture spec prompt in the same session
      const analysisForPrompt = {
        framework: ctx.analysis.framework,
        project_path: ctx.projectPath,
      };
      fetchAndSendSpecPrompt({
        signal: ctx.controller.signal,
        isRegenSpec: ctx.isRegenSpec,
        projectPath: ctx.projectPath,
        analysis: analysisForPrompt,
        sendMessage: ctx.sendMessage,
      });
    } else {
      // No spec step — go to preview
      ctx.pendingStepRef.current = null;
      ctx.setExpandedFiles(new Set(files.map((f) => f.filePath)));
      ctx.setPhase("preview");
    }
  } else {
    ctx.pendingStepRef.current = null;
    ctx.setStepStatuses((prev) =>
      prev.map((s) => (s.label.includes("Hook") ? { ...s, state: "error" } : s)),
    );
    ctx.setError("AI did not produce any files with // FILE: markers. Try regenerating.");
    ctx.setPhase("idle");
  }
};
