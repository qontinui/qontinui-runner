/**
 * What the author sees when the runner refuses a save: a cron that can never
 * fire used to be stored silently, and the routes now answer with the reason, so
 * the reason has to reach the author and survive long enough to act on.
 *
 * vitest runs in `environment: "node"` and these are stateful handlers, so they
 * are pinned by source text, as the other `.wiring.test.ts` files are.
 */
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";

const read = (path: string) => readFileSync(resolve(__dirname, path), "utf8");

/** The text from `start` up to the first occurrence of `end` after it. */
function between(source: string, start: string, end: string): string {
  const from = source.indexOf(start);
  expect(from, `missing: ${start}`).toBeGreaterThanOrEqual(0);
  const to = source.indexOf(end, from + start.length);
  expect(to, `missing after ${start}: ${end}`).toBeGreaterThanOrEqual(0);
  return source.slice(from, to);
}

describe("trigger editor", () => {
  const tab = read("./TriggersTab.tsx");

  it("stays open when a save is refused, so what was typed survives", () => {
    const handleSave = between(tab, "const handleSave = async", "const handleDelete");
    expect(handleSave).toContain("if (!saved) return;");
    expect(handleSave.indexOf("if (!saved) return;")).toBeLessThan(
      handleSave.indexOf("setEditorOpen(false)"),
    );
  });

  it("clears the refusal when the author cancels", () => {
    const editor = between(tab, "<TriggerEditor", "/>");
    expect(between(editor, "onCancel=", "}}")).toContain("clearSaveRefusals()");
  });
});

describe("useTriggers", () => {
  const hook = read("../../hooks/useTriggers.ts");

  it("keeps a refused save on screen until the save succeeds", () => {
    const create = between(hook, "const createTrigger = useCallback", "const updateTrigger");
    const update = between(hook, "const updateTrigger = useCallback", "const deleteTrigger");
    for (const save of [create, update]) {
      expect(save).toContain("clearSaveRefusals();");
      expect(save).toContain("addSaveRefusalToast(");
      expect(save).not.toContain('addToast("error"');
    }
    const refusal = between(hook, "const addSaveRefusalToast", "[addToast, clearSaveRefusals]");
    expect(refusal).toContain('addToast("error", message, true)');
  });

  it("leaves every other failure toast to expire", () => {
    const addToast = between(hook, "const addToast = useCallback", "const dismissToast");
    expect(addToast).toContain("sticky = false");
    expect(addToast).toContain("if (sticky) return;");
    // A cleanup filtering on type would remove a delete or toggle failure too.
    expect(between(hook, "const clearSaveRefusals", "}, [])")).toContain("!t.sticky");
  });

  it("reads delete and toggle success from err, because their success has data null", () => {
    const remove = between(hook, "const deleteTrigger = useCallback", "const setEnabled");
    const toggle = between(hook, "const setEnabled = useCallback", "[loadTriggers, addToast");
    for (const action of [remove, toggle]) {
      expect(action).toContain("if (err === null) {");
      expect(action).not.toContain("data !== null");
    }
  });
});
