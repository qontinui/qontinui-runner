/**
 * Tests for compileStateMachineFromSpecs.
 *
 * Focus: the duplicate-elements quarantine path. Successful compilation
 * paths are exercised indirectly by the rest of the runner; these tests
 * pin two invariants:
 *   1. Element uniqueness is enforced **per-spec**: two states inside the
 *      same spec sharing an element triggers a quarantine.
 *   2. The same element appearing in two different specs is FINE — that's
 *      the whole point of per-spec scoping (e.g. "Save" button on
 *      TasksPage and ChecksPage shouldn't collide).
 */

import { describe, it, expect } from "vitest";
import { compileStateMachineFromSpecs, MIN_SUPPORT } from "./compile-state-machine";
import type { DiscoveryArtifact, DiscoveryCluster } from "./compile-state-machine";
import type { SpecConfig, SpecState } from "./spec-prompt-builder";

/**
 * Build a minimal SpecConfig with one or more states. Each state claims a
 * single button element with the given accessible-name label.
 */
function makeSpec(
  specDescription: string,
  states: Array<{ stateId: string; label: string }>,
): SpecConfig {
  const specStates: SpecState[] = states.map(({ stateId, label }) => ({
    id: stateId,
    name: stateId,
    elements: [
      {
        role: "button",
        accessibleName: label,
        tagName: "button",
      },
    ],
    transitions: [],
  }));
  return {
    version: "1",
    description: specDescription,
    groups: [],
    stateMachine: {
      states: specStates,
    },
  };
}

/** Convenience for the common single-state case. */
function makeSingleStateSpec(specDescription: string, stateId: string, label: string): SpecConfig {
  return makeSpec(specDescription, [{ stateId, label }]);
}

/**
 * Build a spec whose states each claim `elementCount` distinct button
 * elements. Element count is what the opaque cluster-size-proximity arm of
 * `matchArtifactCluster` keys on, so it is the lever these artifact tests
 * pull to steer a state at a specific cluster.
 */
function makeSpecWithElementCounts(
  specDescription: string,
  states: Array<{ stateId: string; elementCount: number }>,
): SpecConfig {
  const specStates: SpecState[] = states.map(({ stateId, elementCount }) => ({
    id: stateId,
    name: stateId,
    elements: Array.from({ length: elementCount }, (_, i) => ({
      role: "button",
      accessibleName: `${stateId}-element-${i}`,
      tagName: "button",
    })),
    transitions: [],
  }));
  return {
    version: "1",
    description: specDescription,
    groups: [],
    stateMachine: { states: specStates },
  };
}

// ---------------------------------------------------------------------------
// Real discovery-artifact fixtures
// ---------------------------------------------------------------------------

/**
 * Clusters below are copied VERBATIM out of a real
 * `project.state_discovery_artifacts.artifact` row on the local canonical
 * Postgres (artifact `d61bf283-6d49-4a53-8d70-a3a63db65a1c`, derived
 * 2026-07-26, window_days=90, 96 clusters). Nothing here is hand-shaped.
 *
 * That matters: a HAND-WRITTEN fixture in the old, wrong shape
 * (`{ elements, support, contrast, state_hash, last_observed }`) is exactly
 * what let the field-name bug survive review in BOTH readers. The Python
 * adapter (`DiscoveredState.to_dict`, qontinui/src/qontinui/discovery/models.py)
 * has only ever emitted `{ id, name, stateImageIds, screenshotIds,
 * confidence, metadata }`, and the Rust `Cluster` struct
 * (src-tauri/src/workflow_generation/spec_authoring.rs, fixed in PR #886)
 * reads exactly those names. Do not "simplify" these fixtures by inventing
 * fields — re-copy from a live row instead.
 *
 * Verified properties relied on by the tests below:
 *   - every element id carries the adapter's `reg:` namespace prefix;
 *   - CLUSTER_8 and CLUSTER_12 have ZERO element ids in common;
 *   - `confidence` is the only numeric quality signal present.
 */
const CLUSTER_8: DiscoveryCluster = {
  id: "fp_state_9435bd238223",
  name: "ee71472921be0054 (8 elements)",
  metadata: {},
  confidence: 0.88,
  screenshotIds: [
    "0616cdde-f48c-487b-b3a3-85a02c1d13fc",
    "22244487-d0a1-477e-b06e-189fb746a84d",
    "405ca15b-dae1-4724-af17-840bb8d56756",
    "4a771955-baaa-4803-9f35-ba0abb3050eb",
    "53eb5a9b-407b-42e2-b485-9974bc23ac8b",
    "61682ccc-3f0f-4e87-9e84-00a48485fc67",
    "73636835-2955-4174-a7c5-c9728bdfa73b",
    "858a2434-3be1-4d46-a4fc-47b470ec0caf",
    "97358724-0198-49ef-a4cd-007eb50145c7",
    "9ecb9a63-6817-46c9-97ca-68895939e17d",
    "a4b71611-85af-40b2-88aa-1a25bddc2192",
    "b005277d-d5d5-4955-acce-3113cf57ce67",
    "be30f8a1-f87a-404f-a335-9ddbfa69d1b6",
    "c5514767-da6f-4698-b46c-6e4dee34ee15",
  ],
  stateImageIds: [
    "reg:1fc090c7a51cc2f7",
    "reg:474fe9de9ec0afa0",
    "reg:4ea236c5b6f6cdf6",
    "reg:947f3b0a439020e0",
    "reg:a8d9d273f563d469",
    "reg:d1a3a917bfde8264",
    "reg:ee71472921be0054",
    "reg:f1c9d3323a34c219",
  ],
};

const CLUSTER_12: DiscoveryCluster = {
  id: "fp_state_d6950e0342f9",
  name: "c903de4e7d0234d3 (12 elements)",
  metadata: {},
  confidence: 0.8290756251918218,
  screenshotIds: [
    "0d43cb6d-30c6-43a0-bc5d-978ee413fd82",
    "3548f81e-438d-4e94-8713-80ce684a08df",
    "a0cc4ba3-b39e-480a-a17d-4730e22f2fcf",
    "b37c94b4-3f34-43ee-90cf-8f238a2c2123",
    "de6f8c61-0097-4eaa-8b32-68f1ccffc45a",
  ],
  stateImageIds: [
    "reg:2cb8fe51659b8838",
    "reg:2cdbd39bcf0340d8",
    "reg:4600966709943788",
    "reg:61e42e6c010ac100",
    "reg:67016769ddda63cd",
    "reg:92bc5420f878857e",
    "reg:a0d22181d1e29247",
    "reg:a9d1a04ca830be28",
    "reg:acf487457099fbfd",
    "reg:c903de4e7d0234d3",
    "reg:d738c06805ef0817",
    "reg:e073f42cc5c5c905",
  ],
};

/** Real cluster whose `confidence` sits BELOW `MIN_SUPPORT` (0.38 < 0.75). */
const CLUSTER_3_LOW_CONFIDENCE: DiscoveryCluster = {
  id: "fp_state_0abb343ad800",
  name: "393bc9604e5d7fd3 (3 elements)",
  metadata: {},
  confidence: 0.3805149978319906,
  screenshotIds: ["5260b270-349e-4e71-b71b-27f9f1be42db"],
  stateImageIds: ["reg:393bc9604e5d7fd3", "reg:7792c1fc3bd23a67", "reg:ee3d2ae6123fa07b"],
};

/** Artifact envelope with the real row's `id` / `derived_at`. */
function makeArtifact(clusters: DiscoveryCluster[]): DiscoveryArtifact {
  return {
    id: "d61bf283-6d49-4a53-8d70-a3a63db65a1c",
    spec_id: null,
    derived_at: "2026-07-26T22:13:52.417609Z",
    artifact: { states: clusters },
  };
}

describe("compileStateMachineFromSpecs", () => {
  it("returns compiled=true with no conflicts for disjoint specs", () => {
    const inputs = [
      { specId: "a", config: makeSingleStateSpec("a", "state-a", "Alpha") },
      { specId: "b", config: makeSingleStateSpec("b", "state-b", "Bravo") },
    ];
    const result = compileStateMachineFromSpecs(inputs);
    expect(result.compiled).toBe(true);
    if (result.compiled) {
      expect(result.stats.statesCompiled).toBe(2);
      expect(result.stateMachine.states).toHaveLength(2);
    }
  });

  it("allows the same element across two different specs", () => {
    // Per-spec scoping means a "Severity" button on the EM spec and a
    // "Severity" button on the Findings spec do NOT collide — different
    // specs are different scopes, even when the elements look identical.
    const inputs = [
      { specId: "em", config: makeSingleStateSpec("em", "em-filtered", "Severity") },
      {
        specId: "findings",
        config: makeSingleStateSpec("findings", "findings-display", "Severity"),
      },
    ];
    const result = compileStateMachineFromSpecs(inputs);
    expect(result.compiled).toBe(true);
    if (result.compiled) {
      expect(result.stats.statesCompiled).toBe(2);
      expect(result.stateMachine.states).toHaveLength(2);
    }
  });

  it("quarantines the compilation when two states inside one spec share an element", () => {
    // Two states in the SAME spec both claim a button with accessibleName
    // "Severity" → one element-key, two state IDs in the same scope →
    // intra-spec collision → quarantine.
    const config = makeSpec("em", [
      { stateId: "em-filtered", label: "Severity" },
      { stateId: "em-summary", label: "Severity" },
    ]);
    const result = compileStateMachineFromSpecs([{ specId: "em", config }]);
    expect(result.compiled).toBe(false);
    if (!result.compiled) {
      expect(result.reason).toBe("quarantined");
      expect(result.quarantine.reason).toBe("duplicate-elements");
      expect(result.quarantine.conflicts).toHaveLength(1);
      const conflict = result.quarantine.conflicts[0];
      expect(conflict.elementKey).toContain("Severity");
      const stateIds = conflict.states.map((s) => s.stateId).sort();
      expect(stateIds).toEqual(["em-filtered", "em-summary"]);
      // Original spec set is preserved so operators can inspect offline.
      expect(result.quarantine.specs).toHaveLength(1);
      // ID is present and non-empty for the filename.
      expect(result.quarantine.id.length).toBeGreaterThan(0);
      expect(result.quarantine.detectedAt).toMatch(/\d{4}-\d{2}-\d{2}T/);
    }
  });

  it("emits exactly one aggregate warning per quarantined compilation", () => {
    const originalWarn = console.warn;
    const calls: string[] = [];
    console.warn = (...args: unknown[]) => {
      calls.push(args.map((a) => (typeof a === "string" ? a : JSON.stringify(a))).join(" "));
    };
    try {
      // Two specs each with their OWN intra-spec duplicate. Under the new
      // per-spec rule that produces 2 conflicts (one per spec), but still
      // exactly one aggregate console.warn call.
      const inputs = [
        {
          specId: "a",
          config: makeSpec("a", [
            { stateId: "sa1", label: "Dup" },
            { stateId: "sa2", label: "Dup" },
          ]),
        },
        {
          specId: "b",
          config: makeSpec("b", [
            { stateId: "sb1", label: "OtherDup" },
            { stateId: "sb2", label: "OtherDup" },
          ]),
        },
      ];
      const result = compileStateMachineFromSpecs(inputs);
      expect(result.compiled).toBe(false);
      if (!result.compiled) {
        expect(result.quarantine.conflicts).toHaveLength(2);
      }
      const quarantineWarnings = calls.filter((c) =>
        c.includes("[compile-state-machine] quarantined compilation"),
      );
      expect(quarantineWarnings).toHaveLength(1);
    } finally {
      console.warn = originalWarn;
    }
  });

  it("still throws when two observed/ai-fallback states inside one spec collide (hard invariant)", () => {
    // The stricter enforcement is preserved: observed/ai-fallback collisions
    // are an authoritative-source disagreement and throw synchronously so
    // the bug is surfaced immediately rather than buried in a quarantine.
    // Per-spec scoping means both observed states must live in the SAME
    // spec to trigger the throw.
    //
    // Both states carry 8 elements, so cluster-size proximity steers BOTH at
    // CLUSTER_8; promotion then replaces their (distinct) spec elements with
    // that cluster's single shared fingerprint set → collision.
    const config = makeSpecWithElementCounts("a", [
      { stateId: "sa", elementCount: 8 },
      { stateId: "sb", elementCount: 8 },
    ]);
    expect(() =>
      compileStateMachineFromSpecs([{ specId: "a", config }], makeArtifact([CLUSTER_8])),
    ).toThrow(/one-state-per-element invariant violated/);
  });
});

describe("discovery-artifact provenance promotion", () => {
  it("promotes a spec state to `observed` from a REAL-shaped artifact", () => {
    // This is the regression the whole change exists for: before the fix the
    // reader looked for `elements` / `support` / `contrast`, none of which the
    // adapter emits, so `matchArtifactCluster` returned undefined for every
    // cluster and NO state could ever reach `observed`.
    const config = makeSpecWithElementCounts("dashboard", [
      { stateId: "dashboard-loaded", elementCount: 8 },
    ]);
    const result = compileStateMachineFromSpecs(
      [{ specId: "dashboard", config }],
      makeArtifact([CLUSTER_8, CLUSTER_12, CLUSTER_3_LOW_CONFIDENCE]),
    );

    expect(result.compiled).toBe(true);
    if (!result.compiled) return;

    const [state] = result.stateMachine.states;
    expect(state.provenance).toBe("observed");
    expect(result.stats.provenanceCounts.observed).toBe(1);

    // Meta reflects the adapter's real field names, not the phantom ones.
    // `support` is sourced from `confidence`; `observationCount` from the
    // render-set (`screenshotIds`), NOT the element list; `lastObserved`
    // from the artifact's own derivation time (clusters carry no timestamp).
    expect(state.provenanceMeta?.support).toBe(0.88);
    expect(state.provenanceMeta?.observationCount).toBe(CLUSTER_8.screenshotIds?.length);
    expect(state.provenanceMeta?.observationCount).toBe(14);
    expect(state.provenanceMeta?.lastObserved).toBe("2026-07-26T22:13:52.417609Z");

    // Elements were replaced by the cluster's fingerprints, with the
    // adapter's `reg:` namespace prefix stripped (mirrors the Rust reader).
    expect(state.requiredElements).toHaveLength(8);
    const fingerprints = state.requiredElements.map((q) => q.attributes?.["data-fingerprint"]);
    expect(fingerprints.every((f) => typeof f === "string" && !f.startsWith("reg:"))).toBe(true);
    expect(fingerprints).toContain("ee71472921be0054");
    expect(fingerprints).toContain("1fc090c7a51cc2f7");
  });

  it("steers each state at the size-closest cluster", () => {
    // Two specs so the (disjoint) observed element sets can't collide on
    // Separate specs mirror how these states really live; CLUSTER_8 and
    // CLUSTER_12 share zero element ids, so nothing here depends on per-spec
    // scoping. The point is purely which cluster each state selects.
    const inputs = [
      {
        specId: "eight",
        config: makeSpecWithElementCounts("eight", [{ stateId: "s8", elementCount: 8 }]),
      },
      {
        specId: "twelve",
        config: makeSpecWithElementCounts("twelve", [{ stateId: "s12", elementCount: 12 }]),
      },
    ];
    const result = compileStateMachineFromSpecs(inputs, makeArtifact([CLUSTER_8, CLUSTER_12]));

    expect(result.compiled).toBe(true);
    if (!result.compiled) return;

    const byId = new Map(result.stateMachine.states.map((s) => [s.id, s]));
    expect(byId.get("s8")?.provenance).toBe("observed");
    expect(byId.get("s8")?.provenanceMeta?.support).toBe(CLUSTER_8.confidence);
    expect(byId.get("s8")?.provenanceMeta?.observationCount).toBe(14);

    expect(byId.get("s12")?.provenance).toBe("observed");
    expect(byId.get("s12")?.provenanceMeta?.support).toBe(CLUSTER_12.confidence);
    expect(byId.get("s12")?.provenanceMeta?.observationCount).toBe(5);
  });

  it("leaves a state `ai-generated` when the matched cluster is below MIN_SUPPORT", () => {
    // Same code path, same real shape — only `confidence` differs. Pins that
    // the support gate is what rejects this state, not a failed match: the
    // 3-element spec state lines up exactly with the 3-element cluster.
    expect(CLUSTER_3_LOW_CONFIDENCE.confidence).toBeLessThan(MIN_SUPPORT);
    const config = makeSpecWithElementCounts("sparse", [
      { stateId: "sparse-state", elementCount: 3 },
    ]);
    const result = compileStateMachineFromSpecs(
      [{ specId: "sparse", config }],
      makeArtifact([CLUSTER_3_LOW_CONFIDENCE]),
    );

    expect(result.compiled).toBe(true);
    if (!result.compiled) return;
    const [state] = result.stateMachine.states;
    expect(state.provenance).toBe("ai-generated");
    expect(state.provenanceMeta).toBeUndefined();
    // Spec-authored elements survive untouched.
    expect(state.requiredElements).toHaveLength(3);
    expect(state.requiredElements[0].ariaLabel).toBe("sparse-state-element-0");
  });

  it("ignores the phantom pre-#886 artifact keys entirely", () => {
    // Guard against a regression back to reading `elements` / `support` /
    // `contrast`. An artifact carrying ONLY those keys has, from the
    // reader's point of view, zero elements in every cluster — so nothing
    // may promote. If this ever goes green as `observed`, someone has
    // re-added a phantom field.
    const phantom = {
      artifact: {
        states: [{ state_hash: "abc", elements: ["e1", "e2", "e3"], support: 1.0, contrast: 1.0 }],
      },
    } as unknown as DiscoveryArtifact;
    const config = makeSpecWithElementCounts("phantom", [{ stateId: "p", elementCount: 3 }]);
    const result = compileStateMachineFromSpecs([{ specId: "phantom", config }], phantom);

    expect(result.compiled).toBe(true);
    if (!result.compiled) return;
    expect(result.stateMachine.states[0].provenance).toBe("ai-generated");
  });

  it("decodes structured fingerprints the way the Rust reader does", () => {
    // `fingerprint_to_criteria` in spec_authoring.rs: id: > aria: >
    // tag:<tag>:<text> > role:, else opaque → data-fingerprint. Same
    // vocabulary, same `reg:` strip, so both readers turn one artifact into
    // the same criteria.
    const structured: DiscoveryCluster = {
      id: "fp_state_structured",
      name: null,
      confidence: 0.9,
      screenshotIds: ["r1", "r2"],
      stateImageIds: [
        "reg:id:save-button",
        "reg:aria:Save changes",
        "reg:tag:button:Save",
        "reg:role:button",
        "reg:016e88557e9c2b1e",
      ],
    };
    const config = makeSpecWithElementCounts("structured", [{ stateId: "st", elementCount: 5 }]);
    const result = compileStateMachineFromSpecs(
      [{ specId: "structured", config }],
      makeArtifact([structured]),
    );

    expect(result.compiled).toBe(true);
    if (!result.compiled) return;
    const [state] = result.stateMachine.states;
    expect(state.provenance).toBe("observed");
    // `clusterElementIds` sorts the bare fingerprints, so compare as a set.
    expect(state.requiredElements).toEqual(
      expect.arrayContaining([
        { id: "save-button" },
        { ariaLabel: "Save changes" },
        { tagName: "button", text: "Save" },
        { role: "button" },
        { attributes: { "data-fingerprint": "016e88557e9c2b1e" } },
      ]),
    );
    expect(state.requiredElements).toHaveLength(5);
  });

  it("still detects collisions on id-only decoded fingerprints", () => {
    // `id:<x>` decodes to an id-only ElementQuery, which has no role/name/tag
    // for `elementQueryKey` to hash. If that keyed to "" the uniqueness pass
    // would skip it and two observed states could silently both claim the
    // same element — the exact invariant this compiler exists to protect.
    const idOnly: DiscoveryCluster = {
      id: "fp_state_id_only",
      name: null,
      confidence: 0.9,
      screenshotIds: ["r1"],
      stateImageIds: ["reg:id:save-button"],
    };
    const config = makeSpecWithElementCounts("dup", [
      { stateId: "d1", elementCount: 1 },
      { stateId: "d2", elementCount: 1 },
    ]);
    expect(() =>
      compileStateMachineFromSpecs([{ specId: "dup", config }], makeArtifact([idOnly])),
    ).toThrow(/one-state-per-element invariant violated/);
  });

  it("treats empty-payload prefixes as opaque rather than as decoded criteria", () => {
    // A bare `id:` would decode to `{ id: "" }` — which LOOKS decoded but
    // keys to "" in `elementQueryKey`, silently dropping out of the
    // uniqueness pass. Such fingerprints must fall through to the opaque
    // `data-fingerprint` handle instead.
    const degenerate: DiscoveryCluster = {
      id: "fp_state_degenerate",
      name: null,
      confidence: 0.9,
      screenshotIds: ["r1"],
      stateImageIds: ["reg:id:", "reg:aria:", "reg:role:", "reg:tag:"],
    };
    const config = makeSpecWithElementCounts("degenerate", [{ stateId: "dg", elementCount: 4 }]);
    const result = compileStateMachineFromSpecs(
      [{ specId: "degenerate", config }],
      makeArtifact([degenerate]),
    );

    expect(result.compiled).toBe(true);
    if (!result.compiled) return;
    const [state] = result.stateMachine.states;
    expect(state.provenance).toBe("observed");
    expect(state.requiredElements).toEqual([
      { attributes: { "data-fingerprint": "aria:" } },
      { attributes: { "data-fingerprint": "id:" } },
      { attributes: { "data-fingerprint": "role:" } },
      { attributes: { "data-fingerprint": "tag:" } },
    ]);
  });

  it("dedupes repeated element ids within one cluster", () => {
    const duped: DiscoveryCluster = {
      id: "fp_state_duped",
      name: null,
      confidence: 0.9,
      screenshotIds: ["r1", "r2"],
      stateImageIds: ["reg:aaaa1111bbbb2222", "reg:aaaa1111bbbb2222", "reg:cccc3333dddd4444"],
    };
    const config = makeSpecWithElementCounts("duped", [{ stateId: "dp", elementCount: 2 }]);
    const result = compileStateMachineFromSpecs(
      [{ specId: "duped", config }],
      makeArtifact([duped]),
    );

    expect(result.compiled).toBe(true);
    if (!result.compiled) return;
    // Three raw ids, two distinct — dedup happens before the criteria are
    // built, so the state claims two elements, not three.
    expect(result.stateMachine.states[0].requiredElements).toHaveLength(2);
  });

  it("promotes at exactly MIN_SUPPORT (the gate is inclusive)", () => {
    const boundary: DiscoveryCluster = {
      ...CLUSTER_8,
      id: "fp_state_boundary",
      confidence: MIN_SUPPORT,
    };
    const config = makeSpecWithElementCounts("boundary", [{ stateId: "b", elementCount: 8 }]);
    const result = compileStateMachineFromSpecs(
      [{ specId: "boundary", config }],
      makeArtifact([boundary]),
    );

    expect(result.compiled).toBe(true);
    if (!result.compiled) return;
    expect(result.stateMachine.states[0].provenance).toBe("observed");
  });

  it("omits lastObserved when the artifact carries no derived_at", () => {
    const config = makeSpecWithElementCounts("nodate", [{ stateId: "nd", elementCount: 8 }]);
    const result = compileStateMachineFromSpecs([{ specId: "nodate", config }], {
      artifact: { states: [CLUSTER_8] },
    });

    expect(result.compiled).toBe(true);
    if (!result.compiled) return;
    const [state] = result.stateMachine.states;
    expect(state.provenance).toBe("observed");
    expect(state.provenanceMeta?.lastObserved).toBeUndefined();
    // The other meta fields are still populated — a missing timestamp must
    // not take the whole promotion down with it.
    expect(state.provenanceMeta?.support).toBe(0.88);
  });
});
