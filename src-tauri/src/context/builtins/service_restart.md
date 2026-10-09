## Service Restart Commands

Use these commands to restart development services after making code changes.

### Restart Commands

**To restart services, use PowerShell:**

```powershell
# Restart backend (FastAPI)
cd {{WORKSPACE}}; .\dev-start.ps1 -Backend

# Restart frontend (Next.js)
cd {{WORKSPACE}}; .\dev-start.ps1 -Frontend
```

`dev-start.ps1` is Windows-only. On Linux there is no equivalent restart command
here: report that the backend/frontend were not restarted rather than killing
processes by hand. Never kill processes by name (`Stop-Process -Name python`,
`Stop-Process -Name node`, `taskkill /IM node.exe`, ...): that takes down unrelated
tools and, for `node` / `powershell`, live Claude Code sessions.

### The runner: never restart it — read and report

An agent never stops, kills, restarts or rebuilds a running runner (served policy
`production-and-cost` `runner-lifecycle`), and so never runs any `dev-start.ps1`
switch that stops the runner (`-All`, `-Runner`, `-Fresh`, `-Supervisor`, `-Stop`,
`-StopRunner`, `-StopSupervisor`). Instead:

```bash
curl -sS --max-time 15 http://127.0.0.1:9876/restart-readiness
```

Report `safe_to_restart`, `terminal_sessions.count`, `ai_sessions.count` and
`reason` from the response. A failed read (unreachable, non-2xx, unparseable, or a
404 from a build that predates the endpoint) is UNKNOWN, never "safe". Then stop:
the operator restarts the runner. Verify runner code changes on an ephemeral build
(`cargo-guard.sh check` / `cargo-guard.sh test`) instead.

### Customization

This is a built-in context. To customize for your environment:

1. Go to **Contexts** tab in the runner
2. Create a new User Context named "Service Restart Commands"
3. Add your custom restart commands
4. Your version will override this built-in version

### Placeholder

`{{WORKSPACE}}` is replaced with your workspace root path at runtime.
