# Technical Debt Report (Read-Only)

Identify and document technical debt in the codebase without making changes.

## Instructions

**IMPORTANT**: This is a READ-ONLY command. Do NOT modify any files. Only analyze and report.

---

### Phase 1: Define Debt Categories

Technical debt to identify:

| Category | Description | Priority |
|----------|-------------|----------|
| **Architecture** | Poor structure, god classes, tight coupling | High |
| **Code Quality** | Complexity, duplication, dead code | Medium |
| **Testing** | Missing tests, flaky tests, poor coverage | High |
| **Documentation** | Missing docs, outdated comments | Low |
| **Dependencies** | Outdated deps, security vulnerabilities | High |
| **Performance** | N+1 queries, inefficient algorithms | Medium |
| **Security** | Vulnerabilities, hardcoded secrets | Critical |
| **Maintainability** | Magic numbers, unclear naming, no types | Medium |

---

### Phase 2: Automated Analysis

Run analysis tools to identify debt:

```bash
cd $PWD/qontinui-devtools

# Architecture debt
poetry run qontinui-devtools architecture god-classes /path/to/project
poetry run qontinui-devtools import check /path/to/project

# Code quality debt
poetry run qontinui-devtools quality dead-code /path/to/project

# Security debt
poetry run qontinui-devtools security scan /path/to/project

# Type coverage debt
poetry run qontinui-devtools types coverage /path/to/project
```

For dependencies:
```bash
cd /path/to/project
# Python
poetry show --outdated
pip-audit  # if available

# JavaScript
npm outdated
npm audit
```

---

### Phase 3: Manual Code Analysis

Search for common debt patterns:

#### TODO/FIXME/HACK Comments
```bash
# Search for debt markers
grep -rn "TODO\|FIXME\|HACK\|XXX\|WORKAROUND" /path/to/project --include="*.py"
```

#### Magic Numbers and Strings
Look for:
- Hardcoded values that should be constants
- Unexplained numeric literals
- Repeated string literals

#### Complex Code
Identify:
- Functions longer than 50 lines
- Deeply nested conditionals (>3 levels)
- Functions with many parameters (>5)
- Files with many imports (>15)

#### Code Duplication
Look for:
- Copy-pasted code blocks
- Similar functions with minor variations
- Repeated patterns that should be abstracted

---

### Phase 4: Test Debt Analysis

```bash
cd /path/to/project

# Check test coverage
poetry run pytest --cov=<package> --cov-report=term-missing 2>&1 | tail -50

# Find modules without tests
# Compare src files to test files
```

Identify:
- Modules with 0% coverage
- Critical paths without tests
- Flaky or disabled tests
- Tests with no assertions

---

### Phase 5: Dependency Debt

Check for:
- Outdated packages (security risk)
- Unused dependencies
- Conflicting version requirements
- Missing lockfile

---

### Phase 6: Generate Report

Create a structured debt report:

```markdown
# Technical Debt Report: {project}
Date: {date}

## Summary
- Total debt items: {count}
- Critical: {count}
- High: {count}
- Medium: {count}
- Low: {count}
- Estimated effort: {rough estimate}

## Critical Debt (Security/Stability Risk)

### DEBT-001: {Title}
- **Category**: Security
- **Location**: `path/to/file.py:123`
- **Description**: {What the problem is}
- **Impact**: {Why it matters}
- **Suggested Fix**: {How to resolve}
- **Effort**: Small/Medium/Large

## High Priority Debt (Architecture/Testing)

### DEBT-002: {Title}
...

## Medium Priority Debt (Code Quality)

### DEBT-003: {Title}
...

## Low Priority Debt (Documentation/Style)

### DEBT-004: {Title}
...

## Dependency Issues

| Package | Current | Latest | Risk |
|---------|---------|--------|------|
| {name}  | {ver}   | {ver}  | {level} |

## TODO/FIXME Items

| File | Line | Comment |
|------|------|---------|
| {file} | {line} | {text} |

## Recommendations

### Quick Wins (< 1 hour each)
1. {item}

### Medium Effort (1-4 hours each)
1. {item}

### Large Refactors (> 4 hours each)
1. {item}

## Metrics for Tracking

- Lines of code: {count}
- Test coverage: {percent}
- Type coverage: {percent}
- TODO count: {count}
- Outdated dependencies: {count}
```

---

### Output

Present the report directly in the chat. Do NOT create files.

This report can be used as input for `/refactor-srp` or `/improve-all` commands.
