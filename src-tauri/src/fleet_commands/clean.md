# Clean Code (Lint)

Run the linting pipeline for the current repository. Fix all errors iteratively until the codebase passes.

**NOTE: Code formatting is NOT a concern.** Do NOT run black, isort, prettier, or any formatters.

## Instructions

**IMPORTANT: This command handles Python and JS/TS projects only.** A repository of markdown and config files, or one in another language, is out of scope.

1. **Detect project type** by checking for `pyproject.toml` (Python) or `package.json` (JS/TS) in the current directory and in its immediate subdirectories (a monorepo often keeps them in e.g. `backend/` and `frontend/`), and run the matching pipeline from each directory that has one
   - If neither is found, exit immediately with "No Python or JS/TS project found here; /clean does not handle other languages"

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
