/**
 * Prompt-library `invoke` wrapper + wire types.
 *
 * Mirrors `coordinatorApi.ts`'s `getFleetHealth` pattern: one thin typed
 * wrapper over the Rust `list_prompt_templates` Tauri command
 * (`src-tauri/src/prompt_library.rs`), which fetches coord's
 * `kind=prompt_template` prompt documents through the device-JWT agent
 * door, hydrates + frontmatter-parses them in Rust, and returns typed
 * data in one call — no YAML parsing in the frontend.
 */

import { invoke } from "@tauri-apps/api/core";

/** One option of a `select` parameter — the Rust `SkillParameterOption`. */
export interface PromptParameterOption {
  label: string;
  value: string;
}

/** Conditional-visibility rule — the Rust `ParameterDependency`. */
export interface PromptParameterDependency {
  param: string;
  value: unknown;
}

/**
 * Typed argument schema entry — the existing Rust `SkillParameter` shape
 * (`src-tauri/src/skills/mod.rs`), serialized verbatim.
 */
export interface PromptParameter {
  name: string;
  type: "string" | "number" | "boolean" | "select";
  label: string;
  description: string;
  required: boolean;
  default?: unknown;
  options?: PromptParameterOption[];
  placeholder?: string;
  min?: number;
  max?: number;
  pattern?: string;
  depends_on?: PromptParameterDependency;
}

/** What runs when the prompt's primary affordance is used. */
export type PromptDefaultAction = "spawn" | "insert" | "copy";

/** One fully-hydrated prompt template. */
export interface PromptTemplate {
  /** Stable slug (the document name; also the `/slug` command). */
  name: string;
  title: string;
  description: string;
  category: string;
  icon?: string;
  default_action: PromptDefaultAction;
  /** Document `current_version` — the `v{n}` badge. */
  version: number;
  parameters: PromptParameter[];
  /** Markdown body after frontmatter (the `{{name}}` render target). */
  body: string;
  /** Present when the document's frontmatter failed to parse — the
   *  prompt degrades to parameterless with its raw body. */
  parse_error?: string;
}

/**
 * Structured auth state — the `get_fleet_health` contract:
 * - `ok`           — normal
 * - `unauthorized` — a device token was present but coord rejected it
 * - `unpaired`     — no device token locally (needs pairing)
 */
export interface PromptLibraryAuth {
  state: "ok" | "unauthorized" | "unpaired";
}

export interface PromptLibraryResponse {
  prompts: PromptTemplate[];
  auth: PromptLibraryAuth;
  /** Human-readable explanation when the library is degraded (auth
   *  rejection, coord unreachable, stale cache) — never a silent empty. */
  reason?: string;
}

/** Fetch the prompt library via the runner proxy command. */
export async function listPromptTemplates(): Promise<PromptLibraryResponse> {
  return invoke<PromptLibraryResponse>("list_prompt_templates");
}
