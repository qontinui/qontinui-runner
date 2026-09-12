# Full Codebase Audit (Read-Only)

Perform a comprehensive read-only analysis of the codebase. This command produces reports without modifying any code.

## Instructions

**IMPORTANT**: This is a READ-ONLY command. Do NOT modify any files. Only analyze and report.

---

### Phase 1: Identify Target

1. **Determine the project to audit**:
   - If in a specific project directory, audit that project
   - If at the parent directory, ask which project to audit
   - Available repositories:
     - **Python**: qontinui, qontinui-devtools, qontinui-train, qontinui-finetune, qontinui-gym, qontinui-mcp, qontinui-schemas, qontinui-web-mcp
     - **Python + TypeScript**: qontinui-web (backend + frontend), multistate (library + docs-site)
     - **TypeScript + Rust**: qontinui-runner (Tauri app)
     - **Docusaurus**: qontinui-docs, multistate/docs-site

2. **Detect project type**:
   - Python: Look for `pyproject.toml`
   - TypeScript/JavaScript: Look for `package.json`

---

### Phase 2: Run Analysis (Python Projects)

Use qontinui-devtools for comprehensive analysis:

```bash
cd $PWD/qontinui-devtools

# Run all analysis tools
poetry run qontinui-devtools analyze /path/to/project --report /tmp/audit_report.html
poetry run qontinui-devtools import check /path/to/project
poetry run qontinui-devtools architecture god-classes /path/to/project
poetry run qontinui-devtools quality dead-code /path/to/project
poetry run qontinui-devtools security scan /path/to/project
poetry run qontinui-devtools types coverage /path/to/project
poetry run qontinui-devtools concurrency check /path/to/project
```

Also run standard linting tools in check-only mode:

```bash
cd /path/to/project
poetry run ruff check . --no-fix
poetry run mypy --package <package_name> 2>&1 | head -100
```

---

### Phase 3: Run Analysis (TypeScript/JavaScript Projects)

```bash
cd /path/to/project
npm run lint 2>&1 | head -100
npm run typecheck 2>&1 | head -100
```

Also analyze:
- Unused dependencies: `npx depcheck`
- Bundle size: Check for large imports
- Security: `npm audit`

---

### Phase 4: Code Structure Analysis

Manually analyze (use Glob, Grep, Read tools):

1. **File organization**:
   - Count files per directory
   - Identify oversized files (>500 lines)
   - Check for consistent naming conventions

2. **Complexity hotspots**:
   - Functions with deep nesting (>4 levels)
   - Files with many imports (>15)
   - Classes with many methods (>15)

3. **Documentation gaps**:
   - Public functions without docstrings
   - Complex logic without comments
   - Missing README files in modules

4. **Test coverage**:
   - Identify untested modules
   - Check test-to-code ratio
   - Look for test patterns

---

### Phase 5: Generate Report

Create a structured report with these sections:

```markdown
# Codebase Audit Report: {project}
Date: {date}

## Executive Summary
- Overall health: {Good/Fair/Needs Attention}
- Critical issues: {count}
- High priority issues: {count}
- Total findings: {count}

## Critical Issues (Fix Immediately)
- Security vulnerabilities
- Circular dependencies
- Runtime errors

## High Priority Issues
- Type errors
- Dead code (high confidence)
- SRP violations (god classes)

## Medium Priority Issues
- Missing type annotations
- Code complexity
- Test coverage gaps

## Low Priority Issues
- Style inconsistencies
- Minor refactoring opportunities
- Documentation gaps

## Recommendations
1. {Prioritized list of actions}

## Metrics
- Lines of code: {count}
- Files: {count}
- Type coverage: {percent}
- Test coverage: {percent if available}
```

---

### Output

Present the report directly in the chat. Do NOT create files.

If HTML report was generated, mention its location: `/tmp/audit_report.html`
