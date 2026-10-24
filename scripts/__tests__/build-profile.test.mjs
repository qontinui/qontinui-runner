// `node --test scripts/__tests__/build-profile.test.mjs`
// Pins build:exe's profile selection (plan
// 2026-10-19-primary-runner-is-a-debug-build-and-nothing-shows-it, Phase 3).
// Every expected value is a literal.
import assert from "node:assert/strict";
import { test } from "node:test";

import {
  announcement,
  cargoArgs,
  debugOptInWarning,
  executablesFromCargoJson,
  profileDir,
  resolveProfile,
} from "../lib/build-profile.mjs";
import { PRIMARY_BINS, SIDECAR_BINS, SIDECAR_PACKAGES } from "../lib/sidecars.mjs";

test("no flag and no env builds release-checked", () => {
  assert.deepEqual(resolveProfile([], {}), { profile: "release-checked", source: "default" });
});

test("--profile dev and --profile=dev both pick dev", () => {
  assert.deepEqual(resolveProfile(["--profile", "dev"], {}), { profile: "dev", source: "--profile" });
  assert.deepEqual(resolveProfile(["--profile=dev"], {}), { profile: "dev", source: "--profile" });
});

test("the env var picks a profile, and the flag beats it", () => {
  const env = { QONTINUI_RUNNER_BUILD_PROFILE: "dev" };
  assert.deepEqual(resolveProfile([], env), { profile: "dev", source: "QONTINUI_RUNNER_BUILD_PROFILE" });
  assert.deepEqual(resolveProfile(["--profile", "release"], env), { profile: "release", source: "--profile" });
});

test("a blank env var is the default, not an empty profile", () => {
  assert.equal(resolveProfile([], { QONTINUI_RUNNER_BUILD_PROFILE: "  " }).profile, "release-checked");
});

test("malformed requests are refused, never silently defaulted", () => {
  assert.ok(resolveProfile(["--profile"], {}).error);
  assert.ok(resolveProfile(["--profile", "Dev"], {}).error, "cargo profile names are case-sensitive");
  assert.ok(resolveProfile(["--release"], {}).error);
  assert.ok(resolveProfile([], { QONTINUI_RUNNER_BUILD_PROFILE: "dev release" }).error);
});

test("cargo's dev profile writes to debug/; others to their own name", () => {
  assert.equal(profileDir("dev"), "debug");
  assert.equal(profileDir("release-checked"), "release-checked");
  assert.equal(profileDir("release"), "release");
});

test("the announcement names the profile and its knobs", () => {
  assert.equal(announcement("release-checked"), "build:exe: profile=release-checked (opt-level 2, debug-assertions on)");
  assert.equal(announcement("dev"), "build:exe: profile=dev (opt-level 0, debug-assertions on)");
  assert.equal(announcement("release"), "build:exe: profile=release (opt-level 2, debug-assertions off)");
  assert.equal(announcement("perf"), "build:exe: profile=perf (opt-level unknown, debug-assertions unknown)");
});

test("only a dev opt-in warns, and the warning names who chose it", () => {
  assert.equal(debugOptInWarning("release-checked", "default"), null);
  const w = debugOptInWarning("dev", "--profile");
  assert.match(w, /^build:exe: WARN: building a DEBUG runner/);
  assert.match(w, /chosen by --profile/);
});

test("the cargo invocation builds the runner and every sidecar in one profile", () => {
  assert.deepEqual(cargoArgs("release-checked", ["qontinui-runner", "qontinui-pty-holder"], ["qontinui-pr", "qontinui-shim"]), [
    "build",
    "--profile",
    "release-checked",
    "-p",
    "qontinui-runner",
    "-p",
    "qontinui-pty-holder",
    "--bin",
    "qontinui-runner",
    "--bin",
    "qontinui-pr",
    "--bin",
    "qontinui-shim",
    "--features",
    "qontinui-runner/custom-protocol",
    "--message-format=json-render-diagnostics",
  ]);
});

test("the primary roster is the bundle sidecars plus the shim", () => {
  assert.deepEqual(SIDECAR_BINS, ["qontinui_profile", "qontinui-pr", "qontinui-pty-holder"]);
  assert.deepEqual(SIDECAR_PACKAGES, ["qontinui-runner", "qontinui-pty-holder"]);
  assert.deepEqual(PRIMARY_BINS, ["qontinui_profile", "qontinui-pr", "qontinui-pty-holder", "qontinui-shim"]);
});

test("artifact paths come from cargo's JSON, last message wins, others ignored", () => {
  const lines = [
    "not json",
    JSON.stringify({ reason: "compiler-artifact", target: { name: "qontinui-pr" }, executable: "C:/t/release-checked/old.exe" }),
    JSON.stringify({ reason: "compiler-artifact", target: { name: "qontinui-pr" }, executable: "C:/t/release-checked/qontinui-pr.exe" }),
    JSON.stringify({ reason: "compiler-artifact", target: { name: "serde" }, executable: null }),
    JSON.stringify({ reason: "build-finished", success: true }),
  ].join("\n");
  const found = executablesFromCargoJson(lines, ["qontinui-pr", "qontinui-shim"]);
  assert.equal(found.get("qontinui-pr"), "C:/t/release-checked/qontinui-pr.exe");
  assert.equal(found.has("qontinui-shim"), false);
});
