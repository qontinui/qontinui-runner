/**
 * Source-level parity between `scripts/perf-harness.mjs`'s private HTTP clients
 * and the shared `scripts/lib/runner-clients.mjs` (plan
 * `2026-10-11-terminal-page-performance-kpis-across-setups` Phase 3).
 *
 * perf-harness keeps its private `SupervisorClient` / `RunnerClient` until the
 * switch sequenced behind runner#1856. Until then the shared module must stay a
 * superset: every method perf-harness defines exists in the shared class with
 * the same body, modulo the one mechanical difference (`httpJson(` there is
 * `this.http(` here, which only adds test-time `fetch` injection) and formatting.
 * After the switch perf-harness imports the shared module and parity holds
 * trivially.
 */

/** Strip block and line comments (not `://` inside strings). */
export function stripComments(text) {
  return text.replace(/\/\*[\s\S]*?\*\//g, "").replace(/(^|[^:"'`\\])\/\/.*$/gm, "$1");
}

/**
 * The normal form two method bodies are compared in: comments gone, the
 * `httpJson(` → `this.http(` substitution applied, every whitespace run removed,
 * and trailing commas before a closing bracket dropped (prettier adds them when it
 * wraps a call, and `this.http` is one character longer than `httpJson`).
 */
export function normalize(src) {
  return stripComments(src)
    .replace(/\bhttpJson\(/g, "this.http(")
    .replace(/\s+/g, "")
    .replace(/,([)\]}])/g, "$1");
}

/** The text of a top-level `class <name> {` … `\n}` block, or null. */
export function classSource(text, name) {
  const m = new RegExp(`^(?:export )?class ${name} \\{$`, "m").exec(text);
  if (!m) return null;
  const end = text.indexOf("\n}\n", m.index);
  return end < 0 ? null : text.slice(m.index, end + 2);
}

/** The text of a top-level `[async ]function <name>(` … `\n}` block, or null. */
export function functionSource(text, name) {
  const m = new RegExp(`^(?:export )?(?:async )?function ${name}\\(`, "m").exec(text);
  if (!m) return null;
  const end = text.indexOf("\n}\n", m.index);
  return end < 0 ? null : text.slice(m.index, end + 2).replace(/^export /, "");
}

/**
 * Methods of a class block: name → raw source (signature through the closing
 * `  }`). Methods are the two-space-indented members of a prettier-formatted class.
 */
export function classMethods(classText) {
  const out = new Map();
  const lines = classText.split("\n");
  for (let i = 0; i < lines.length; i += 1) {
    const m = /^ {2}(?:async )?([A-Za-z_]\w*)\(/.exec(lines[i]);
    if (!m) continue;
    let j = i;
    while (j < lines.length && lines[j] !== "  }") j += 1;
    out.set(m[1], lines.slice(i, j + 1).join("\n"));
    i = j;
  }
  return out;
}

const CLASSES = ["SupervisorClient", "RunnerClient"];

/**
 * Compare a perf-harness source text against the shared module's.
 *
 * @returns {{mode: "imported" | "private", checked: string[], problems: string[]}}
 */
export function parityReport(harnessText, clientsText) {
  const imports = /from\s+["']\.\/lib\/runner-clients\.mjs["']/.test(harnessText);
  const privateClasses = CLASSES.filter((c) => classSource(harnessText, c) !== null);
  if (imports && privateClasses.length === 0) return { mode: "imported", checked: [], problems: [] };
  const problems = [];
  const checked = [];
  if (privateClasses.length !== CLASSES.length) {
    problems.push(`perf-harness defines ${privateClasses.join(", ") || "neither client"} privately and does not import the shared module`);
  }
  for (const cls of privateClasses) {
    const theirs = classMethods(classSource(harnessText, cls));
    const oursText = classSource(clientsText, cls);
    if (!oursText) {
      problems.push(`runner-clients.mjs has no ${cls}`);
      continue;
    }
    const ours = classMethods(oursText);
    for (const [name, src] of theirs) {
      if (name === "constructor") continue; // ours adds the optional `opts` (fetch injection); checked behaviourally
      checked.push(`${cls}.${name}`);
      if (!ours.has(name)) {
        problems.push(`${cls}.${name} is in perf-harness but missing from runner-clients.mjs`);
      } else if (normalize(src) !== normalize(ours.get(name))) {
        problems.push(`${cls}.${name} differs: perf-harness has\n${src}\nrunner-clients.mjs has\n${ours.get(name)}`);
      }
    }
  }
  for (const fn of ["httpJson", "unwrap"]) {
    const theirs = functionSource(harnessText, fn);
    if (theirs === null) continue;
    checked.push(fn);
    if (functionSource(clientsText, fn) === null) problems.push(`runner-clients.mjs has no ${fn}`);
  }
  return { mode: "private", checked, problems };
}
