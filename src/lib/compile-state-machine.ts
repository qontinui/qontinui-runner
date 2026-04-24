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
} from "@qontinui/ui-bridge-auto";

// ---------------------------------------------------------------------------
// Provenance types and thresholds
// ---------------------------------------------------------------------------

/**
 * Source of a compiled state's element list.
 *
 * - `ai-generated` — Spec JSON, not yet enough observations to promote.
 * - `observed`     — Elements came from co-occurrence discovery artifact;
 *                    the authoritative source.
 * - `ai-fallback`  — State was previously `observed` but was invalidated
 *                    (e.g. recent refactor); using JSON elements again
 *                    until observations re-accumulate.
 */
export type StateProvenance = "ai-generated" | "observed" | "ai-fallback";

/** Minimum per-state support (fraction) required to promote to `observed`. */
export const MIN_SUPPORT = 0.75;
/** Minimum contrast (gap from cross-cluster support) required for promotion. */
export const MIN_CONTRAST = 0.1;
/** Window during which a recently-invalidated state stays in `ai-fallback`. */
export const INVALIDATION_WINDOW_HOURS = 24;

// ---------------------------------------------------------------------------
// Discovery artifact + invalidation state shapes (kept loose on purpose)
// ---------------------------------------------------------------------------

/**
 * Shape of the artifact persisted by `POST /state-discovery/derive`.
 * Kept intentionally loose: Python clustering emits opaque fingerprint
 * strings for `elements` and a `state_hash` rather than a human-readable
 * name, so matching is by element-set shape rather than by name.
 */
export interface DiscoveryArtifact {
  id?: string;
  spec_id?: string | null;
  derived_at?: string;
  artifact: {
    states: Array<{
      state_hash?: string;
      elements?: string[];
      support?: number;
      contrast?: number;
      last_observed?: string;
    }>;
  };
}

/**
 * Optional per-state invalidation metadata keyed by compiled-state ID.
 * Values with `invalidatedAt` inside the 24h window force `ai-fallback`.
 */
export type InvalidationState = Record<string, { invalidatedAt: string }>;

/** Metadata attached to a state describing how its provenance was decided. */
export interface StateProvenanceMeta {
  support?: number;
  contrast?: number;
  observationCount?: number;
  lastObserved?: string;
  /** Only populated when `provenance === "ai-fallback"`. */
  invalidatedAt?: string;
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
 * Compile all spec stateMachine sections into a single runtime state machine.
 *
 * @param specs               Authored spec configs.
 * @param discoveryArtifact   Optional output of `/state-discovery/derive`.
 *                            When provided, states whose element-set
 *                            overlaps a cluster with sufficient support
 *                            and contrast are promoted to `observed`.
 * @param invalidationState   Optional map of compiled-state-id → invalidation
 *                            timestamp. States invalidated within
 *                            `INVALIDATION_WINDOW_HOURS` force `ai-fallback`.
 */
export function compileStateMachineFromSpecs(
  specs: SpecConfig[],
  discoveryArtifact?: DiscoveryArtifact,
  invalidationState?: InvalidationState,
): CompilationResult {
  const warnings: string[] = [];
  const allStates: StateDefinitionWithProvenance[] = [];
  const allTransitions: TransitionDefinition[] = [];
  const seenStateIds = new Set<string>();
  const provenanceCounts: Record<StateProvenance, number> = {
    "ai-generated": 0,
    observed: 0,
    "ai-fallback": 0,
  };
  let specsProcessed = 0;

  for (const spec of specs) {
    if (!spec.stateMachine?.states?.length) continue;
    specsProcessed++;

    for (const specState of spec.stateMachine.states) {
      // Validate unique state ID
      if (seenStateIds.has(specState.id)) {
        warnings.push(`Duplicate state ID: "${specState.id}" — skipped`);
        continue;
      }
      seenStateIds.add(specState.id);

      // Compile base state then decide its provenance.
      const baseState = convertState(specState);
      const promotion = choosePromotion(specState, discoveryArtifact, invalidationState);

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

  // Cross-state element uniqueness. Observed/ai-fallback collisions still
  // throw (authoritative claims that disagree = hard bug). Any remaining
  // cross-state duplicates — even the ai-generated ones that used to only
  // log a warning-per-duplicate — now quarantine the whole compilation.
  //
  // Rationale: per-duplicate warnings were drowned out (48+ per snapshot
  // in testing) and downstream consumers still trusted the partial output.
  // VGA training + runtime state inference both require the
  // one-state-per-element invariant, so producing any result at all
  // when the invariant is violated is worse than producing nothing.
  const conflicts = collectElementCollisions(allStates);

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

  const now = Date.now();
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
function convertElementCriteria(criteria: Record<string, unknown>): ElementQuery {
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
// Provenance: spec-state ↔ artifact-cluster matching & promotion
// ---------------------------------------------------------------------------

interface PromotionChoice {
  provenance: StateProvenance;
  /** New element list to use, or `undefined` to keep the spec's list. */
  elements?: ElementQuery[];
  meta?: StateProvenanceMeta;
}

/**
 * Decide a compiled state's provenance given the discovery artifact and
 * any pending invalidation. See the "Merging" table in the observation
 * pipeline plan for the priority order.
 */
function choosePromotion(
  specState: SpecState,
  artifact: DiscoveryArtifact | undefined,
  invalidationState: InvalidationState | undefined,
): PromotionChoice {
  // Priority 1: recent invalidation → ai-fallback.
  const invalidation = invalidationState?.[specState.id];
  if (invalidation?.invalidatedAt) {
    const invalidatedMs = Date.parse(invalidation.invalidatedAt);
    if (!Number.isNaN(invalidatedMs)) {
      const ageHours = (Date.now() - invalidatedMs) / 3_600_000;
      if (ageHours >= 0 && ageHours < INVALIDATION_WINDOW_HOURS) {
        return {
          provenance: "ai-fallback",
          meta: { invalidatedAt: invalidation.invalidatedAt },
        };
      }
    }
  }

  // Priority 2: artifact match with sufficient support + contrast → observed.
  const match = artifact ? matchArtifactCluster(specState, artifact) : undefined;
  if (
    match &&
    (match.cluster.support ?? 0) >= MIN_SUPPORT &&
    (match.cluster.contrast ?? 0) >= MIN_CONTRAST
  ) {
    const elements = artifactElementsToQueries(match.cluster.elements ?? []);
    if (elements.length > 0) {
      return {
        provenance: "observed",
        elements,
        meta: {
          support: match.cluster.support,
          contrast: match.cluster.contrast,
          observationCount: match.cluster.elements?.length,
          lastObserved: match.cluster.last_observed,
        },
      };
    }
  }

  // Priority 3: default — keep AI-authored elements.
  return { provenance: "ai-generated" };
}

/**
 * Match a spec state to an artifact cluster.
 *
 * The TS side does not have access to the Rust `stable_element_fingerprint`
 * function, and in practice artifact `elements[]` are opaque hash strings
 * produced by clustering. So we cannot compare spec elements to artifact
 * elements directly. The fallback heuristic for this first iteration:
 *
 * 1. Build a loose key for each spec element from `(role, accessibleName
 *    ?? label ?? textContent, tagName)`.
 * 2. If any artifact cluster's `elements[]` contain a string that equals
 *    one of these loose keys, prefer that cluster (rich case — means the
 *    artifact side exposed structured fingerprints, not opaque hashes).
 * 3. Otherwise fall back to cluster-size proximity: pick the cluster whose
 *    element count is closest to the spec state's element count.
 * 4. If the artifact has no states, return undefined and the caller will
 *    choose `ai-generated`.
 */
function matchArtifactCluster(
  specState: SpecState,
  artifact: DiscoveryArtifact,
): { cluster: DiscoveryArtifact["artifact"]["states"][number] } | undefined {
  const clusters = artifact.artifact?.states ?? [];
  if (clusters.length === 0) return undefined;

  const specKeys = specState.elements.map(looseElementKey).filter((k) => k.length > 0);

  // Rich case: artifact elements look like structured keys.
  if (specKeys.length > 0) {
    let best: { cluster: (typeof clusters)[number]; overlap: number } | undefined;
    for (const cluster of clusters) {
      const els = cluster.elements ?? [];
      let overlap = 0;
      for (const key of specKeys) {
        if (els.includes(key)) overlap++;
      }
      if (overlap > 0 && (!best || overlap > best.overlap)) {
        best = { cluster, overlap };
      }
    }
    if (best) return { cluster: best.cluster };
  }

  // Opaque case: match by cluster-size proximity.
  const specCount = specState.elements.length;
  if (specCount === 0) return undefined;
  let closest: { cluster: (typeof clusters)[number]; diff: number } | undefined;
  for (const cluster of clusters) {
    const count = cluster.elements?.length ?? 0;
    if (count === 0) continue;
    const diff = Math.abs(count - specCount);
    if (!closest || diff < closest.diff) {
      closest = { cluster, diff };
    }
  }
  // Require at least rough shape alignment; if no cluster exists within
  // 2× the spec count, treat as unmatched — safer to fall back to AI.
  if (closest && closest.diff <= Math.max(2, Math.ceil(specCount / 2))) {
    return { cluster: closest.cluster };
  }
  return undefined;
}

/**
 * Loose element key mirroring the matching formula specified in the plan:
 *   `${role}|${(accessibleName||label||textContent||'').slice(0,60)}|${tagName?.toLowerCase()||''}`
 * Used both for spec elements and (opportunistically) for artifact
 * elements that happen to carry structured text.
 */
function looseElementKey(criteria: Record<string, unknown>): string {
  const role = (criteria.role as string | undefined) ?? "";
  const name =
    (criteria.accessibleName as string | undefined) ??
    (criteria.label as string | undefined) ??
    (criteria.textContent as string | undefined) ??
    "";
  const tag = ((criteria.tagName as string | undefined) ?? "").toLowerCase();
  return `${role}|${name.slice(0, 60)}|${tag}`;
}

/**
 * Convert artifact element strings to engine ElementQuery objects. If
 * the string matches the loose-key shape (`role|name|tagName`) we recover
 * structured fields; otherwise we stash the opaque fingerprint in an
 * attribute so downstream consumers can still reason about uniqueness.
 */
function artifactElementsToQueries(elements: string[]): ElementQuery[] {
  const queries: ElementQuery[] = [];
  for (const el of elements) {
    if (!el) continue;
    const parts = el.split("|");
    if (parts.length === 3) {
      const [role, name, tag] = parts;
      const q: ElementQuery = {};
      if (role) q.role = role;
      if (name) q.ariaLabel = name;
      if (tag) q.tagName = tag;
      queries.push(q);
    } else {
      // Opaque fingerprint — preserve it in a synthetic attribute for
      // equality checks in enforceElementUniqueness.
      queries.push({ attributes: { "data-fingerprint": el } });
    }
  }
  return queries;
}

// ---------------------------------------------------------------------------
// Cross-state uniqueness check
// ---------------------------------------------------------------------------

/**
 * Collect every cross-state element collision. Observed/ai-fallback
 * double-claims throw immediately (authoritative sources disagreeing is a
 * hard bug); every other duplicate is returned as a conflict. The caller
 * uses the returned list to decide whether to quarantine the compilation.
 */
function collectElementCollisions(
  states: StateDefinitionWithProvenance[],
): ElementCollisionConflict[] {
  // Map element-key → array of {stateId, provenance}.
  const seen = new Map<string, Array<{ stateId: string; provenance: StateProvenance }>>();
  for (const state of states) {
    for (const el of state.requiredElements) {
      const key = elementQueryKey(el);
      if (!key) continue;
      const bucket = seen.get(key) ?? [];
      bucket.push({ stateId: state.id, provenance: state.provenance });
      seen.set(key, bucket);
    }
  }

  const conflicts: ElementCollisionConflict[] = [];
  for (const [key, bucket] of seen.entries()) {
    if (bucket.length < 2) continue;
    // Deduplicate by stateId — multiple copies of the same element
    // criteria in a single state are fine; cross-state duplicates are not.
    const byState = new Map<string, StateProvenance>();
    for (const entry of bucket) {
      if (!byState.has(entry.stateId)) byState.set(entry.stateId, entry.provenance);
    }
    if (byState.size < 2) continue;

    const observedOrFallback = [...byState.entries()].filter(
      ([, p]) => p === "observed" || p === "ai-fallback",
    );
    if (observedOrFallback.length >= 2) {
      const stateList = observedOrFallback.map(([id, p]) => `${id} (${p})`).join(", ");
      throw new Error(
        `[compile-state-machine] one-state-per-element invariant violated: ` +
          `element "${key}" is claimed by multiple observed/ai-fallback states: ${stateList}`,
      );
    }
    conflicts.push({
      elementKey: key,
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
 */
function elementQueryKey(q: ElementQuery): string {
  const fp = q.attributes?.["data-fingerprint"];
  if (fp) return `fp:${fp}`;
  const role = q.role ?? "";
  const name = q.ariaLabel ?? q.text ?? q.textContains ?? "";
  const tag = (q.tagName ?? "").toLowerCase();
  if (!role && !name && !tag) return "";
  return `lk:${role}|${name.slice(0, 60)}|${tag}`;
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/** Check if a state ID exists in any spec's stateMachine section. */
function hasStateInSpecs(stateId: string, specs: SpecConfig[]): boolean {
  return specs.some((spec) => spec.stateMachine?.states?.some((s) => s.id === stateId) ?? false);
}
