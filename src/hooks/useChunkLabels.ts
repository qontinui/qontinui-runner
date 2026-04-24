/**
 * useChunkLabels
 *
 * React hook exposing per-config chunk-label overrides to the chunked
 * state-machine graph view. Labels are persisted in Postgres via the
 * Tauri commands declared in `src-tauri/src/commands/chunk_labels.rs`:
 *
 *   - list_chunk_labels({ configId }) → Array<{ chunk_id, label, updated_at }>
 *   - upsert_chunk_label({ configId, chunkId, label })
 *   - delete_chunk_label({ configId, chunkId })
 *
 * `chunk_id` is the stable djb2 hash of a chunk's sorted state ids
 * produced by `chunkStateMachine` in `@qontinui/workflow-utils`, so the
 * same chunk produces the same id across reloads and small input
 * permutations. An empty saveLabel() argument signals a revert — the
 * override is deleted and the view falls back to the auto-derived name.
 */

import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { createLogger } from "@/lib/logger";

const log = createLogger("useChunkLabels");

/** Wire shape returned by `list_chunk_labels`. Fields are camelCase via
 *  serde rename on the Rust side. */
interface ChunkLabelRow {
  chunkId: string;
  label: string;
  updatedAt?: string;
}

export interface UseChunkLabelsResult {
  /** chunk_id → user-chosen label. */
  labels: Map<string, string>;
  /** Save (or delete via empty string) a chunk label. */
  saveLabel: (chunkId: string, label: string) => void;
}

function isTauriAvailable(): boolean {
  if (typeof window === "undefined") return false;
  const w = window as unknown as {
    __TAURI_INTERNALS__?: unknown;
    __TAURI__?: unknown;
  };
  return typeof w.__TAURI_INTERNALS__ !== "undefined" || typeof w.__TAURI__ !== "undefined";
}

/**
 * Load chunk labels for the given config id. When `configId` is null
 * the hook returns an empty map and `saveLabel` is a no-op (so callers
 * can mount it unconditionally above the config-selection boundary).
 */
export function useChunkLabels(configId: string | null): UseChunkLabelsResult {
  const [labels, setLabels] = useState<Map<string, string>>(() => new Map());

  useEffect(() => {
    if (!configId || !isTauriAvailable()) {
      setLabels(new Map());
      return;
    }
    let cancelled = false;
    invoke<ChunkLabelRow[]>("list_chunk_labels", { configId })
      .then((rows) => {
        if (cancelled) return;
        const m = new Map<string, string>();
        for (const r of rows) {
          if (r?.chunkId && typeof r.label === "string") {
            m.set(r.chunkId, r.label);
          }
        }
        setLabels(m);
      })
      .catch((e) => {
        if (cancelled) return;
        log.warn("list_chunk_labels failed", e);
        setLabels(new Map());
      });
    return () => {
      cancelled = true;
    };
  }, [configId]);

  const saveLabel = useCallback(
    (chunkId: string, label: string) => {
      if (!configId || !isTauriAvailable()) return;
      const trimmed = label.trim();
      if (trimmed === "") {
        invoke("delete_chunk_label", { configId, chunkId })
          .then(() => {
            setLabels((prev) => {
              if (!prev.has(chunkId)) return prev;
              const next = new Map(prev);
              next.delete(chunkId);
              return next;
            });
          })
          .catch((e) => log.warn("delete_chunk_label failed", e));
      } else {
        invoke("upsert_chunk_label", { configId, chunkId, label: trimmed })
          .then(() => {
            setLabels((prev) => {
              if (prev.get(chunkId) === trimmed) return prev;
              const next = new Map(prev);
              next.set(chunkId, trimmed);
              return next;
            });
          })
          .catch((e) => log.warn("upsert_chunk_label failed", e));
      }
    },
    [configId],
  );

  return { labels, saveLabel };
}
