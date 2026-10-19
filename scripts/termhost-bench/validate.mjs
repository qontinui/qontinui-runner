#!/usr/bin/env node
/**
 * termhost-bench validator — a JSON Schema 2020-12 validator for the subset of the
 * language `result-schema.json` and the profile schema use, plus the results-file
 * checks a schema cannot express. Node built-ins only (plan D8: no npm dependency).
 *
 * Plan `2026-10-11-terminal-page-performance-kpis-across-setups`, Phase 1. Arming:
 * a results file that omits a KPI's state, under-samples a measured KPI, or renders
 * an unmeasured KPI as 0 is rejected here.
 *
 * FAIL CLOSED. A schema using a keyword this validator does not implement is
 * refused at compile time rather than silently ignored: an ignored keyword is a
 * constraint that passes everything, which is the green-but-vacuous failure this
 * harness exists to prevent. `format` is an annotation in 2020-12's default
 * vocabulary and is treated as one (date-times are checked by `pattern` instead).
 *
 * Usage:
 *   node scripts/termhost-bench/validate.mjs <results.json> [...]
 *     exit 0 = every file valid, 1 = a file is invalid, 2 = usage / read / parse error
 */

import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { KPI_CATALOG } from "./kpi-catalog.mjs";

const HERE = dirname(fileURLToPath(import.meta.url));
export const RESULT_SCHEMA_PATH = join(HERE, "result-schema.json");

/** Keywords that constrain an instance; each is implemented below. */
export const SUPPORTED_KEYWORDS = Object.freeze([
  "$ref",
  "$defs",
  "type",
  "enum",
  "const",
  "required",
  "properties",
  "additionalProperties",
  "propertyNames",
  "minProperties",
  "maxProperties",
  "items",
  "minItems",
  "maxItems",
  "uniqueItems",
  "minimum",
  "maximum",
  "exclusiveMinimum",
  "exclusiveMaximum",
  "minLength",
  "maxLength",
  "pattern",
  "allOf",
  "anyOf",
  "oneOf",
  "not",
  "if",
  "then",
  "else",
]);

/** Keywords that never constrain an instance. */
export const ANNOTATION_KEYWORDS = Object.freeze([
  "$schema",
  "$id",
  "$comment",
  "title",
  "description",
  "default",
  "examples",
  "format",
  "deprecated",
  "readOnly",
  "writeOnly",
]);

const KNOWN = new Set([...SUPPORTED_KEYWORDS, ...ANNOTATION_KEYWORDS]);
const TYPES = new Set(["null", "boolean", "object", "array", "number", "integer", "string"]);

/**
 * Walk a schema and refuse any keyword outside the supported subset.
 *
 * @param {unknown} schema
 * @param {string} [at] JSON pointer of `schema` inside the root
 */
export function assertSupportedSchema(schema, at = "#") {
  if (typeof schema === "boolean") return;
  if (!isPlainObject(schema)) throw new TypeError(`schema at ${at} is not an object or boolean`);
  for (const [key, value] of Object.entries(schema)) {
    if (!KNOWN.has(key)) throw new Error(`unsupported schema keyword "${key}" at ${at}`);
    const here = `${at}/${escapePointer(key)}`;
    switch (key) {
      case "properties":
      case "$defs":
        if (!isPlainObject(value)) throw new TypeError(`${here} must be an object`);
        for (const [k, sub] of Object.entries(value)) {
          assertSupportedSchema(sub, `${here}/${escapePointer(k)}`);
        }
        break;
      case "additionalProperties":
      case "propertyNames":
      case "items":
      case "not":
      case "if":
      case "then":
      case "else":
        assertSupportedSchema(value, here);
        break;
      case "allOf":
      case "anyOf":
      case "oneOf":
        if (!Array.isArray(value) || value.length === 0) {
          throw new TypeError(`${here} must be a non-empty array`);
        }
        value.forEach((sub, i) => assertSupportedSchema(sub, `${here}/${i}`));
        break;
      case "type": {
        const list = Array.isArray(value) ? value : [value];
        for (const t of list) if (!TYPES.has(t)) throw new Error(`unknown type "${t}" at ${here}`);
        break;
      }
      case "pattern":
        new RegExp(value, "u");
        break;
      case "$ref":
        if (typeof value !== "string" || !value.startsWith("#")) {
          throw new Error(`only local "#..." $ref is supported (at ${here}: ${value})`);
        }
        break;
      default:
        break;
    }
  }
}

/**
 * Compile a root schema into a validating function.
 *
 * @param {object | boolean} root
 * @returns {(instance: unknown) => {valid: boolean, errors: {path: string, keyword: string, message: string}[]}}
 */
export function compileSchema(root) {
  assertSupportedSchema(root);
  const regexCache = new Map();
  const ctx = {
    root,
    regex(pattern) {
      let re = regexCache.get(pattern);
      if (!re) {
        re = new RegExp(pattern, "u");
        regexCache.set(pattern, re);
      }
      return re;
    },
  };
  // Resolve every $ref once up front so a dangling reference fails at compile time.
  forEachRef(root, (ref) => resolveRef(root, ref));
  return (instance) => {
    const errors = [];
    validateNode(root, instance, "", ctx, errors);
    return { valid: errors.length === 0, errors };
  };
}

/**
 * One-shot validation.
 *
 * @param {object | boolean} schema
 * @param {unknown} instance
 */
export function validateAgainst(schema, instance) {
  return compileSchema(schema)(instance);
}

function validateNode(schema, value, path, ctx, errors) {
  if (schema === true) return;
  if (schema === false) {
    errors.push(err(path, "false", "no value is allowed here"));
    return;
  }
  if (schema.$ref !== undefined)
    validateNode(resolveRef(ctx.root, schema.$ref), value, path, ctx, errors);

  if (schema.type !== undefined) {
    const types = Array.isArray(schema.type) ? schema.type : [schema.type];
    if (!types.some((t) => hasType(value, t))) {
      errors.push(err(path, "type", `expected ${types.join(" | ")}, got ${describe(value)}`));
      return;
    }
  }
  if (schema.enum !== undefined && !schema.enum.some((e) => deepEqual(e, value))) {
    errors.push(err(path, "enum", `${short(value)} is not one of ${short(schema.enum)}`));
  }
  if (schema.const !== undefined && !deepEqual(schema.const, value)) {
    errors.push(err(path, "const", `expected ${short(schema.const)}, got ${short(value)}`));
  }

  if (typeof value === "number") validateNumber(schema, value, path, errors);
  if (typeof value === "string") validateString(schema, value, path, ctx, errors);
  if (Array.isArray(value)) validateArray(schema, value, path, ctx, errors);
  if (isPlainObject(value)) validateObject(schema, value, path, ctx, errors);

  if (schema.allOf) for (const sub of schema.allOf) validateNode(sub, value, path, ctx, errors);
  if (schema.anyOf) {
    const branches = schema.anyOf.map((sub) => collect(sub, value, path, ctx));
    if (!branches.some((b) => b.length === 0)) {
      errors.push(
        err(path, "anyOf", `matched none of ${branches.length} alternatives: ${firstOf(branches)}`),
      );
    }
  }
  if (schema.oneOf) {
    const branches = schema.oneOf.map((sub) => collect(sub, value, path, ctx));
    const passing = branches.filter((b) => b.length === 0).length;
    if (passing !== 1) {
      errors.push(
        err(
          path,
          "oneOf",
          passing === 0
            ? `matched none of ${branches.length} alternatives: ${firstOf(branches)}`
            : `matched ${passing} alternatives; exactly one is allowed`,
        ),
      );
    }
  }
  if (schema.not !== undefined && collect(schema.not, value, path, ctx).length === 0) {
    errors.push(err(path, "not", "matched a schema it must not match"));
  }
  if (schema.if !== undefined) {
    const passes = collect(schema.if, value, path, ctx).length === 0;
    const branch = passes ? schema.then : schema.else;
    if (branch !== undefined) validateNode(branch, value, path, ctx, errors);
  }
}

function validateNumber(schema, value, path, errors) {
  if (schema.minimum !== undefined && value < schema.minimum) {
    errors.push(err(path, "minimum", `${value} < minimum ${schema.minimum}`));
  }
  if (schema.maximum !== undefined && value > schema.maximum) {
    errors.push(err(path, "maximum", `${value} > maximum ${schema.maximum}`));
  }
  if (schema.exclusiveMinimum !== undefined && value <= schema.exclusiveMinimum) {
    errors.push(
      err(path, "exclusiveMinimum", `${value} <= exclusiveMinimum ${schema.exclusiveMinimum}`),
    );
  }
  if (schema.exclusiveMaximum !== undefined && value >= schema.exclusiveMaximum) {
    errors.push(
      err(path, "exclusiveMaximum", `${value} >= exclusiveMaximum ${schema.exclusiveMaximum}`),
    );
  }
}

function validateString(schema, value, path, ctx, errors) {
  const length = [...value].length;
  if (schema.minLength !== undefined && length < schema.minLength) {
    errors.push(err(path, "minLength", `length ${length} < minLength ${schema.minLength}`));
  }
  if (schema.maxLength !== undefined && length > schema.maxLength) {
    errors.push(err(path, "maxLength", `length ${length} > maxLength ${schema.maxLength}`));
  }
  if (schema.pattern !== undefined && !ctx.regex(schema.pattern).test(value)) {
    errors.push(err(path, "pattern", `${short(value)} does not match /${schema.pattern}/`));
  }
}

function validateArray(schema, value, path, ctx, errors) {
  if (schema.minItems !== undefined && value.length < schema.minItems) {
    errors.push(err(path, "minItems", `${value.length} items < minItems ${schema.minItems}`));
  }
  if (schema.maxItems !== undefined && value.length > schema.maxItems) {
    errors.push(err(path, "maxItems", `${value.length} items > maxItems ${schema.maxItems}`));
  }
  if (schema.uniqueItems === true) {
    for (let i = 0; i < value.length; i += 1) {
      for (let j = i + 1; j < value.length; j += 1) {
        if (deepEqual(value[i], value[j])) {
          errors.push(err(path, "uniqueItems", `items ${i} and ${j} are equal`));
        }
      }
    }
  }
  if (schema.items !== undefined) {
    value.forEach((item, i) => validateNode(schema.items, item, `${path}/${i}`, ctx, errors));
  }
}

function validateObject(schema, value, path, ctx, errors) {
  const keys = Object.keys(value);
  if (schema.required) {
    for (const key of schema.required) {
      if (!Object.prototype.hasOwnProperty.call(value, key)) {
        errors.push(err(path, "required", `missing required property "${key}"`));
      }
    }
  }
  if (schema.minProperties !== undefined && keys.length < schema.minProperties) {
    errors.push(err(path, "minProperties", `${keys.length} properties < ${schema.minProperties}`));
  }
  if (schema.maxProperties !== undefined && keys.length > schema.maxProperties) {
    errors.push(err(path, "maxProperties", `${keys.length} properties > ${schema.maxProperties}`));
  }
  const props = schema.properties ?? {};
  for (const key of keys) {
    const child = `${path}/${escapePointer(key)}`;
    if (schema.propertyNames !== undefined) {
      const nameErrors = collect(schema.propertyNames, key, child, ctx);
      if (nameErrors.length) {
        errors.push(
          err(
            child,
            "propertyNames",
            `property name "${key}" is invalid: ${nameErrors[0].message}`,
          ),
        );
      }
    }
    if (Object.prototype.hasOwnProperty.call(props, key)) {
      validateNode(props[key], value[key], child, ctx, errors);
    } else if (schema.additionalProperties !== undefined) {
      if (schema.additionalProperties === false) {
        errors.push(err(child, "additionalProperties", `property "${key}" is not allowed`));
      } else {
        validateNode(schema.additionalProperties, value[key], child, ctx, errors);
      }
    }
  }
}

function collect(schema, value, path, ctx) {
  const errors = [];
  validateNode(schema, value, path, ctx, errors);
  return errors;
}

function firstOf(branches) {
  return branches
    .map((b, i) => `[${i}] ${b[0] ? `${b[0].path || "/"} ${b[0].message}` : "?"}`)
    .join("; ");
}

function resolveRef(root, ref) {
  if (ref === "#") return root;
  if (!ref.startsWith("#/")) throw new Error(`unsupported $ref ${ref}`);
  let node = root;
  for (const raw of ref.slice(2).split("/")) {
    const key = raw.replace(/~1/g, "/").replace(/~0/g, "~");
    if (!isPlainObject(node) || !Object.prototype.hasOwnProperty.call(node, key)) {
      throw new Error(`$ref ${ref} does not resolve`);
    }
    node = node[key];
  }
  return node;
}

/**
 * Call `fn` on every `$ref` in a SCHEMA position. Only subschema keywords are walked,
 * so the data under `enum` / `const` / `default` / `examples` is skipped, while a
 * property that merely has one of those NAMES (`properties: {enum: {...}}`) is a
 * schema like any other and is walked.
 */
function forEachRef(schema, fn) {
  if (!isPlainObject(schema)) return;
  if (typeof schema.$ref === "string") fn(schema.$ref);
  for (const key of ["properties", "$defs"]) {
    if (isPlainObject(schema[key]))
      for (const sub of Object.values(schema[key])) forEachRef(sub, fn);
  }
  for (const key of [
    "additionalProperties",
    "propertyNames",
    "items",
    "not",
    "if",
    "then",
    "else",
  ]) {
    if (schema[key] !== undefined) forEachRef(schema[key], fn);
  }
  for (const key of ["allOf", "anyOf", "oneOf"]) {
    if (Array.isArray(schema[key])) for (const sub of schema[key]) forEachRef(sub, fn);
  }
}

function hasType(value, type) {
  switch (type) {
    case "null":
      return value === null;
    case "boolean":
      return typeof value === "boolean";
    case "object":
      return isPlainObject(value);
    case "array":
      return Array.isArray(value);
    case "number":
      return typeof value === "number" && Number.isFinite(value);
    case "integer":
      return Number.isInteger(value);
    case "string":
      return typeof value === "string";
    default:
      return false;
  }
}

function isPlainObject(v) {
  return v !== null && typeof v === "object" && !Array.isArray(v);
}

/** JSON-value deep equality (2020-12: 1 and 1.0 are equal; key order is irrelevant). */
export function deepEqual(a, b) {
  if (a === b) return true;
  if (typeof a !== typeof b || a === null || b === null) return false;
  if (Array.isArray(a)) {
    return Array.isArray(b) && a.length === b.length && a.every((x, i) => deepEqual(x, b[i]));
  }
  if (typeof a === "object") {
    if (Array.isArray(b)) return false;
    const ka = Object.keys(a);
    const kb = Object.keys(b);
    return (
      ka.length === kb.length &&
      ka.every((k) => Object.prototype.hasOwnProperty.call(b, k) && deepEqual(a[k], b[k]))
    );
  }
  return false;
}

function escapePointer(key) {
  return String(key).replace(/~/g, "~0").replace(/\//g, "~1");
}

function err(path, keyword, message) {
  return { path, keyword, message };
}

function describe(value) {
  if (value === null) return "null";
  if (Array.isArray(value)) return "array";
  if (Number.isInteger(value)) return "integer";
  return typeof value;
}

function short(value) {
  const text = JSON.stringify(value);
  return text.length > 80 ? `${text.slice(0, 77)}...` : text;
}

// ---------------------------------------------------------------------------
// Results files (`termhost-bench/v1`)
// ---------------------------------------------------------------------------

let resultSchemaCache = null;
let resultValidatorCache = null;

/** The committed result schema, parsed once. */
export function loadResultSchema() {
  if (!resultSchemaCache) resultSchemaCache = JSON.parse(readFileSync(RESULT_SCHEMA_PATH, "utf8"));
  return resultSchemaCache;
}

/** Percentile statistics ordered low to high, for the monotonicity check. */
const ORDERED_STATS = ["min", "p50", "p95", "p99", "max"];

/**
 * Validate a parsed results document: the schema first, then the cross-field rules
 * a schema cannot state (percentile order, mean within [min, max], coverage, scenario
 * consistency, duplicate rows, a measured degradation_* whose own base KPI is measured
 * and backed by an N = 1 row of the same setup, variant, build role and arm with that KPI
 * measured, and a slope over the N ladder counting no more N levels than the file holds).
 * Semantic checks run only on a schema-valid document.
 *
 * @param {unknown} doc
 * @param {{schema?: object}} [opts] defaults to the committed result-schema.json
 * @returns {{valid: boolean, errors: {path: string, keyword: string, message: string}[]}}
 */
export function validateResults(doc, { schema } = {}) {
  let validator;
  if (schema) {
    validator = compileSchema(schema);
  } else {
    if (!resultValidatorCache) resultValidatorCache = compileSchema(loadResultSchema());
    validator = resultValidatorCache;
  }
  const first = validator(doc);
  if (!first.valid) return first;
  const errors = semanticErrors(doc);
  return { valid: errors.length === 0, errors };
}

function semanticErrors(doc) {
  const errors = [];
  const seen = new Map();
  doc.rows.forEach((row, r) => {
    const at = `/rows/${r}`;
    const { scenario, setup } = row;
    if (scenario.run_index >= scenario.run_count) {
      errors.push(
        err(
          `${at}/scenario/run_index`,
          "semantic",
          `run_index ${scenario.run_index} >= run_count ${scenario.run_count}`,
        ),
      );
    }
    if (scenario.v_target > scenario.n) {
      errors.push(
        err(
          `${at}/scenario/v_target`,
          "semantic",
          `v_target ${scenario.v_target} > n ${scenario.n}`,
        ),
      );
    }
    if (Number.isInteger(scenario.v_actual) && scenario.v_actual > scenario.n) {
      errors.push(
        err(
          `${at}/scenario/v_actual`,
          "semantic",
          `v_actual ${scenario.v_actual} > n ${scenario.n}`,
        ),
      );
    }
    const role = isPlainObject(setup.build) && setup.build.role ? setup.build.role : "-";
    const key = [setup.id, setup.variant, role, scenario.n, scenario.arm, scenario.run_index].join(
      "|",
    );
    if (seen.has(key)) {
      errors.push(
        err(
          at,
          "semantic",
          `duplicate row: same setup, variant, build role, n, arm and run_index as /rows/${seen.get(key)}`,
        ),
      );
    } else {
      seen.set(key, r);
    }
    for (const [id, kpi] of Object.entries(row.kpis)) kpiSemantics(`${at}/kpis/${id}`, kpi, errors);
  });
  errors.push(...baselineErrors(doc));
  errors.push(...ladderErrors(doc));
  return errors;
}

const DEGRADATIONS = KPI_CATALOG.filter((k) => k.degradation_of);

/**
 * One subject at one arm: setup, variant, build role (A/B builds never share a baseline)
 * and arm. Degradation baselines and slopes over the N ladder are computed within one.
 *
 * @param {{setup: object, scenario: object}} row
 * @returns {string}
 */
export function subjectKey(row) {
  const role = isPlainObject(row.setup.build) && row.setup.build.role ? row.setup.build.role : "-";
  return [row.setup.id, row.setup.variant, role, row.scenario.arm].join("|");
}

/**
 * A measured degradation_* is a ratio of two measured values: the row's own base KPI (the
 * catalog's degradation_of, the numerator) and the same KPI on an N = 1 row for the same
 * subject and arm (the denominator). An N = 1 row is itself the baseline, so a measured
 * degradation there is a contradiction (it is not_applicable(baseline_level)).
 */
function baselineErrors(doc) {
  const errors = [];
  const baselines = new Map();
  for (const row of doc.rows) {
    if (row.scenario.n !== 1) continue;
    const key = subjectKey(row);
    if (!baselines.has(key)) baselines.set(key, []);
    baselines.get(key).push(row);
  }
  doc.rows.forEach((row, r) => {
    for (const d of DEGRADATIONS) {
      if (row.kpis[d.id]?.state !== "measured") continue;
      const at = `/rows/${r}/kpis/${d.id}`;
      if (row.scenario.n === 1) {
        errors.push(
          err(at, "semantic", `a measured ${d.id} on an N = 1 row, which is the baseline itself`),
        );
        continue;
      }
      if (row.kpis[d.degradation_of.kpi]?.state !== "measured") {
        errors.push(
          err(
            at,
            "semantic",
            `a measured ${d.id} on a row whose own ${d.degradation_of.kpi} is not measured`,
          ),
        );
        continue;
      }
      const base = (baselines.get(subjectKey(row)) ?? []).filter(
        (b) => b.kpis[d.degradation_of.kpi]?.state === "measured",
      );
      if (base.length === 0) {
        errors.push(
          err(
            at,
            "semantic",
            `measured ${d.id} has no N = 1 row for the same setup, variant, build role and arm with a measured ${d.degradation_of.kpi}`,
          ),
        );
      }
    }
  });
  return errors;
}

const LADDER_KPIS = KPI_CATALOG.filter((k) => k.needs.includes("n_ladder"));

/**
 * A KPI over the N ladder (memory_mb_per_session) is computed within one file, so the N
 * levels it counts (its n, measured or not) cannot exceed the distinct N of the file's
 * rows for the same subject and arm whose base KPI (derived_from) is measured.
 */
function ladderErrors(doc) {
  const levels = new Map();
  for (const row of doc.rows) {
    for (const k of LADDER_KPIS) {
      const base = k.derived_from[0];
      if (row.kpis[base]?.state !== "measured") continue;
      const key = `${subjectKey(row)}|${base}`;
      if (!levels.has(key)) levels.set(key, new Set());
      levels.get(key).add(row.scenario.n);
    }
  }
  const errors = [];
  doc.rows.forEach((row, r) => {
    for (const k of LADDER_KPIS) {
      const value = row.kpis[k.id];
      if (typeof value?.n !== "number") continue;
      const base = k.derived_from[0];
      const have = levels.get(`${subjectKey(row)}|${base}`)?.size ?? 0;
      if (value.n > have) {
        errors.push(
          err(
            `/rows/${r}/kpis/${k.id}`,
            "semantic",
            `${k.id} counts ${value.n} N levels; this file holds ${have} for the same setup, variant, build role and arm with a measured ${base}`,
          ),
        );
      }
    }
  });
  return errors;
}

function kpiSemantics(at, kpi, errors) {
  if (kpi.coverage && kpi.coverage.observed > kpi.coverage.expected) {
    errors.push(
      err(
        `${at}/coverage`,
        "semantic",
        `observed ${kpi.coverage.observed} > expected ${kpi.coverage.expected}`,
      ),
    );
  }
  if (kpi.state !== "measured" || !kpi.stats) return;
  const present = ORDERED_STATS.filter((s) => s in kpi.stats);
  for (let i = 1; i < present.length; i += 1) {
    if (kpi.stats[present[i - 1]] > kpi.stats[present[i]]) {
      errors.push(
        err(
          `${at}/stats`,
          "semantic",
          `${present[i - 1]} ${kpi.stats[present[i - 1]]} > ${present[i]} ${kpi.stats[present[i]]}`,
        ),
      );
    }
  }
  if ("mean" in kpi.stats) {
    if ("max" in kpi.stats && kpi.stats.mean > kpi.stats.max) {
      errors.push(err(`${at}/stats`, "semantic", `mean ${kpi.stats.mean} > max ${kpi.stats.max}`));
    }
    if ("min" in kpi.stats && kpi.stats.mean < kpi.stats.min) {
      errors.push(err(`${at}/stats`, "semantic", `mean ${kpi.stats.mean} < min ${kpi.stats.min}`));
    }
  }
}

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

/**
 * @param {string[]} files
 * @param {{out?: (s: string) => void, errOut?: (s: string) => void}} [io]
 * @returns {number} exit code
 */
export function runCli(
  files,
  { out = (s) => process.stdout.write(s), errOut = (s) => process.stderr.write(s) } = {},
) {
  if (files.length === 0 || files.includes("--help") || files.includes("-h")) {
    errOut("usage: node scripts/termhost-bench/validate.mjs <results.json> [...]\n");
    return 2;
  }
  let code = 0;
  for (const file of files) {
    let doc;
    try {
      doc = JSON.parse(readFileSync(resolve(file), "utf8"));
    } catch (e) {
      errOut(`${file}: cannot read or parse: ${e.message}\n`);
      return 2;
    }
    const result = validateResults(doc);
    if (result.valid) {
      out(`valid: ${file}\n`);
    } else {
      code = 1;
      out(
        `INVALID: ${file} (${result.errors.length} error${result.errors.length === 1 ? "" : "s"})\n`,
      );
      for (const e of result.errors) out(`  ${e.path || "/"} [${e.keyword}] ${e.message}\n`);
    }
  }
  return code;
}

const invokedDirectly =
  process.argv[1] && resolve(process.argv[1]) === resolve(fileURLToPath(import.meta.url));
if (invokedDirectly) process.exitCode = runCli(process.argv.slice(2));
