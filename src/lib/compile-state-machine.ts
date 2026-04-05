/**
 * State Machine Compiler — converts spec stateMachine sections into a
 * runtime state machine loadable by the AutomationEngine.
 *
 * Collects all specs with stateMachine sections, flattens states and
 * transitions, converts spec element criteria to ElementQuery format,
 * and validates the result.
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
// Public API
// ---------------------------------------------------------------------------

export interface CompilationResult {
  stateMachine: PersistedStateMachine;
  stats: {
    specsProcessed: number;
    statesCompiled: number;
    transitionsCompiled: number;
    warnings: string[];
  };
}

/**
 * Compile all spec stateMachine sections into a single runtime state machine.
 */
export function compileStateMachineFromSpecs(
  specs: SpecConfig[],
): CompilationResult {
  const warnings: string[] = [];
  const allStates: StateDefinition[] = [];
  const allTransitions: TransitionDefinition[] = [];
  const seenStateIds = new Set<string>();
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

      // Convert state
      allStates.push(convertState(specState));

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

  const now = Date.now();
  return {
    stateMachine: {
      version: "1.0.0",
      createdAt: now,
      updatedAt: now,
      states: allStates,
      transitions: allTransitions,
    },
    stats: {
      specsProcessed,
      statesCompiled: allStates.length,
      transitionsCompiled: allTransitions.length,
      warnings,
    },
  };
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
function convertElementCriteria(
  criteria: Record<string, unknown>,
): ElementQuery {
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
    for (const [key, value] of Object.entries(
      criteria.dataAttributes as Record<string, string>,
    )) {
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
function convertAction(
  specAction: SpecTransitionAction,
): EngineTransitionAction {
  return {
    target: convertElementCriteria(specAction.target),
    action: specAction.action,
    params: specAction.params,
    waitAfter: specAction.waitAfter as EngineTransitionAction["waitAfter"],
  };
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/** Check if a state ID exists in any spec's stateMachine section. */
function hasStateInSpecs(stateId: string, specs: SpecConfig[]): boolean {
  return specs.some(
    (spec) => spec.stateMachine?.states?.some((s) => s.id === stateId) ?? false,
  );
}
