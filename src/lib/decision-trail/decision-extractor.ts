/**
 * Decision Extractor — Auto-detect potential decisions from code changes.
 *
 * Heuristics analyze diffs and file changes to identify architectural decisions,
 * then format them for AI enrichment.
 */

import type { DecisionCategory } from "@/types/decision-trail";

export interface DetectedChange {
  type: "new-table" | "new-dependency" | "new-endpoint" | "new-directory" | "explicit-comment" | "new-file";
  file: string;
  detail: string;
}

export interface DecisionCandidate {
  category: DecisionCategory;
  title: string;
  changes: DetectedChange[];
}

/** Patterns that indicate an explicit decision in code comments. */
const DECISION_COMMENT_PATTERNS = [
  /instead\s+of/i,
  /rather\s+than/i,
  /chose\s+to/i,
  /decided\s+to/i,
  /we\s+chose/i,
  /trade-?off/i,
  /design\s+decision/i,
];

/** Detect potential decisions from a list of changed files and their diffs. */
export function detectDecisions(
  changedFiles: { path: string; diff?: string; isNew: boolean }[],
): DecisionCandidate[] {
  const candidates: DecisionCandidate[] = [];

  for (const file of changedFiles) {
    const changes: DetectedChange[] = [];

    // New CREATE TABLE in SQL files
    if (file.diff && /CREATE\s+TABLE/i.test(file.diff)) {
      const tableMatch = file.diff.match(/CREATE\s+TABLE\s+(?:IF\s+NOT\s+EXISTS\s+)?(\w+)/i);
      if (tableMatch) {
        changes.push({
          type: "new-table",
          file: file.path,
          detail: `New table: ${tableMatch[1]}`,
        });
      }
    }

    // New dependency in Cargo.toml or package.json
    if (file.path.endsWith("Cargo.toml") && file.diff) {
      const depMatches = file.diff.matchAll(/^\+\s*(\w[\w-]*)\s*=/gm);
      for (const m of depMatches) {
        changes.push({
          type: "new-dependency",
          file: file.path,
          detail: `New Rust dependency: ${m[1]}`,
        });
      }
    }
    if (file.path.endsWith("package.json") && file.diff) {
      const depMatches = file.diff.matchAll(/^\+\s*"([@\w/-]+)":\s*"/gm);
      for (const m of depMatches) {
        changes.push({
          type: "new-dependency",
          file: file.path,
          detail: `New npm dependency: ${m[1]}`,
        });
      }
    }

    // New Tauri command / Axum route
    if (file.diff && (/\.route\(/.test(file.diff) || /#\[tauri::command\]/.test(file.diff))) {
      changes.push({
        type: "new-endpoint",
        file: file.path,
        detail: "New API endpoint or Tauri command",
      });
    }

    // Explicit decision comments in diff
    if (file.diff) {
      for (const pattern of DECISION_COMMENT_PATTERNS) {
        if (pattern.test(file.diff)) {
          const lineMatch = file.diff
            .split("\n")
            .find((l) => l.startsWith("+") && pattern.test(l));
          if (lineMatch) {
            changes.push({
              type: "explicit-comment",
              file: file.path,
              detail: lineMatch.replace(/^\+\s*\/\/\s*/, "").trim(),
            });
          }
          break;
        }
      }
    }

    // New file in a new directory pattern
    if (file.isNew) {
      changes.push({
        type: "new-file",
        file: file.path,
        detail: `New file created`,
      });
    }

    if (changes.length > 0) {
      const category = inferCategory(changes);
      const title = inferTitle(changes);
      candidates.push({ category, title, changes });
    }
  }

  return deduplicateCandidates(candidates);
}

function inferCategory(changes: DetectedChange[]): DecisionCategory {
  const types = new Set(changes.map((c) => c.type));
  if (types.has("new-table")) return "data-model";
  if (types.has("new-dependency")) return "technology";
  if (types.has("new-endpoint")) return "architecture";
  if (types.has("explicit-comment")) return "design";
  return "architecture";
}

function inferTitle(changes: DetectedChange[]): string {
  const first = changes[0];
  if (first.type === "new-table") return first.detail;
  if (first.type === "new-dependency") return first.detail;
  if (first.type === "explicit-comment") return first.detail.slice(0, 80);
  if (first.type === "new-endpoint") return `New API surface in ${first.file.split("/").pop()}`;
  return `Changes in ${first.file.split("/").pop()}`;
}

function deduplicateCandidates(candidates: DecisionCandidate[]): DecisionCandidate[] {
  const seen = new Set<string>();
  return candidates.filter((c) => {
    const key = `${c.category}:${c.title}`;
    if (seen.has(key)) return false;
    seen.add(key);
    return true;
  });
}

/** Build an AI enrichment prompt from detected changes. */
export function buildEnrichmentPrompt(candidates: DecisionCandidate[]): string {
  const changeList = candidates
    .flatMap((c) => c.changes)
    .map((c) => `- ${c.detail} (${c.file})`)
    .join("\n");

  return `Given these code changes:\n${changeList}\n\nFor each distinct architectural decision, generate a JSON array of objects with:\n- title: What was decided (imperative, concise)\n- summary: 2-3 sentence explanation\n- rationale: WHY this was chosen\n- category: one of architecture, technology, design, integration, performance, security, ux, data-model\n- scale: "tactical" or "strategic"\n- alternatives: [{name, reasonRejected}] — what else might have been considered\n- tradeoffs: string[] — known downsides accepted\n- tags: string[] — relevant keywords\n\nRespond with valid JSON only.`;
}
