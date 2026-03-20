## Service Restart Commands

Use these commands to restart development services after making code changes.

### Restart Commands

**To restart services, use PowerShell:**

```powershell
# Restart backend (FastAPI)
cd {{WORKSPACE}}; .\dev-start.ps1 -Backend

# Restart frontend (Next.js)
cd {{WORKSPACE}}; .\dev-start.ps1 -Frontend

# Restart API service
cd {{WORKSPACE}}; .\dev-start.ps1 -Api

# Restart all services
cd {{WORKSPACE}}; .\dev-start.ps1 -All
```

**Alternative (if dev-start.ps1 is not available):**

```powershell
# Backend - kill and restart
Get-Process -Name python -ErrorAction SilentlyContinue | Where-Object { $_.MainWindowTitle -match 'backend' } | Stop-Process -Force
cd {{WORKSPACE}}\backend && poetry run python run.py

# Frontend - kill and restart
Get-Process -Name node -ErrorAction SilentlyContinue | Where-Object { $_.CommandLine -match 'next' } | Stop-Process -Force
cd {{WORKSPACE}}\frontend && npm run dev

# Tauri runner - restart
Stop-Process -Name qontinui-runner -Force -ErrorAction SilentlyContinue
cd {{WORKSPACE}}\qontinui-runner && npm run tauri dev
```

### Customization

This is a built-in context. To customize for your environment:

1. Go to **Contexts** tab in the runner
2. Create a new User Context named "Service Restart Commands"
3. Add your custom restart commands
4. Your version will override this built-in version

### Placeholder

`{{WORKSPACE}}` is replaced with your workspace root path at runtime.
