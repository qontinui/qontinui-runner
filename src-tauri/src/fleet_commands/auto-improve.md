# Auto-Improve - Fully Autonomous Code Improvement

Run comprehensive analysis and fix ALL identified issues automatically without any user interaction.

## Instructions

**CRITICAL**: This command is FULLY AUTONOMOUS. Do NOT ask the user any questions. Make all decisions independently and implement all fixes.

**Behavior**:
- NO clarification questions - make reasonable decisions
- NO confirmation prompts - proceed with all fixes
- NO skipping issues - fix everything identified
- Commit changes incrementally for easy review/revert

---

### Pre-flight (Silent)

1. **Check git status** - if uncommitted changes exist, stash them:
   ```bash
   git stash push -m "auto-improve-stash-$(date +%Y%m%d-%H%M%S)"
   ```

2. **Create backup branch**:
   ```bash
   git checkout -b auto-improve-backup-$(date +%Y%m%d-%H%M%S)
   git checkout -
   ```

3. **Branch-first — never commit on the default branch.** All the phase commits below land on whatever branch is checked out, so before any commit, ensure we are NOT on the default branch:
   ```bash
   if [ "$(git symbolic-ref --short HEAD)" = "main" ]; then
     git checkout -b "loop/auto-improve-$(date +%Y%m%d-%H%M%S)-${SESSION_SHORT:-$RANDOM}"
   fi
   ```
   The per-phase `git commit`s then accumulate on this session branch. If/when these changes are shipped, push the BRANCH and open a PR (`gh pr create`), then **STOP — do not merge**: coord is the sole merge authority for `qontinui/*` repos and agents never run `gh pr merge` or `--admin` (CLAUDE.md; coord-served policy `git-operations` `merge-authority`). Opening the PR IS shipping. **Never push to the default branch directly** ([[feedback_no_direct_pushes_to_main_loops_use_branches]] — a loop committing + pushing on `main` caused a fleet-wide CI-red incident 2026-06-07).

4. **Detect project type** from `pyproject.toml`, `package.json`, or `Cargo.toml`

---

### Phase 1: Full Analysis

Run ALL analysis tools silently, capture output:

#### Python Projects
```bash
cd $PWD/qontinui-devtools
poetry run qontinui-devtools analyze /path/to/project --report /tmp/auto_improve.html
poetry run qontinui-devtools import check /path/to/project
poetry run qontinui-devtools architecture god-classes /path/to/project
poetry run qontinui-devtools quality dead-code /path/to/project
poetry run qontinui-devtools security scan /path/to/project
poetry run qontinui-devtools types coverage /path/to/project
```

#### TypeScript Projects
```bash
cd $PWD/qontinui-devtools
poetry run qontinui-devtools ts analyze /path/to/project
```

#### Rust Projects
```bash
cd $PWD/qontinui-devtools
poetry run qontinui-devtools rust analyze /path/to/project
```

Also run linters to get current error counts:
```bash
# Python
poetry run ruff check . 2>&1 | tail -5
poetry run mypy --package <pkg> 2>&1 | tail -10

# TypeScript
npm run lint 2>&1 | tail -10
npm run typecheck 2>&1 | tail -10
```

---

### Phase 2: Security Fixes (Autonomous)

Fix ALL security issues without asking:

| Issue | Autonomous Fix |
|-------|----------------|
| Hardcoded secrets | Move to `os.environ.get("VAR")` with descriptive name |
| SQL injection | Convert to parameterized queries |
| Command injection | Use `subprocess.run([...], shell=False)` |
| Path traversal | Add `pathlib.Path.resolve().is_relative_to()` check |
| eval/exec | Replace with `ast.literal_eval` or remove |
| Pickle loads | Replace with JSON or add comment if intentional internal use |

**Decision rule**: If a security fix might break functionality, implement it anyway but add a `# TODO: verify this security fix` comment.

```bash
git add -A && git commit -m "fix: address security vulnerabilities"
```

---

### Phase 3: Circular Dependencies (Autonomous)

Fix ALL circular imports:

| Pattern | Autonomous Fix |
|---------|----------------|
| Type-only import in cycle | Move to `TYPE_CHECKING` block |
| Shared code in cycle | Extract to new `_common.py` module |
| Runtime import in cycle | Use lazy import inside function |

**Decision rule**: Prefer `TYPE_CHECKING` first, then extraction, then lazy imports.

```bash
git add -A && git commit -m "refactor: resolve circular dependencies"
```

---

### Phase 4: Dead Code Removal (Autonomous)

Remove ALL dead code with confidence > 0.85:

| Item | Autonomous Action |
|------|-------------------|
| Unused imports | Remove immediately |
| Unused functions (private) | Remove immediately |
| Unused functions (public) | Remove but add to commit message |
| Unused classes (private) | Remove immediately |
| Unused classes (public) | Remove but add to commit message |
| Unused variables | Remove immediately |
| Commented-out code | Remove immediately |

**Decision rule**: If it's public API (no underscore prefix), remove it anyway but document in commit message.

```bash
git add -A && git commit -m "refactor: remove dead code

Removed public APIs (verify not used externally):
- function_name in module.py
- ClassName in other.py"
```

---

### Phase 5: God Class Refactoring (Autonomous)

Refactor ALL classes with LCOM > 0.7 AND > 15 methods:

1. **Analyze responsibilities** - group methods by the attributes they use
2. **Extract classes** - one per responsibility group
3. **Use composition** - original class delegates to extracted classes
4. **Name clearly** - `{Original}{Responsibility}` pattern

**Decision rule**: Always extract. Name extracted classes descriptively. Keep original class as facade if needed.

```bash
git add -A && git commit -m "refactor: split god classes for better modularity"
```

---

### Phase 6: Type Annotations (Autonomous)

Add types to ALL untyped code:

| Code Pattern | Autonomous Type |
|--------------|-----------------|
| `def foo(x):` | Infer from usage, default to `Any` if unclear |
| `def foo(x=None):` | `x: <type> \| None = None` |
| `def foo(x=[]):` | `x: list[<type>] \| None = None` with `x = x or []` |
| Return without annotation | Infer from return statements |
| Class attributes | Add class-level annotations |

**Decision rule**: Use `Any` only when type is truly dynamic. Prefer specific types. Use modern syntax (`list[str]` not `List[str]`).

```bash
git add -A && git commit -m "feat: add comprehensive type annotations"
```

---

### Phase 7: Linting Fixes (Autonomous)

Fix ALL linting issues:

```bash
# Python
poetry run black .
poetry run isort .
poetry run ruff check . --fix --unsafe-fixes

# TypeScript
npm run lint:fix
npx prettier --write .
```

Fix remaining issues manually (ruff/eslint can't auto-fix):
- Line too long: break into multiple lines
- Complexity too high: extract helper functions
- Unused variables: remove or prefix with `_`

```bash
git add -A && git commit -m "style: fix all linting issues"
```

---

### Phase 8: TODO/FIXME Resolution (Autonomous)

Resolve ALL TODO/FIXME/HACK comments:

| TODO Type | Autonomous Action |
|-----------|-------------------|
| "TODO: add error handling" | Add try/except with logging |
| "TODO: implement" | Implement based on context or raise NotImplementedError |
| "FIXME: bug here" | Analyze and fix the bug |
| "HACK: workaround" | Implement proper solution |
| "TODO: refactor" | Refactor the code |
| "TODO: test this" | Skip (no test generation) |
| Unclear/large TODOs | Leave with `# TODO(auto-improve): needs manual review` |

**Decision rule**: Attempt to resolve. If resolution would take >50 lines of new code, mark for manual review instead.

```bash
git add -A && git commit -m "fix: resolve TODO and FIXME items"
```

---

### Phase 9: Dependency Updates (Autonomous)

Update ALL dependencies:

```bash
# Python - update to latest compatible versions
poetry update

# If poetry update fails, update individually
poetry show --outdated | while read pkg rest; do
  poetry update "$pkg" || true
done

# TypeScript
npm update
```

**Decision rule**: Update everything. If tests/build fail after update, revert that specific package.

```bash
# Verify updates don't break anything
poetry run pytest -x --tb=short || git checkout pyproject.toml poetry.lock
npm test || git checkout package.json package-lock.json

git add -A && git commit -m "chore: update dependencies"
```

---

### Phase 10: Final Cleanup (Autonomous)

```bash
# Run all linters one final time
poetry run black .
poetry run isort .
poetry run ruff check . --fix
poetry run mypy --package <pkg>

# Verify no errors
git add -A && git commit -m "chore: final cleanup" || true
```

---

### Phase 11: Summary Report

Generate summary (output to chat, no file):

```markdown
# Auto-Improve Complete

## Changes Made
- Security fixes: {count}
- Circular dependencies resolved: {count}
- Dead code removed: {lines} lines
- God classes refactored: {count}
- Type annotations added: {count} functions
- Linting issues fixed: {count}
- TODOs resolved: {count}
- Dependencies updated: {count}

## Commits Created
1. `{hash}` - fix: address security vulnerabilities
2. `{hash}` - refactor: resolve circular dependencies
3. `{hash}` - refactor: remove dead code
4. `{hash}` - refactor: split god classes
5. `{hash}` - feat: add type annotations
6. `{hash}` - style: fix linting issues
7. `{hash}` - fix: resolve TODOs
8. `{hash}` - chore: update dependencies

## Items Needing Manual Review
- {list any items marked for manual review}

## Backup
- Branch: auto-improve-backup-{timestamp}
- Stash: auto-improve-stash-{timestamp} (if applicable)

To revert all changes:
git reset --hard auto-improve-backup-{timestamp}
```

---

### Parallel Execution Strategy

Use Task agents for maximum speed:

1. **Phase 4 (Dead code)**: Parallel agents per module
2. **Phase 5 (God classes)**: Parallel agents per class
3. **Phase 6 (Types)**: Parallel agents per file batch (5 files each)
4. **Phase 8 (TODOs)**: Parallel agents per file

Merge results and commit after each phase.

---

### Error Recovery (Autonomous)

| Error | Autonomous Recovery |
|-------|---------------------|
| Tests fail | Revert last commit, continue with next phase |
| Lint fails | Run auto-fix again, ignore remaining |
| Type errors | Add `# type: ignore[code]` and continue |
| Import errors | Revert file changes, continue |
| Build fails | Revert to last working commit |

**Never leave codebase broken**. If unrecoverable:
```bash
git reset --hard auto-improve-backup-{timestamp}
```

---

### Notes

- This command makes extensive autonomous changes
- All changes are atomic commits for easy revert
- Backup branch and stash provide full recovery
- No user input required at any point
- Designed for overnight/background execution
- Review commits after completion
