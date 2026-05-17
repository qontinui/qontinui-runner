## Command Step Schema

The `command` step type executes shell commands, API requests, checks, and MCP calls during workflow phases. The AI already knows standard shell commands — this context documents the **workflow step wrapper format** so commands are configured correctly.

### Allowed Phases

`setup` and `completion` for shell commands. `verification` for checks and API requests. Shell commands cannot be used in `agentic` phases.

- **Setup**: Environment preparation (install deps, start services, create dirs, seed data)
- **Completion**: Cleanup, notifications, final builds, artifact collection
- **Verification**: Checks (lint, typecheck), API requests with assertions

### JSON Schema

```json
{
  "type": "command",
  "id": "uuid-v4 (required)",
  "name": "Descriptive step name (required)",
  "phase": "setup" | "completion" | "verification",
  "command": "string (required for shell commands) — the actual shell command to execute",
  "working_directory": "string (optional) — absolute path where the command runs",
  "timeout_seconds": number (optional, default: 60) — seconds before the step is killed,
  "fail_on_error": boolean (optional, default: true) — whether non-zero exit fails the workflow
}
```

### Field Guidance

**`command`** (required for shell commands)

- Must be a real, syntactically valid command — no placeholders like `echo TODO` or `/path/to/script`
- Use the shell syntax appropriate for the target OS (Windows: PowerShell/cmd, Linux/macOS: bash)
- Chain related commands with `&&` when order matters
- Pipe output when needed: `npm run build 2>&1`

**`working_directory`** (recommended)

- Always set this for commands that depend on project structure (npm, cargo, poetry, etc.)
- Must be a real absolute path — never `/path/to/project` or similar placeholders
- Example: `C:/Users/jspin/Documents/qontinui-root/qontinui-web/frontend`

**`timeout_seconds`** (optional, default: 60)

- `npm install` / `pip install`: 120–180s (network-dependent)
- `npm run build` / `cargo build`: 120–300s (CPU-bound)
- Quick commands (`mkdir`, `cp`, `echo`): 10–30s
- Service health checks: 30–60s
- Database migrations: 60–120s

**`fail_on_error`** (optional, default: true)

- `true` for critical setup: dependency installation, builds, migrations
- `false` for best-effort cleanup: removing temp files, stopping optional services
- `false` for idempotent checks: `mkdir -p` (already exists is OK), `docker stop` (already stopped is OK)

### Examples

**Setup: Install dependencies**

```json
{
  "type": "command",
  "id": "a1b2c3d4-1111-4aaa-b111-111111111111",
  "name": "Install npm dependencies",
  "phase": "setup",
  "command": "npm install",
  "working_directory": "C:/Users/jspin/Documents/qontinui-root/qontinui-web/frontend",
  "timeout_seconds": 120,
  "fail_on_error": true
}
```

**Setup: Start a background service**

```json
{
  "type": "command",
  "id": "b2c3d4e5-2222-4bbb-c222-222222222222",
  "name": "Start dev server",
  "phase": "setup",
  "command": "npm run dev &",
  "working_directory": "C:/Users/jspin/Documents/qontinui-root/qontinui-web/frontend",
  "timeout_seconds": 10,
  "fail_on_error": false
}
```

**Completion: Production build verification**

```json
{
  "type": "command",
  "id": "c3d4e5f6-3333-4ccc-d333-333333333333",
  "name": "Production build verification",
  "phase": "completion",
  "command": "npx next build",
  "working_directory": "C:/Users/jspin/Documents/qontinui-root/qontinui-web/frontend",
  "timeout_seconds": 300,
  "fail_on_error": true
}
```

**Completion: Cleanup temp files (best-effort)**

```json
{
  "type": "command",
  "id": "d4e5f6a7-4444-4ddd-e444-444444444444",
  "name": "Clean temp artifacts",
  "phase": "completion",
  "command": "rm -rf /tmp/workflow-artifacts",
  "timeout_seconds": 10,
  "fail_on_error": false
}
```

### Common Patterns by Tech Stack

| Task               | Command                                               | Timeout |
| ------------------ | ----------------------------------------------------- | ------- |
| Node.js deps       | `npm install` or `npm ci`                             | 120s    |
| Python deps        | `poetry install` or `pip install -r requirements.txt` | 120s    |
| Rust deps          | `cargo build`                                         | 300s    |
| TypeScript check   | `npx tsc --noEmit`                                    | 60s     |
| Python lint        | `poetry run ruff check .`                             | 30s     |
| Next.js build      | `npx next build`                                      | 300s    |
| Database migration | `poetry run alembic upgrade head`                     | 120s    |
| Docker compose     | `docker compose up -d`                                | 60s     |
| Git operations     | `git pull --ff-only`                                  | 30s     |
