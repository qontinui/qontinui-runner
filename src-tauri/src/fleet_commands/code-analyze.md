# Code Analysis with qontinui-devtools

Run comprehensive code analysis on Python, TypeScript/JavaScript, and Rust projects using qontinui-devtools.

## Instructions

This command analyzes code and provides actionable recommendations for improvement.

**Supported Languages:**
- Python (backend code)
- TypeScript/JavaScript (frontend code)
- Rust (native code)

---

## Python Analysis

### Run Analysis Tools

```bash
cd $PWD/qontinui-devtools

# 1. Check for circular dependencies
poetry run qontinui-devtools import check /path/to/python/code

# 2. Find god classes (SRP violations)
poetry run qontinui-devtools architecture god-classes /path/to/python/code

# 3. Detect dead code
poetry run qontinui-devtools quality dead-code /path/to/python/code

# 4. Security vulnerability scan
poetry run qontinui-devtools security scan /path/to/python/code

# 5. Type coverage analysis
poetry run qontinui-devtools types coverage /path/to/python/code

# 6. Dependency health check
poetry run qontinui-devtools deps check /path/to/python/project

# 7. Comprehensive analysis with HTML report
poetry run qontinui-devtools analyze /path/to/python/code --report /tmp/analysis_report.html
```

### Python Commands Reference

| Command | Purpose |
|---------|---------|
| `import check <path>` | Detect circular dependencies |
| `architecture god-classes <path>` | Find classes violating SRP |
| `quality dead-code <path>` | Find unused code |
| `security scan <path>` | Security vulnerability scan |
| `types coverage <path>` | Analyze type hint coverage |
| `deps check <path>` | Check dependency health |
| `concurrency check <path>` | Detect race conditions |
| `analyze <path> --report file.html` | Comprehensive HTML report |

---

## TypeScript/JavaScript Analysis

### Run Analysis Tools

```bash
cd $PWD/qontinui-devtools

# 1. Check for circular dependencies
poetry run qontinui-devtools ts check /path/to/ts/code

# 2. Detect dead code (unused exports)
poetry run qontinui-devtools ts dead-code /path/to/ts/code

# 3. Type coverage analysis
poetry run qontinui-devtools ts types /path/to/ts/code

# 4. Code complexity analysis
poetry run qontinui-devtools ts complexity /path/to/ts/code

# 5. Comprehensive analysis
poetry run qontinui-devtools ts analyze /path/to/ts/code
```

### TypeScript Commands Reference

| Command | Purpose |
|---------|---------|
| `ts check <path>` | Detect circular import dependencies |
| `ts dead-code <path>` | Find unused exports and code |
| `ts types <path>` | Analyze TypeScript type coverage |
| `ts complexity <path>` | Measure code complexity |
| `ts analyze <path>` | Run all analyses |

### TypeScript Options

```bash
# With strict mode (for CI/CD)
poetry run qontinui-devtools ts check /path --strict

# With custom thresholds
poetry run qontinui-devtools ts types /path --threshold 80
poetry run qontinui-devtools ts complexity /path --max-complexity 15

# Skip specific analyses
poetry run qontinui-devtools ts analyze /path --skip-dead-code
```

---

## React Analysis

### Run React Doctor

[React Doctor](https://github.com/millionco/react-doctor) scores React codebases 0-100 across 60+ rules covering state/effects, performance, architecture, bundle size, security, correctness, and accessibility.

```bash
# Analyze a React project (verbose output, non-interactive)
npx -y react-doctor@latest /path/to/react/project --verbose --yes

# Score only (quick check)
npx -y react-doctor@latest /path/to/react/project --score --yes

# Diff mode (only check changed files)
npx -y react-doctor@latest /path/to/react/project --diff --yes

# Skip specific checks
npx -y react-doctor@latest /path/to/react/project --verbose --yes --no-lint --no-dead-code
```

### React Doctor Options

| Option | Purpose |
|--------|---------|
| `--verbose` | Show detailed output for each rule |
| `--score` | Only output the numeric score (0-100) |
| `--yes` | Skip interactive prompts (required for automation) |
| `--diff` | Only analyze changed files (faster) |
| `--no-lint` | Skip lint-based rules |
| `--no-dead-code` | Skip dead code detection |

### When to Use

| Repository | Path |
|------------|------|
| qontinui-web/frontend | `$PWD/qontinui-web/frontend` |
| qontinui-runner | `$PWD/qontinui-runner` |
| qontinui-mobile | `$PWD/qontinui-mobile` |
| ui-bridge | `$PWD/ui-bridge` |
| multistate/docs-site | `$PWD/multistate/docs-site` |
| qontinui-docs | `$PWD/qontinui-docs` |

Only run on repositories that have React as a dependency. Non-React TypeScript projects will produce meaningless output.

### React Doctor Priority Guidelines

| Priority | Categories |
|----------|------------|
| Critical | Security findings, correctness bugs |
| High | Performance issues, architecture problems |
| Medium | State/effects anti-patterns, accessibility gaps |
| Low | Style and convention issues |

---

## Rust Analysis

### Run Analysis Tools

```bash
cd $PWD/qontinui-devtools

# 1. Check for circular module dependencies
poetry run qontinui-devtools rust import check /path/to/rust/src

# 2. Detect dead code (unused functions, structs)
poetry run qontinui-devtools rust dead-code /path/to/rust/src

# 3. Analyze unsafe code usage
poetry run qontinui-devtools rust unsafe /path/to/rust/src

# 4. Code complexity analysis
poetry run qontinui-devtools rust complexity /path/to/rust/src

# 5. Comprehensive analysis
poetry run qontinui-devtools rust analyze /path/to/rust/src
```

### Rust Commands Reference

| Command | Purpose |
|---------|---------|
| `rust import check <path>` | Detect circular module dependencies |
| `rust dead-code <path>` | Find unused code |
| `rust unsafe <path>` | Analyze unsafe block usage |
| `rust complexity <path>` | Measure code complexity |
| `rust analyze <path>` | Run all analyses |

---

## Priority Guidelines

### Critical (Fix Immediately)
- Circular dependencies (cause import/compilation issues)
- Security vulnerabilities (hardcoded secrets, injection attacks)
- Unsafe code without justification (Rust)
- Race conditions

### High Priority
- Dead imports and unused code (high confidence >0.90)
- God classes with LCOM > 0.8 and >20 methods
- High cyclomatic complexity (>15)
- Missing type annotations in public APIs

### Medium Priority
- Type coverage gaps
- Code complexity issues
- Functions with missing return types

### Low Priority
- Minor cohesion issues
- Dead code with lower confidence (<0.85)

---

## Example: Full Qontinui Analysis

```bash
cd $PWD/qontinui-devtools

# Python repositories
poetry run qontinui-devtools analyze /path/to/qontinui/src --report /tmp/qontinui_analysis.html
poetry run qontinui-devtools analyze /path/to/qontinui-web/backend/app --report /tmp/backend_analysis.html
poetry run qontinui-devtools analyze /path/to/qontinui-devtools/src --report /tmp/devtools_analysis.html
poetry run qontinui-devtools analyze /path/to/qontinui-train --report /tmp/train_analysis.html
poetry run qontinui-devtools analyze /path/to/qontinui-finetune --report /tmp/finetune_analysis.html
poetry run qontinui-devtools analyze /path/to/qontinui-gym --report /tmp/gym_analysis.html
poetry run qontinui-devtools analyze /path/to/qontinui-mcp --report /tmp/mcp_analysis.html
poetry run qontinui-devtools analyze /path/to/qontinui-schemas --report /tmp/schemas_analysis.html
poetry run qontinui-devtools analyze /path/to/qontinui-web-mcp --report /tmp/web_mcp_analysis.html
poetry run qontinui-devtools analyze /path/to/multistate/src --report /tmp/multistate_analysis.html

# TypeScript frontends
poetry run qontinui-devtools ts analyze /path/to/qontinui-web/frontend/src
poetry run qontinui-devtools ts analyze /path/to/qontinui-runner/src
poetry run qontinui-devtools ts analyze /path/to/qontinui-docs  # Docusaurus
poetry run qontinui-devtools ts analyze /path/to/multistate/docs-site/src

# Rust (qontinui-runner backend)
poetry run qontinui-devtools rust analyze /path/to/qontinui-runner/src-tauri/src
```

### Repository Reference

| Repository | Type | Analysis Command |
|------------|------|------------------|
| qontinui | Python | `analyze /path/to/qontinui/src` |
| qontinui-web/backend | Python | `analyze /path/to/qontinui-web/backend/app` |
| qontinui-web/frontend | TypeScript | `ts analyze /path/to/qontinui-web/frontend/src` |
| qontinui-runner (TS) | TypeScript | `ts analyze /path/to/qontinui-runner/src` |
| qontinui-runner (Rust) | Rust | `rust analyze /path/to/qontinui-runner/src-tauri/src` |
| qontinui-devtools | Python | `analyze /path/to/qontinui-devtools/src` |
| qontinui-train | Python | `analyze /path/to/qontinui-train` |
| qontinui-finetune | Python | `analyze /path/to/qontinui-finetune` |
| qontinui-docs | TypeScript | `ts analyze /path/to/qontinui-docs` |
| qontinui-gym | Python | `analyze /path/to/qontinui-gym` |
| qontinui-mcp | Python | `analyze /path/to/qontinui-mcp` |
| qontinui-schemas | Python | `analyze /path/to/qontinui-schemas` |
| qontinui-web-mcp | Python | `analyze /path/to/qontinui-web-mcp` |
| multistate (lib) | Python | `analyze /path/to/multistate/src` |
| multistate (docs) | TypeScript | `ts analyze /path/to/multistate/docs-site/src` |

---

## Notes

- Focus on high-confidence findings first
- Some "dead code" may be exported APIs - verify before deleting
- Security findings may be false positives - review each one
- God class detection uses LCOM metric: higher values = lower cohesion
- TypeScript tools use regex parsing (no Node.js required)
- Rust tools use regex parsing (no rustc/cargo required)
