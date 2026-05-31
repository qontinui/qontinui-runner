#!/usr/bin/env node
// @ts-nocheck
//
// TypeScript Language-Service helper for the Code-Semantics twin observer
// (Phase 2a + 2b). Long-lived Node process speaking the newline-delimited
// JSON stdio protocol defined in
//   src-tauri/src/mcp/code_semantics/CONTRACT.md  §A
//
// It is supervised by the Rust sidecar (`src-tauri/src/mcp/code_semantics/`).
// One helper child per (repo, language) scope; v1 default scope = the runner's
// own TS frontend project.
//
// Engine = the TypeScript **Language Service API** (resolved types + in-memory
// overlay documents for predict-effect). Overlays NEVER touch disk.
//
// Protocol (one JSON object per line on stdin/stdout):
//   Request:  {"id": <int>, "cmd": "<command>", ...args}
//   Response: {"id": <int>, "ok": true,  ...result}
//          |  {"id": <int>, "ok": false, "error": "<msg>"}
//
// `typescript` is resolved from (in order): $QONTINUI_TS_PATH, the runner
// frontend node_modules/typescript, then the global require resolution. If TS
// cannot be resolved the helper exits non-zero with a clear stderr message so
// the sidecar can degrade to the code_graph fallback.

import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';
import * as path from 'node:path';
import * as fs from 'node:fs';
import * as crypto from 'node:crypto';
import * as readline from 'node:readline';

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);

// --------------------------------------------------------------------------
// Resolve the `typescript` package (CONTRACT §A resolution order).
// --------------------------------------------------------------------------
function resolveTypeScript() {
  const candidates = [];

  // 1. Explicit override.
  if (process.env.QONTINUI_TS_PATH) {
    candidates.push(process.env.QONTINUI_TS_PATH);
  }

  // 2. The runner frontend node_modules/typescript. The helper lives at
  //    src-tauri/resources/code-semantics/, so the frontend root is two dirs
  //    up from src-tauri (../../../). Also try CWD as a fallback root.
  const frontendRoots = [
    path.resolve(__dirname, '..', '..', '..'), // <repo>/ (frontend root)
    process.cwd(),
  ];
  for (const root of frontendRoots) {
    candidates.push(path.join(root, 'node_modules', 'typescript'));
  }

  for (const cand of candidates) {
    try {
      // Resolve the package's main entry from its directory.
      const pkgJson = path.join(cand, 'package.json');
      if (fs.existsSync(pkgJson)) {
        const req = createRequire(path.join(cand, 'index.js'));
        // typescript's main is lib/typescript.js
        const ts = req('./lib/typescript.js');
        if (ts && ts.version) {
          return { ts, from: cand };
        }
      }
    } catch {
      // try next candidate
    }
  }

  // 3. Global / ambient require resolution relative to this helper.
  try {
    const req = createRequire(__filename);
    const ts = req('typescript');
    if (ts && ts.version) {
      return { ts, from: req.resolve('typescript') };
    }
  } catch {
    // fall through to failure
  }

  return null;
}

const resolved = resolveTypeScript();
if (!resolved) {
  process.stderr.write(
    'code-semantics helper: cannot resolve the `typescript` package. ' +
      'Tried $QONTINUI_TS_PATH, the runner frontend node_modules/typescript, ' +
      'and global require resolution. Install TypeScript or set QONTINUI_TS_PATH.\n'
  );
  process.exit(2);
}

const ts = resolved.ts;
process.stderr.write(
  `code-semantics helper: typescript ${ts.version} from ${resolved.from}\n`
);

// --------------------------------------------------------------------------
// Scope state: a single LanguageService + its overlay map.
// --------------------------------------------------------------------------
const scriptVersions = new Map(); // fileName -> integer version
const overlay = new Map(); // fileName -> overlaid content (predict-effect)
let parsedConfig = null; // ts.ParsedCommandLine
let languageService = null;
let projectPath = null; // resolved tsconfig path
let indexed = false;

function norm(p) {
  // Canonicalize to the LS's preferred separators (forward slashes).
  return p.replace(/\\/g, '/');
}

function bumpVersion(fileName) {
  const key = norm(fileName);
  const v = (scriptVersions.get(key) || 0) + 1;
  scriptVersions.set(key, v);
}

function getVersion(fileName) {
  const key = norm(fileName);
  if (!scriptVersions.has(key)) scriptVersions.set(key, 1);
  return String(scriptVersions.get(key));
}

function readFileWithOverlay(fileName) {
  const key = norm(fileName);
  if (overlay.has(key)) return overlay.get(key);
  try {
    return ts.sys.readFile(fileName);
  } catch {
    return undefined;
  }
}

function createLanguageServiceHost(config) {
  const host = {
    getScriptFileNames: () => {
      // Union of the project's file list and any overlaid files.
      const set = new Set(config.fileNames.map(norm));
      for (const f of overlay.keys()) set.add(f);
      return Array.from(set);
    },
    getScriptVersion: (fileName) => getVersion(fileName),
    getScriptSnapshot: (fileName) => {
      const text = readFileWithOverlay(fileName);
      if (text === undefined) return undefined;
      return ts.ScriptSnapshot.fromString(text);
    },
    getCurrentDirectory: () => path.dirname(projectPath),
    getCompilationSettings: () => config.options,
    getDefaultLibFileName: (options) => ts.getDefaultLibFilePath(options),
    fileExists: (fileName) => {
      if (overlay.has(norm(fileName))) return true;
      return ts.sys.fileExists(fileName);
    },
    readFile: (fileName) => readFileWithOverlay(fileName),
    readDirectory: ts.sys.readDirectory,
    directoryExists: ts.sys.directoryExists,
    getDirectories: ts.sys.getDirectories,
    realpath: ts.sys.realpath,
    useCaseSensitiveFileNames: () => ts.sys.useCaseSensitiveFileNames,
  };
  return host;
}

// --------------------------------------------------------------------------
// Helpers: signatures, hashes, locations.
// --------------------------------------------------------------------------
function sha1(s) {
  return crypto.createHash('sha1').update(s, 'utf8').digest('hex');
}

function lineColOf(sourceFile, pos) {
  const lc = ts.getLineAndCharacterOfPosition(sourceFile, pos);
  return { line: lc.line + 1, col: lc.character + 1 }; // 1-based
}

function symbolKind(decl) {
  if (ts.isFunctionDeclaration(decl) || ts.isFunctionExpression(decl)) return 'function';
  if (ts.isMethodDeclaration(decl) || ts.isMethodSignature(decl)) return 'method';
  if (ts.isClassDeclaration(decl)) return 'class';
  if (ts.isInterfaceDeclaration(decl)) return 'interface';
  if (ts.isTypeAliasDeclaration(decl)) return 'type';
  if (ts.isEnumDeclaration(decl)) return 'enum';
  if (ts.isVariableDeclaration(decl) || ts.isVariableStatement(decl)) return 'variable';
  if (ts.isArrowFunction(decl)) return 'function';
  return 'variable';
}

// Produce a resolved signature string for a symbol at a declaration node.
function resolveSignature(checker, symbol, node) {
  try {
    const type = checker.getTypeOfSymbolAtLocation(symbol, node);
    // Prefer call signatures for callables.
    const callSigs = type.getCallSignatures();
    if (callSigs.length > 0) {
      return callSigs.map((s) => checker.signatureToString(s)).join(' | ');
    }
    return checker.typeToString(
      type,
      node,
      ts.TypeFormatFlags.NoTruncation | ts.TypeFormatFlags.WriteArrayAsGenericType
    );
  } catch {
    return checker.symbolToString(symbol);
  }
}

// Walk all top-level + nested declarations of a source file, calling cb(node, name).
function forEachNamedDeclaration(sourceFile, cb) {
  function visit(node) {
    let name;
    if (
      ts.isFunctionDeclaration(node) ||
      ts.isClassDeclaration(node) ||
      ts.isInterfaceDeclaration(node) ||
      ts.isTypeAliasDeclaration(node) ||
      ts.isEnumDeclaration(node)
    ) {
      name = node.name && node.name.getText(sourceFile);
      if (name) cb(node, name);
    } else if (ts.isVariableStatement(node)) {
      for (const d of node.declarationList.declarations) {
        if (d.name && ts.isIdentifier(d.name)) cb(d, d.name.getText(sourceFile));
      }
    } else if (ts.isMethodDeclaration(node) && node.name) {
      cb(node, node.name.getText(sourceFile));
    }
    ts.forEachChild(node, visit);
  }
  ts.forEachChild(sourceFile, visit);
}

function nameNodeOf(decl) {
  if (decl.name) return decl.name;
  return decl;
}

function isExportedDecl(decl) {
  // Walk modifiers; also treat module-level `export` statements.
  const mods = ts.canHaveModifiers(decl) ? ts.getModifiers(decl) : undefined;
  if (mods && mods.some((m) => m.kind === ts.SyntaxKind.ExportKeyword)) return true;
  // VariableDeclaration: the export keyword is on the parent VariableStatement.
  let n = decl;
  while (n) {
    if (ts.isVariableStatement(n)) {
      const m = ts.getModifiers(n);
      if (m && m.some((x) => x.kind === ts.SyntaxKind.ExportKeyword)) return true;
    }
    n = n.parent;
  }
  return false;
}

// Collect exported symbols of a source file => Map(name -> {kind, signatureHash, node}).
function exportedSymbolMap(program, sourceFile) {
  const checker = program.getTypeChecker();
  const out = new Map();
  const moduleSymbol = checker.getSymbolAtLocation(sourceFile);
  if (moduleSymbol) {
    const exports = checker.getExportsOfModule(moduleSymbol);
    for (const sym of exports) {
      const decls = sym.getDeclarations() || [];
      const decl = decls[0];
      if (!decl) continue;
      const sig = resolveSignature(checker, sym, decl);
      out.set(sym.getName(), {
        kind: decl ? symbolKind(decl) : 'variable',
        signatureHash: sha1(sig),
        signature: sig,
      });
    }
  }
  return out;
}

// --------------------------------------------------------------------------
// Command handlers.
// --------------------------------------------------------------------------
function cmdInit(args) {
  const project = args.project;
  if (!project) throw new Error('init: missing "project"');

  let tsconfigPath;
  if (project.endsWith('.json')) {
    tsconfigPath = path.resolve(project);
  } else {
    tsconfigPath = path.join(path.resolve(project), 'tsconfig.json');
  }
  if (!fs.existsSync(tsconfigPath)) {
    throw new Error(`init: tsconfig not found at ${tsconfigPath}`);
  }

  const configFile = ts.readConfigFile(tsconfigPath, ts.sys.readFile);
  if (configFile.error) {
    throw new Error(
      `init: failed to read tsconfig: ${ts.flattenDiagnosticMessageText(
        configFile.error.messageText,
        '\n'
      )}`
    );
  }
  const basePath = path.dirname(tsconfigPath);
  parsedConfig = ts.parseJsonConfigFileContent(
    configFile.config,
    ts.sys,
    basePath,
    undefined,
    tsconfigPath
  );

  projectPath = tsconfigPath;
  scriptVersions.clear();
  overlay.clear();

  const host = createLanguageServiceHost(parsedConfig);
  languageService = ts.createLanguageService(host, ts.createDocumentRegistry());
  // Force a program build so subsequent queries are warm.
  const program = languageService.getProgram();
  indexed = true;

  return {
    ready: true,
    file_count: program ? program.getSourceFiles().length : parsedConfig.fileNames.length,
    project: norm(projectPath),
  };
}

function requireReady() {
  if (!languageService || !indexed) {
    throw new Error('not initialized: send "init" first');
  }
}

function cmdStatus() {
  let fileCount = 0;
  if (languageService) {
    const program = languageService.getProgram();
    fileCount = program ? program.getSourceFiles().length : 0;
  }
  return {
    indexed,
    file_count: fileCount,
    project: projectPath ? norm(projectPath) : null,
  };
}

function cmdSymbolLookup(args) {
  requireReady();
  const wantName = args.name;
  if (!wantName) throw new Error('symbol_lookup: missing "name"');
  const wantKind = args.kind || null;
  const wantFile = args.file ? norm(path.resolve(args.file)) : null;

  const program = languageService.getProgram();
  const checker = program.getTypeChecker();
  const resolvedList = [];

  for (const sourceFile of program.getSourceFiles()) {
    if (sourceFile.isDeclarationFile) continue;
    const sfName = norm(sourceFile.fileName);
    if (wantFile && sfName !== wantFile) continue;

    forEachNamedDeclaration(sourceFile, (decl, name) => {
      if (name !== wantName) return;
      const kind = symbolKind(decl);
      if (wantKind && kind !== wantKind) return;
      const nameNode = nameNodeOf(decl);
      const sym = checker.getSymbolAtLocation(nameNode) || checker.getSymbolAtLocation(decl);
      const sig = sym ? resolveSignature(checker, sym, decl) : decl.getText(sourceFile);
      const loc = lineColOf(sourceFile, nameNode.getStart(sourceFile));
      resolvedList.push({
        file: sfName,
        name,
        kind,
        signature: sig,
        signature_hash: sha1(sig),
        def_location: { file: sfName, line: loc.line, col: loc.col },
      });
    });
  }

  return { exists: resolvedList.length > 0, resolved: resolvedList };
}

function findDeclByNameKind(program, file, name, kind) {
  const wantFile = norm(path.resolve(file));
  for (const sourceFile of program.getSourceFiles()) {
    if (norm(sourceFile.fileName) !== wantFile) continue;
    let found = null;
    forEachNamedDeclaration(sourceFile, (decl, dn) => {
      if (found) return;
      if (dn !== name) return;
      if (kind && symbolKind(decl) !== kind) return;
      found = { sourceFile, decl };
    });
    return found;
  }
  return null;
}

function offsetOfLineCol(sourceFile, line, col) {
  // line/col are 1-based.
  return ts.getPositionOfLineAndCharacter(sourceFile, line - 1, col - 1);
}

function cmdSignature(args) {
  requireReady();
  if (!args.file) throw new Error('signature: missing "file"');
  const program = languageService.getProgram();
  const checker = program.getTypeChecker();

  let sourceFile;
  let node;
  if (typeof args.line === 'number' && typeof args.col === 'number') {
    const wantFile = norm(path.resolve(args.file));
    sourceFile = program
      .getSourceFiles()
      .find((sf) => norm(sf.fileName) === wantFile);
    if (!sourceFile) return { found: false };
    const pos = offsetOfLineCol(sourceFile, args.line, args.col);
    node = findNodeAtPosition(sourceFile, pos);
  } else if (args.name) {
    const found = findDeclByNameKind(program, args.file, args.name, args.kind || null);
    if (!found) return { found: false };
    sourceFile = found.sourceFile;
    node = nameNodeOf(found.decl);
  } else {
    throw new Error('signature: provide either (name[,kind]) or (line,col)');
  }

  if (!node) return { found: false };
  const sym = checker.getSymbolAtLocation(node);
  if (!sym) return { found: false };

  const decl = (sym.getDeclarations() || [])[0] || node;
  const type = checker.getTypeOfSymbolAtLocation(sym, decl);
  const callSigs = type.getCallSignatures();

  let signature;
  let generics = [];
  let params = [];
  let returnType = '';

  if (callSigs.length > 0) {
    const s = callSigs[0];
    signature = checker.signatureToString(s);
    const typeParams = s.getTypeParameters() || [];
    generics = typeParams.map((tp) => checker.typeToString(tp));
    params = s.getParameters().map((p) => {
      const pdecl = (p.getDeclarations() || [])[0] || decl;
      const pt = checker.getTypeOfSymbolAtLocation(p, pdecl);
      return { name: p.getName(), type: checker.typeToString(pt) };
    });
    returnType = checker.typeToString(s.getReturnType());
  } else {
    signature = checker.typeToString(type, decl, ts.TypeFormatFlags.NoTruncation);
    returnType = signature;
  }

  const docParts = sym.getDocumentationComment(checker);
  const doc = ts.displayPartsToString(docParts);

  return {
    found: true,
    signature,
    signature_hash: sha1(signature),
    generics,
    params,
    return_type: returnType,
    doc,
  };
}

function findNodeAtPosition(sourceFile, pos) {
  function find(node) {
    if (pos >= node.getStart(sourceFile) && pos < node.getEnd()) {
      const child = ts.forEachChild(node, (c) => (find(c) ? c : undefined));
      if (child) {
        const deeper = find(child);
        if (deeper) return deeper;
      }
      return node;
    }
    return undefined;
  }
  // Prefer the innermost identifier.
  let best;
  function visit(node) {
    if (pos >= node.getStart(sourceFile) && pos < node.getEnd()) {
      if (ts.isIdentifier(node)) best = node;
      ts.forEachChild(node, visit);
    }
  }
  visit(sourceFile);
  return best || find(sourceFile);
}

function classifyRefKind(sourceFile, node) {
  let n = node;
  // definition: the name node of a declaration.
  const parent = n.parent;
  if (
    parent &&
    ((ts.isFunctionDeclaration(parent) ||
      ts.isClassDeclaration(parent) ||
      ts.isInterfaceDeclaration(parent) ||
      ts.isTypeAliasDeclaration(parent) ||
      ts.isEnumDeclaration(parent) ||
      ts.isMethodDeclaration(parent) ||
      ts.isVariableDeclaration(parent)) &&
      parent.name === n)
  ) {
    return 'definition';
  }
  // import: inside an import declaration / specifier.
  while (n) {
    if (ts.isImportDeclaration(n) || ts.isImportSpecifier(n) || ts.isImportClause(n)) {
      return 'import';
    }
    if (ts.isCallExpression(n) && n.expression && isWithin(n.expression, node)) {
      return 'call';
    }
    if (
      ts.isHeritageClause(n) ||
      ts.isClassDeclaration(n) && n.heritageClauses && containsNode(n.heritageClauses, node)
    ) {
      return 'impl';
    }
    n = n.parent;
  }
  return 'reference';
}

function isWithin(outer, inner) {
  return inner.getStart() >= outer.getStart() && inner.getEnd() <= outer.getEnd();
}
function containsNode(arr, node) {
  for (const el of arr) {
    if (node.getStart() >= el.getStart() && node.getEnd() <= el.getEnd()) return true;
  }
  return false;
}

function cmdFindReferences(args) {
  requireReady();
  if (!args.file) throw new Error('find_references: missing "file"');
  const program = languageService.getProgram();
  const wantFile = norm(path.resolve(args.file));
  const sourceFile = program.getSourceFiles().find((sf) => norm(sf.fileName) === wantFile);
  if (!sourceFile) return { references: [] };

  let pos;
  if (typeof args.line === 'number' && typeof args.col === 'number') {
    pos = offsetOfLineCol(sourceFile, args.line, args.col);
  } else if (args.name) {
    const found = findDeclByNameKind(program, args.file, args.name, args.kind || null);
    if (!found) return { references: [] };
    pos = nameNodeOf(found.decl).getStart(found.sourceFile);
  } else {
    throw new Error('find_references: provide either name or (line,col)');
  }

  const refs = languageService.getReferencesAtPosition(sourceFile.fileName, pos) || [];
  const out = [];
  for (const ref of refs) {
    const refSf = program.getSourceFiles().find((sf) => norm(sf.fileName) === norm(ref.fileName));
    let kind = 'reference';
    if (refSf) {
      const node = findNodeAtPosition(refSf, ref.textSpan.start);
      if (node) {
        if (ref.isDefinition) kind = 'definition';
        else kind = classifyRefKind(refSf, node);
      } else if (ref.isDefinition) {
        kind = 'definition';
      }
      const lc = lineColOf(refSf, ref.textSpan.start);
      out.push({ file: norm(ref.fileName), line: lc.line, col: lc.col, kind });
    } else {
      out.push({ file: norm(ref.fileName), line: 0, col: 0, kind: ref.isDefinition ? 'definition' : 'reference' });
    }
  }
  return { references: out };
}

function diagnosticsToErrors(diags) {
  const out = [];
  for (const d of diags) {
    let line = 0;
    let col = 0;
    if (d.file && typeof d.start === 'number') {
      const lc = ts.getLineAndCharacterOfPosition(d.file, d.start);
      line = lc.line + 1;
      col = lc.character + 1;
    }
    out.push({
      file: d.file ? norm(d.file.fileName) : '',
      line,
      col,
      code: d.code,
      message: ts.flattenDiagnosticMessageText(d.messageText, '\n'),
    });
  }
  return out;
}

function computeDiagnostics(file) {
  const semantic = languageService.getSemanticDiagnostics(file);
  const syntactic = languageService.getSyntacticDiagnostics(file);
  return [...syntactic, ...semantic];
}

function cmdTypecheck(args) {
  requireReady();
  if (!args.file) throw new Error('typecheck: missing "file"');
  const file = path.resolve(args.file);

  if (!args.overlay_patch) {
    const diags = computeDiagnostics(file);
    const errors = diagnosticsToErrors(diags);
    return {
      ok: errors.length === 0,
      errors,
      overlay: false,
      changed_signatures: [],
      removed_symbols: [],
    };
  }

  // --- 2b predict-effect overlay path ---
  const patch = args.overlay_patch;
  if (!patch.file || typeof patch.new_text !== 'string') {
    throw new Error('typecheck: overlay_patch requires {file, new_text}');
  }
  const overlayFile = norm(path.resolve(patch.file));

  // Capture BEFORE state of the overlaid file's exported symbols.
  const program0 = languageService.getProgram();
  const beforeSf = program0
    .getSourceFiles()
    .find((sf) => norm(sf.fileName) === overlayFile);
  const beforeExports = beforeSf ? exportedSymbolMap(program0, beforeSf) : new Map();

  // Apply overlay in-memory only.
  overlay.set(overlayFile, patch.new_text);
  bumpVersion(overlayFile);

  let result;
  try {
    // Recompute diagnostics for the requested file under the overlay.
    const diags = computeDiagnostics(file);
    const errors = diagnosticsToErrors(diags);

    const program1 = languageService.getProgram();
    const afterSf = program1
      .getSourceFiles()
      .find((sf) => norm(sf.fileName) === overlayFile);
    const afterExports = afterSf ? exportedSymbolMap(program1, afterSf) : new Map();

    // changed_signatures: exported symbols present in both whose hash changed.
    const changedSignatures = [];
    for (const [name, before] of beforeExports) {
      const after = afterExports.get(name);
      if (after && after.signatureHash !== before.signatureHash) {
        changedSignatures.push({
          name,
          before_hash: before.signatureHash,
          after_hash: after.signatureHash,
        });
      }
    }

    // removed_symbols: exported symbols deleted by the overlay that are still
    // referenced elsewhere (the Contradiction / active-negation signal).
    const removedSymbols = [];
    for (const [name, before] of beforeExports) {
      if (afterExports.has(name)) continue;
      // Find references to the now-removed symbol using its pre-overlay
      // definition position. We must look at the BEFORE program's def site,
      // but the overlay has bumped the version — so query references against
      // the overlaid LS which will report external (non-overlay-file) usages.
      const referencedBy = findExternalReferencesByName(overlayFile, name);
      if (referencedBy.length > 0) {
        removedSymbols.push({ name, kind: before.kind, referenced_by: referencedBy });
      }
    }

    result = {
      ok: errors.length === 0,
      errors,
      overlay: true,
      changed_signatures: changedSignatures,
      removed_symbols: removedSymbols,
    };
  } finally {
    // Always clear the overlay so disk-truth is restored.
    overlay.delete(overlayFile);
    bumpVersion(overlayFile);
  }
  return result;
}

// Find references to `name` (exported by overlayFile) that live OUTSIDE
// overlayFile, by scanning identifier nodes across the program and resolving
// each to a symbol whose declaration is in overlayFile. Used for the
// removed_symbols contradiction signal — the overlay has removed the decl,
// so we resolve against the BEFORE-on-disk content by reading the file fresh.
function findExternalReferencesByName(overlayFile, name) {
  const program = languageService.getProgram();
  const out = [];
  for (const sf of program.getSourceFiles()) {
    if (sf.isDeclarationFile) continue;
    if (norm(sf.fileName) === overlayFile) continue;
    const sfName = norm(sf.fileName);
    function visit(node) {
      if (ts.isIdentifier(node) && node.text === name) {
        // Heuristic: a usage of this identifier in another file that imports
        // from the overlay file. We confirm by checking the file's imports
        // resolve to the overlay file OR simply record name-matched usages in
        // files that import the overlay module.
        const lc = lineColOf(sf, node.getStart(sf));
        out.push({ file: sfName, line: lc.line });
      }
      ts.forEachChild(node, visit);
    }
    // Only scan files that import from the overlay file (binding-accurate-ish).
    if (fileImportsFrom(sf, overlayFile)) {
      visit(sf);
    }
  }
  return out;
}

function fileImportsFrom(sourceFile, overlayFile) {
  const program = languageService.getProgram();
  let imports = false;
  ts.forEachChild(sourceFile, (node) => {
    if (imports) return;
    if (ts.isImportDeclaration(node) && node.moduleSpecifier && ts.isStringLiteral(node.moduleSpecifier)) {
      const spec = node.moduleSpecifier.text;
      const resolved = ts.resolveModuleName(
        spec,
        sourceFile.fileName,
        program.getCompilerOptions(),
        ts.sys
      );
      if (
        resolved.resolvedModule &&
        norm(resolved.resolvedModule.resolvedFileName) === overlayFile
      ) {
        imports = true;
      }
    }
  });
  return imports;
}

// --------------------------------------------------------------------------
// Dispatch + stdio loop.
// --------------------------------------------------------------------------
function handle(req) {
  switch (req.cmd) {
    case 'init':
      return cmdInit(req);
    case 'status':
      return cmdStatus(req);
    case 'symbol_lookup':
      return cmdSymbolLookup(req);
    case 'signature':
      return cmdSignature(req);
    case 'find_references':
      return cmdFindReferences(req);
    case 'typecheck':
      return cmdTypecheck(req);
    default:
      throw new Error(`unknown cmd: ${req.cmd}`);
  }
}

const rl = readline.createInterface({ input: process.stdin });
rl.on('line', (line) => {
  const trimmed = line.trim();
  if (!trimmed) return;
  let req;
  try {
    req = JSON.parse(trimmed);
  } catch (e) {
    process.stdout.write(JSON.stringify({ id: null, ok: false, error: `bad JSON: ${e.message}` }) + '\n');
    return;
  }
  const id = req.id != null ? req.id : null;
  try {
    const result = handle(req);
    process.stdout.write(JSON.stringify({ id, ok: true, ...result }) + '\n');
  } catch (e) {
    process.stdout.write(JSON.stringify({ id, ok: false, error: e && e.message ? e.message : String(e) }) + '\n');
  }
});

rl.on('close', () => process.exit(0));
