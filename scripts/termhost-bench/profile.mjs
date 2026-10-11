/**
 * termhost-bench load profiles (`termhost-bench/profile/v1`) — the statistical shape
 * of one AI session's terminal output, which `generator/ai-session.mjs` turns into a
 * deterministic byte stream and `calibrate.mjs` derives from a recording.
 *
 * Plan `2026-10-11-terminal-page-performance-kpis-across-setups`, D3 and Phase 1.
 *
 * A profile is a Markov chain over phases. Each phase has a kind:
 *   idle     no output (markers only)
 *   spinner  one-row synchronized spinner repaints (runner#1856 `buildSpinnerFrame`)
 *   repaint  synchronized repaints (runner#1856 `buildRepaintFrame`), either a fixed
 *            `rows` count or row counts fitted to a `frame_bytes` distribution
 *   stream   assistant text streamed in small chunks (`chunk_bytes`)
 *   burst    tool output dumped in large chunks (`chunk_bytes`)
 * plus a `duration_ms` distribution and, for every kind but idle, a frame rate.
 * Distributions are quantile tables `[[p, value], ...]` from p = 0 to p = 1, sampled
 * by inverse-CDF linear interpolation (no transcendental functions, so a seed yields
 * the same stream on every platform).
 *
 * `calibrated` is true only for a profile fitted from a recording (`provenance.kind`
 * "calibration", with the recording's `expected` statistics); every results row
 * carries the flag.
 */

import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { dirname, isAbsolute, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { compileSchema } from "./validate.mjs";
import { MARKER_MAX_HZ } from "./marker.mjs";

const HERE = dirname(fileURLToPath(import.meta.url));
export const PROFILES_DIR = join(HERE, "profiles");
export const PROFILE_SCHEMA_ID = "termhost-bench/profile/v1";
export const PHASE_KINDS = Object.freeze(["idle", "spinner", "repaint", "stream", "burst"]);
export const SHIPPED_PROFILES = Object.freeze(["claude-tui", "all-repaint", "bulk"]);

const ID = "^[a-z][a-z0-9_-]*$";
const quantiles = { $ref: "#/$defs/quantiles" };
const forbid = (...keys) => ({ not: { anyOf: keys.map((k) => ({ required: [k] })) } });
const whenKind = (kinds, then) => ({
  if: { required: ["kind"], properties: { kind: { enum: kinds } } },
  then,
});

export const PROFILE_SCHEMA = Object.freeze({
  $schema: "https://json-schema.org/draft/2020-12/schema",
  title: "termhost-bench load profile (termhost-bench/profile/v1)",
  type: "object",
  required: [
    "schema",
    "id",
    "title",
    "calibrated",
    "provenance",
    "geometry",
    "marker_hz",
    "initial_phase",
    "phases",
    "transitions",
  ],
  additionalProperties: false,
  properties: {
    schema: { const: PROFILE_SCHEMA_ID },
    id: { type: "string", pattern: "^[a-z][a-z0-9-]*$" },
    title: { type: "string", minLength: 1 },
    description: { type: "string" },
    calibrated: { type: "boolean" },
    provenance: {
      type: "object",
      required: ["kind", "source"],
      additionalProperties: false,
      properties: {
        kind: { enum: ["calibration", "assumption", "synthetic"] },
        source: { type: "string", minLength: 1 },
        recordings: { type: "integer", minimum: 1 },
        duration_s: { type: "number", exclusiveMinimum: 0 },
        calibrated_at: { type: "string", pattern: "^\\d{4}-\\d{2}-\\d{2}T" },
      },
    },
    geometry: {
      type: "object",
      required: ["cols", "rows"],
      additionalProperties: false,
      properties: {
        cols: { type: "integer", minimum: 20, maximum: 1000 },
        rows: { type: "integer", minimum: 2, maximum: 500 },
      },
    },
    marker_hz: { type: "number", exclusiveMinimum: 0, maximum: MARKER_MAX_HZ },
    alt_screen: { type: "boolean" },
    initial_phase: { type: "string", pattern: ID },
    phases: {
      type: "object",
      minProperties: 1,
      propertyNames: { pattern: ID },
      additionalProperties: { $ref: "#/$defs/phase" },
    },
    transitions: {
      type: "object",
      propertyNames: { pattern: ID },
      additionalProperties: {
        type: "object",
        minProperties: 1,
        propertyNames: { pattern: ID },
        additionalProperties: { type: "number", exclusiveMinimum: 0 },
      },
    },
    expected: { $ref: "#/$defs/expected" },
  },
  $defs: {
    quantiles: {
      type: "array",
      minItems: 2,
      items: {
        type: "array",
        minItems: 2,
        maxItems: 2,
        items: { type: "number", minimum: 0 },
      },
    },
    phase: {
      type: "object",
      required: ["kind", "duration_ms"],
      additionalProperties: false,
      properties: {
        kind: { enum: [...PHASE_KINDS] },
        duration_ms: quantiles,
        fps: { type: "number", exclusiveMinimum: 0, maximum: 1000 },
        rows: { type: "integer", minimum: 1 },
        frame_bytes: quantiles,
        chunk_bytes: quantiles,
      },
      allOf: [
        whenKind(["idle"], forbid("fps", "rows", "frame_bytes", "chunk_bytes")),
        whenKind(["spinner"], {
          required: ["fps"],
          ...forbid("rows", "frame_bytes", "chunk_bytes"),
        }),
        whenKind(["repaint"], {
          required: ["fps"],
          oneOf: [{ required: ["rows"] }, { required: ["frame_bytes"] }],
          ...forbid("chunk_bytes"),
        }),
        whenKind(["stream", "burst"], {
          required: ["fps", "chunk_bytes"],
          ...forbid("rows", "frame_bytes"),
        }),
      ],
    },
    expected: {
      description: "Statistics measured from the calibration recording (calibrate.mjs).",
      type: "object",
      required: ["bytes_per_s", "sync_share", "phase_share", "frame_bytes"],
      additionalProperties: false,
      properties: {
        bytes_per_s: { type: "number", minimum: 0 },
        sync_share: { type: "number", minimum: 0, maximum: 1 },
        phase_share: {
          type: "object",
          propertyNames: { pattern: ID },
          additionalProperties: { type: "number", minimum: 0, maximum: 1 },
        },
        frame_bytes: quantiles,
      },
    },
  },
});

let profileValidator = null;

/**
 * Validate a profile: the schema, then the cross-field rules.
 *
 * @param {unknown} profile
 * @returns {{valid: boolean, errors: {path: string, message: string}[]}}
 */
export function validateProfile(profile) {
  if (!profileValidator) profileValidator = compileSchema(PROFILE_SCHEMA);
  const first = profileValidator(profile);
  if (!first.valid) return first;
  const errors = [];
  const add = (path, message) => errors.push({ path, keyword: "semantic", message });
  const ids = Object.keys(profile.phases);
  if (!ids.includes(profile.initial_phase)) {
    add("/initial_phase", `initial_phase "${profile.initial_phase}" is not a phase`);
  }
  for (const id of ids) {
    const phase = profile.phases[id];
    for (const key of ["duration_ms", "frame_bytes", "chunk_bytes"]) {
      if (phase[key]) quantileErrors(phase[key], `/phases/${id}/${key}`, add);
    }
    if (phase.duration_ms && phase.duration_ms[0][1] < 1) {
      add(`/phases/${id}/duration_ms`, "the shortest phase must last at least 1 ms");
    }
    if (phase.chunk_bytes && phase.chunk_bytes[0][1] < 1) {
      add(`/phases/${id}/chunk_bytes`, "a chunk must be at least 1 byte");
    }
    if (phase.frame_bytes && phase.frame_bytes[0][1] < 1) {
      add(`/phases/${id}/frame_bytes`, "a frame must be at least 1 byte");
    }
    if (phase.rows !== undefined && phase.rows > profile.geometry.rows) {
      add(
        `/phases/${id}/rows`,
        `rows ${phase.rows} exceeds the geometry's ${profile.geometry.rows}`,
      );
    }
    if (!profile.transitions[id]) add(`/transitions`, `phase "${id}" has no outgoing transitions`);
  }
  for (const [from, targets] of Object.entries(profile.transitions)) {
    if (!ids.includes(from)) add(`/transitions/${from}`, `"${from}" is not a phase`);
    for (const to of Object.keys(targets)) {
      if (!ids.includes(to)) add(`/transitions/${from}/${to}`, `"${to}" is not a phase`);
    }
  }
  const isCalibration = profile.provenance.kind === "calibration";
  if (profile.calibrated !== isCalibration) {
    add("/calibrated", 'calibrated must be true exactly when provenance.kind is "calibration"');
  }
  if (profile.calibrated && !profile.expected) {
    add("/expected", "a calibrated profile carries the recording's expected statistics");
  }
  if (profile.expected) quantileErrors(profile.expected.frame_bytes, "/expected/frame_bytes", add);
  return { valid: errors.length === 0, errors };
}

function quantileErrors(table, path, add) {
  if (table[0][0] !== 0) add(path, "a quantile table starts at p = 0");
  if (table[table.length - 1][0] !== 1) add(path, "a quantile table ends at p = 1");
  for (let i = 1; i < table.length; i += 1) {
    if (!(table[i][0] > table[i - 1][0])) add(path, `p must strictly increase (entry ${i})`);
    if (table[i][1] < table[i - 1][1]) add(path, `values must not decrease (entry ${i})`);
  }
}

/**
 * Inverse-CDF sample of a quantile table by linear interpolation.
 *
 * @param {number[][]} table [[p, value], ...], p from 0 to 1
 * @param {number} u uniform in [0, 1)
 * @returns {number}
 */
export function sampleQuantiles(table, u) {
  for (let i = 1; i < table.length; i += 1) {
    const [p1, v1] = table[i];
    if (u <= p1) {
      const [p0, v0] = table[i - 1];
      return p1 === p0 ? v1 : v0 + ((u - p0) / (p1 - p0)) * (v1 - v0);
    }
  }
  return table[table.length - 1][1];
}

/**
 * `sha256:<hex>` over the profile's canonical JSON (`canonicalJson`: sorted keys, no
 * whitespace), so key order and formatting never change a digest.
 *
 * @param {object} profile
 * @returns {string}
 */
export function profileDigest(profile) {
  return `sha256:${createHash("sha256").update(canonicalJson(profile)).digest("hex")}`;
}

/**
 * Canonical JSON: object keys sorted (UTF-16 code-unit order) at every level, no
 * whitespace, undefined members dropped. Two profiles that differ only in key order or
 * formatting have one text, hence one digest — and the generator reads transitions in
 * sorted key order too, so they also generate one stream.
 *
 * @param {unknown} value
 * @returns {string}
 */
export function canonicalJson(value) {
  if (Array.isArray(value)) return `[${value.map(canonicalJson).join(",")}]`;
  if (value !== null && typeof value === "object") {
    const members = Object.keys(value)
      .filter((k) => value[k] !== undefined)
      .sort()
      .map((k) => `${JSON.stringify(k)}:${canonicalJson(value[k])}`);
    return `{${members.join(",")}}`;
  }
  return JSON.stringify(value);
}

/**
 * A profile id (`claude-tui`) resolves to `profiles/<id>.json`; anything with a path
 * separator or a `.json` suffix is a file path.
 *
 * @param {string} spec
 * @returns {string} absolute path
 */
export function resolveProfilePath(spec) {
  if (typeof spec !== "string" || spec.length === 0)
    throw new TypeError("profile spec must be a non-empty string");
  if (spec.endsWith(".json") || spec.includes("/") || spec.includes("\\")) {
    return isAbsolute(spec) ? spec : resolve(spec);
  }
  if (!/^[a-z][a-z0-9-]*$/.test(spec)) throw new RangeError(`bad profile id "${spec}"`);
  return join(PROFILES_DIR, `${spec}.json`);
}

/**
 * Load and validate a profile; throws with every error listed.
 *
 * @param {string} spec id or path
 * @returns {{profile: object, path: string, digest: string}}
 */
export function loadProfile(spec) {
  const path = resolveProfilePath(spec);
  const profile = JSON.parse(readFileSync(path, "utf8"));
  assertValidProfile(profile, path);
  return { profile, path, digest: profileDigest(profile) };
}

/**
 * @param {object} profile
 * @param {string} [label]
 */
export function assertValidProfile(profile, label = "profile") {
  const result = validateProfile(profile);
  if (!result.valid) {
    const lines = result.errors.map((e) => `  ${e.path || "/"} ${e.message}`).join("\n");
    throw new Error(`invalid profile ${label}:\n${lines}`);
  }
}
