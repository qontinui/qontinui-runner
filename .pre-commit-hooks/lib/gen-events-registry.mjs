// gen-events-registry.mjs — read the type registry out of `schema_export.rs`.
//
// Used by `lib/gen-events-file-attribution.sh`, which decides, file by file,
// whether a push could have moved a drifted binding. That decision needs two
// facts the generated tree cannot give it:
//
//   1. WHICH NAMES are registered — at the merge-base as well as in the working
//      tree — so a binding that exists on one side only can be blamed on the
//      push only when the push added or removed that registration;
//   2. WHERE EACH REGISTERED TYPE IS DEFINED — so a binding whose type lives in
//      the qontinui-schemas crates is not blamed on a push that changed some
//      unrelated runner module.
//
// Usage: node gen-events-registry.mjs <path to schema_export.rs>
//
// Output on stdout, exit 0:
//   @macro <the add! macro's body, whitespace-collapsed>
//   <name>\t<external|local>          one line per registered name, sorted
//
// Exit 1 — a PREMISE does not hold, so the registry cannot be read this way:
//   @violation <reason>
// Exit 2 — usage or I/O error (message on stderr).
//
// The caller treats exit 1 and exit 2 alike: it falls back to blaming the
// whole push, which is what the hook did before per-file attribution existed.
// So every doubt here is a violation. A strict reader that falls back too
// often costs the pusher today's message; a lenient one that misreads the
// registry would clear a push that moved a binding.
//
// THE PREMISES (each is checked, none is assumed):
//
//   * the file lexes: every comment, string, raw string and char literal is
//     terminated, and every brace balances;
//   * there is exactly one `fn export_all_schemas`, and inside it:
//       - exactly one `macro_rules!`, named `add`;
//       - exactly one `let mut <map> = Map::new();`, and that map is named
//         exactly three times: the `let`, ONE `.insert(` inside the macro, and
//         the final `Object(<map>)`. Nothing else can put an entry in it;
//       - the macro body is exactly `<map>.insert($name.to_string(),
//         serde_json::to_value(schema_for!($ty)).unwrap())`, so the map key
//         is the literal name;
//       - no `cfg` anywhere, so the set of names cannot depend on features,
//         the target or build-script cfgs;
//       - no control flow (`if`, `match`, loops, closures, nested fns,
//         early returns), so every `add!` runs whenever the function does;
//       - no `mod` item inside it, and no `include!` or path attribute
//         anywhere in the file, so no code is spliced in from another file;
//       - every `add!(` invocation parses as `add!("Name", <type>)`.
//
// The caller adds the one premise only a regeneration can check: the parsed
// working-tree names must equal the set of files the regeneration emitted.
//
// WHAT "external" MEANS. Every path root in the registered type resolves,
// through this file's `use` declarations, to one of EXTERNAL_CRATES. Those are
// the crates the qontinui-schemas sibling provides, plus serde_json for
// `Value`. A runner push cannot edit their source. Anything else is `local`,
// including:
//   - `crate::`, `self::`, `super::`, and an alias of one of those;
//   - prelude names such as `String`;
//   - an alias declared twice with different targets;
//   - any type that is not a plain path with generic arguments.
// When any glob import is in scope, every type is `local`. A `mod` declared in
// the file binds its name as local. Scoping is otherwise flattened: a `use`
// in a nested block counts for the whole function, and an alias bound twice,
// differently, is `local`.

import { readFileSync } from 'node:fs';

const EXTERNAL_CRATES = new Set(['qontinui_types', 'qontinui_vision_core', 'serde_json']);
const NAME_RE = /^[A-Za-z_][A-Za-z0-9_]*$/;

class Violation extends Error {}

function violation(reason) {
  throw new Violation(reason);
}

const isIdentChar = (c) => c !== undefined && /[A-Za-z0-9_]/.test(c);

// ── lexer ───────────────────────────────────────────────────────────────────
//
// Produces the source with every comment replaced by one space, every string
// literal (plain, byte, C, raw) replaced by `"S<n>"` and its contents kept in
// `strings[n]`, and every char literal replaced by `'c'`. After this, a brace,
// paren or keyword in `code` is real code: none can come from a comment or a
// literal.
function lex(src) {
  const strings = [];
  let code = '';
  let i = 0;
  const n = src.length;

  const pushString = (value) => {
    code += `"S${strings.length}"`;
    strings.push(value);
  };

  while (i < n) {
    const c = src[i];
    const next = src[i + 1];

    if (c === '/' && next === '/') {
      while (i < n && src[i] !== '\n') i++;
      code += ' ';
      continue;
    }
    if (c === '/' && next === '*') {
      let depth = 1;
      i += 2;
      while (i < n && depth > 0) {
        if (src[i] === '/' && src[i + 1] === '*') {
          depth++;
          i += 2;
        } else if (src[i] === '*' && src[i + 1] === '/') {
          depth--;
          i += 2;
        } else {
          i++;
        }
      }
      if (depth > 0) violation('unterminated block comment');
      code += ' ';
      continue;
    }

    // Raw strings: r"..", r#".."#, br"..", cr"..". The prefix only counts at
    // an identifier boundary, so `bar"` is an identifier followed by a string.
    if ((c === 'r' || c === 'b' || c === 'c') && !isIdentChar(src[i - 1])) {
      const raw = /^(?:b|c)?r(#*)"/.exec(src.slice(i, i + 260));
      if (raw) {
        const hashes = raw[1];
        const close = '"' + hashes;
        const start = i + raw[0].length;
        const end = src.indexOf(close, start);
        if (end < 0) violation('unterminated raw string');
        pushString(src.slice(start, end));
        i = end + close.length;
        continue;
      }
      if ((c === 'b' || c === 'c') && next === '"') {
        i += 1; // the plain-string branch below reads the literal
        continue;
      }
      if (c === 'b' && next === "'") {
        i += 1; // the char branch below reads it
        continue;
      }
    }

    if (c === '"') {
      let j = i + 1;
      let value = '';
      while (j < n && src[j] !== '"') {
        if (src[j] === '\\') {
          value += src.slice(j, j + 2);
          j += 2;
        } else {
          value += src[j];
          j++;
        }
      }
      if (j >= n) violation('unterminated string literal');
      pushString(value);
      i = j + 1;
      continue;
    }

    if (c === "'") {
      // A char literal ('x', '\n', '\u{..}', '\''), or a lifetime/label ('a).
      if (next === '\\') {
        const end = src.indexOf("'", i + 3);
        if (end < 0 || end - i > 12) violation('unterminated char literal');
        code += "'c'";
        i = end + 1;
        continue;
      }
      const cp = src.codePointAt(i + 1);
      const width = cp !== undefined && cp > 0xffff ? 2 : 1;
      if (src[i + 1 + width] === "'") {
        code += "'c'";
        i += 2 + width;
        continue;
      }
      code += "'"; // a lifetime; the identifier after it is ordinary code
      i++;
      continue;
    }

    code += c;
    i++;
  }
  return { code, strings };
}

// The index of the bracket that closes the one at `open`.
function matching(code, open, openCh, closeCh) {
  let depth = 0;
  for (let i = open; i < code.length; i++) {
    if (code[i] === openCh) depth++;
    else if (code[i] === closeCh) {
      depth--;
      if (depth === 0) return i;
    }
  }
  violation(`unbalanced ${openCh}${closeCh}`);
}

const count = (text, re) => (text.match(re) || []).length;

// ── use declarations ────────────────────────────────────────────────────────

// Expand one use tree (the text between `use` and `;`) into bindings.
function expandUseTree(text) {
  const tokens = text.match(/::|[A-Za-z_][A-Za-z0-9_]*|[{},*]|\S/g) || [];
  let pos = 0;
  const bindings = [];
  let glob = false;

  const parseTree = (prefix) => {
    if (tokens[pos] === '::') pos++; // a leading `::` (2015-style absolute)
    if (tokens[pos] === '{') {
      pos++;
      while (tokens[pos] !== '}') {
        if (pos >= tokens.length) violation('unterminated use group');
        parseTree(prefix);
        if (tokens[pos] === ',') pos++;
        else if (tokens[pos] !== '}') violation(`unexpected '${tokens[pos]}' in a use group`);
      }
      pos++;
      return;
    }
    if (tokens[pos] === '*') {
      pos++;
      glob = true;
      return;
    }
    const path = [...prefix];
    for (;;) {
      const tok = tokens[pos];
      if (tok === undefined || !NAME_RE.test(tok)) violation(`unexpected '${tok}' in a use path`);
      path.push(tok);
      pos++;
      if (tokens[pos] !== '::') break;
      pos++;
      if (tokens[pos] === '{' || tokens[pos] === '*') {
        parseTree(path);
        return;
      }
    }
    let name = path[path.length - 1];
    let target = path;
    if (name === 'self') {
      target = path.slice(0, -1);
      name = target[target.length - 1];
    }
    if (tokens[pos] === 'as') {
      pos++;
      name = tokens[pos];
      if (name === undefined || !NAME_RE.test(name)) violation('a use alias is not a name');
      pos++;
    }
    if (name !== '_') bindings.push({ name, target });
  };

  parseTree([]);
  if (pos !== tokens.length) violation(`trailing '${tokens[pos]}' in a use declaration`);
  return { bindings, glob };
}

function collectUses(text) {
  const bindings = [];
  let glob = false;
  for (const m of text.matchAll(/\buse\s+([^;]+);/g)) {
    const r = expandUseTree(m[1]);
    bindings.push(...r.bindings);
    glob = glob || r.glob;
  }
  return { bindings, glob };
}

// Remove every `mod <name> { ... }` block: a nested module (the unit tests)
// has its own scope, and its uses do not reach `export_all_schemas`.
function stripModules(code) {
  let out = code;
  for (;;) {
    const m = /\bmod\s+[A-Za-z_][A-Za-z0-9_]*\s*\{/.exec(out);
    if (!m) return out;
    const open = m.index + m[0].length - 1;
    const close = matching(out, open, '{', '}');
    out = out.slice(0, m.index) + ' ' + out.slice(close + 1);
  }
}

// ── classification ──────────────────────────────────────────────────────────

function classify(type, aliases, anyGlob) {
  if (anyGlob) return 'local';
  if (!/^[A-Za-z0-9_:<>,\s]+$/.test(type)) return 'local';
  const tokens = type.match(/::|[A-Za-z_][A-Za-z0-9_]*|[<>,]/g) || [];
  if (tokens.join('') !== type.replace(/\s+/g, '')) return 'local';
  for (let k = 0; k < tokens.length; k++) {
    const tok = tokens[k];
    if (!NAME_RE.test(tok)) continue;
    if (tokens[k - 1] === '::') continue; // a later segment, not a root
    if (!rootIsExternal(tok, aliases)) return 'local';
  }
  return 'external';
}

// Follow the alias chain from a path root to a crate name.
function rootIsExternal(root, aliases) {
  let name = root;
  for (let depth = 0; depth < 8; depth++) {
    const targets = aliases.get(name);
    if (targets === undefined) return EXTERNAL_CRATES.has(name);
    if (targets.size !== 1) return false; // declared twice, differently
    const [target] = targets;
    const first = target.split('::')[0];
    if (first === name) return EXTERNAL_CRATES.has(name);
    name = first;
  }
  return false;
}

// ── main ────────────────────────────────────────────────────────────────────

function parseRegistry(src) {
  const { code, strings } = lex(src);

  const fnRe = /\bfn\s+export_all_schemas\s*\(/g;
  const fns = [...code.matchAll(fnRe)];
  if (fns.length !== 1) violation(`expected one fn export_all_schemas, found ${fns.length}`);
  const open = code.indexOf('{', fns[0].index);
  if (open < 0) violation('export_all_schemas has no body');
  const close = matching(code, open, '{', '}');
  const body = code.slice(open + 1, close);

  // Nothing may make the set of names conditional, or splice code in.
  if (/\bcfg\b/.test(body)) violation('a cfg inside export_all_schemas');
  if (/\binclude\s*!/.test(code)) violation('an include! in the registry file');
  if (/\bmod\s+[A-Za-z_]/.test(body)) violation('a mod item inside export_all_schemas');
  if (/#\s*!?\s*\[[^\]]*\bpath\s*=/.test(code)) violation('a path attribute in the registry file');
  // Every add! must run whenever the function runs: no control flow and no
  // closure or nested fn that could skip or repeat one. A runtime condition
  // would make "registered" depend on something other than the add! lines.
  const control = /\b(if|else|match|for|while|loop|fn|return|break|continue|async|await|move|unsafe|impl)\b|\|/.exec(body);
  if (control) violation(`control flow inside export_all_schemas ('${control[0]}')`);

  // Exactly one macro, `add`.
  const macros = [...body.matchAll(/\bmacro_rules\s*!\s*([A-Za-z_][A-Za-z0-9_]*)\s*\{/g)];
  if (macros.length !== 1 || macros[0][1] !== 'add') {
    violation(`expected exactly one macro_rules! add, found ${macros.length} macro(s)`);
  }
  const mOpen = macros[0].index + macros[0][0].length - 1;
  const mClose = matching(body, mOpen, '{', '}');
  const macroBody = body.slice(mOpen + 1, mClose);
  // The macro must key the map by the literal name and nothing else. Its body
  // is checked against the one shape that does, whitespace aside. A string
  // literal is masked to "S<n>" by the lexer, so a key built with one (a
  // prefix in a format!) could otherwise read as unchanged between two
  // revisions. Any other shape is refused rather than reasoned about.
  const MACRO_SHAPE =
    /^\(\$name:literal,\$ty:ty\)=>\{([A-Za-z_][A-Za-z0-9_]*)\.insert\(\$name\.to_string\(\),serde_json::to_value\(schema_for!\(\$ty\)\)\.unwrap\(\),?\);?\};?$/;
  const shape = MACRO_SHAPE.exec(macroBody.replace(/\s+/g, ''));
  if (!shape) violation('the add! macro is not `m.insert($name.to_string(), serde_json::to_value(schema_for!($ty)).unwrap())`');

  // The map, and every mention of it.
  const lets = [
    ...body.matchAll(
      /\blet\s+mut\s+([A-Za-z_][A-Za-z0-9_]*)\s*(?::[^=;]*)?=\s*(?:serde_json\s*::\s*)?Map\s*::\s*new\s*\(\s*\)\s*;/g,
    ),
  ];
  if (lets.length !== 1) violation(`expected one \`let mut <map> = Map::new();\`, found ${lets.length}`);
  const mapVar = lets[0][1];
  const mentions = count(body, new RegExp(`\\b${mapVar}\\b`, 'g'));
  const inserts = count(macroBody, new RegExp(`\\b${mapVar}\\s*\\.\\s*insert\\s*\\(`, 'g'));
  const returns = new RegExp(`\\bObject\\s*\\(\\s*${mapVar}\\s*\\)\\s*$`).test(body.trim());
  if (shape[1] !== mapVar) violation(`the add! macro inserts into \`${shape[1]}\`, not the map \`${mapVar}\``);
  if (mentions !== 3 || inserts !== 1 || !returns) {
    violation(
      `the map \`${mapVar}\` is used outside the let / add!-insert / final Object(${mapVar}) ` +
        `(${mentions} mentions, ${inserts} insert(s) in the macro, final return ${returns ? 'found' : 'missing'})`,
    );
  }

  // Every add! invocation.
  const names = new Map();
  let invocations = 0;
  for (const m of body.matchAll(/\badd\s*!/g)) {
    invocations++;
    const paren = m.index + m[0].length;
    const rest = body.slice(paren);
    const lead = /^\s*\(/.exec(rest);
    if (!lead) violation('an add! that is not followed by (');
    const pOpen = paren + lead[0].length - 1;
    const pClose = matching(body, pOpen, '(', ')');
    const inner = body.slice(pOpen + 1, pClose);
    const parts = /^\s*"S(\d+)"\s*,\s*([\s\S]*?)\s*,?\s*$/.exec(inner);
    if (!parts) violation(`an add! whose arguments are not ("Name", Type): ${inner.trim().slice(0, 80)}`);
    const name = strings[Number(parts[1])];
    if (!NAME_RE.test(name)) violation(`a registered name that is not an identifier: ${name}`);
    const type = parts[2];
    if (!type) violation(`add!("${name}") has no type`);
    const prior = names.get(name) || [];
    prior.push(type);
    names.set(name, prior);
  }
  if (invocations === 0) violation('no add! registrations');

  // Aliases in scope: the function body's and the file's own top level (minus
  // nested modules such as the unit tests).
  const fileLevel = code.slice(0, fns[0].index) + ' ' + code.slice(close + 1);
  const outside = stripModules(fileLevel);
  const uses = [collectUses(body), collectUses(outside)];
  const aliases = new Map();
  let anyGlob = false;
  const bind = (name, target) => {
    const set = aliases.get(name) || new Set();
    set.add(target);
    aliases.set(name, set);
  };
  for (const u of uses) {
    anyGlob = anyGlob || u.glob;
    for (const b of u.bindings) bind(b.name, b.target.join('::'));
  }
  // A module declared in this file is a name in scope too, and it is local.
  // Bound beside any `use` of the same name, it makes that name ambiguous,
  // so a block-scoped alias cannot pass a file-level module off as external.
  for (const m of fileLevel.matchAll(/\bmod\s+([A-Za-z_][A-Za-z0-9_]*)\b/g)) bind(m[1], `crate::${m[1]}`);

  const lines = [];
  for (const name of [...names.keys()].sort()) {
    const classes = new Set(names.get(name).map((t) => classify(t, aliases, anyGlob)));
    lines.push(`${name}\t${classes.size === 1 ? [...classes][0] : 'local'}`);
  }
  return { macro: macroBody.replace(/\s+/g, ' ').trim(), lines };
}

const file = process.argv[2];
if (!file) {
  console.error('usage: node gen-events-registry.mjs <schema_export.rs>');
  process.exit(2);
}
let src;
try {
  src = readFileSync(file, 'utf8');
} catch (err) {
  console.error(`cannot read ${file}: ${err.message}`);
  process.exit(2);
}
try {
  const { macro, lines } = parseRegistry(src);
  process.stdout.write(`@macro ${macro}\n${lines.join('\n')}\n`);
} catch (err) {
  if (err instanceof Violation) {
    process.stdout.write(`@violation ${err.message}\n`);
    process.exit(1);
  }
  console.error(`gen-events-registry: ${err.stack || err}`);
  process.exit(2);
}
