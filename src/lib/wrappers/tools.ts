/**
 * Wrapper actions <-> AI-agent tool list.
 *
 * Phase 4 surfaces every installed wrapper's actions to AI agents as tools.
 * To make those tools coexist with built-ins and other MCP tools we encode
 * each action's wrapper-id + action-id pair into a single, regex-safe tool
 * name and keep a side-table for the inverse mapping.
 *
 * Naming format: `wrapper_<wrapperId>__<actionIdUnderscored>`
 *  - The `wrapper_` prefix lets dispatchers cheaply detect wrapper tools.
 *  - The `__` (double underscore) is the wrapper-id / action-id separator.
 *  - Within the action id, dashes are converted to single underscores so the
 *    name conforms to the typical `^[a-zA-Z0-9_]+$` tool-name regex used by
 *    Anthropic, OpenAI, and the MCP spec.
 *
 * Because the `dash -> underscore` rewrite isn't unambiguously reversible
 * (an action id containing a literal underscore vs. a dash both map to `_`),
 * we never *parse* the tool name to recover the action id at dispatch time.
 * Instead we build a {@link WrapperToolRouteTable} at injection and look up
 * the original `(wrapperId, actionId)` pair by tool-name. See {@link
 * buildWrapperTools}.
 */

import type { ActionDescriptor, InstalledWrapper, ParamSchema } from "./types";

/**
 * Tool definition shape compatible with Anthropic's `tool_use` API and the
 * MCP `tools/list` response. Stays intentionally minimal so callers can
 * extend (e.g., add `cache_control`) without us forcing a particular shape.
 */
export interface WrapperToolDefinition {
  name: string;
  description: string;
  input_schema: ParamSchema;
}

/** Reverse-lookup entry — tool name -> originating wrapper + action. */
export interface WrapperToolRoute {
  toolName: string;
  wrapperId: string;
  actionId: string;
  /** Original action descriptor, kept for UI rendering (description, schema). */
  action: ActionDescriptor;
  /** The wrapper this action lives on, for header rendering. */
  wrapper: InstalledWrapper;
}

/**
 * Map of `tool name -> route`. Built once per session at agent start.
 * Lookup at dispatch time is O(1) and never relies on ambiguous parsing.
 */
export type WrapperToolRouteTable = Map<string, WrapperToolRoute>;

const TOOL_PREFIX = "wrapper_";
const SEPARATOR = "__";

/**
 * Encode `(wrapperId, actionId)` into a tool name. Visible for tests and
 * any caller that wants to forward-compute names without building the full
 * route table.
 */
export function encodeToolName(wrapperId: string, actionId: string): string {
  // We don't normalize wrapperId — wrappers control their own ids and are
  // already constrained to slug-shaped strings by the manifest schema.
  // ActionIds may contain `-`, so rewrite those to `_`.
  const safeAction = actionId.replace(/-/g, "_");
  return `${TOOL_PREFIX}${wrapperId}${SEPARATOR}${safeAction}`;
}

/** Returns true if a name was emitted by {@link encodeToolName}. */
export function isWrapperToolName(name: string): boolean {
  return name.startsWith(TOOL_PREFIX) && name.includes(SEPARATOR);
}

/**
 * Build the full agent tool list (one tool per (wrapper, action) pair) and
 * the reverse-mapping route table from a list of installed wrappers.
 *
 * The caller decides which wrappers to include — typically those with
 * status `running` or `idle`, but the filter is the caller's responsibility
 * to keep this module decoupled from status fetching.
 */
export function buildWrapperTools(wrappers: InstalledWrapper[]): {
  tools: WrapperToolDefinition[];
  routes: WrapperToolRouteTable;
} {
  const tools: WrapperToolDefinition[] = [];
  const routes: WrapperToolRouteTable = new Map();

  for (const wrapper of wrappers) {
    const actions = wrapper.actions ?? [];
    for (const action of actions) {
      const toolName = encodeToolName(wrapper.id, action.id);
      // Defend against duplicate ids defensively. Two actions with the same
      // dash-vs-underscore-collapse signature would clobber each other; keep
      // the first and skip the rest so the agent doesn't see a phantom tool.
      if (routes.has(toolName)) continue;

      const description = buildDescription(wrapper, action);

      tools.push({
        name: toolName,
        description,
        // Pass the action's paramSchema through verbatim — it's already a
        // JSONSchema-shaped object per the wrapper framework's contract.
        input_schema: action.paramSchema,
      });
      routes.set(toolName, {
        toolName,
        wrapperId: wrapper.id,
        actionId: action.id,
        action,
        wrapper,
      });
    }
  }

  return { tools, routes };
}

function buildDescription(wrapper: InstalledWrapper, action: ActionDescriptor): string {
  // Prefer the action's own description if set; fall back to a synthetic
  // "<wrapperDisplayName>: <actionId>" so the agent always has *something*.
  if (action.description && action.description.trim().length > 0) {
    return action.description.trim();
  }
  const display = wrapper.manifest?.displayName ?? wrapper.id;
  return `${display}: ${action.id}`;
}

/**
 * Filter wrappers to those whose `status` is acceptable for tool injection.
 * Treats unknown / undefined status as acceptable — the runner may not
 * surface status on the list endpoint, and we'd rather expose a tool whose
 * dispatch lazy-spawns the wrapper than hide it because of a stale field.
 */
export function isWrapperEligibleForTools(wrapper: InstalledWrapper): boolean {
  const status = wrapper.status;
  if (!status) return true;
  return status === "running" || status === "idle" || status === "unknown";
}
