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

/** Runtime status reported by `GET /wrappers/:id/status`. */
export type WrapperStatus = "running" | "idle" | "error" | "degraded" | "unknown";

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
  /** May be omitted; resolve via `GET /wrappers/:id/status` when needed. */
  status?: WrapperStatus;
  /** Live process port if running. */
  port?: number;
}

/** Status payload returned by `GET /wrappers/:id/status`. */
export interface WrapperStatusInfo {
  status: WrapperStatus;
  port?: number;
  message?: string;
  pid?: number;
  startedAt?: number;
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
