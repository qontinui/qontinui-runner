# Validate Command

Post-write validation to catch issues before they become bugs.

## What This Command Does

Runs comprehensive validation on recent code changes:
1. **Static analysis**: Linters, type checkers
2. **Tests**: Unit tests, integration tests
3. **Code review**: Self-review for common issues
4. **Best practices check**: Against knowledge base patterns

## Validation Steps

### Step 1: Identify Changed Files

```bash
# Get recently modified files
git diff --name-only HEAD
git diff --cached --name-only

# Or check specific files the user mentioned
```

### Step 2: Run Language-Specific Linters

#### Python Files
```bash
# Format check (don't auto-fix yet, show issues first)
black --check .

# Import sorting check
isort --check-only .

# Linting
ruff check .

# Type checking
mypy .
```

#### TypeScript Files
```bash
# Type checking
npx tsc --noEmit

# Linting
npx eslint .

# Format checking
npx prettier --check .
```

#### Rust Files
```bash
# Format check
cargo fmt -- --check

# Linting
cargo clippy

# Type checking (build check)
cargo check
```

### Step 3: Run Tests

```bash
# Python
pytest -v

# TypeScript/JavaScript
npm test

# Rust
cargo test
```

### Step 4: Self-Code Review

Review the changed code against knowledge base patterns:

#### Check Against Common Errors
Read knowledge-base/debugging/errors.md and look for:
- Missing null/None checks
- Potential TypeErrors
- Missing error handling
- Off-by-one errors

#### Check Against Best Practices
Based on file type, consult:
- knowledge-base/best-practices/python.md
- knowledge-base/best-practices/typescript.md
- knowledge-base/best-practices/rust.md

Look for:
- Missing type annotations
- Functions that are too long (>50 lines)
- Deep nesting (>3 levels)
- SRP violations
- Missing docstrings/comments for complex logic
- Magic numbers/strings
- Inconsistent naming

#### Check Against Qontinui Patterns
Read knowledge-base/qontinui-specific/common-pitfalls.md and check:
- Using "workflows" not "processes"
- Proper logging in place
- Following existing architecture patterns

### Step 5: Debuggability Assessment

For each function/class modified:

**Ask:**
- Is this testable in isolation?
- Are variable names clear?
- Is the logic easy to follow?
- Is there adequate logging?
- Are errors informative?
- Could this be simplified?

**Red flags:**
- Complex nested conditionals
- Functions doing multiple things
- Hidden side effects
- Unclear variable names (x, tmp, data)
- Missing error messages
- No logging in complex logic

### Step 6: Report Results

Provide structured output:

```markdown
## Validation Results

### Static Analysis
✓ Black formatting: PASSED
✓ isort imports: PASSED
✓ Ruff linting: PASSED
✗ mypy type checking: 3 errors found

### Type Errors
1. file.py:42 - Missing return type annotation
2. file.py:56 - Argument type mismatch
3. file.py:78 - Incompatible type in assignment

### Tests
✓ Unit tests: 15/15 passed
✓ Integration tests: 8/8 passed

### Code Review Findings

#### Best Practices Issues
- file.py:100 - Function `process_data` is 75 lines (suggest split)
- file.py:150 - Deep nesting (4 levels, suggest extract functions)
- file.py:200 - Missing type annotation on parameter

#### Debuggability Concerns
- file.py:120 - Complex conditional, consider extracting to named function
- file.py:160 - No logging in error handling block
- file.py:180 - Generic error message, add more context

#### Qontinui-Specific Issues
- file.py:90 - Using "process" variable name, should be "workflow"

### Recommendations

High Priority (Fix Now):
1. Fix mypy type errors
2. Add logging to error handling (file.py:160)
3. Rename "process" to "workflow" (file.py:90)

Medium Priority (Consider):
1. Split `process_data` function (file.py:100)
2. Extract nested conditionals (file.py:120, 150)
3. Improve error messages (file.py:180)

Low Priority (Nice to Have):
1. Add docstrings to complex functions
2. Consider adding more test cases for edge cases

### Overall Assessment
Status: ⚠️ ISSUES FOUND (3 high priority issues)

Fix high priority issues before proceeding.
```

### Step 7: Auto-Fix Option (Optional)

If user approves, can auto-fix some issues:

```bash
# Python formatting
black .
isort .
ruff check --fix .

# TypeScript formatting
npx prettier --write .
npx eslint --fix .

# Rust formatting
cargo fmt
```

**Note:** Only auto-fix formatting/imports. Manual review needed for logic issues.

### Step 8: Re-validate

After fixes applied:
- Re-run static analysis
- Re-run tests
- Confirm all issues resolved

```markdown
## Re-validation Results

✓ All static analysis checks passing
✓ All tests passing
✓ No high priority issues remaining

Status: ✅ VALIDATION PASSED

Code is ready to commit.
```

## When to Use This Command

**Use after:**
- Implementing a feature
- Fixing a bug
- Refactoring code
- Before creating a pull request
- Before committing

**Use as part of workflow:**
1. Implement changes
2. Run `/validate`
3. Fix issues found
4. Run `/validate` again
5. Commit when passing

## Integration with Other Commands

- After `/debug` fixes: Run `/validate` to ensure fix is clean
- After `/review-before-code` implementation: Run `/validate` at each step
- Before `/clean-commit`: Validation should pass

## Autonomous Operation

This command runs autonomously:
- Executes all checks automatically
- Reports findings
- Optionally applies auto-fixes
- Re-validates after fixes

Ask user only for:
- Permission to apply auto-fixes
- Which priority level of issues to fix
- Whether to proceed with commit if minor issues remain
