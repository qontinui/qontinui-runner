/**
 * Tests for the `AppFreshnessSettings` pure helpers.
 *
 * The runner's vitest config is `environment: "node"` (no jsdom) — sibling
 * tests like `LockYieldPolicySettings.test.tsx` set the precedent of testing
 * the exported pure helpers rather than rendering.
 *
 * The cases that matter are the ones covering the gap between what the FORM
 * says and what the SERVER ends up storing: `UpdateAppRequest` is applied with
 * COALESCE, so an omitted field is a no-op rather than a clear. Every gate in
 * the panel is derived from `effectiveAfterSave` for that reason, and these
 * tests pin it.
 */

import { describe, it, expect } from "vitest";

import {
  buildPatchBody,
  effectiveAfterSave,
  formOf,
  isFalselyFresh,
  isKnownStrategy,
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

/** A configured pull_build app — the common starting point below. */
const BUILT_APP = app({
  updateStrategy: "pull_build",
  buildCommand: "npm run build",
  startCommand: "npm start",
});

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
    const f = formOf(BUILT_APP);
    expect(f.buildCommand).toBe("npm run build");
    expect(f.startCommand).toBe("npm start");
  });
});

describe("isKnownStrategy", () => {
  it("is true for a recognised or absent strategy", () => {
    expect(isKnownStrategy(app())).toBe(true);
    expect(isKnownStrategy(app({ updateStrategy: "pull_build" }))).toBe(true);
  });

  it("makes an untouched rogue row NOT dirty, so Save cannot overwrite it", () => {
    // Both sides of the comparison degrade identically, so the row reads
    // clean. This is what makes the row's warning text say "this build cannot
    // change the stored value" rather than promising an overwrite.
    const rogue = app({ updateStrategy: "pull_build_and_deploy" });
    expect(sameForm(effectiveAfterSave(rogue, formOf(rogue)), formOf(rogue))).toBe(true);
  });

  it("IS dirty once a strategy is chosen — the overwrite is reachable", () => {
    // The other half of the truth the row's warning must tell: selecting a
    // strategy and giving it a command enables Save, and the PATCH overwrites
    // the value this build could not read. The warning says so; this pins that
    // it is a real path and not a theoretical one.
    const rogue = app({ updateStrategy: "pull_build_and_deploy" });
    const chosen = form({ updateStrategy: "pull_build", buildCommand: "make" });
    expect(sameForm(effectiveAfterSave(rogue, chosen), formOf(rogue))).toBe(false);
    expect(buildPatchBody(chosen).updateStrategy).toBe("pull_build");
  });

  it("is false when formOf silently degraded the stored value", () => {
    // This is what lets the row SAY it is misrepresenting the stored strategy
    // rather than quietly overwriting it on the next unrelated edit.
    expect(isKnownStrategy(app({ updateStrategy: "pull_build_and_deploy" }))).toBe(false);
  });
});

describe("sameForm", () => {
  it("is true for identical forms", () => {
    expect(sameForm(form(), form())).toBe(true);
  });

  it("detects a change in any one field", () => {
    expect(sameForm(form(), form({ updateStrategy: "pull_build" }))).toBe(false);
    expect(sameForm(form(), form({ buildCommand: "make" }))).toBe(false);
    expect(sameForm(form(), form({ startCommand: "make run" }))).toBe(false);
  });
});

describe("buildPatchBody", () => {
  it("always sends updateStrategy", () => {
    expect(buildPatchBody(form())).toEqual({ updateStrategy: "pull_only" });
    expect(buildPatchBody(form({ updateStrategy: "pull_build", buildCommand: "make" }))).toEqual({
      updateStrategy: "pull_build",
      buildCommand: "make",
    });
  });

  it("OMITS a blank command instead of sending an empty string", () => {
    // The load-bearing case. `UpdateAppRequest` is COALESCE-applied, and the
    // engine runs `if let Some(cmd)` checking only the exit status — an empty
    // shell command exits 0, so a persisted "" would mark the app freshly
    // built without building, and the fresh_only dispatcher would route tests
    // to a host serving stale code.
    const body = buildPatchBody(form({ updateStrategy: "pull_build" }));
    expect(Object.keys(body)).toEqual(["updateStrategy"]);
  });

  it("treats whitespace-only as blank", () => {
    const body = buildPatchBody(
      form({ updateStrategy: "pull_build", buildCommand: "   ", startCommand: "\t\n" }),
    );
    expect(Object.keys(body)).toEqual(["updateStrategy"]);
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

  it("NEVER sends commands under pull_only, even when the form holds them", () => {
    // Otherwise a command typed while pull_build was selected, then abandoned
    // by switching back to pull_only (which hides the inputs), is persisted
    // invisibly — and executed the moment anyone flips the app back.
    const body = buildPatchBody(
      form({ updateStrategy: "pull_only", buildCommand: "rm -rf dist", startCommand: "npm start" }),
    );
    expect(Object.keys(body)).toEqual(["updateStrategy"]);
    expect(body.updateStrategy).toBe("pull_only");
  });

  it("emits exactly the expected key set when everything is filled", () => {
    const body = buildPatchBody(
      form({
        updateStrategy: "pull_build",
        buildCommand: "npm run build",
        startCommand: "npm start",
      }),
    );
    // Pinned as a key set, not a type check: `UpdateAppRequest` has no
    // deny_unknown_fields, so a misspelt key would PATCH, return 200, and
    // change nothing.
    expect(Object.keys(body).sort()).toEqual(["buildCommand", "startCommand", "updateStrategy"]);
  });
});

describe("effectiveAfterSave", () => {
  it("is the stored row when nothing was edited", () => {
    expect(effectiveAfterSave(BUILT_APP, formOf(BUILT_APP))).toEqual(formOf(BUILT_APP));
  });

  it("KEEPS a stored command that the operator cleared in the form", () => {
    // The COALESCE consequence: clearing the input omits the key, so the
    // column is untouched. Modelling this is what stops the panel reporting
    // success for an edit that immediately undoes itself.
    const cleared = form({ updateStrategy: "pull_build", buildCommand: "", startCommand: "" });
    expect(effectiveAfterSave(BUILT_APP, cleared)).toEqual({
      updateStrategy: "pull_build",
      buildCommand: "npm run build",
      startCommand: "npm start",
    });
  });

  it("applies an edited command over the stored one", () => {
    const edited = form({
      updateStrategy: "pull_build",
      buildCommand: "make",
      startCommand: "npm start",
    });
    expect(effectiveAfterSave(BUILT_APP, edited).buildCommand).toBe("make");
  });

  it("keeps stored commands when switching to pull_only", () => {
    // buildPatchBody omits them, so the columns survive — the row is still
    // configured if the operator switches back.
    const switched = formOf({ ...BUILT_APP, updateStrategy: "pull_only" });
    const effective = effectiveAfterSave(BUILT_APP, switched);
    expect(effective.updateStrategy).toBe("pull_only");
    expect(effective.buildCommand).toBe("npm run build");
  });
});

/** What the panel computes: does saving change the stored row? */
function dirty(a: RegisteredApp, f: AppFreshnessForm): boolean {
  return !sameForm(effectiveAfterSave(a, f), formOf(a));
}

describe("dirty (effectiveAfterSave vs stored — the Save gate)", () => {
  it("is false for an untouched row", () => {
    expect(dirty(BUILT_APP, formOf(BUILT_APP))).toBe(false);
  });

  it("is false for a no-op PATCH — clearing an already-stored command", () => {
    // Save must not offer an action that reports success and changes nothing.
    const cleared = form({ updateStrategy: "pull_build", buildCommand: "", startCommand: "" });
    expect(dirty(BUILT_APP, cleared)).toBe(false);
  });

  it("is false for whitespace-only edits, which trim to the stored value", () => {
    const padded = form({
      updateStrategy: "pull_build",
      buildCommand: "  npm run build  ",
      startCommand: "npm start",
    });
    expect(dirty(BUILT_APP, padded)).toBe(false);
  });

  it("is true for a real command change", () => {
    const edited = form({
      updateStrategy: "pull_build",
      buildCommand: "make",
      startCommand: "npm start",
    });
    expect(dirty(BUILT_APP, edited)).toBe(true);
  });

  it("is true for a strategy change alone", () => {
    const switched = formOf({ ...BUILT_APP, updateStrategy: "pull_only" });
    expect(dirty(BUILT_APP, switched)).toBe(true);
  });

  it("is true when adding the first command to a bare app", () => {
    const bare = app({ updateStrategy: "pull_build" });
    expect(dirty(bare, form({ updateStrategy: "pull_build", buildCommand: "make" }))).toBe(true);
  });
});

describe("isFalselyFresh", () => {
  it("is true for pull_build with no command that would run", () => {
    // The engine takes neither `if let Some` arm, returns Ok(()), and marks
    // the app fresh at the new SHA with nothing built — the same false-fresh
    // outcome the omit-blank rule prevents, reached via NULL/NULL.
    const bare = app({ updateStrategy: "pull_build" });
    expect(isFalselyFresh(effectiveAfterSave(bare, form({ updateStrategy: "pull_build" })))).toBe(
      true,
    );
  });

  it("is false when either command will be set", () => {
    const bare = app({ updateStrategy: "pull_build" });
    expect(
      isFalselyFresh(
        effectiveAfterSave(bare, form({ updateStrategy: "pull_build", buildCommand: "make" })),
      ),
    ).toBe(false);
    expect(
      isFalselyFresh(
        effectiveAfterSave(bare, form({ updateStrategy: "pull_build", startCommand: "npm start" })),
      ),
    ).toBe(false);
  });

  it("is false when the inputs are blank but the stored commands survive", () => {
    // The distinction the raw form cannot make: blank inputs on a configured
    // app are a no-op, not a wipe, so this is NOT the dangerous state.
    const cleared = form({ updateStrategy: "pull_build", buildCommand: "", startCommand: "" });
    expect(isFalselyFresh(effectiveAfterSave(BUILT_APP, cleared))).toBe(false);
  });

  it("is false for pull_only regardless of commands", () => {
    // pull_only never claims a build happened, so an absent command is fine.
    expect(isFalselyFresh(effectiveAfterSave(app(), form()))).toBe(false);
  });
});
