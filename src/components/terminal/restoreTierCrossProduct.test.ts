/**
 * Cross-seam guard for the restore-tier predicate (plan
 * `2026-08-23-single-source-derived-facts`, item 1 "Verification" + 12c
 * enforcement #2).
 *
 * The "is this record resumable at full tier" predicate exists twice, in two
 * languages: `classifyRestoreAction` here, and `mirrored_restore_tier` in
 * `src-tauri/src/session/restore_record_emitter.rs`, which decides the
 * `restore_tier` the coord mirror promises to a peer. The Rust copy once
 * claimed to mirror this one "exactly" while omitting the `isValidSessionId`
 * gate — a comment asserting fidelity with nothing enforcing it.
 *
 * No test can run both functions, so both are pinned to ONE committed table:
 * `__fixtures__/restore-tier-crossproduct.json`, the full cross product
 *
 *   {valid id, id with a shell metacharacter}
 *   × {authoritative, observed, reconciled}
 *   × {confirmed, not}
 *   × {transcript present, absent, unprobed}
 *   × {full-tier provider, terminal-only provider}
 *
 * with the expected verdict per row. THIS suite asserts the table is exactly
 * what `classifyRestoreAction` returns; the Rust test
 * `emitter_tier_matches_the_frontend_classifier_on_every_crossproduct_row`
 * asserts the emitter's wire tier matches every row (`"auto-resume"` ⇔
 * `"full"`). Change either predicate without the other and one side goes red.
 *
 * Regenerate after an INTENTIONAL classifier change (then make the Rust side
 * agree, or its test fails):
 *
 *   UPDATE_RESTORE_TIER_FIXTURE=1 npx vitest run src/components/terminal/restoreTierCrossProduct.test.ts
 *
 * The terminal-only provider: no shipped provider declares `terminal-only`
 * yet, so both suites register a fixture provider named
 * `fixture-terminal-only` through their own seam (a module mock here, an
 * explicit adapter in Rust). `claude` rows go through the REAL registry on
 * both sides.
 *
 * Ids: the fixture deliberately avoids a trailing-newline id. JavaScript's `$`
 * matches before a final `\n`, so `isValidSessionId("abc\n")` is true here
 * while Rust's `is_valid_session_id` refuses it — a documented, deliberate
 * divergence toward MORE refusal (see `src-tauri/src/session/session_id.rs`),
 * not a mirror row.
 */

import { readFileSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { describe, expect, it, vi } from "vitest";

vi.mock("./providerAdapter", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./providerAdapter")>();
  const terminalOnly = {
    ...actual.claudeDescriptor,
    provider: "fixture-terminal-only",
    restoreTier: () => "terminal-only" as const,
  };
  return {
    ...actual,
    providerDescriptorFor: (p: string | undefined) =>
      p === "fixture-terminal-only" ? terminalOnly : actual.providerDescriptorFor(p),
  };
});

import { providerDescriptorFor, type RestoreTier } from "./providerAdapter";
import { classifyRestoreAction, type RestoreAction } from "./useTerminalInitialization";

const FIXTURE_PATH = resolve(
  dirname(fileURLToPath(import.meta.url)),
  "__fixtures__/restore-tier-crossproduct.json",
);

const IDS = [
  { idKind: "valid", sessionId: "3f2b1c9d-4e5a-4b6c-8d7e-9f0a1b2c3d4e" },
  { idKind: "shell-metacharacter", sessionId: "abc; rm -rf /" },
] as const;
const ORIGINS = ["authoritative", "observed", "reconciled"] as const;
const CONFIRMED = [true, false] as const;
const TRANSCRIPTS = ["present", "absent", "unprobed"] as const;
const PROVIDERS = [
  { providerTier: "full", provider: "claude" },
  { providerTier: "terminal-only", provider: "fixture-terminal-only" },
] as const satisfies ReadonlyArray<{ providerTier: RestoreTier; provider: string }>;

/** Coord wire spelling of the restorable-at tier (Rust `RestoreTier::wire_str`). */
type WireTier = "full" | "terminal_only";

interface FixtureRow {
  idKind: (typeof IDS)[number]["idKind"];
  sessionId: string;
  origin: (typeof ORIGINS)[number];
  confirmed: boolean;
  transcript: (typeof TRANSCRIPTS)[number];
  providerTier: RestoreTier;
  provider: string;
  /** `classifyRestoreAction`'s verdict. */
  action: RestoreAction;
  /** The emitter's mirrored `restore_tier` — `"full"` iff `action === "auto-resume"`. */
  wireTier: WireTier;
}

interface Fixture {
  description: string;
  regenerate: string;
  rows: FixtureRow[];
}

function transcriptExists(t: FixtureRow["transcript"]): boolean | undefined {
  if (t === "present") return true;
  if (t === "absent") return false;
  return undefined;
}

function computeRows(): FixtureRow[] {
  const rows: FixtureRow[] = [];
  for (const { idKind, sessionId } of IDS)
    for (const origin of ORIGINS)
      for (const confirmed of CONFIRMED)
        for (const transcript of TRANSCRIPTS)
          for (const { providerTier, provider } of PROVIDERS) {
            const action = classifyRestoreAction({
              claudeSessionId: sessionId,
              origin,
              confirmedAt: confirmed ? 1 : undefined,
              provider,
              transcriptExists: transcriptExists(transcript),
            });
            rows.push({
              idKind,
              sessionId,
              origin,
              confirmed,
              transcript,
              providerTier,
              provider,
              action,
              wireTier: action === "auto-resume" ? "full" : "terminal_only",
            });
          }
  return rows;
}

function renderFixture(rows: FixtureRow[]): string {
  const fixture: Fixture = {
    description:
      "Cross-seam restore-tier table (plan 2026-08-23-single-source-derived-facts item 1). " +
      "`action` is classifyRestoreAction's verdict; `wireTier` is the restore_tier the Rust " +
      "restore_record_emitter must mirror for the same record. Pinned by " +
      "src/components/terminal/restoreTierCrossProduct.test.ts AND " +
      "src-tauri/src/session/restore_record_emitter.rs. Do not hand-edit.",
    regenerate:
      "UPDATE_RESTORE_TIER_FIXTURE=1 npx vitest run src/components/terminal/restoreTierCrossProduct.test.ts",
    rows,
  };
  return `${JSON.stringify(fixture, null, 2)}\n`;
}

describe("restore-tier cross product — classifyRestoreAction pins the shared fixture", () => {
  const rows = computeRows();

  if (process.env.UPDATE_RESTORE_TIER_FIXTURE === "1") {
    writeFileSync(FIXTURE_PATH, renderFixture(rows));
  }

  it("the mocked registry resolves both provider tiers (claude through the REAL descriptor)", () => {
    expect(providerDescriptorFor("claude").restoreTier()).toBe("full");
    expect(providerDescriptorFor("fixture-terminal-only").restoreTier()).toBe("terminal-only");
  });

  it("covers the full 2×3×2×3×2 cross product", () => {
    expect(rows).toHaveLength(72);
  });

  it("the committed fixture is exactly classifyRestoreAction's table", () => {
    // Byte-level, not just deep-equal: a hand-edited or reordered fixture is
    // also drift, and the Rust side reads the same bytes.
    expect(readFileSync(FIXTURE_PATH, "utf8")).toBe(renderFixture(rows));
  });

  it("negative control: the table is not vacuous — every verdict occurs", () => {
    const actions = new Set(rows.map((r) => r.action));
    expect(actions).toEqual(new Set(["auto-resume", "terminal-only", "skip-invalid"]));
    // At least one row is `full` on both sides, so an emitter that returned
    // `terminal_only` unconditionally cannot pass the Rust half.
    expect(rows.some((r) => r.wireTier === "full")).toBe(true);
  });

  it("every shell-metacharacter row is skip-invalid and mirrors terminal_only", () => {
    const bad = rows.filter((r) => r.idKind === "shell-metacharacter");
    expect(bad).toHaveLength(36);
    for (const r of bad) {
      expect(r.action).toBe("skip-invalid");
      expect(r.wireTier).toBe("terminal_only");
    }
  });

  it("full requires all five gates — exactly the four valid-id rows that pass them", () => {
    const full = rows.filter((r) => r.wireTier === "full");
    expect(full).toHaveLength(4);
    for (const r of full) {
      expect(r.idKind).toBe("valid");
      expect(["authoritative", "observed"]).toContain(r.origin);
      expect(r.confirmed).toBe(true);
      expect(r.transcript).not.toBe("absent");
      expect(r.providerTier).toBe("full");
    }
  });
});
