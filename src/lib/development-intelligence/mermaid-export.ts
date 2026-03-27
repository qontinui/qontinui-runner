/**
 * Mermaid Export
 *
 * Converts ReactFlow nodes and edges into Mermaid graph syntax
 * that renders in GitHub, Notion, etc.
 */

import type { Node, Edge } from "@xyflow/react";

export interface MermaidExportOptions {
  direction?: "LR" | "TB" | "RL" | "BT";
  title?: string;
}

/**
 * Convert ReactFlow nodes/edges to Mermaid graph markdown.
 */
export function toMermaid(
  nodes: Node[],
  edges: Edge[],
  options: MermaidExportOptions = {},
): string {
  const direction = options.direction ?? "LR";
  const lines: string[] = [`graph ${direction}`];

  if (options.title) {
    lines.push(`  %% ${options.title}`);
  }

  // Sanitize ID for Mermaid (no colons, slashes, etc.)
  const sanitize = (id: string) =>
    id.replace(/[^a-zA-Z0-9_-]/g, "_");

  // Get label from node data
  const getLabel = (node: Node): string => {
    const data = node.data as Record<string, unknown>;
    if (typeof data?.label === "string") return data.label;
    const feature = data?.feature as Record<string, unknown> | undefined;
    if (typeof feature?.label === "string") return feature.label;
    return node.id;
  };

  // Escape characters that break Mermaid syntax
  const escapeLabel = (s: string) =>
    s.replace(/"/g, "&quot;").replace(/\|/g, "&#124;");

  for (const node of nodes) {
    const id = sanitize(node.id);
    const label = escapeLabel(getLabel(node));
    lines.push(`  ${id}["${label}"]`);
  }

  lines.push("");

  for (const edge of edges) {
    const src = sanitize(edge.source);
    const tgt = sanitize(edge.target);
    const label = typeof edge.label === "string" ? escapeLabel(edge.label) : undefined;

    if (label) {
      lines.push(`  ${src} -->|${label}| ${tgt}`);
    } else {
      lines.push(`  ${src} --> ${tgt}`);
    }
  }

  return lines.join("\n");
}

/**
 * Convert to Mermaid and wrap in a markdown code block.
 */
export function toMermaidMarkdown(
  nodes: Node[],
  edges: Edge[],
  options: MermaidExportOptions = {},
): string {
  return "```mermaid\n" + toMermaid(nodes, edges, options) + "\n```";
}

/**
 * Copy Mermaid output to clipboard.
 */
export async function copyMermaidToClipboard(
  nodes: Node[],
  edges: Edge[],
  options: MermaidExportOptions = {},
): Promise<void> {
  const markdown = toMermaidMarkdown(nodes, edges, options);
  await navigator.clipboard.writeText(markdown);
}
