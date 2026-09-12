# Clean Code (Lint)

Run the linting pipeline for the current repository. Fix all errors iteratively until the codebase passes.

**NOTE: Code formatting is NOT a concern.** Do NOT run black, isort, prettier, or any formatters.

## Instructions

**IMPORTANT: Skip qontinui-claude-config.** This is a configuration repository with markdown files - no linting needed.

1. **Detect project type** by checking for `pyproject.toml` (Python) or `package.json` (JS/TS)
   - If the current directory is `qontinui-claude-config`, exit immediately with "No linting needed for config repo"

2. **For Python projects**, run in order:
   - `poetry run ruff check . --fix` - Lint with auto-fix
   - `poetry run mypy --package app` - Type check (adjust package name as needed)

3. **For mypy errors**, fix iteratively:
   - Run mypy and capture all errors
   - Use multiple parallel Task agents to fix errors in batches
   - Re-run mypy after fixes
   - Repeat until 0 errors
   - Reference common fix patterns:
     - `# type: ignore[arg-type]` for SQLAlchemy filter issues
     - `# type: ignore[assignment]` for Column type mismatches
     - `# type: ignore[unreachable]` for valid runtime null checks
     - `# type: ignore[import-untyped]` for libraries without stubs
     - Change `param: str = None` to `param: str | None = None`

4. **For JS/TS projects**:
   - `npm run lint:fix` or `yarn lint:fix`
   - `npm run typecheck`

5. **Report results**: Show before/after error counts

Do NOT commit or push - this command only cleans the code.
