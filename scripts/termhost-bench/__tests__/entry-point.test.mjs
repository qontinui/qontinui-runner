// isEntryPoint decides whether cli.mjs / compare.mjs run at all: a false
// "not me" makes the script exit 0 having done nothing.
import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { isEntryPoint as cliEntry } from "../cli.mjs";
import { isEntryPoint as compareEntry } from "../compare.mjs";

for (const [name, isEntryPoint] of [["cli", cliEntry], ["compare", compareEntry]]) {
  test(`${name}: isEntryPoint matches the module directly, through a symlink, and not another file`, (t) => {
    const dir = mkdtempSync(join(tmpdir(), "tb-entry-"));
    try {
      const mod = join(dir, "mod.mjs");
      writeFileSync(mod, "");
      const url = pathToFileURL(mod).href;
      assert.equal(isEntryPoint(mod, url), true);
      assert.equal(isEntryPoint(join(dir, ".", "mod.mjs"), url), true, "an unnormalised path");
      const other = join(dir, "other.mjs");
      writeFileSync(other, "");
      assert.equal(isEntryPoint(other, url), false);
      assert.equal(isEntryPoint(undefined, url), false);
      assert.equal(isEntryPoint(join(dir, "missing.mjs"), url), false, "an unresolvable argv falls back to resolve()");
      const link = join(dir, "link.mjs");
      try {
        symlinkSync(mod, link);
      } catch {
        t.diagnostic("no symlink permission here; the symlink arm is unexercised");
        return;
      }
      assert.equal(isEntryPoint(link, url), true, "a symlinked launch must still run");
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });
}
