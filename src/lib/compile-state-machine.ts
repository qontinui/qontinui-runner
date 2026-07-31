/**
 * State Machine Compiler — converts spec stateMachine sections into a
 * runtime state machine loadable by the AutomationEngine.
 *
 * Collects all specs with stateMachine sections, flattens states and
 * transitions, converts spec element criteria to ElementQuery format,
 * and validates the result.
 *
 * This compiler also supports the observation-driven provenance pipeline
 * (see `qontinui-dev-notes/session-prompts/state-definition-observation-pipeline.md`).
 * Callers may optionally pass a `discoveryArtifact` produced by
 * `POST /state-discovery/derive` plus an `invalidationState` map; states
 * are then tagged `observed`, `ai-fallback`, or `ai-generated` and the
 * observed/ai-fallback subset is checked for cross-state element
 * uniqueness (the one-state-per-element invariant).
 *
 * Spec states are matched to artifact clusters by an EXCLUSIVE greedy
 * assignment computed once per compile (`assignClustersToStates`): one
 * cluster per state, one state per cluster. Selecting a cluster per state
 * independently — as this module did before — hands the same cluster to
 * every state with the same element count and so violates the very
 * uniqueness invariant above.
 *
 * Exclusivity removes the compiler's SELF-INFLICTED uniqueness violations,
 * not every possible one. Co-occurrence clusters may legitimately share
 * elements with each other, so an artifact can still hand two states two
 * different clusters that name the same element — a genuine
 * authoritative-source disagreement, and one this compiler still throws on.
 */

import type {
  SpecConfig,
  SpecState,
  SpecTransition,
  SpecTransitionAction,
} from "./spec-prompt-builder";
import type {
  StateDefinition,
  TransitionDefinition,
  TransitionAction as EngineTransitionAction,
  ElementQuery,
  PersistedStateMachine,
} from "@qontinui/ui-bridge-auto/runtime";

// ---------------------------------------------------------------------------
// Provenance types and thresholds
// ---------------------------------------------------------------------------

/**
 * Source of a compiled state's element list.
 *
 * - `ai-generated`   — Spec JSON, not yet enough observations to promote.
 * - `observed`       — Elements came from co-occurrence discovery artifact;
 *                      the authoritative source.
 * - `ai-fallback`    — State was previously `observed` but was invalidated
 *                      (e.g. recent refactor); using JSON elements again
 *                      until observations re-accumulate.
 * - `git-supervised` — A git event (commit / branch switch / spec_changed)
 *                      identifies this state as appearing in the on-disk
 *                      version-controlled spec. Weaker than `observed`
 *                      (no runtime co-occurrence evidence) but stronger
 *                      than bare `ai-generated` (the spec is reproducible
 *                      from VCS). See D5 git supervision channel plan.
 */
export type StateProvenance = "ai-generated" | "observed" | "ai-fallback" | "git-supervised";

/**
 * Minimum per-cluster support required to promote to `observed`.
 *
 * "Support" is the discovery adapter's `confidence` field — the only numeric
 * quality signal it emits (`DiscoveredState.to_dict` in
 * `qontinui/src/qontinui/discovery/models.py`). A former `MIN_CONTRAST`
 * threshold was deleted: it gated on a `contrast` field no producer has ever
 * emitted, so `?? 0` made the whole `observed` lane permanently unreachable.
 */
export const MIN_SUPPORT = 0.75;
/** Window during which a recently-invalidated state stays in `ai-fallback`. */
export const INVALIDATION_WINDOW_HOURS = 24;

// ---------------------------------------------------------------------------
// Discovery artifact + invalidation state shapes (kept loose on purpose)
// ---------------------------------------------------------------------------

/**
 * One discovered state from a `state_discovery_artifacts.artifact` blob — a
 * cluster of elements that share a render-set signature.
 *
 * Field names mirror the Python adapter's serialization
 * (`DiscoveredState.to_dict` in `qontinui/src/qontinui/discovery/models.py`),
 * which emits camelCase `stateImageIds` / `screenshotIds`, and are kept in
 * lockstep with the Rust reader's `Cluster` struct
 * (`src-tauri/src/workflow_generation/spec_authoring.rs`).
 *
 * An earlier revision of BOTH readers declared an `elements` key plus
 * `support` / `contrast` / `state_hash` / `last_observed` — none of which the
 * adapter has ever emitted. The Rust half was corrected by PR #886; this is
 * the TypeScript half. Verified against a live row in
 * `project.state_discovery_artifacts`: every cluster carries exactly
 * `{ id, name, stateImageIds, screenshotIds, confidence, metadata }`.
 */
export interface DiscoveryCluster {
  /** Cluster id (deterministic from the discovery side), e.g. `fp_state_ab12`. */
  id: string;
  /** Human-readable name. Falls back to `id` when absent. */
  name?: string | null;
  /**
   * Element ids belonging to this state. The adapter prefixes them with
   * `reg:`; readers strip that back to the bare fingerprint that capture
   * wrote into `co_occurrence_observations`.
   */
  stateImageIds?: string[];
  /**
   * Observation ids this state was seen in — the state's **render-set**, i.e.
   * the signature clustering grouped on. Its length is the state's real
   * observation count.
   */
  screenshotIds?: string[];
  /** Clustering confidence in [0, 1]. The only numeric quality signal emitted. */
  confidence?: number;
  /** Free-form adapter metadata; empty in practice today. */
  metadata?: Record<string, unknown>;
}

/**
 * Shape of the artifact persisted by `POST /state-discovery/derive`.
 * Element ids are opaque `reg:<16-hex>` fingerprints rather than
 * human-readable names, so matching is by element-set shape, not by name.
 */
export interface DiscoveryArtifact {
  id?: string;
  spec_id?: string | null;
  derived_at?: string;
  artifact: {
    states: DiscoveryCluster[];
  };
}

/**
 * Strip the discovery adapter's `reg:` namespace prefix from an element id,
 * yielding the bare fingerprint stored on the observation. Mirrors
 * `project_cluster_to_state` on the Rust side.
 */
function stripElementIdPrefix(elementId: string): string {
  return elementId.startsWith("reg:") ? elementId.slice(4) : elementId;
}

/** Bare (prefix-stripped) element ids for a cluster, deduped and sorted. */
function clusterElementIds(cluster: DiscoveryCluster): string[] {
  const bare = (cluster.stateImageIds ?? []).filter(Boolean).map(stripElementIdPrefix);
  return [...new Set(bare)].sort();
}

/**
 * Optional per-state invalidation metadata keyed by compiled-state ID.
 * Values with `invalidatedAt` inside the 24h window force `ai-fallback`.
 */
export type InvalidationState = Record<string, { invalidatedAt: string }>;

/**
 * Optional per-state git-supervision proposals keyed by compiled-state ID.
 *
 * Populated by the `useGitSupervision` hook from buffered Tauri
 * `git-supervision` events. When a state ID appears in this map AND the
 * artifact match was not strong enough to promote to `observed`, the
 * compile pipeline tags it `git-supervised` instead of falling back to
 * bare `ai-generated`.
 *
 * "Recent" semantics (per Phase 1 of the D5 plan): presence in the map
 * is recency — the upstream ring buffer prunes old entries (default 256
 * events), so the consumer hook only re-emits proposals it has actually
 * received. No second-layer time check is performed here.
 */
export type GitProposalState = Record<string, { proposedAt: string; kind: string }>;

/** Metadata attached to a state describing how its provenance was decided. */
export interface StateProvenanceMeta {
  /** Cluster support — the discovery adapter's `confidence`, in [0, 1]. */
  support?: number;
  /** Size of the matched cluster's render-set (`screenshotIds.length`). */
  observationCount?: number;
  /** Derivation time of the artifact the match came from (ISO-8601). */
  lastObserved?: string;
  /**
   * Only populated when `provenance === "observed"`. Which tier of
   * {@link assignClustersToStates} awarded the cluster:
   *
   * - `element-overlap` — the state and the cluster canonically named the
   *   same element(s). Direct evidence; see `sharedElementCount`.
   * - `size-proximity`  — no shared element; the cluster was merely the
   *   right shape. A bounded GUESS; see `sizeDifference`.
   *
   * This distinction has to be on the record. Both tiers produce an
   * identical-looking `observed` state whose spec-authored elements have been
   * replaced by artifact fingerprints, so without it a consumer cannot tell a
   * promotion backed by evidence from one backed by a size coincidence — and
   * on a real opaque-digest artifact the size tier is by far the common case.
   */
  matchTier?: "element-overlap" | "size-proximity";
  /** Elements the state and cluster shared. Only for `element-overlap`. */
  sharedElementCount?: number;
  /** `|clusterElements − stateElements|`. Only for `size-proximity`. */
  sizeDifference?: number;
  /** Only populated when `provenance === "ai-fallback"`. */
  invalidatedAt?: string;
  /** Only populated when `provenance === "git-supervised"`. ISO-8601. */
  gitProposedAt?: string;
  /**
   * Only populated when `provenance === "git-supervised"`. Carries the
   * triggering git event kind ("commit" / "branch_switch" / "tag" /
   * "spec_changed" / "working_tree_drift") so consumers can audit *why*
   * a state was tagged git-supervised without having to cross-reference
   * the event ring.
   */
  gitProposalKind?: string;
}

/**
 * A compiled state with provenance metadata. Extends the engine's
 * `StateDefinition` (which does not know about provenance) with the two
 * extra fields the pipeline needs.
 */
export type StateDefinitionWithProvenance = StateDefinition & {
  provenance: StateProvenance;
  provenanceMeta?: StateProvenanceMeta;
};

/**
 * `PersistedStateMachine` enriched with provenance-bearing states.
 * Type-compatible with the base shape because provenance fields are
 * additive and consumers that don't care can treat states as plain
 * `StateDefinition`s.
 */
export type PersistedStateMachineWithProvenance = Omit<PersistedStateMachine, "states"> & {
  states: StateDefinitionWithProvenance[];
};

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/** A single cross-state element collision. */
export interface ElementCollisionConflict {
  /** Stable element-query key (same shape as `elementQueryKey`). */
  elementKey: string;
  /** State IDs that all claim this element, with their provenance. */
  states: Array<{ stateId: string; provenance: StateProvenance }>;
}

/**
 * Metadata written to the quarantine store when compilation is rejected.
 * Callers persist this via `persistQuarantinedCompilation`.
 */
export interface QuarantineRecord {
  /** Synthetic identifier for this quarantined compilation (for filename). */
  id: string;
  /** Why the compilation was quarantined. */
  reason: "duplicate-elements";
  /** ISO-8601 timestamp of detection. */
  detectedAt: string;
  /** All cross-state collisions found during compilation. */
  conflicts: ElementCollisionConflict[];
  /** Original spec-set fed into the compiler, for offline inspection. */
  specs: SpecConfig[];
}

/**
 * Shared stats bundle, included on both success and quarantine outcomes so
 * consumers can still surface provenance counts / specs-processed etc.
 */
export interface CompilationStats {
  specsProcessed: number;
  statesCompiled: number;
  transitionsCompiled: number;
  warnings: string[];
  provenanceCounts: Record<StateProvenance, number>;
}

/**
 * Discriminated union. When `compiled === false` the compilation was
 * rejected (quarantined) and the caller MUST NOT load the partial output.
 * Historically this was a plain object with `stateMachine` always present;
 * consumers that destructured `{ stateMachine, stats }` now need to check
 * `compiled` first. See `proj_ui_bridge_sm_element_uniqueness.md`.
 */
export type CompilationResult =
  | {
      compiled: true;
      stateMachine: PersistedStateMachineWithProvenance;
      stats: CompilationStats;
    }
  | {
      compiled: false;
      reason: "quarantined";
      quarantine: QuarantineRecord;
      stats: CompilationStats;
    };

/**
 * A single (specId, config) input to the compiler. The `specId` is the
 * stable identifier provided by the discovered-specs loader and is used
 * to scope per-spec invariants such as element uniqueness.
 */
export interface SpecCompilerInput {
  specId: string;
  config: SpecConfig;
}

/**
 * Compile all spec stateMachine sections into a single runtime state machine.
 *
 * Element uniqueness is enforced **per-spec** — the same element across two
 * specs is allowed; the same element across two states within one spec is a
 * quarantine. The disambiguator is the loader-provided `specId`.
 *
 * @param inputs              Authored specs paired with their stable
 *                            registry `specId`. The `specId` scopes the
 *                            element-uniqueness check.
 * @param discoveryArtifact   Optional output of `/state-discovery/derive`.
 *                            When provided, states whose element-set
 *                            overlaps a cluster with sufficient support
 *                            are promoted to `observed`.
 * @param invalidationState   Optional map of compiled-state-id → invalidation
 *                            timestamp. States invalidated within
 *                            `INVALIDATION_WINDOW_HOURS` force `ai-fallback`.
 * @param gitProposals        Optional map of compiled-state-id → git
 *                            supervision proposal metadata. States named
 *                            here without a strong-enough observed match
 *                            are tagged `git-supervised` instead of
 *                            `ai-generated`. See [D5 plan Phase 1 §3.3].
 *
 * @throws Every *ordinary* rejection is returned as `{ compiled: false }`;
 *         this function has exactly ONE non-union exit. When two
 *         `observed`/`ai-fallback` states inside one spec claim the same
 *         element, two authoritative sources disagree — a hard bug, not bad
 *         input — and an `Error` is thrown synchronously so it surfaces
 *         immediately instead of being buried in a quarantine record. Every
 *         call site must guard for it.
 */
export function compileStateMachineFromSpecs(
  inputs: SpecCompilerInput[],
  discoveryArtifact?: DiscoveryArtifact,
  invalidationState?: InvalidationState,
  gitProposals?: GitProposalState,
): CompilationResult {
  // One clock for the whole compile. Both the invalidation window and the
  // emitted timestamps read it, so a state cannot be judged "not invalidated"
  // by the assignment pre-pass and "invalidated" by the promotion a
  // millisecond later. Deliberate side effect: the state machine's
  // `createdAt` / `updatedAt` are now compile-START rather than compile-end.
  const now = Date.now();
  const warnings: string[] = [];
  const allStates: StateDefinitionWithProvenance[] = [];
  const allTransitions: TransitionDefinition[] = [];
  const seenStateIds = new Set<string>();
  // Track which spec each compiled state belongs to so the uniqueness
  // check can bucket per-spec rather than globally.
  const stateToSpec = new Map<string, string>();
  const provenanceCounts: Record<StateProvenance, number> = {
    "ai-generated": 0,
    observed: 0,
    "ai-fallback": 0,
    "git-supervised": 0,
  };
  let specsProcessed = 0;

  // Bare SpecConfig list, used by orphan-reference checks and preserved
  // on the quarantine record for offline inspection.
  const specs: SpecConfig[] = inputs.map((i) => i.config);

  // Exclusive spec-state → cluster assignment, computed ONCE for the whole
  // compile. This cannot be done per state: "at most one state per cluster"
  // is a property of the whole (state, cluster) bipartite set, so a per-state
  // call can only ever re-derive the same locally-best answer and hand the
  // same cluster to every state that shares an element count. See
  // {@link assignClustersToStates}.
  const clusterAssignment = discoveryArtifact
    ? assignClustersToStates(inputs, discoveryArtifact, invalidationState, now)
    : undefined;

  for (const input of inputs) {
    const spec = input.config;
    if (!spec.stateMachine?.states?.length) continue;
    specsProcessed++;

    for (const specState of spec.stateMachine.states) {
      // Validate unique state ID
      if (seenStateIds.has(specState.id)) {
        warnings.push(`Duplicate state ID: "${specState.id}" — skipped`);
        continue;
      }
      seenStateIds.add(specState.id);
      stateToSpec.set(specState.id, input.specId);

      // Compile base state then decide its provenance.
      const baseState = convertState(specState);
      const promotion = choosePromotion(
        specState.id,
        clusterAssignment?.get(specState.id),
        discoveryArtifact,
        invalidationState,
        gitProposals,
        now,
      );

      const compiledState: StateDefinitionWithProvenance = {
        ...baseState,
        requiredElements: promotion.elements ?? baseState.requiredElements,
        provenance: promotion.provenance,
      };
      if (promotion.meta) compiledState.provenanceMeta = promotion.meta;

      provenanceCounts[promotion.provenance]++;
      allStates.push(compiledState);

      // Convert transitions owned by this state
      for (const specTransition of specState.transitions) {
        const transition = convertTransition(specTransition, specState.id);

        // Validate state references
        const orphanActivate = specTransition.activateStates.filter(
          (id) => !seenStateIds.has(id) && !hasStateInSpecs(id, specs),
        );
        const orphanDeactivate = specTransition.deactivateStates.filter(
          (id) => !seenStateIds.has(id) && !hasStateInSpecs(id, specs),
        );
        if (orphanActivate.length > 0) {
          warnings.push(
            `Transition "${specTransition.id}" activates unknown states: ${orphanActivate.join(", ")}`,
          );
        }
        if (orphanDeactivate.length > 0) {
          warnings.push(
            `Transition "${specTransition.id}" deactivates unknown states: ${orphanDeactivate.join(", ")}`,
          );
        }

        allTransitions.push(transition);
      }
    }
  }

  // Per-spec element uniqueness. Observed/ai-fallback collisions still
  // throw (authoritative claims that disagree = hard bug). Any remaining
  // intra-spec cross-state duplicates — even the ai-generated ones that
  // used to only log a warning-per-duplicate — now quarantine the whole
  // compilation.
  //
  // Note the per-spec scoping: the same "Save" button on TasksPage and
  // ChecksPage is fine because they live in different specs; only two
  // states inside the SAME spec claiming the same element collide.
  //
  // Rationale: per-duplicate warnings were drowned out (48+ per snapshot
  // in testing) and downstream consumers still trusted the partial output.
  // VGA training + runtime state inference both require the
  // one-state-per-element invariant, so producing any result at all
  // when the invariant is violated is worse than producing nothing.
  const conflicts = collectElementCollisions(allStates, stateToSpec);

  const stats: CompilationStats = {
    specsProcessed,
    statesCompiled: allStates.length,
    transitionsCompiled: allTransitions.length,
    warnings,
    provenanceCounts,
  };

  if (conflicts.length > 0) {
    const quarantine: QuarantineRecord = {
      id: makeQuarantineId(),
      reason: "duplicate-elements",
      detectedAt: new Date().toISOString(),
      conflicts,
      specs,
    };
    const duplicatedElementCount = conflicts.length;
    const affectedStateCount = new Set(conflicts.flatMap((c) => c.states.map((s) => s.stateId)))
      .size;
    // One aggregate log line instead of one per duplicate. Callers who want
    // the full list can read `quarantine.conflicts` from the returned record
    // or the persisted file.
    console.warn(
      `[compile-state-machine] quarantined compilation ${quarantine.id}: ` +
        `${duplicatedElementCount} duplicated element(s) across ${affectedStateCount} state(s); ` +
        `downstream consumers will skip this result`,
    );
    stats.warnings.push(
      `quarantined: ${duplicatedElementCount} duplicated element(s) across ${affectedStateCount} state(s) — see quarantine record ${quarantine.id}`,
    );
    return { compiled: false, reason: "quarantined", quarantine, stats };
  }

  return {
    compiled: true,
    stateMachine: {
      version: "1.0.0",
      createdAt: now,
      updatedAt: now,
      states: allStates,
      transitions: allTransitions,
    },
    stats,
  };
}

/**
 * Generate a short quarantine ID. Not cryptographically strong — just
 * unique enough to identify a record in the quarantine directory.
 */
function makeQuarantineId(): string {
  const ts = new Date().toISOString().replace(/[:.]/g, "-");
  const rand = Math.random().toString(36).slice(2, 8);
  return `quarantine-${ts}-${rand}`;
}

// ---------------------------------------------------------------------------
// Converters
// ---------------------------------------------------------------------------

/** Convert a spec state to an engine StateDefinition. */
function convertState(specState: SpecState): StateDefinition {
  return {
    id: specState.id,
    name: specState.name,
    requiredElements: specState.elements.map(convertElementCriteria),
    pathCost: 1.0,
  };
}

/** Convert a spec transition to an engine TransitionDefinition. */
function convertTransition(
  specTransition: SpecTransition,
  owningStateId: string,
): TransitionDefinition {
  return {
    id: specTransition.id,
    name: specTransition.name,
    fromStates: [owningStateId],
    activateStates: specTransition.activateStates,
    exitStates: specTransition.deactivateStates,
    actions: specTransition.process.map(convertAction),
    pathCost: 1.0,
  };
}

/**
 * Convert spec element criteria to an engine ElementQuery.
 *
 * Spec criteria use the assertion target format:
 *   { role, textContent, accessibleName, tagName, dataAttributes, placeholder }
 *
 * Engine queries use:
 *   { role, text, ariaLabel, tagName, attributes, placeholder }
 */
export function convertElementCriteria(criteria: Record<string, unknown>): ElementQuery {
  const query: ElementQuery = {};

  if (criteria.role) query.role = criteria.role as string;
  if (criteria.textContent) query.text = criteria.textContent as string;
  if (criteria.textContains) query.textContains = criteria.textContains as string;
  if (criteria.accessibleName) query.ariaLabel = criteria.accessibleName as string;
  if (criteria.tagName) query.tagName = criteria.tagName as string;
  // placeholder maps to text (input placeholder text)
  if (criteria.placeholder && !query.text) query.text = criteria.placeholder as string;
  if (criteria.id) query.id = criteria.id as string;

  // Convert dataAttributes → attributes
  if (criteria.dataAttributes && typeof criteria.dataAttributes === "object") {
    query.attributes = {};
    for (const [key, value] of Object.entries(criteria.dataAttributes as Record<string, string>)) {
      // Spec uses short keys ("page-id"), engine uses full ("data-page-id")
      const attrKey = key.startsWith("data-") ? key : `data-${key}`;
      query.attributes[attrKey] = value;
    }
  }

  // Pass through any fields already in ElementQuery format
  if (criteria.text && !query.text) query.text = criteria.text as string;
  if (criteria.ariaLabel && !query.ariaLabel) query.ariaLabel = criteria.ariaLabel as string;
  if (criteria.attributes && !query.attributes) {
    query.attributes = criteria.attributes as Record<string, string>;
  }

  return query;
}

/** Convert a spec transition action to an engine TransitionAction. */
function convertAction(specAction: SpecTransitionAction): EngineTransitionAction {
  return {
    target: specAction.target ? convertElementCriteria(specAction.target) : {},
    action: specAction.action,
    params: specAction.params,
    waitAfter: specAction.waitAfter as EngineTransitionAction["waitAfter"],
  };
}

// ---------------------------------------------------------------------------
// Provenance: spec-state ↔ artifact-cluster assignment & promotion
// ---------------------------------------------------------------------------

interface PromotionChoice {
  provenance: StateProvenance;
  /** New element list to use, or `undefined` to keep the spec's list. */
  elements?: ElementQuery[];
  meta?: StateProvenanceMeta;
}

/**
 * A cluster admitted to the assignment pool, with everything derived from it
 * hoisted out of the O(states × clusters) scoring pass. Previously
 * `clusterElementIds` was re-run for every (state, cluster) pair inside a
 * per-state call; it now runs once per cluster, and the decoded queries are
 * reused verbatim as the promoted state's element list.
 */
interface PoolCluster {
  cluster: DiscoveryCluster;
  /**
   * Decoded element queries — exactly what a promotion installs on the state.
   * Non-empty by pool admission, so a promotion can never produce an
   * element-less `observed` state.
   */
  elements: ElementQuery[];
  /**
   * Canonical keys of `elements`, in the SAME form `elementQueryKey`
   * produces for compiled spec elements. Tier-1 overlap is a set
   * intersection over these.
   */
  keys: Set<string>;
}

/** A compiled state eligible to claim a cluster, with its keys hoisted. */
interface PoolState {
  stateId: string;
  /** Number of compiled elements — the size-proximity term. */
  elementCount: number;
  /** Canonical keys of the state's compiled elements. */
  keys: Set<string>;
}

/**
 * Match strength tier. Lower is stronger, and a tier-1 pair ALWAYS outranks
 * every tier-2 pair regardless of the numbers inside them — the two tiers
 * measure different things (shared elements vs. merely similar size) and are
 * deliberately not commensurable.
 *
 * - `1` — structured overlap: the state and the cluster canonically name at
 *   least one of the SAME elements. Direct evidence.
 * - `2` — size proximity: no shared element, but the cluster is roughly the
 *   right shape. A guess, and only ever a fallback.
 */
type MatchTier = 1 | 2;

/** One scored (state, cluster) pair, before greedy assignment. */
interface MatchCandidate {
  stateId: string;
  /** Index into the sorted pool; also the final determinism tie-break. */
  clusterIndex: number;
  tier: MatchTier;
  /** Higher is better WITHIN a tier; never compared across tiers. */
  strength: number;
}

/**
 * One state's winning claim: the cluster it was awarded, plus WHY. The tier
 * and its evidence are carried through to `provenanceMeta` so a `size-proximity`
 * guess is distinguishable from an `element-overlap` match after the fact.
 */
interface ClusterClaim {
  pool: PoolCluster;
  tier: MatchTier;
  /** Shared element keys for tier 1; size difference for tier 2. */
  evidence: number;
}

/**
 * Exclusive assignment: `stateId → ClusterClaim`. A state appears at most
 * once, and no two states map to the same cluster.
 */
type ClusterAssignment = ReadonlyMap<string, ClusterClaim>;

/**
 * Size-proximity admissibility bound, preserved verbatim from the pre-exclusive
 * matcher: a cluster is a plausible fallback match only if its element count is
 * within `max(2, ceil(specCount / 2))` of the state's.
 *
 * The bound matters MORE under exclusive assignment, not less. Without it,
 * greedy assignment degenerates into "everyone gets whatever is left": the last
 * unassigned state would take a wildly wrong-sized cluster simply because it was
 * the only one still free, and be promoted to `observed` on that basis. Falling
 * back to `ai-generated` is strictly better than a confidently wrong `observed`.
 */
function sizeProximityTolerance(elementCount: number): number {
  return Math.max(2, Math.ceil(elementCount / 2));
}

/**
 * Total order over candidates. Every field is compared, so the sort is a true
 * total order and does not depend on `Array.prototype.sort` stability.
 *
 * Order: stronger tier, then stronger score within the tier, then **state id
 * ascending**, then cluster (pool index, which is cluster id ascending).
 *
 * The id-ascending tie-breaks are the determinism contract, and are not
 * decorative: two spec states with the same element count produce candidates
 * of identical tier AND identical strength for every cluster, so without a
 * deterministic tie-break the winner would be decided by input order or by
 * engine sort internals — and the same specs would compile to different
 * observed element sets on different runs.
 *
 * Comparison is by `<` on the raw strings, NOT `localeCompare`: the order has
 * to be identical on every machine that compiles the same specs, and
 * `localeCompare` is locale-sensitive.
 *
 * The id-ascending rule is borrowed from the sibling Rust reader's
 * determinism contract (`load_and_project_skeleton`,
 * src-tauri/src/workflow_generation/spec_authoring.rs). Note that is a
 * borrowed convention, not a shared contract: that function projects clusters
 * 1:1 into states and does no state↔cluster matching, so there is nothing
 * there to keep in lockstep with this — changing one does not oblige changing
 * the other.
 */
function compareCandidates(a: MatchCandidate, b: MatchCandidate): number {
  if (a.tier !== b.tier) return a.tier - b.tier;
  if (a.strength !== b.strength) return b.strength - a.strength;
  if (a.stateId !== b.stateId) return a.stateId < b.stateId ? -1 : 1;
  return a.clusterIndex - b.clusterIndex;
}

/**
 * Build the pool of clusters that could actually promote a state, sorted by
 * cluster id ascending so pool indices are a deterministic tie-break.
 *
 * Two filters, both of which exist because an exclusive claim now has a COST —
 * a cluster one state takes is a cluster no other state can have:
 *
 * 1. `confidence < MIN_SUPPORT` — such a cluster can never promote anything
 *    (the support gate would reject it), so letting it consume an exclusive
 *    claim is a pure loss: it would block the claiming state from taking a
 *    promotable cluster while promoting nobody. Before exclusivity this was
 *    a harmless post-hoc gate; now it has to be a pre-filter.
 * 2. zero decoded elements — an element-less cluster promotes a state to
 *    `observed` with nothing to match on. Note the count is taken AFTER
 *    decoding, not from the raw id list: `["reg:"]` is one raw id that
 *    decodes to zero queries.
 */
function buildClusterPool(artifact: DiscoveryArtifact): PoolCluster[] {
  const clusters = artifact.artifact?.states ?? [];
  const pool: PoolCluster[] = [];
  for (const cluster of clusters) {
    // Negated ACCEPT rather than a REJECT comparison, deliberately: `<` and
    // `>=` are not complements under NaN, and a cast-in artifact can carry
    // one. Written as `< MIN_SUPPORT` a NaN confidence would sail through the
    // pool and be persisted as `provenanceMeta.support: NaN`.
    if (!((cluster.confidence ?? 0) >= MIN_SUPPORT)) continue;
    const elements = artifactElementsToQueries(clusterElementIds(cluster));
    if (elements.length === 0) continue;
    const keys = new Set(elements.map(elementQueryKey).filter((k) => k.length > 0));
    pool.push({ cluster, elements, keys });
  }
  pool.sort((a, b) => (a.cluster.id < b.cluster.id ? -1 : a.cluster.id > b.cluster.id ? 1 : 0));
  return pool;
}

/**
 * Collect the states that may claim a cluster, in compile order.
 *
 * Mirrors the main loop's two skip rules exactly, because assigning a cluster
 * to a state the compile will not promote wastes the cluster:
 *   - duplicate state ids: first occurrence wins, same as `seenStateIds`;
 *   - states under an active invalidation: priority 1 forces `ai-fallback`
 *     regardless of any artifact match, so they must not hold a claim.
 */
function collectPoolStates(
  inputs: SpecCompilerInput[],
  invalidationState: InvalidationState | undefined,
  now: number,
): PoolState[] {
  const states: PoolState[] = [];
  const seen = new Set<string>();
  for (const input of inputs) {
    for (const specState of input.config.stateMachine?.states ?? []) {
      if (seen.has(specState.id)) continue;
      seen.add(specState.id);
      if (activeInvalidation(specState.id, invalidationState, now)) continue;
      const elements = specState.elements.map(convertElementCriteria);
      if (elements.length === 0) continue;
      states.push({
        stateId: specState.id,
        elementCount: elements.length,
        keys: new Set(elements.map(elementQueryKey).filter((k) => k.length > 0)),
      });
    }
  }
  return states;
}

/** Number of canonical element keys two sets share. */
function keyOverlap(a: Set<string>, b: Set<string>): number {
  // Iterate the smaller set; `Set.has` is O(1).
  const [small, large] = a.size <= b.size ? [a, b] : [b, a];
  let overlap = 0;
  for (const key of small) {
    if (large.has(key)) overlap++;
  }
  return overlap;
}

/**
 * Greedy best-match-per-cluster assignment.
 *
 * Every artifact cluster may be claimed by at most ONE spec state, and every
 * spec state matches at most one cluster.
 *
 * ## Why this has to be exclusive
 *
 * The predecessor picked a cluster per spec state INDEPENDENTLY, so two states
 * with the same element count both selected the same cluster, were both
 * promoted to `observed`, and were handed that cluster's identical fingerprint
 * set as their element list. `collectElementCollisions` then correctly saw two
 * authoritative states claiming the same elements and THREW. That is not an
 * edge case: measured over the real spec corpus, 64 of 67 specs have at least
 * two states sharing an element count. The lane stayed green only because every
 * caller passed `undefined` for the artifact.
 *
 * ## Algorithm
 *
 * 1. Score every (state, cluster) pair once — see {@link MatchTier}.
 * 2. Sort all candidates into a deterministic total order
 *    ({@link compareCandidates}).
 * 3. Walk them in order, taking a pair whenever neither its state nor its
 *    cluster is already claimed.
 *
 * Greedy on a total order is deterministic and O(n log n) in the candidate
 * count, but it is not optimal, and the cost is cardinality rather than just
 * score: a state can be starved out of the ONLY cluster it was admissible
 * for by a state that had alternatives. (State `x` admits only `c10`; state
 * `y` admits `c10` at diff 0 and `c12` at diff 2; greedy gives `y` → `c10`
 * and leaves `x` unmatched, where `x` → `c10`, `y` → `c12` would promote
 * both.) Maximising matched pairs would need Hungarian-style matching. The
 * trade is deliberate: the scores are heuristics over noisy signals, and a
 * stable, explainable assignment is worth more than an optimal one over
 * weights that are themselves guesses — an unmatched state falls back to its
 * AI-authored elements, which is a safe outcome, not a broken one.
 *
 * A state that loses every cluster it was admissible for simply gets no match
 * and falls through to `git-supervised` / `ai-generated`. That is the intended
 * outcome: it is better for the loser to keep its AI-authored elements than to
 * be promoted onto a cluster another state has a stronger claim to.
 */
function assignClustersToStates(
  inputs: SpecCompilerInput[],
  artifact: DiscoveryArtifact,
  invalidationState: InvalidationState | undefined,
  now: number,
): ClusterAssignment {
  const assignment = new Map<string, ClusterClaim>();
  const pool = buildClusterPool(artifact);
  if (pool.length === 0) return assignment;
  const states = collectPoolStates(inputs, invalidationState, now);
  if (states.length === 0) return assignment;

  const candidates: MatchCandidate[] = [];
  for (const state of states) {
    for (let clusterIndex = 0; clusterIndex < pool.length; clusterIndex++) {
      const pc = pool[clusterIndex];

      // Tier 1 — structured overlap. Both sides are keyed with
      // `elementQueryKey`, the compiler's own canonical element identity: the
      // very predicate `collectElementCollisions` uses to decide whether two
      // states claim the SAME element. So "this state and this cluster share
      // an element" means exactly what it says, and a tier-1 match is real
      // evidence rather than a coincidence of sizes.
      //
      // This replaces an arm that compared spec `role|name|tag` triples
      // against RAW artifact ids. That arm was documented as dead and was:
      // the adapter emits `reg:`-prefixed digests and structured `id:` /
      // `aria:` / `tag:` / `role:` fingerprints, none of which is ever equal
      // to a bare triple. Keying both sides through the decoder makes the
      // tier REACHABLE — `id:save-button` now matches a spec element carrying
      // that id — which is why it is kept rather than deleted.
      //
      // Reachable, not yet routine: on the artifact rows we have, every
      // element id is a 16-hex digest that decodes opaque and keys as
      // `fp:<digest>`, which no spec element can ever produce. So today this
      // tier fires only for the structured vocabulary the Rust
      // `fingerprint_to_criteria` also emits, and size proximity carries the
      // real corpus. `provenanceMeta.matchTier` records which one won so that
      // ratio is measurable rather than assumed.
      const overlap = keyOverlap(state.keys, pc.keys);
      if (overlap > 0) {
        candidates.push({ stateId: state.stateId, clusterIndex, tier: 1, strength: overlap });
        // No tier-2 twin for a pair that already has a tier-1 candidate: tier
        // 1 always sorts first, so the twin could only ever be reached after
        // this same cluster was already claimed by this same state.
        continue;
      }

      // Tier 2 — size proximity, bounded. Negated so that, as everywhere else,
      // higher `strength` is better.
      const diff = Math.abs(pc.elements.length - state.elementCount);
      if (diff <= sizeProximityTolerance(state.elementCount)) {
        candidates.push({ stateId: state.stateId, clusterIndex, tier: 2, strength: -diff });
      }
    }
  }

  candidates.sort(compareCandidates);

  const claimedStates = new Set<string>();
  const claimedClusters = new Set<number>();
  for (const candidate of candidates) {
    if (claimedStates.has(candidate.stateId)) continue;
    if (claimedClusters.has(candidate.clusterIndex)) continue;
    claimedStates.add(candidate.stateId);
    claimedClusters.add(candidate.clusterIndex);
    assignment.set(candidate.stateId, {
      pool: pool[candidate.clusterIndex],
      tier: candidate.tier,
      // `strength` is negated for tier 2 so that higher is always better;
      // undo that here so the recorded evidence reads as the size difference
      // it actually is.
      evidence: candidate.tier === 1 ? candidate.strength : -candidate.strength,
    });
  }
  return assignment;
}

/**
 * The state's invalidation timestamp if one is active right now, else
 * `undefined`. Shared by the assignment pre-pass (which must not hand a
 * cluster to a state that priority 1 will force to `ai-fallback`) and by
 * `choosePromotion` itself, so the two can never disagree.
 */
function activeInvalidation(
  stateId: string,
  invalidationState: InvalidationState | undefined,
  now: number,
): string | undefined {
  const invalidation = invalidationState?.[stateId];
  if (!invalidation?.invalidatedAt) return undefined;
  const invalidatedMs = Date.parse(invalidation.invalidatedAt);
  if (Number.isNaN(invalidatedMs)) return undefined;
  const ageHours = (now - invalidatedMs) / 3_600_000;
  if (ageHours < 0 || ageHours >= INVALIDATION_WINDOW_HOURS) return undefined;
  return invalidation.invalidatedAt;
}

/**
 * Decide a compiled state's provenance given the discovery artifact and
 * any pending invalidation. See the "Merging" table in the observation
 * pipeline plan for the priority order.
 *
 * Priority order (top wins):
 *   1.   recent invalidation → ai-fallback
 *   2.   this state's exclusive cluster claim → observed
 *   2.5. git supervision proposal (state present in the on-disk spec
 *        AND named in `gitProposals`) → git-supervised
 *   3.   default → ai-generated
 *
 * This function no longer SEARCHES for a cluster — it is handed the one the
 * whole-compile assignment awarded this state, or nothing. Cluster selection
 * is a property of the entire (state, cluster) set and cannot be decided one
 * state at a time; see {@link assignClustersToStates}.
 *
 * The 2.5 lane sits between observed and ai-generated by design: a git
 * proposal proves the state exists in version control (reproducible,
 * auditable) but carries no runtime co-occurrence evidence, so it's
 * weaker than `observed`. See D5 Phase 1 plan §3.3.
 */
function choosePromotion(
  stateId: string,
  assigned: ClusterClaim | undefined,
  artifact: DiscoveryArtifact | undefined,
  invalidationState: InvalidationState | undefined,
  gitProposals: GitProposalState | undefined,
  now: number,
): PromotionChoice {
  // Priority 1: recent invalidation → ai-fallback.
  const invalidatedAt = activeInvalidation(stateId, invalidationState, now);
  if (invalidatedAt) {
    return { provenance: "ai-fallback", meta: { invalidatedAt } };
  }

  // Priority 2: this state's EXCLUSIVE cluster claim → observed.
  //
  // Both gates the old inline code applied here are now pool-admission
  // invariants rather than post-hoc checks (see `buildClusterPool`): an
  // assigned cluster always has `confidence >= MIN_SUPPORT` and always decodes
  // to at least one element. Re-checking them here would be dead code, and
  // dead defensive code is how the `contrast` gate hid an unreachable lane.
  if (assigned) {
    const meta: StateProvenanceMeta = {
      support: assigned.pool.cluster.confidence,
      // The render-set is what the state was *observed* in; the element
      // list is what it is made of. Reporting the latter as an
      // observation count (as this did before) was simply wrong.
      observationCount: assigned.pool.cluster.screenshotIds?.length,
      // Clusters carry no per-state timestamp, so the artifact's own
      // derivation time is the honest recency signal.
      lastObserved: artifact?.derived_at,
    };
    // Record HOW the cluster was won, not just that it was. See
    // `StateProvenanceMeta.matchTier`.
    if (assigned.tier === 1) {
      meta.matchTier = "element-overlap";
      meta.sharedElementCount = assigned.evidence;
    } else {
      meta.matchTier = "size-proximity";
      meta.sizeDifference = assigned.evidence;
    }
    return { provenance: "observed", elements: assigned.pool.elements, meta };
  }

  // Priority 2.5: git supervision proposal. The spec state is present
  // on disk (we wouldn't be compiling it otherwise) and the upstream
  // git-supervision ring named it — that's "sufficient support" per
  // Phase 1. Recency is implicit: the consumer hook prunes old entries
  // before producing the map.
  const proposal = gitProposals?.[stateId];
  if (proposal) {
    return {
      provenance: "git-supervised",
      meta: {
        gitProposedAt: proposal.proposedAt,
        gitProposalKind: proposal.kind,
      },
    };
  }

  // Priority 3: default — keep AI-authored elements.
  return { provenance: "ai-generated" };
}

/**
 * Convert bare (already `reg:`-stripped, see {@link clusterElementIds})
 * artifact fingerprints to engine ElementQuery objects.
 *
 * Decoding priority mirrors the Rust `fingerprint_to_criteria`
 * (`spec_authoring.rs`) so both readers turn the same fingerprint into the
 * same criteria: `id: > aria: > tag:<tag>:<text> > role:`, then the TS-only
 * loose-key shape (`role|name|tagName`), then opaque.
 *
 * Opaque fingerprints are parked in a `data-fingerprint` attribute rather
 * than in `text`: `stable_element_fingerprint` returns a one-way 16-char
 * SHA-256 prefix, so asserting it as visible text would be a criterion that
 * can never match. The attribute keeps a stable handle for the uniqueness
 * pass (see `elementQueryKey`) without inventing a false signal.
 */
function artifactElementsToQueries(elements: string[]): ElementQuery[] {
  const queries: ElementQuery[] = [];
  for (const el of elements) {
    if (!el) continue;
    const q: ElementQuery = {};
    // Every branch guards on a NON-EMPTY payload. A bare `id:` / `aria:` /
    // `role:` would otherwise decode to `{id: ""}` etc., which looks decoded
    // but keys to "" in `elementQueryKey` — silently skipping the uniqueness
    // pass, the exact hole the `id:` key branch exists to close. Empty
    // payloads fall through to the opaque handle instead.
    if (el.startsWith("id:")) {
      if (el.length > 3) q.id = el.slice(3);
    } else if (el.startsWith("aria:")) {
      if (el.length > 5) q.ariaLabel = el.slice(5);
    } else if (el.startsWith("tag:")) {
      // `tag:<tag>:<text>` — split on the first colon only.
      const rest = el.slice(4);
      const sep = rest.indexOf(":");
      const tag = sep >= 0 ? rest.slice(0, sep) : rest;
      const text = sep >= 0 ? rest.slice(sep + 1) : "";
      if (tag) q.tagName = tag;
      if (text) q.text = text;
    } else if (el.startsWith("role:")) {
      if (el.length > 5) q.role = el.slice(5);
    } else {
      const parts = el.split("|");
      if (parts.length === 3) {
        const [role, name, tag] = parts;
        if (role) q.role = role;
        if (name) q.ariaLabel = name;
        if (tag) q.tagName = tag;
      }
    }
    // Anything that decoded to nothing is an opaque digest.
    queries.push(Object.keys(q).length > 0 ? q : { attributes: { "data-fingerprint": el } });
  }
  return queries;
}

// ---------------------------------------------------------------------------
// Cross-state uniqueness check
// ---------------------------------------------------------------------------

/**
 * Collect every intra-spec cross-state element collision. The bucket key
 * combines the owning spec's `specId` with the element key, so two specs
 * that legitimately reuse the same element (e.g. a "Save" button on both
 * TasksPage and ChecksPage) do NOT collide. Only two states inside the
 * SAME spec sharing an element produce a conflict.
 *
 * Observed/ai-fallback double-claims still throw immediately (authoritative
 * sources disagreeing is a hard bug); every other duplicate is returned as
 * a conflict. The caller uses the returned list to decide whether to
 * quarantine the compilation.
 */
function collectElementCollisions(
  states: StateDefinitionWithProvenance[],
  stateToSpec: Map<string, string>,
): ElementCollisionConflict[] {
  // Map `${specId}::${elementKey}` → array of {stateId, provenance}.
  // Per-spec scoping is the whole point of this function — see doc above.
  const seen = new Map<string, Array<{ stateId: string; provenance: StateProvenance }>>();
  for (const state of states) {
    const specId = stateToSpec.get(state.id);
    // States without a registered specId would be a programmer error
    // upstream — fall back to a synthetic bucket so we still detect
    // duplicates among the orphans rather than silently merging them
    // with another spec's elements.
    const scope = specId ?? `__unscoped__:${state.id}`;
    for (const el of state.requiredElements) {
      const key = elementQueryKey(el);
      if (!key) continue;
      const scopedKey = `${scope}::${key}`;
      const bucket = seen.get(scopedKey) ?? [];
      bucket.push({ stateId: state.id, provenance: state.provenance });
      seen.set(scopedKey, bucket);
    }
  }

  const conflicts: ElementCollisionConflict[] = [];
  for (const [scopedKey, bucket] of seen.entries()) {
    if (bucket.length < 2) continue;
    // Deduplicate by stateId — multiple copies of the same element
    // criteria in a single state are fine; cross-state duplicates are not.
    const byState = new Map<string, StateProvenance>();
    for (const entry of bucket) {
      if (!byState.has(entry.stateId)) byState.set(entry.stateId, entry.provenance);
    }
    if (byState.size < 2) continue;

    // Strip the `${specId}::` prefix when surfacing the element key so
    // callers and quarantine consumers see the same key shape they did
    // before per-spec scoping.
    const sepIdx = scopedKey.indexOf("::");
    const elementKey = sepIdx >= 0 ? scopedKey.slice(sepIdx + 2) : scopedKey;

    const observedOrFallback = [...byState.entries()].filter(
      ([, p]) => p === "observed" || p === "ai-fallback",
    );
    if (observedOrFallback.length >= 2) {
      const stateList = observedOrFallback.map(([id, p]) => `${id} (${p})`).join(", ");
      throw new Error(
        `[compile-state-machine] one-state-per-element invariant violated: ` +
          `element "${elementKey}" is claimed by multiple observed/ai-fallback states: ${stateList}`,
      );
    }
    conflicts.push({
      elementKey,
      states: [...byState.entries()].map(([stateId, provenance]) => ({ stateId, provenance })),
    });
  }
  return conflicts;
}

/**
 * Stable string key for an ElementQuery suitable for equality checks in
 * the uniqueness pass. We prefer the opaque fingerprint attribute (exact
 * artifact-level match) before falling back to the loose role/name/tag
 * triple.
 *
 * Exported so static spec validators (see `src/specs/specs.compile.test.ts`)
 * can apply the SAME canonical form the runtime compiler uses — otherwise
 * a static check could pass while the runtime quarantines the same specs.
 */
export function elementQueryKey(q: ElementQuery): string {
  const fp = q.attributes?.["data-fingerprint"];
  if (fp) return `fp:${fp}`;
  const role = q.role ?? "";
  const name = q.ariaLabel ?? q.text ?? q.textContains ?? "";
  const tag = (q.tagName ?? "").toLowerCase();
  // An `id:`-shaped artifact fingerprint decodes to an id-only query. Without
  // this branch such a query keys to "" and is skipped by the uniqueness
  // pass, so two states could both claim it undetected. Deliberately last:
  // when a loose triple exists it stays the key, so this is purely additive
  // over what previously returned "".
  if (!role && !name && !tag) return q.id ? `id:${q.id}` : "";
  return `lk:${role}|${name.slice(0, 60)}|${tag}`;
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/** Check if a state ID exists in any spec's stateMachine section. */
function hasStateInSpecs(stateId: string, specs: SpecConfig[]): boolean {
  return specs.some((spec) => spec.stateMachine?.states?.some((s) => s.id === stateId) ?? false);
}
