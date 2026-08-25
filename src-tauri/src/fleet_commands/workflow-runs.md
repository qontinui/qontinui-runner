# Query Workflow Runs

Find and display workflow runs across all runner instances.

## Arguments
- `$ARGUMENTS` - Optional: filter by name, status, type, time range, or instance

## Instructions

### Step 1: Discover All Runner Instances

```bash
# Try each known port to find active instances
for port in 9876 9877 9878; do
  powershell -NoProfile -Command "(Invoke-WebRequest -Uri \"http://localhost:${port}/status\" -UseBasicParsing -TimeoutSec 2).Content" 2>/dev/null | python3 -c "
import json, sys
d = json.load(sys.stdin)
data = d.get('data', d)
print(f'Port {${port}}: {data.get(\"instance_name\", \"primary\")} (running)')
" 2>/dev/null || true
done
```

Also try the instances endpoint for a complete picture:
```bash
for port in 9876 9877 9878; do
  powershell -NoProfile -Command "(Invoke-WebRequest -Uri \"http://localhost:${port}/instances\" -UseBasicParsing -TimeoutSec 2).Content" 2>/dev/null | python3 -c "
import json, sys
d = json.load(sys.stdin)
for inst in d.get('data', d):
    print(f'  {str(inst.get(\"name\",\"primary\")):20} port:{inst[\"port\"]}  reachable:{inst[\"reachable\"]}')
" 2>/dev/null && break || true
done
```

### Step 2: Query Runs from All Active Instances

For each reachable instance, query task runs:

```bash
for port in 9876 9877 9878; do
  echo "=== Port $port ==="
  powershell -NoProfile -Command "(Invoke-WebRequest -Uri 'http://localhost:${port}/task-runs?limit=30' -UseBasicParsing -TimeoutSec 3).Content" 2>/dev/null | python3 -c "
import json, sys
data = json.load(sys.stdin)
for t in data:
    ts = t.get('created_at','')[:19]
    name = t['task_name'][:90]
    status = t['status']
    ttype = t['task_type']
    sessions = t.get('sessions_count', '?')
    print(f'  {status:12} | {ttype:12} | {sessions:>3} sess | {ts} | {name}')
" 2>/dev/null || echo "  (not reachable)"
done
```

### Step 3: Apply Filters

Parse `$ARGUMENTS` to filter the results:

- **By name**: If arguments contain a quoted string or recognizable workflow name, filter `task_name` with case-insensitive substring match
- **By status**: `failed`, `completed`, `running`, `stopped` — filter by status field
- **By type**: `ai`, `reflection`, `fixer`, `follow_up`, `task` — filter by task_type
- **By time**: `last hour`, `last 4 hours`, `today`, `yesterday` — filter by created_at
- **By instance/port**: `port:9878`, `instance:workflows` — filter by which port returned the run

If no arguments are provided, show all runs from the last 24 hours.

### Step 4: Display Results

Show a consolidated table across all instances:

```
## Workflow Runs [filter applied]

| Status | Type | Port | Sessions | Created | Name |
|--------|------|------|----------|---------|------|
| ... | ... | ... | ... | ... | ... |
```

Group related runs (same workflow name prefix) together when possible.

### Step 5: Get Details (if specific run requested)

If the user asks about a specific run or the filter returns a single run:

```bash
# Get task details
powershell -NoProfile -Command "(Invoke-WebRequest -Uri 'http://localhost:{port}/task-runs/{id}' -UseBasicParsing -TimeoutSec 5).Content"

# Get output (last 15000 chars)
powershell -NoProfile -Command "(Invoke-WebRequest -Uri 'http://localhost:{port}/task-runs/{id}/output?tail_chars=15000' -UseBasicParsing -TimeoutSec 5).Content"

# Get workflow state
powershell -NoProfile -Command "(Invoke-WebRequest -Uri 'http://localhost:{port}/task-runs/{id}/workflow-state' -UseBasicParsing -TimeoutSec 5).Content"
```

### Rules

- Always query ALL active instances — runs may be on any port
- Use PowerShell `Invoke-WebRequest` for HTTP calls (not curl)
- Parse JSON with `python3 -c "import json, sys; ..."` (no jq on Windows)
- Show the port number so the user knows which instance has each run
- Keep output concise — summarize, don't dump raw JSON
- If a run has a summary/ai_summary field, show it
