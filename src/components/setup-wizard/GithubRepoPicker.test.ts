/**
 * Pure-logic tests for GithubRepoPicker helpers.
 *
 * The runner's vitest config uses `environment: "node"` (no jsdom/RTL), so we
 * follow the repo precedent (`CommitTrafficLight.test.tsx`) and exercise the
 * exported pure helpers rather than rendering JSX. The picker's rendered shell
 * is covered by manual + UI Bridge tests.
 *
 * Coverage:
 *   1. `classifyLoadError` — maps each raw backend signal to its specific
 *      headline, with deterministic 401-before-403 precedence.
 *   2. `deriveGithubPickerView` — the loading → error → signed_out →
 *      not_connected → connected precedence, including the regression case
 *      where a thrown auth error must NOT collapse to the Connect-GitHub CTA.
 */

import { describe, it, expect } from "vitest";

import { classifyLoadError, deriveGithubPickerView } from "./GithubRepoPicker";

const HEADLINE_401 =
  "Your Qontinui sign-in has expired. Re-open Settings → Account and sign in again.";
const HEADLINE_403 =
  "Your account isn't authorized for this workspace's GitHub connection. (Couldn't resolve your tenant.)";
const HEADLINE_NETWORK = "Couldn't reach Qontinui. Check your connection and retry.";
const HEADLINE_GENERIC = "Couldn't load your GitHub repositories.";

describe("classifyLoadError", () => {
  it("maps a 401 to the expired-sign-in headline", () => {
    expect(classifyLoadError("Backend returned 401: unauthorized")).toBe(HEADLINE_401);
  });

  it("maps a 403 to the not-authorized headline", () => {
    expect(classifyLoadError("Backend returned 403: tenant_not_resolved")).toBe(HEADLINE_403);
  });

  it("maps a 'Failed to reach' error to the network headline", () => {
    expect(classifyLoadError("Failed to reach Qontinui backend: connection refused")).toBe(
      HEADLINE_NETWORK,
    );
  });

  it("falls back to the generic headline for any other failure", () => {
    expect(classifyLoadError("Backend returned 500: boom")).toBe(HEADLINE_GENERIC);
  });

  it("checks 401 before 403 (deterministic precedence)", () => {
    // A message that mentions both codes must resolve to the 401 headline,
    // proving the most-specific-first ordering is stable.
    const raw = "Backend returned 401: token expired (was previously 403)";
    expect(classifyLoadError(raw)).toBe(HEADLINE_401);
    expect(classifyLoadError(raw)).not.toBe(HEADLINE_403);
  });
});

describe("deriveGithubPickerView", () => {
  it("returns 'loading' when loading, regardless of other flags", () => {
    expect(
      deriveGithubPickerView({
        loading: true,
        loadError: null,
        signedIn: true,
        connected: true,
      }),
    ).toBe("loading");
  });

  it("returns 'error' when a load error is present, winning over signed_out", () => {
    expect(
      deriveGithubPickerView({
        loading: false,
        loadError: "x",
        signedIn: false,
        connected: false,
      }),
    ).toBe("error");
  });

  it("returns 'signed_out' when not signed in and no error", () => {
    expect(
      deriveGithubPickerView({
        loading: false,
        loadError: null,
        signedIn: false,
        connected: false,
      }),
    ).toBe("signed_out");
  });

  it("returns 'not_connected' when signed in but the App isn't connected", () => {
    expect(
      deriveGithubPickerView({
        loading: false,
        loadError: null,
        signedIn: true,
        connected: false,
      }),
    ).toBe("not_connected");
  });

  it("returns 'connected' when signed in and connected", () => {
    expect(
      deriveGithubPickerView({
        loading: false,
        loadError: null,
        signedIn: true,
        connected: true,
      }),
    ).toBe("connected");
  });

  it("REGRESSION: a thrown 401 while not-connected renders 'error', NOT 'not_connected'", () => {
    // The exact bug being fixed: an auth failure thrown by github_list_repos must
    // surface as the error view, never collapse into the Connect-GitHub CTA.
    expect(
      deriveGithubPickerView({
        loading: false,
        loadError: "Backend returned 401: unauthorized",
        signedIn: true,
        connected: false,
      }),
    ).toBe("error");
  });
});
