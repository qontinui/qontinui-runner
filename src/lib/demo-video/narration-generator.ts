/**
 * Narration Generator
 *
 * Generates timestamped narration output in SRT and Markdown formats
 * from a completed demo execution result.
 */

import type { DemoScript, DemoExecutionResult, StepTimestamp } from "./types";

// =============================================================================
// Time Formatting
// =============================================================================

function formatSrtTime(ms: number): string {
  const totalSecs = Math.floor(ms / 1000);
  const hours = Math.floor(totalSecs / 3600);
  const minutes = Math.floor((totalSecs % 3600) / 60);
  const seconds = totalSecs % 60;
  const millis = ms % 1000;
  return `${String(hours).padStart(2, "0")}:${String(minutes).padStart(2, "0")}:${String(seconds).padStart(2, "0")},${String(millis).padStart(3, "0")}`;
}

function formatTimestamp(ms: number): string {
  const totalSecs = Math.floor(ms / 1000);
  const minutes = Math.floor(totalSecs / 60);
  const seconds = totalSecs % 60;
  return `${minutes}:${String(seconds).padStart(2, "0")}`;
}

// =============================================================================
// SRT Generation
// =============================================================================

export function generateSrt(steps: StepTimestamp[]): string {
  return steps
    .map((step, i) => {
      const index = i + 1;
      const start = formatSrtTime(step.startMs);
      const end = formatSrtTime(step.endMs);
      return `${index}\n${start} --> ${end}\n${step.narration}`;
    })
    .join("\n\n");
}

// =============================================================================
// Markdown Narration Script
// =============================================================================

export function generateMarkdownNarration(
  script: DemoScript,
  steps: StepTimestamp[]
): string {
  const lines: string[] = [
    `# ${script.title} — Narration Script`,
    "",
    `> ${script.description}`,
    "",
  ];

  for (const step of steps) {
    const ts = formatTimestamp(step.startMs);
    lines.push(`**[${ts}]** ${step.narration}`);
    lines.push("");
  }

  const totalTs = formatTimestamp(steps[steps.length - 1]?.endMs ?? 0);
  lines.push(`---`);
  lines.push(`*Total duration: ${totalTs}*`);

  return lines.join("\n");
}

// =============================================================================
// Combined Output
// =============================================================================

export interface NarrationOutput {
  srt: string;
  markdown: string;
}

export function generateNarration(
  script: DemoScript,
  result: DemoExecutionResult
): NarrationOutput {
  return {
    srt: generateSrt(result.steps),
    markdown: generateMarkdownNarration(script, result.steps),
  };
}
