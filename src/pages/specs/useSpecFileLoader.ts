/**
 * useSpecFileLoader — Load and save spec files from/to disk via Tauri
 *
 * Supports all spec file extensions:
 * - .spec.uibridge.json (page spec)
 * - .architecture.uibridge.json
 * - .api.uibridge.json
 * - .data.uibridge.json
 * - .deps.uibridge.json
 * - .constraints.uibridge.json
 */

import { useState, useCallback } from "react";
import { open, save } from "@tauri-apps/plugin-dialog";
import { readTextFile, writeTextFile } from "@tauri-apps/plugin-fs";
import type { LoadedSpec } from "./types";

/** Spec kinds that can be loaded from file (excludes style-guide) */
type FileSpecKind = "page-spec" | "architecture" | "api" | "data" | "dependency" | "constraint";

const KIND_EXTENSIONS: Record<FileSpecKind, string> = {
  "page-spec": ".spec.uibridge.json",
  architecture: ".architecture.uibridge.json",
  api: ".api.uibridge.json",
  data: ".data.uibridge.json",
  dependency: ".deps.uibridge.json",
  constraint: ".constraints.uibridge.json",
};

interface UseSpecFileLoaderReturn {
  loadFromFile: () => Promise<LoadedSpec | null>;
  saveToFile: (spec: LoadedSpec) => Promise<boolean>;
  isLoading: boolean;
  error: string | null;
}

/** Detect spec kind from filename */
function detectKindFromPath(path: string): FileSpecKind | null {
  const lower = path.toLowerCase();
  if (lower.endsWith(".architecture.uibridge.json")) return "architecture";
  if (lower.endsWith(".api.uibridge.json")) return "api";
  if (lower.endsWith(".data.uibridge.json")) return "data";
  if (lower.endsWith(".deps.uibridge.json")) return "dependency";
  if (lower.endsWith(".constraints.uibridge.json")) return "constraint";
  if (lower.endsWith(".spec.uibridge.json")) return "page-spec";
  return null;
}

/** Detect spec kind from parsed JSON content */
function detectKindFromContent(data: Record<string, unknown>): FileSpecKind | null {
  if (data.groups && Array.isArray(data.groups)) return "page-spec";
  if (data.techStack && data.patterns) return "architecture";
  if (data.endpoints && data.basePath) return "api";
  if (data.entities && data.relations && data.database) return "data";
  if (data.modules && data.links) return "dependency";
  if (data.performance || data.bundleBudgets || data.browsers) return "constraint";
  return null;
}

/** Extract a spec ID from file path */
function specIdFromPath(path: string): string {
  const filename = path.replace(/\\/g, "/").split("/").pop() || path;
  return filename
    .replace(/\.architecture\.uibridge\.json$/i, "")
    .replace(/\.api\.uibridge\.json$/i, "")
    .replace(/\.data\.uibridge\.json$/i, "")
    .replace(/\.deps\.uibridge\.json$/i, "")
    .replace(/\.constraints\.uibridge\.json$/i, "")
    .replace(/\.spec\.uibridge\.json$/i, "")
    .replace(/\.json$/i, "");
}

export function useSpecFileLoader(): UseSpecFileLoaderReturn {
  const [isLoading, setIsLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const loadFromFile = useCallback(async (): Promise<LoadedSpec | null> => {
    setError(null);
    setIsLoading(true);

    try {
      const selected = await open({
        multiple: false,
        filters: [
          {
            name: "Spec Files",
            extensions: ["json"],
          },
        ],
      });

      if (!selected) {
        setIsLoading(false);
        return null;
      }

      const content = await readTextFile(selected);
      const data = JSON.parse(content) as Record<string, unknown>;

      let kind = detectKindFromPath(selected);
      if (!kind) {
        kind = detectKindFromContent(data);
      }
      if (!kind) {
        setError("Could not determine spec type from file");
        setIsLoading(false);
        return null;
      }

      const specId = specIdFromPath(selected);

      const spec: LoadedSpec = {
        specId,
        appName: "File",
        kind,
        config: data as never,
        source: "file",
      };

      setIsLoading(false);
      return spec;
    } catch (err) {
      const msg = err instanceof Error ? err.message : "Failed to load spec file";
      setError(msg);
      setIsLoading(false);
      return null;
    }
  }, []);

  const saveToFile = useCallback(async (spec: LoadedSpec): Promise<boolean> => {
    setError(null);

    try {
      const ext = KIND_EXTENSIONS[spec.kind as FileSpecKind] || ".json";
      const defaultName = `${spec.specId}${ext}`;

      const selected = await save({
        defaultPath: defaultName,
        filters: [
          {
            name: "Spec Files",
            extensions: ["json"],
          },
        ],
      });

      if (!selected) return false;

      const json = JSON.stringify(spec.config, null, 2);
      await writeTextFile(selected, json);
      return true;
    } catch (err) {
      const msg = err instanceof Error ? err.message : "Failed to save spec file";
      setError(msg);
      return false;
    }
  }, []);

  return { loadFromFile, saveToFile, isLoading, error };
}
