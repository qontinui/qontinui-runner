import { readFile } from "../../integrationApi";

/**
 * Read an existing file from the project (returns empty string on failure).
 *
 * The step machine's "empty string on failure / abort" contract over
 * `integrationApi.readFile`: a rejected request, an unsuccessful envelope, an
 * empty body, or an aborted `signal` all resolve to `""`.
 */
export async function readProjectFile(
  projectPath: string,
  filePath: string,
  signal: AbortSignal,
): Promise<string> {
  try {
    const data = await readFile(projectPath, filePath, signal);
    if (signal.aborted) return "";
    return data.success && data.data ? data.data : "";
  } catch {
    return "";
  }
}
