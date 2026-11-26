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

/**
 * Which arm of the one sign-in rule answered. The first five are BOUNDED: the
 * rule had every input it reads, so the target is its answer. The last three
 * are UNBOUNDED: the answer is production only because an input was unknown.
 *
 * - `saved` — the saved URL is the target.
 * - `no_saved_url` / `not_http` — nothing usable is saved (rule 2).
 * - `refused_by_build` — this build will not dial the saved URL (rule 3).
 * - `machine_local_on_gate` — a machine-local URL on a first-run screen (rule 4).
 * - `read_rejected` — the `get_web_integration_status` read itself failed.
 * - `settings_fault` — the read answered with a non-null `settingsFault`, so its
 *   `backendUrl` is a placeholder default, not the operator's saved choice.
 * - `verdict_missing` — the read answered without a boolean
 *   `backendUrlRefused` / `backendUrlMachineLocal` verdict (needs build skew).
 */
export type SignInArm =
  | "saved"
  | "no_saved_url"
  | "not_http"
  | "refused_by_build"
  | "machine_local_on_gate"
  | "read_rejected"
  | "settings_fault"
  | "verdict_missing";

/**
 * The one sign-in rule's answer, with whether it is BOUNDED.
 *
 * `bounded: false` is the definition of "couldn't read this runner's saved
 * backend settings" — audience question `signin-gate.external-operator.2`
 * (status: PROPOSED in `audience_profile/external-operator` v3, an assumption
 * until an operator confirms it): a rejected status read, a non-null
 * `settingsFault`, or a missing build / machine-local verdict. The target is
 * then production, but only as a placeholder: a consumer that would WRITE it
 * (every gate surface — `finalize_signed_in` saves the URL it signed in
 * against) must not act on it silently.
 */
export interface SignInDecision {
  target: string;
  /** `null` when the target is the saved URL. */
  reason: string | null;
  bounded: boolean;
  arm: SignInArm;
}

/** The unbounded arms a status RE-READ can fix: a transient read failure, or a
 *  `settingsFault` that self-heals once settings.json is readable again. A
 *  `verdict_missing` answer needs build skew, which no re-read changes. */
export function isRereadableArm(arm: SignInArm): boolean {
  return arm === "read_rejected" || arm === "settings_fault";
}

/** The arms whose production answer rests on an unknown input. */
const UNBOUNDED_ARMS: ReadonlySet<SignInArm> = new Set<SignInArm>([
  "read_rejected",
  "settings_fault",
  "verdict_missing",
]);

function production(arm: Exclude<SignInArm, "saved">, reason: string): SignInDecision {
  return { target: PRODUCTION_BACKEND_URL, reason, bounded: !UNBOUNDED_ARMS.has(arm), arm };
}

function decide(status: SavedBackendStatus | null, surface: SignInSurface): SignInDecision {
  // (1) settings authoritative
  if (!status) {
    return production(
      "read_rejected",
      "The runner's status could not be read, so the saved URL is not used.",
    );
  }
  if (status.settingsFault != null) {
    return production(
      "settings_fault",
      "The runner's settings could not be read, so the saved URL is not used.",
    );
  }
  const saved = typeof status.backendUrl === "string" ? status.backendUrl.trim() : "";
  if (!saved) return production("no_saved_url", "No backend URL is saved.");
  // (2) absolute http(s) URL
  let proto: string;
  try {
    proto = new URL(saved).protocol;
  } catch {
    proto = "";
  }
  if (proto !== "https:" && proto !== "http:") {
    return production("not_http", "The saved backend URL is not an absolute http(s) URL.");
  }
  // (3) the build would dial it
  if (status.backendUrlRefused === true) {
    return production(
      "refused_by_build",
      "The saved backend URL is machine-local, and this release build does not dial machine-local backends.",
    );
  }
  if (status.backendUrlRefused !== false) {
    return production(
      "verdict_missing",
      "The runner did not report a build verdict for the saved URL.",
    );
  }
  // (4) machine-local only on an operator surface
  if (status.backendUrlMachineLocal === true) {
    if (surface === "operator") return { target: saved, reason: null, bounded: true, arm: "saved" };
    return production(
      "machine_local_on_gate",
      "The saved backend URL is machine-local, and this first-run sign-in screen does not use machine-local backends.",
    );
  }
  if (status.backendUrlMachineLocal !== false) {
    return production(
      "verdict_missing",
      "The runner did not report whether the saved URL is machine-local (verdict missing).",
    );
  }
  return { target: saved, reason: null, bounded: true, arm: "saved" };
}

/** Pure decision for `surface`, given the status read (`null` when the read
 *  failed). The two projections below read this same decision. */
export function signInDecisionFrom(
  status: SavedBackendStatus | null,
  surface: SignInSurface,
): SignInDecision {
  return decide(status, surface);
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

/** ONE `get_web_integration_status` read, decided. A rejected read is the
 *  `read_rejected` arm; this never rejects. */
export async function readSignInDecision(surface: SignInSurface): Promise<SignInDecision> {
  let status: SavedBackendStatus | null;
  try {
    status = await invoke<SavedBackendStatus>("get_web_integration_status");
  } catch {
    status = null;
  }
  return signInDecisionFrom(status, surface);
}

/** Read the saved status and decide, re-reading ONCE when the first answer is
 *  an unbounded arm a re-read can fix ({@link isRereadableArm}); the second
 *  answer is returned as-is. A `verdict_missing` answer is returned without a
 *  re-read. Never rejects.
 *
 *  An unbounded answer is REPORTED, never resolved to a silent production
 *  sign-in. The gate surfaces (LoginScreen, GithubRepoPicker) withhold on
 *  `bounded: false`: they start no sign-in, and offer the operator an explicit
 *  "Sign in to api.qontinui.io" that says what it does to the saved backend,
 *  beside "Retry and sign in" on the re-readable arms (`SignInFallbackNotice`).
 *  So `finalize_signed_in`, which writes the URL it signed in against into
 *  `web_integration.backend_url`, replaces a saved backend only by the
 *  operator's visible choice. The operator surfaces (auth banner, Settings →
 *  Account, via `useSavedBackendStatus`) never sign in on a rejected or faulted
 *  read and offer Retry; on `verdict_missing` they name the reason beside the
 *  effective target before the click. */
export async function resolveSignInDecision(surface: SignInSurface): Promise<SignInDecision> {
  const first = await readSignInDecision(surface);
  if (!isRereadableArm(first.arm)) return first;
  return readSignInDecision(surface);
}
