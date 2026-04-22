/**
 * Build a compact catalog of actual UI elements per spec so the AI planner
 * generates instructions using real element labels (e.g. "Compile State
 * Machine") rather than hallucinated names (e.g. "Compile").
 *
 * The catalog is sent alongside the prompt to /prompt-home/plan and injected
 * into the system prompt by the Rust handler.
 */

import { getGlobalSpecStore } from "@qontinui/ui-bridge/specs";
import type { SpecConfig, SpecGroup, SpecAssertion } from "@qontinui/ui-bridge/specs";

const MAX_LABELS_PER_GROUP = 6;
const MAX_GROUPS_PER_SPEC = 6;
const MAX_CATALOG_CHARS = 20_000;

function extractLabel(a: SpecAssertion): string | null {
  const t = a.target;
  if (!t) return null;
  if (t.label) return t.label.trim();
  if (t.type === "elementId") return t.elementId;
  if (t.type === "ctr") return t.logicalName;
  if (t.type === "search") {
    const c = t.criteria as Record<string, unknown> | undefined;
    const text = (c?.textContent ?? c?.text ?? c?.ariaLabel) as string | undefined;
    if (text) return text.trim();
  }
  return null;
}

function formatGroup(group: SpecGroup): string | null {
  const labels = new Set<string>();
  for (const a of group.assertions ?? []) {
    const l = extractLabel(a);
    if (l) labels.add(l.length > 60 ? l.slice(0, 57) + "..." : l);
    if (labels.size >= MAX_LABELS_PER_GROUP) break;
  }
  if (labels.size === 0) return null;
  return `  ${group.name}: ${Array.from(labels).join(", ")}`;
}

function formatSpec(specId: string, cfg: SpecConfig): string {
  const lines: string[] = [];
  const header = cfg.description
    ? `${specId} — ${cfg.description.replace(/\s+/g, " ").slice(0, 140)}`
    : specId;
  lines.push(header);
  const groups = (cfg.groups ?? []).slice(0, MAX_GROUPS_PER_SPEC);
  for (const g of groups) {
    const line = formatGroup(g);
    if (line) lines.push(line);
  }
  return lines.join("\n");
}

/**
 * Read every spec from the global SpecStore and produce a compact catalog
 * suitable for injection into the planner's system prompt. Returns an empty
 * string if no specs are loaded (caller should treat as "no catalog").
 */
export function buildPageCatalog(): string {
  const store = getGlobalSpecStore();
  const all = store.getAll();
  if (all.size === 0) return "";

  const sections: string[] = [];
  let total = 0;
  for (const [specId, cfg] of all) {
    const section = formatSpec(specId, cfg);
    if (total + section.length > MAX_CATALOG_CHARS) break;
    sections.push(section);
    total += section.length + 2;
  }
  return sections.join("\n\n");
}
