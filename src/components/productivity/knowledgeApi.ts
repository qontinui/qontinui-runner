/**
 * Knowledge Browser — Tauri command wrapper.
 *
 * Thin async shim around `invoke('search_knowledge', ...)`. Mirrors the
 * `Vec<KnowledgeHit>` shape returned by the Rust side; v1 only consumes
 * the FTS path.
 */

import { invoke } from "@tauri-apps/api/core";

/**
 * camelCase mirror of `database::pg::productivity_knowledge::KnowledgeRow`.
 * The Rust struct uses `#[serde(rename_all = "camelCase")]`.
 */
export interface KnowledgeRow {
  id: string;
  taskId: string | null;
  sessionId: string | null;
  area: string;
  summary: string;
  body: string;
  createdAt: string;
  hasEmbedding: boolean;
}

/** A search hit — the row plus a relevance score (FTS rank or cosine). */
export interface KnowledgeHit {
  row: KnowledgeRow;
  score: number;
}

/**
 * Run an FTS search over `productivity_knowledge`. `areaFilter` is an
 * exact-match on the row's `area`; pass `null` to search every area.
 * `topK` is clamped server-side to a sane range.
 */
export async function searchKnowledge(
  query: string,
  areaFilter: string | null,
  topK: number,
): Promise<KnowledgeHit[]> {
  return invoke<KnowledgeHit[]>("search_knowledge", {
    query,
    areaFilter,
    topK,
  });
}
