/**
 * Approach-D Conductor — Phase 5 recipe registry (the consumer seam).
 *
 * THE CONTRACT BETWEEN PROGRAMS. An {@link OrchestrationRecipe} is a pure
 * DATA description of a reusable orchestration: a goal template (with
 * positional `{0}` / named `{url}` slots filled from `/orchestrate`'s
 * trailing args), the default phase composition the conductor should run,
 * and optional hints. Recipes are consumed by the `/orchestrate` command
 * handler (`orchestrateCommand.ts`) — they are NOT commands, and adding one
 * NEVER touches the command resolver/parser or registers a new
 * {@link CommandAction}.
 *
 * ## How a downstream consumer extends this (DATA ONLY)
 *
 * The website→mobile layer-6 consumer plan (and any future vertical) plugs
 * in by adding ONE entry to {@link RECIPES} — no new slash command, no
 * resolver change, no React wiring:
 *
 * ```ts
 * RECIPES["website-to-mobile"] = {
 *   id: "website-to-mobile",
 *   title: "Website → Mobile app",
 *   goal_template:
 *     "Build a qontinui-mobile app that mirrors the website at {0}. ...",
 *   default_phases: ["plan", "implement", "test"],
 * };
 * ```
 *
 * `/orchestrate website-to-mobile https://example.com` then resolves the
 * recipe, fills `{0}` with the url, and POSTs `{goal, recipe, phases}` to the
 * conductor. The recipe id rides along on the wire (`recipe`) so the run row
 * records its provenance and the conductor can branch on it.
 *
 * Free-form `/orchestrate <goal>` (no matching recipe id) bypasses this map
 * entirely and POSTs `{goal}` with no recipe — recipes are an OPT-IN
 * convenience layer over the always-available free-form path.
 */

/**
 * A reusable orchestration template. Pure data — see the module docstring
 * for the consumer-extension contract.
 */
export interface OrchestrationRecipe {
  /**
   * Stable wire id. The first token after `/orchestrate` is matched against
   * this (case-insensitively); on a hit the remaining tokens fill the
   * template. Rides along to the conductor as the `recipe` field.
   */
  id: string;
  /** Display label for help / palette surfaces. */
  title: string;
  /**
   * The seed goal with positional (`{0}`, `{1}`, …) and/or named (`{url}`)
   * slots. `{0}` is the first trailing arg, `{1}` the second, etc.; `{args}`
   * (or `{*}`) expands to the full trailing arg string. Named slots are
   * filled from {@link OrchestrationRecipe.argNames} by position.
   */
  goal_template: string;
  /**
   * Ordered phase names the conductor composes for this recipe, e.g.
   * `["plan", "implement", "test"]`. Posted as `phases`.
   */
  default_phases: string[];
  /**
   * Optional positional → named slot mapping so a template can read
   * `{url}` instead of `{0}`. `argNames[0] === "url"` makes the first
   * trailing arg available as both `{0}` and `{url}`.
   */
  argNames?: string[];
  /**
   * Optional free-form hints carried for the conductor / authoring UI.
   * Intentionally loose (`Record<string, unknown>`) — the contract is the
   * id + template + phases; hints are advisory and never required to
   * resolve a recipe.
   */
  hints?: Record<string, unknown>;
}

/**
 * The recipe data map. Consumers extend by ADDING an entry here — never by
 * adding a command or touching the resolver. Keyed by `id` for O(1) lookup.
 */
export const RECIPES: Record<string, OrchestrationRecipe> = {
  /**
   * STUB consumer recipe proving the data-only extension model. The
   * website→mobile layer-6 plan registers against THIS shape: a goal
   * template taking a `<url>` and the standard plan→implement→test phase
   * composition. Replace/extend the template body when that consumer lands;
   * the contract (id + `{url}` slot + phases) is what downstream code binds
   * to.
   */
  "website-to-mobile": {
    id: "website-to-mobile",
    title: "Website → Mobile app",
    goal_template:
      "Build a qontinui-mobile application that faithfully mirrors the website at {url}. " +
      "Inventory the site's pages and primary user flows, design an equivalent native " +
      "navigation + screen set, implement the screens against the existing mobile app " +
      "scaffold, and verify each flow end-to-end against the source site.",
    default_phases: ["plan", "implement", "test"],
    argNames: ["url"],
    hints: {
      consumer: "layer-6 website-to-mobile",
      sourceKind: "url",
    },
  },
};

/** Result of resolving a recipe id + trailing args into a seeded run. */
export interface ResolvedRecipe {
  recipe: OrchestrationRecipe;
  goal: string;
  phases: string[];
}

/**
 * Fill a recipe's `goal_template` with the trailing args.
 *
 * Slot forms supported:
 *  - `{0}`, `{1}`, … — positional (0-based) trailing args.
 *  - `{args}` / `{*}` — the full trailing arg string (space-joined).
 *  - `{name}` — named, resolved via `argNames` (position of `name`).
 *
 * Unfilled slots collapse to empty string (a recipe invoked with too few
 * args still produces a usable — if terser — goal rather than literal
 * `{0}` text leaking to the conductor). Whitespace is normalized so a
 * collapsed slot doesn't leave a double space.
 */
export function fillTemplate(recipe: OrchestrationRecipe, args: string[]): string {
  const named = new Map<string, string>();
  (recipe.argNames ?? []).forEach((name, i) => {
    named.set(name.toLowerCase(), args[i] ?? "");
  });
  const all = args.join(" ");

  const filled = recipe.goal_template.replace(/\{([^}]+)\}/g, (_match, raw: string) => {
    const key = raw.trim();
    if (key === "*" || key.toLowerCase() === "args") return all;
    if (/^\d+$/.test(key)) return args[Number(key)] ?? "";
    return named.get(key.toLowerCase()) ?? "";
  });

  // Collapse any whitespace runs left by empty slots; trim the ends.
  return filled.replace(/[ \t]{2,}/g, " ").trim();
}

/**
 * Resolve the first token after `/orchestrate` against {@link RECIPES}.
 * Returns the seeded goal + phases when the token names a known recipe, or
 * `null` when it does not (the caller then treats the WHOLE argument as a
 * free-form goal). Lookup is case-insensitive on the recipe id.
 */
export function resolveRecipe(firstToken: string, restArgs: string[]): ResolvedRecipe | null {
  const recipe = RECIPES[firstToken.toLowerCase()];
  if (!recipe) return null;
  return {
    recipe,
    goal: fillTemplate(recipe, restArgs),
    phases: [...recipe.default_phases],
  };
}
