/**
 * Type definitions for the wrapper subsystem.
 *
 * Mirrors the shape the runner's `/wrappers/*` HTTP endpoints return, plus the
 * `qontinui.wrapper` manifest field declared by each wrapper's package.json
 * and the action descriptors emitted by `--manifest-only` mode.
 *
 * See `wrapper-runner-integration-plan.md` (Phase 1.2 / 1.3) and
 * `ui-bridge/packages/ui-bridge-wrapper/src/manifest-only.ts` for the source
 * contracts these types match.
 */

/** Transport kinds declared in a wrapper's manifest. */
export type WrapperTransport = "api" | "headless" | "headed" | "live";

/**
 * Lifecycle state reported by `GET /wrappers/:id/status` — mirror of Rust
 * `wrappers::manager::WrapperState` (`#[serde(rename_all = "lowercase")]`).
 *
 * - `stopped`: no subprocess.
 * - `running`: spawned and health-checked; the ONLY state `port_for` routes
 *   dispatches to.
 * - `degraded`: the subprocess is still owned (so it can be stopped) but its
 *   health checks are failing; dispatch will NOT route to it.
 *
 * Use the predicates in `./status` rather than comparing these inline.
 */
export type WrapperState = "stopped" | "running" | "degraded";

/**
 * What the UI shows for a wrapper: the backend's {@link WrapperState}, or
 * `unknown` when the status read itself failed (never a backend state).
 */
export type WrapperStatus = WrapperState | "unknown";

/** Single environment variable a wrapper exposes for credential configuration. */
export interface WrapperEnvVar {
  name: string;
  required?: boolean;
  secret?: boolean;
  description?: string;
}

/**
 * Shape of the `qontinui.wrapper` field in a wrapper's package.json. Forward-
 * compatible: extra fields are preserved verbatim.
 */
export interface WrapperManifest {
  manifestVersion: 1;
  id: string;
  displayName: string;
  description?: string;
  transport: WrapperTransport;
  categories?: string[];
  envVars?: WrapperEnvVar[];
  /** Forward-compatible extras. */
  [extra: string]: unknown;
}

/**
 * paramSchema shape produced by `paramSchemaOf` in the wrapper framework.
 * It's a JSON-Schema subset: an object with `properties` (each property is a
 * tiny schema object with at minimum a `type` field) and an optional
 * `required` array.
 */
export interface ParamSchema {
  type: "object";
  properties?: Record<string, ParamSchemaProperty>;
  required?: string[];
  additionalProperties?: boolean;
  [extra: string]: unknown;
}

export interface ParamSchemaProperty {
  type?: string;
  description?: string;
  enum?: Array<string | number>;
  default?: unknown;
  items?: ParamSchemaProperty;
  properties?: Record<string, ParamSchemaProperty>;
  required?: string[];
  /** Forward-compatible extras (format, minimum, maximum, etc.). */
  [extra: string]: unknown;
}

/** Action descriptor surfaced from the wrapper's --manifest-only output. */
export interface ActionDescriptor {
  id: string;
  paramSchema: ParamSchema;
  exclusive?: boolean;
  description?: string;
}

/** Installed wrapper as returned by `GET /wrappers` and `GET /wrappers/:id`. */
export interface InstalledWrapper {
  id: string;
  packageName: string;
  version: string;
  manifest: WrapperManifest;
  actions: ActionDescriptor[];
  installPath?: string;
  installedAt?: number;
  updatedAt?: number;
}

/**
 * Status payload returned by `GET /wrappers/:id/status` — mirror of Rust
 * `wrappers::manager::WrapperStatus`, which serializes with its snake_case
 * field names (pinned by `status_wire_shape_matches_the_ts_mirror` in
 * `manager.rs`). The lifecycle field is `state`, not `status`: reading
 * `.status` here always yielded `undefined`, so every wrapper rendered as
 * "unknown" with a Start button (plan 2026-08-23-single-source-derived-facts
 * item 10).
 */
export interface WrapperStatusInfo {
  id: string;
  state: WrapperState;
  port: number | null;
  pid: number | null;
  /** Unix epoch milliseconds. */
  started_at_ms: number | null;
  /** Unix epoch milliseconds. */
  last_dispatch_at_ms: number | null;
  consecutive_health_failures: number;
}

/** Payload of `POST /wrappers/:id/start` — the spawned wrapper's port. */
export interface WrapperStartedInfo {
  port: number;
}

/** Credential listing entry — values are NEVER returned from the runner. */
export interface CredentialEntry {
  name: string;
  hasValue: boolean;
  description?: string;
  secret?: boolean;
  required?: boolean;
}

/** Result of a dispatch call. */
export interface DispatchResult<T = unknown> {
  result?: T;
  error?: string;
}

/** Registry entry as listed by `GET /wrappers/registry`. */
export interface RegistryWrapperEntry {
  id: string;
  package: string;
  version?: string;
  displayName: string;
  description?: string;
  categories?: string[];
  author?: { name?: string; url?: string };
  repo?: string;
  license?: string;
  verified?: boolean;
}

/** Top-level shape of registry.json. */
export interface RegistryListing {
  version: number;
  wrappers: RegistryWrapperEntry[];
}
