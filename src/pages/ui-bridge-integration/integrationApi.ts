/**
 * integrationApi — typed wrappers over the runner's file-level
 * `/ui-bridge/integration/*` endpoints.
 *
 * ONE request/response contract for `read-file`, `read-page-source`,
 * `write-hooks` and `cache-architecture-spec`, shared by
 * `HookGenerationPanel`, `useSpecSync` and `ProjectExplainerPage`.
 *
 * Contract: each wrapper POSTs a JSON body and resolves to the endpoint's
 * `{ success, data?, error? }` envelope exactly as the runner sent it. It does
 * NOT interpret `success` and does NOT swallow errors — a network failure, an
 * abort of the passed `signal`, or an unparseable body rejects, the same as an
 * inline `fetch` + `resp.json()` would. That keeps each call site's own error
 * policy (swallow to "", surface to the UI, bail on abort) in the call site.
 */

import { getApiBase } from "@/lib/runner-api";
import type {
  ApiResponse,
  ModificationType,
  ReadPageSourceResult,
  WriteHooksResult,
} from "./types";

/** One file in a `write-hooks` request (the subset of `FileModification` the endpoint reads). */
export interface WriteHooksFile {
  file_path: string;
  modification_type: ModificationType;
  new_content: string;
}

async function postIntegration<T>(
  endpoint: string,
  body: unknown,
  signal?: AbortSignal,
): Promise<ApiResponse<T>> {
  const resp = await fetch(`${getApiBase()}/ui-bridge/integration/${endpoint}`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
    signal,
  });
  return (await resp.json()) as ApiResponse<T>;
}

/** Read one file, relative to `projectPath`. `data` is the file's text. */
export function readFile(
  projectPath: string,
  filePath: string,
  signal?: AbortSignal,
): Promise<ApiResponse<string>> {
  return postIntegration<string>(
    "read-file",
    { project_path: projectPath, file_path: filePath },
    signal,
  );
}

/** Read a page component's source plus its imports up to `maxDepth` (default 2). */
export function readPageSource(
  params: { projectPath: string; componentPath: string; maxDepth?: number },
  signal?: AbortSignal,
): Promise<ApiResponse<ReadPageSourceResult>> {
  return postIntegration<ReadPageSourceResult>(
    "read-page-source",
    {
      project_path: params.projectPath,
      component_path: params.componentPath,
      max_depth: params.maxDepth ?? 2,
    },
    signal,
  );
}

/** Write generated files into the project. */
export function writeHooks(
  projectPath: string,
  files: WriteHooksFile[],
  signal?: AbortSignal,
): Promise<ApiResponse<WriteHooksResult>> {
  return postIntegration<WriteHooksResult>(
    "write-hooks",
    { project_path: projectPath, files },
    signal,
  );
}

/** Cache an architecture spec so the Architecture page can show it. */
export function cacheArchitectureSpec(
  projectPath: string,
  specJson: string,
  signal?: AbortSignal,
): Promise<ApiResponse<unknown>> {
  return postIntegration<unknown>(
    "cache-architecture-spec",
    { project_path: projectPath, spec_json: specJson },
    signal,
  );
}
