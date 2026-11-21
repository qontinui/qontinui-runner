/**
 * The ONE rule for which backend a runner sign-in mints its Cognito device
 * JWT against. Every `cognito_sign_in` caller that signs in to the SAVED
 * backend goes through here: LoginScreen and the setup-wizard
 * GithubRepoPicker (the "gate" surfaces), Settings → Account and the in-app
 * WebIntegrationAuthBanner (the "operator" surfaces). The Backend Connection
 * tab's Connect button deliberately does NOT: it signs in to the URL the
 * operator just typed there.
 *
 * > The sign-in target is the saved URL when (1) settings are authoritative,
 * > (2) it is an absolute http(s) URL, (3) the runner itself would dial it
 * > (`backendUrlRefused === false`, so never machine-local in a release
 * > build), and (4) if it is machine-local, the request comes from an
 * > operator surface. Every other case targets production.
 *
 * Why the saved URL matters at all: `finalize_signed_in`
 * (`src-tauri/src/commands/auth.rs`, step 6) WRITES the URL it signed in
 * against back into `web_integration.backend_url`. A hard-coded target
 * silently overwrote a custom backend saved on Settings → Backend Connection.
 *
 * Why the machine-local and refused verdicts come from the runner: the
 * build's refusal (`persisted_backend_url_refused`, `api_config.rs`) is the
 * single expression of "this build will not dial that URL". A TS copy of the
 * host classes drifted from it, so `get_web_integration_status` now serves
 * `backendUrlMachineLocal` / `backendUrlRefused` and this file only reads
 * them. A missing or non-boolean verdict is UNKNOWN and takes production.
 *
 * Why gate and operator surfaces differ: a GATE surface runs before this
 * runner has any sign-in. In a debug build (the `release-checked` primary
 * included) the saved value there is almost always the loopback build default
 * (`default_web_integration_backend_url`), which records no choice and points
 * at a local backend that is usually not running. An OPERATOR surface appears
 * in a running, already-configured app next to the Backend Connection tab,
 * where a local value is a deliberate choice — the local-backend dev loop.
 * Release behaviour is identical on every surface, because the build's own
 * refusal (3) applies first.
 */

import { invoke } from "@tauri-apps/api/core";

/** Production web backend — the sign-in target whenever the saved URL is not. */
export const PRODUCTION_BACKEND_URL = "https://api.qontinui.io";

/** Which kind of screen is asking. See the file header. */
export type SignInSurface = "gate" | "operator";

/** The subset of `get_web_integration_status` this decision reads. */
export interface SavedBackendStatus {
  backendUrl?: string | null;
  /** Non-null when settings.json was unreadable: `backendUrl` is then a
   *  placeholder default, not the operator's saved choice. */
  settingsFault?: unknown;
  /** Runner verdict: the saved URL's host is loopback or bind-all. */
  backendUrlMachineLocal?: boolean;
  /** Runner verdict: this build will not dial the saved URL (a machine-local
   *  URL in a release build). */
  backendUrlRefused?: boolean;
}

interface SignInDecision {
  target: string;
  /** `null` when the target is the saved URL. */
  reason: string | null;
}

function production(reason: string): SignInDecision {
  return { target: PRODUCTION_BACKEND_URL, reason };
}

function decide(status: SavedBackendStatus | null, surface: SignInSurface): SignInDecision {
  // (1) settings authoritative
  if (!status || status.settingsFault != null) {
    return production("The runner's settings could not be read, so the saved URL is not used.");
  }
  const saved = typeof status.backendUrl === "string" ? status.backendUrl.trim() : "";
  if (!saved) return production("No backend URL is saved.");
  // (2) absolute http(s) URL
  let proto: string;
  try {
    proto = new URL(saved).protocol;
  } catch {
    proto = "";
  }
  if (proto !== "https:" && proto !== "http:") {
    return production("The saved backend URL is not an absolute http(s) URL.");
  }
  // (3) the build would dial it
  if (status.backendUrlRefused === true) {
    return production(
      "The saved backend URL is machine-local, and this release build does not dial machine-local backends.",
    );
  }
  if (status.backendUrlRefused !== false) {
    return production("The runner did not report a build verdict for the saved URL.");
  }
  // (4) machine-local only on an operator surface
  if (status.backendUrlMachineLocal === true) {
    if (surface === "operator") return { target: saved, reason: null };
    return production(
      "The saved backend URL is machine-local, and this first-run sign-in screen does not use machine-local backends.",
    );
  }
  if (status.backendUrlMachineLocal !== false) {
    return production(
      "The runner did not report whether the saved URL is machine-local (verdict missing).",
    );
  }
  return { target: saved, reason: null };
}

/** Pure decision: which backend a sign-in from `surface` targets, given the
 *  status read (`null` when the read failed). */
export function signInBackendUrlFrom(
  status: SavedBackendStatus | null,
  surface: SignInSurface,
): string {
  return decide(status, surface).target;
}

/** One-line reason the sign-in target is NOT the saved URL, or `null` when it
 *  is. Pairs with {@link signInBackendUrlFrom}: both read one decision. */
export function signInTargetExplanation(
  status: SavedBackendStatus | null,
  surface: SignInSurface,
): string | null {
  return decide(status, surface).reason;
}

/** Read the saved status and decide. A failed read targets production. */
export async function resolveSignInBackendUrl(surface: SignInSurface): Promise<string> {
  try {
    return signInBackendUrlFrom(
      await invoke<SavedBackendStatus>("get_web_integration_status"),
      surface,
    );
  } catch {
    return signInBackendUrlFrom(null, surface);
  }
}
