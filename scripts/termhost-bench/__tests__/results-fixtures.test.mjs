/**
 * The committed results fixtures are exactly what the builders in helpers/results.mjs
 * produce, so a fixture can never drift from the code that documents how it was made.
 *
 *   node --test scripts/termhost-bench/__tests__/results-fixtures.test.mjs
 */

import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";

import { FIXTURES_DIR, renderFixtures } from "./helpers/results.mjs";
import { validateResults } from "../validate.mjs";

test("the results fixtures are byte-identical to the helper output", () => {
  for (const [name, text] of Object.entries(renderFixtures())) {
    assert.equal(
      readFileSync(join(FIXTURES_DIR, name), "utf8"),
      text,
      `${name} drifted: run node scripts/termhost-bench/__tests__/helpers/results.mjs --write`,
    );
  }
});

test("the helper's fixtures are valid results files", () => {
  for (const [name, text] of Object.entries(renderFixtures())) {
    assert.deepEqual(validateResults(JSON.parse(text)).errors, [], name);
  }
});
