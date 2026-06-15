/**
 * Public surface of the terminal-page command-action registry.
 *
 * Phase 1 plumbing only — no actions registered yet, no resolver tiers
 * wired up. See `./audit.md` for the 12-action target and
 * `plans/2026-05-28-terminal-page-redesign-plan.md` for the full design.
 */

export type {
  CommandAction,
  CommandResult,
  RegistryListener,
  ResolverContext,
} from "./types";

export {
  getAll,
  getById,
  getBySlash,
  register,
  subscribe,
  unregister,
} from "./registry";

export { useCommandAction } from "./useCommandAction";

export { useTerminalCommands } from "./useTerminalCommands";
export type { TerminalCommandsContext } from "./useTerminalCommands";

export { fuzzyScore } from "./fuzzy";
export type { FuzzyMatch } from "./fuzzy";

export { resolve } from "./resolve";
export type { ResolveMatch } from "./resolve";

export { coerceToken, parseArgs, tokenize } from "./parse";

export { matchPattern } from "./patterns";
export type { PatternMatch } from "./patterns";

export { callRegistry } from "./uibridge";

export { getRegistryPaletteActions } from "./palette-projection";
export type { PaletteActionLike } from "./palette-projection";

export { interpretCommand, normalizeInterpretKey } from "./interpret";
export type { InterpretMatch, InterpretOptions } from "./interpret";

export {
  useOrchestrateCommand,
  planOrchestration,
  splitOrchestrateArg,
  TERMINAL_OPEN_PAGE_EVENT,
} from "./orchestrateCommand";
export type { TerminalOpenPageDetail } from "./orchestrateCommand";

export {
  RECIPES,
  resolveRecipe,
  fillTemplate,
} from "./recipes";
export type { OrchestrationRecipe, ResolvedRecipe } from "./recipes";
