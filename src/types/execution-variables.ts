/**
 * Execution Variables types
 *
 * Types for configuring authentication and custom variables
 * used during API request execution in the Test Orchestrator.
 */

/**
 * Source of authentication for API requests
 */
export type AuthSource = "captured" | "manual" | "environment";

/**
 * Source of a custom variable value
 */
export type VariableSource = "manual" | "environment";

/**
 * A custom variable for execution
 */
export interface CustomVariable {
  /** Variable name (used as {{name}} in substitution) */
  name: string;
  /** Source of the value */
  source: VariableSource;
  /** Manual value (when source is "manual") */
  value?: string;
  /** Environment variable name (when source is "environment") */
  envVar?: string;
  /** Optional description for the variable */
  description?: string;
}

/**
 * Settings for execution variables
 */
export interface ExecutionVariablesSettings {
  /** Authentication source for API requests */
  authSource: AuthSource;
  /** Header name to use for authentication (default: "Authorization") */
  authHeaderName: string;
  /** Manual auth token (when authSource is "manual") */
  authToken?: string;
  /** Environment variable name for auth token (when authSource is "environment") */
  authEnvVar: string;
  /** Custom variables for substitution */
  customVariables: CustomVariable[];
}

/**
 * Status of a resolved variable
 */
export interface ResolvedVariableStatus {
  name: string;
  source: string;
  hasValue: boolean;
  description?: string;
}

/**
 * Resolved execution context with actual values
 */
export interface ResolvedExecutionContext {
  /** Auth source being used */
  authSource: string;
  /** Status of the auth token (not the actual token for security) */
  authTokenStatus: string;
  /** Auth header name */
  authHeaderName: string;
  /** Resolved custom variables status */
  customVariables: ResolvedVariableStatus[];
}

/**
 * Default execution variables settings
 */
export const DEFAULT_EXECUTION_VARIABLES: ExecutionVariablesSettings = {
  authSource: "captured",
  authHeaderName: "Authorization",
  authToken: undefined,
  authEnvVar: "API_AUTH_TOKEN",
  customVariables: [],
};

/**
 * Create a default custom variable
 */
export function createDefaultCustomVariable(): CustomVariable {
  return {
    name: "",
    source: "manual",
    value: "",
    envVar: "",
    description: "",
  };
}

/**
 * Get human-readable label for auth source
 */
export function getAuthSourceLabel(source: AuthSource): string {
  switch (source) {
    case "captured":
      return "Use Captured Headers";
    case "manual":
      return "Manual Token";
    case "environment":
      return "Environment Variable";
    default:
      return source;
  }
}

/**
 * Get description for auth source
 */
export function getAuthSourceDescription(source: AuthSource): string {
  switch (source) {
    case "captured":
      return "Use the authorization headers from the saved API requests. Tokens may expire over time.";
    case "manual":
      return "Enter a static authorization token to use for all requests.";
    case "environment":
      return "Read the authorization token from an environment variable at runtime.";
    default:
      return "";
  }
}
