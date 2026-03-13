/**
 * Constraints API Client
 *
 * HTTP client for the constraint engine endpoints on the runner backend (port 9876).
 * All functions use tracedFetch for cross-service trace propagation.
 */

import type {
  Constraint,
  ConstraintResult,
  ReadConfigResponse,
  ValidateConfigResponse,
  WriteConfigResponse,
} from "@qontinui/shared-types/constraints";
import { getApiBase, tracedFetch } from "@/lib/runner-api";

/** Unwrap the runner's ApiResponse<T> envelope, returning the inner data. */
async function unwrapApiResponse<T>(response: Response): Promise<T> {
  const body = await response.json();
  if (body.success && body.data !== undefined) {
    return body.data as T;
  }
  return body as T;
}

/**
 * Fetch active constraints for a project.
 *
 * Merges built-in constraints with project-level overrides and custom constraints
 * from constraints.toml.
 */
export async function fetchActiveConstraints(projectPath?: string): Promise<Constraint[]> {
  const params = new URLSearchParams();
  if (projectPath) {
    params.set("project_path", projectPath);
  }
  const qs = params.toString();
  const url = `${getApiBase()}/constraints/active${qs ? `?${qs}` : ""}`;

  const response = await tracedFetch(url);
  if (!response.ok) {
    const text = await response.text();
    throw new Error(`Failed to fetch active constraints: ${text}`);
  }
  return unwrapApiResponse<Constraint[]>(response);
}

/**
 * Fetch the raw TOML config content and file path.
 *
 * Returns an empty `toml` string and no `path` if no constraints.toml exists.
 */
export async function fetchConstraintConfig(projectPath?: string): Promise<ReadConfigResponse> {
  const params = new URLSearchParams();
  if (projectPath) {
    params.set("project_path", projectPath);
  }
  const qs = params.toString();
  const url = `${getApiBase()}/constraints/config${qs ? `?${qs}` : ""}`;

  const response = await tracedFetch(url);
  if (!response.ok) {
    const text = await response.text();
    throw new Error(`Failed to fetch constraint config: ${text}`);
  }
  return unwrapApiResponse<ReadConfigResponse>(response);
}

/**
 * Validate and write a TOML config string.
 *
 * The backend validates the TOML before writing. If validation fails,
 * `valid` will be false and `errors` will contain details.
 */
export async function saveConstraintConfig(
  toml: string,
  projectPath?: string,
): Promise<WriteConfigResponse> {
  const url = `${getApiBase()}/constraints/config`;
  const response = await tracedFetch(url, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ toml, project_path: projectPath }),
  });
  if (!response.ok) {
    const text = await response.text();
    throw new Error(`Failed to save constraint config: ${text}`);
  }
  return unwrapApiResponse<WriteConfigResponse>(response);
}

/**
 * Validate a TOML config string without writing it.
 *
 * Useful for real-time validation in the editor before the user commits to saving.
 */
export async function validateConstraintConfig(toml: string): Promise<ValidateConfigResponse> {
  const url = `${getApiBase()}/constraints/validate`;
  const response = await tracedFetch(url, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ toml }),
  });
  if (!response.ok) {
    const text = await response.text();
    throw new Error(`Failed to validate constraint config: ${text}`);
  }
  return unwrapApiResponse<ValidateConfigResponse>(response);
}

/**
 * Fetch constraint evaluation results for a specific task run.
 *
 * Optionally filter by iteration number.
 */
export async function fetchConstraintResults(
  taskRunId: string,
  iteration?: number,
): Promise<ConstraintResult[]> {
  const params = new URLSearchParams();
  if (iteration !== undefined) {
    params.set("iteration", String(iteration));
  }
  const qs = params.toString();
  const url = `${getApiBase()}/constraints/results/${encodeURIComponent(taskRunId)}${qs ? `?${qs}` : ""}`;

  const response = await tracedFetch(url);
  if (!response.ok) {
    const text = await response.text();
    throw new Error(`Failed to fetch constraint results: ${text}`);
  }
  return unwrapApiResponse<ConstraintResult[]>(response);
}
