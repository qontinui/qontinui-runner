# Organize Development Notes

Move development/planning files from the current repository to qontinui-dev-notes.

## Instructions

**IMPORTANT: Skip qontinui-claude-config entirely.** This repository contains intentional markdown files (slash commands) that should never be moved.

1. **Identify the current project name** from the directory (e.g., `qontinui-web`, `qontinui-runner`)
   - If the current directory is `qontinui-claude-config`, exit immediately with no action

2. **Find files to move** - scan for:
   - `PLAN*.md`, `TODO*.md`, `NOTES*.md`
   - `IMPLEMENTATION*.md`, `DESIGN*.md`, `ARCHITECTURE*.md`
   - `ROADMAP*.md`, `SCRATCH*.md`, `DRAFT*.md`
   - Temporary test scripts not in the test suite
   - One-off utility scripts

3. **Files to NEVER move**:
   - `README.md` - Standard documentation
   - `CLAUDE.md` - Claude instructions (also never commit)
   - `CONTRIBUTING.md`, `CHANGELOG.md`, `LICENSE.md`
   - `CODE_OF_CONDUCT.md`, `SECURITY.md`
   - `STATEMENT_OF_PURPOSE.md` - Referenced by reflection workflows
   - Files in `.github/`
   - Files in `docs/` meant for public documentation

4. **Create target directories** in qontinui-dev-notes:
   ```
   $PWD/qontinui-dev-notes/{project-name}/
   ├── docs/      # Planning and design documents
   ├── scripts/   # Temporary utility scripts
   ├── tests/     # One-off test scripts
   └── archive/   # Old/superseded documents
   ```

5. **Move files**:
   - Add date prefix if not present: `YYYY-MM-DD-original-name.md`
   - Move to appropriate subdirectory based on content type
   - Use `mv` command to move files

6. **Stage in dev-notes repo**:
   ```bash
   cd $PWD/qontinui-dev-notes
   git add .
   ```

7. **Report what was moved** - list all files and their new locations

Do NOT commit yet - just organize and stage the files.
