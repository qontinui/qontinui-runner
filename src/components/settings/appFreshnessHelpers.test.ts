/**
 * Tests for the `AppFreshnessSettings` pure helpers.
 *
 * The runner's vitest config is `environment: "node"` (no jsdom) — sibling
 * tests like `LockYieldPolicySettings.test.tsx` set the precedent of testing
 * the exported pure helpers rather than rendering.
 *
 * The `buildPatchBody` cases below are the ones that matter: they pin the two
 * encodings that keep a blank field from silently corrupting the fleet's
 * freshness signal.
 */

import { describe, it, expect } from "vitest";

import {
  buildPatchBody,
  formOf,
  isUpdateStrategy,
  sameForm,
  type AppFreshnessForm,
  type RegisteredApp,
} from "./appFreshnessHelpers";

function app(overrides: Partial<RegisteredApp> = {}): RegisteredApp {
  return {
    appId: "my-app",
    repoRoot: "D:/code/my-app",
    displayName: "My App",
    ...overrides,
  };
}

function form(overrides: Partial<AppFreshnessForm> = {}): AppFreshnessForm {
  return {
    updateStrategy: "pull_only",
    buildCommand: "",
    startCommand: "",
    ...overrides,
  };
}

describe("isUpdateStrategy", () => {
  it("accepts exactly the two values the server validates", () => {
    expect(isUpdateStrategy("pull_only")).toBe(true);
    expect(isUpdateStrategy("pull_build")).toBe(true);
  });

  it("rejects anything else, including near-misses and casing variants", () => {
    for (const value of ["", "pull", "pullBuild", "PULL_BUILD", "pull_build ", "rebuild"]) {
      expect(isUpdateStrategy(value)).toBe(false);
    }
  });
});

describe("formOf", () => {
  it("defaults a missing strategy to pull_only", () => {
    expect(formOf(app()).updateStrategy).toBe("pull_only");
  });

  it("degrades an unrecognised strategy to pull_only rather than throwing", () => {
    // A newer runner could serve a strategy this build has never heard of.
    // pull_only is the safe degradation: it runs no command at all.
    expect(formOf(app({ updateStrategy: "pull_build_and_deploy" })).updateStrategy).toBe(
      "pull_only",
    );
  });

  it("carries a known strategy through", () => {
    expect(formOf(app({ updateStrategy: "pull_build" })).updateStrategy).toBe("pull_build");
  });

  it("maps null/absent commands to empty strings so inputs stay controlled", () => {
    const f = formOf(app({ buildCommand: null, startCommand: undefined }));
    expect(f.buildCommand).toBe("");
    expect(f.startCommand).toBe("");
  });

  it("preserves stored commands verbatim", () => {
    const f = formOf(app({ buildCommand: "npm run build", startCommand: "npm start" }));
    expect(f.buildCommand).toBe("npm run build");
    expect(f.startCommand).toBe("npm start");
  });
});

describe("sameForm", () => {
  it("is true for identical forms — this is what gates the Save button", () => {
    expect(sameForm(form(), form())).toBe(true);
  });

  it("detects a change in any one field", () => {
    expect(sameForm(form(), form({ updateStrategy: "pull_build" }))).toBe(false);
    expect(sameForm(form(), form({ buildCommand: "make" }))).toBe(false);
    expect(sameForm(form(), form({ startCommand: "make run" }))).toBe(false);
  });

  it("round-trips an unedited app to 'not dirty'", () => {
    const a = app({ updateStrategy: "pull_build", buildCommand: "make" });
    expect(sameForm(formOf(a), formOf(a))).toBe(true);
  });
});

describe("buildPatchBody", () => {
  it("always sends updateStrategy", () => {
    expect(buildPatchBody(form())).toEqual({ updateStrategy: "pull_only" });
    expect(buildPatchBody(form({ updateStrategy: "pull_build" }))).toEqual({
      updateStrategy: "pull_build",
    });
  });

  it("OMITS a blank command instead of sending an empty string", () => {
    // The load-bearing case. `UpdateAppRequest` is COALESCE-applied, and the
    // engine runs `if let Some(cmd)` — an empty shell command exits 0, so a
    // persisted "" would mark the app freshly built without building, and the
    // fresh_only dispatcher would route tests to a host serving stale code.
    const body = buildPatchBody(form({ updateStrategy: "pull_build" }));
    expect(body).not.toHaveProperty("buildCommand");
    expect(body).not.toHaveProperty("startCommand");
  });

  it("treats whitespace-only as blank", () => {
    const body = buildPatchBody(
      form({ updateStrategy: "pull_build", buildCommand: "   ", startCommand: "\t\n" }),
    );
    expect(body).not.toHaveProperty("buildCommand");
    expect(body).not.toHaveProperty("startCommand");
  });

  it("trims a command before sending it", () => {
    const body = buildPatchBody(
      form({ updateStrategy: "pull_build", buildCommand: "  npm run build  " }),
    );
    expect(body.buildCommand).toBe("npm run build");
  });

  it("sends each command independently", () => {
    expect(
      buildPatchBody(form({ updateStrategy: "pull_build", startCommand: "npm start" })),
    ).toEqual({ updateStrategy: "pull_build", startCommand: "npm start" });
  });

  it("sends both commands when both are set", () => {
    expect(
      buildPatchBody(
        form({
          updateStrategy: "pull_build",
          buildCommand: "npm run build",
          startCommand: "npm start",
        }),
      ),
    ).toEqual({
      updateStrategy: "pull_build",
      buildCommand: "npm run build",
      startCommand: "npm start",
    });
  });

  it("emits only JSON-serialisable strings", () => {
    // The body is handed straight to JSON.stringify; an undefined value would
    // silently vanish and a non-string would be rejected by serde.
    const body = buildPatchBody(
      form({ updateStrategy: "pull_build", buildCommand: "make", startCommand: "make run" }),
    );
    for (const value of Object.values(body)) {
      expect(typeof value).toBe("string");
    }
  });
});
