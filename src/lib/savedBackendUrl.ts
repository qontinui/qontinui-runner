/**
 * The backend URL the gate sign-in screens (LoginScreen, the setup-wizard
 * GithubRepoPicker) mint a Cognito device JWT against.
 *
 * Why this is not a constant any more: `finalize_signed_in`
 * (`src-tauri/src/commands/auth.rs`, step 6) WRITES the URL it signed in
 * against back into `web_integration.backend_url`. These screens used to pass
 * a hard-coded production URL, so signing in silently overwrote a custom
 * backend the operator had saved on Settings → Backend Connection.
 *
 * Why the saved value is not taken unconditionally: `cognito_sign_in` takes an
 * explicit URL precisely "so a debug build can target prod" (auth.rs), and the
 * saved DEFAULT in any build with debug assertions — the `release-checked`
 * primary included — is loopback (`default_web_integration_backend_url`).
 * Honouring that would send a first sign-in to a local backend that is usually
 * not running.
 *
 * So the saved URL wins only when it is a deliberate, readable, non-loopback
 * choice; every other case keeps the production URL these screens always
 * used. That makes the change a strict narrowing of the old behaviour: the
 * only thing that moves is that a saved custom remote backend is honoured
 * instead of clobbered.
 */

import { invoke } from "@tauri-apps/api/core";

/** Production web backend — the sign-in target when no deliberate remote
 *  backend is saved. */
export const PRODUCTION_BACKEND_URL = "https://api.qontinui.io";

/** The subset of `get_web_integration_status` this decision reads. */
export interface SavedBackendStatus {
  backendUrl?: string | null;
  /** Non-null when settings.json was unreadable: `backendUrl` is then a
   *  placeholder default, not the operator's saved choice. */
  settingsFault?: unknown;
}

function isLoopback(url: string): boolean {
  let host: string;
  try {
    host = new URL(url).hostname.toLowerCase();
  } catch {
    return false;
  }
  return (
    host === "localhost" ||
    host.endsWith(".localhost") ||
    host === "0.0.0.0" ||
    host === "[::1]" ||
    host === "::1" ||
    /^127\.\d+\.\d+\.\d+$/.test(host)
  );
}

/** Pure decision: which backend a gate sign-in targets, given the status read
 *  (`null` when the read failed). */
export function signInBackendUrlFrom(status: SavedBackendStatus | null): string {
  if (!status || status.settingsFault) return PRODUCTION_BACKEND_URL;
  const saved = status.backendUrl?.trim() ?? "";
  if (!saved || isLoopback(saved)) return PRODUCTION_BACKEND_URL;
  try {
    // Only an absolute http(s) URL counts as a deliberate choice.
    const proto = new URL(saved).protocol;
    if (proto !== "https:" && proto !== "http:") return PRODUCTION_BACKEND_URL;
  } catch {
    return PRODUCTION_BACKEND_URL;
  }
  return saved;
}

/** Read the saved status and decide. A failed read keeps the production URL,
 *  exactly as these screens behaved before. */
export async function resolveSignInBackendUrl(): Promise<string> {
  try {
    return signInBackendUrlFrom(await invoke<SavedBackendStatus>("get_web_integration_status"));
  } catch {
    return signInBackendUrlFrom(null);
  }
}
