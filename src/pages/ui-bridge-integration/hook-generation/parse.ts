/**
 * Pure parsers for HookGenerationPanel's AI output.
 *
 * Moved verbatim from `HookGenerationPanel.tsx` (plan
 * 2026-10-04-runner-hook-generation-panel-runs-its-ai-step-machine-from-one-876-line-callback,
 * Phase 1). `extractJsonBlock` is the ONE copy in `src/` — `useSpecSync.ts`
 * imports it from here.
 */

import type { ExplainerSpecSummary, ExplainerCluster } from "@/lib/page-analysis-prompt-builder";

// =============================================================================
// File extraction from AI output
// =============================================================================

export interface GeneratedFile {
  filePath: string;
  content: string;
}

export function extractGeneratedFiles(content: string): GeneratedFile[] {
  const results: GeneratedFile[] = [];
  // Match code blocks with optional language tag, then // FILE: marker on first line
  // Case-insensitive for language tags, allows blank lines between fence and marker
  const regex =
    /```(?:tsx?|jsx?|typescript(?:react)?|javascript(?:react)?)?\s*\r?\n\s*\/\/ FILE:\s*(.+?)\r?\n([\s\S]*?)```/gi;
  let match;
  while ((match = regex.exec(content)) !== null) {
    const filePath = match[1].trim();
    const fileContent = `// FILE: ${filePath}\n${match[2]}`;
    results.push({ filePath, content: fileContent });
  }
  return results;
}

/**
 * Extract Mermaid diagram file from AI output. Mermaid uses `%%` for comments,
 * so the prompt instructs the AI to emit `%% FILE: <path>` inside a
 * ```mermaid block instead of the `// FILE:` convention used for TS files.
 */
export function extractMermaidFile(content: string): GeneratedFile | null {
  const regex = /```mermaid\s*\r?\n\s*%%\s*FILE:\s*(.+?)\r?\n([\s\S]*?)```/i;
  const match = regex.exec(content);
  if (!match) return null;
  const filePath = match[1].trim();
  const body = match[2].trimEnd();
  return { filePath, content: `%% FILE: ${filePath}\n${body}\n` };
}

/**
 * Collect explainer inputs from the set of files just generated during a
 * per-page run: reads each .spec.uibridge.json + its paired .arch.mmd.
 * Returns the parsed spec summaries and a map of specId → mermaid body.
 */
export function gatherExplainerInputs(files: GeneratedFile[]): {
  specs: ExplainerSpecSummary[];
  arch: Map<string, string>;
} {
  const specs: ExplainerSpecSummary[] = [];
  const arch = new Map<string, string>();
  for (const f of files) {
    if (f.filePath.endsWith(".spec.uibridge.json")) {
      try {
        const json = JSON.parse(f.content) as {
          description?: string;
          groups?: Array<{ id?: string; name?: string; description?: string }>;
        };
        const specId = f.filePath.replace(/^.*\//, "").replace(/\.spec\.uibridge\.json$/, "");
        specs.push({
          specId,
          description: (json.description || "").trim(),
          groups: (json.groups || [])
            .filter((g) => g && g.name)
            .map((g) => ({
              id: g.id || "",
              name: g.name || "",
              description: (g.description || "").trim(),
            })),
        });
      } catch {
        /* skip malformed spec */
      }
    } else if (f.filePath.endsWith(".arch.mmd")) {
      const specId = f.filePath.replace(/^.*\//, "").replace(/\.arch\.mmd$/, "");
      // Strip the leading `%% FILE:` comment line from the content.
      const body = f.content.replace(/^%%\s*FILE:.*\r?\n/, "").trim();
      arch.set(specId, body);
    }
  }
  return { specs, arch };
}

/**
 * Heuristic clustering: group specs by shared first token (split by `-`).
 * Singletons are merged into a catch-all "other" cluster so we don't emit
 * clusters of one. Caps total clusters at 8.
 */
export function clusterSpecsByPrefix(specs: ExplainerSpecSummary[]): ExplainerCluster[] {
  const buckets = new Map<string, ExplainerSpecSummary[]>();
  for (const s of specs) {
    const token = s.specId.split(/[-/]/)[0] || s.specId;
    if (!buckets.has(token)) buckets.set(token, []);
    buckets.get(token)!.push(s);
  }
  // Promote buckets with >=2 specs; fold singletons into "other".
  const multi: Array<{ id: string; specs: ExplainerSpecSummary[] }> = [];
  const singletons: ExplainerSpecSummary[] = [];
  for (const [id, items] of buckets) {
    if (items.length >= 2) multi.push({ id, specs: items });
    else singletons.push(...items);
  }
  multi.sort((a, b) => b.specs.length - a.specs.length);
  const top = multi.slice(0, 7);
  const overflow = multi.slice(7).flatMap((m) => m.specs);
  const otherSpecs = [...overflow, ...singletons];
  const result: ExplainerCluster[] = top.map((m) => ({
    id: m.id,
    name: m.id.replace(/\b\w/g, (c) => c.toUpperCase()),
    description: `${m.specs.length} related pages under the "${m.id}" umbrella.`,
    specIds: m.specs.map((s) => s.specId),
  }));
  if (otherSpecs.length > 0) {
    result.push({
      id: "other",
      name: "Other",
      description: `Pages that don't fit cleanly into the other clusters.`,
      specIds: otherSpecs.map((s) => s.specId),
    });
  }
  return result;
}

/**
 * Extract multiple markdown files from AI output. Markdown has no native
 * comment syntax, so the explainer prompts instruct the AI to use an HTML
 * comment on the first line of each file: `<!-- FILE: <path> -->`. Files are
 * delimited by the next FILE comment or end of content.
 */
export function extractMarkdownFiles(content: string): GeneratedFile[] {
  const regex = /<!--\s*FILE:\s*(.+?)\s*-->\s*\r?\n/g;
  const results: GeneratedFile[] = [];
  const matches: Array<{ path: string; startOfBody: number }> = [];
  let match;
  while ((match = regex.exec(content)) !== null) {
    matches.push({ path: match[1].trim(), startOfBody: regex.lastIndex });
  }
  for (let i = 0; i < matches.length; i++) {
    const { path, startOfBody } = matches[i];
    const end = i + 1 < matches.length ? matches[i + 1].startOfBody : content.length;
    // Walk back from `end` to before the next FILE marker's opening `<!--`.
    let bodyEnd = end;
    if (i + 1 < matches.length) {
      // `matches[i+1].startOfBody` points past the `\n` after `-->`. Back up to
      // before the `<!--` that opens the next marker so we don't include it.
      const nextOpen = content.lastIndexOf("<!--", matches[i + 1].startOfBody);
      if (nextOpen > 0) bodyEnd = nextOpen;
    }
    const body = content.slice(startOfBody, bodyEnd).trimEnd();
    results.push({
      filePath: path,
      content: `<!-- FILE: ${path} -->\n${body}\n`,
    });
  }
  return results;
}

/** Maximum size (in bytes) for extracted JSON blocks to prevent caching oversized payloads. */
export const MAX_JSON_BLOCK_SIZE = 1024 * 1024; // 1 MB

export function extractJsonBlock(content: string): string | null {
  // First pass: match ```json blocks specifically (avoids pairing with closing ``` of other code blocks)
  const jsonRegex = /```json\s*\n([\s\S]*?)```/gi;
  let match;
  while ((match = jsonRegex.exec(content)) !== null) {
    const raw = match[1];
    if (raw.length > MAX_JSON_BLOCK_SIZE) continue; // Skip oversized blocks
    try {
      JSON.parse(raw);
      return raw.trim();
    } catch {
      // Not valid JSON, try next block
    }
  }
  // Fallback: try bare ``` blocks (no language tag)
  const bareRegex = /```\s*\n(\s*\{[\s\S]*?\})\s*\n```/g;
  while ((match = bareRegex.exec(content)) !== null) {
    const raw = match[1];
    if (raw.length > MAX_JSON_BLOCK_SIZE) continue; // Skip oversized blocks
    try {
      JSON.parse(raw);
      return raw.trim();
    } catch {
      // Not valid JSON, try next block
    }
  }
  return null;
}
