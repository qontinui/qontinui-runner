/**
 * A batch from the AI schedule builder: one refused task must not lose its reason
 * or the tasks behind it, and a retry must not duplicate what was created.
 *
 * vitest runs in `environment: "node"` and these are stateful handlers, so they
 * are pinned by source text, as the other `.wiring.test.ts` files are.
 */
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";

const read = (path: string) => readFileSync(resolve(__dirname, path), "utf8");

function between(source: string, start: string, end: string): string {
  const from = source.indexOf(start);
  expect(from, `missing: ${start}`).toBeGreaterThanOrEqual(0);
  const to = source.indexOf(end, from + start.length);
  expect(to, `missing after ${start}: ${end}`).toBeGreaterThanOrEqual(0);
  return source.slice(from, to);
}

describe("useScheduler createTasks", () => {
  const body = between(
    read("../../hooks/useScheduler.ts"),
    "const createTasks = useCallback",
    "Update an existing task",
  );

  it("tries every request rather than stopping at a refusal", () => {
    expect(body).toContain("for (const request of requests)");
    expect(body).not.toMatch(/\bbreak;|return refused;\s*}\s*}\s*if \(reasons/);
    expect(body).toContain("refused.push(request);");
  });

  it("sets the error once, after the loop, naming each refusal", () => {
    expect(body.match(/setError\(null\)/g)).toHaveLength(1);
    expect(body.indexOf("setError(null)")).toBeLessThan(body.indexOf("for (const request"));
    expect(body.indexOf('setError(reasons.join("; "))')).toBeGreaterThan(
      body.lastIndexOf("refused.push(request);"),
    );
    expect(body).toContain("reasons.push(`${request.name}: ");
  });
});

describe("SchedulerTab handleAiSubmit", () => {
  it("closes the builder only when nothing was refused", () => {
    const submit = between(
      read("./SchedulerTab.tsx"),
      "const handleAiSubmit = async",
      "const handleAiEditManually",
    );
    expect(submit).toContain("await createTasks(requests)");
    expect(submit).toContain("if (refused.length === 0) setIsAiBuilding(false);");
    expect(submit).not.toContain("await createTask(");
  });
});

describe("AiScheduleBuilder", () => {
  const builder = read("./AiScheduleBuilder.tsx");

  it("removes only what was created, from the current list", () => {
    const accept = between(
      builder,
      "const handleAcceptAll = useCallback",
      "[generatedTasks, onSubmit]",
    );
    expect(accept).toContain("requests.filter((r) => !refused.includes(r))");
    expect(accept).toContain("setGeneratedTasks((current) =>");
    expect(accept).toContain("!created.includes(t.request)");
  });

  it("disables Edit and Discard while a batch runs", () => {
    const edit = between(builder, "onClick={() => handleEditTask(idx)}", "className=");
    expect(edit).toContain("disabled={loading}");
    const discard = between(builder, "onClick={() => setGeneratedTasks(null)}", "className=");
    expect(discard).toContain("disabled={loading}");
  });
});
