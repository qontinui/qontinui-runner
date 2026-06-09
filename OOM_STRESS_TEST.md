# OOM Stress Test Plan — Tauri Webview Log Rendering

## Prerequisites

1. Rebuild both runners after the memory protection changes
2. PG database with a task run that has large datasets (see seeding below)
3. Task Manager / Resource Monitor open to watch `WebView2` process memory

## Test Targets

| Data source | Table | Target row count | Estimated size |
|---|---|---|---|
| Process session output | `process_session_output` | 240K rows | ~48MB |
| Task run events | `task_run_events` | 8K+ rows | ~5MB |
| API requests | `task_run_api_requests` | 4K+ rows | ~22MB (large bodies) |
| MCP calls | `task_run_mcp_calls` | 1K+ rows | ~3MB |
| Playwright results | `task_run_playwright_results` | 500+ rows | ~2MB |
| AWAS steps | `task_run_awas_steps` | 500+ rows | ~1MB |
| Output chunks | `task_run_output_chunks` | 4K+ rows | ~22MB |

## Seed Data (if needed)

If the existing database doesn't have enough rows for a stress test, seed with:

```sql
-- Pick a task_run_id that already exists
-- Example: seed 10K API request rows for a task run
INSERT INTO task_run_api_requests (id, task_run_id, step_id, method, url, resolved_url, status_code, response_time_ms, response_body_type, success, created_at)
SELECT
  gen_random_uuid()::text,
  'YOUR_TASK_RUN_ID',
  'step-' || i,
  CASE WHEN i % 3 = 0 THEN 'POST' ELSE 'GET' END,
  'https://api.example.com/endpoint/' || i,
  'https://api.example.com/endpoint/' || i,
  CASE WHEN i % 10 = 0 THEN 500 ELSE 200 END,
  (random() * 2000)::int,
  'json',
  i % 10 != 0,
  NOW() - (10000 - i) * interval '1 second'
FROM generate_series(1, 10000) AS i;

-- Seed 10K events
INSERT INTO task_run_events (task_run_id, event_type, message, timestamp)
SELECT
  'YOUR_TASK_RUN_ID',
  CASE WHEN i % 3 = 0 THEN 'action' WHEN i % 3 = 1 THEN 'ai_output' ELSE 'general' END,
  'Event message ' || i || ' - ' || repeat('x', 200),
  NOW() - (10000 - i) * interval '1 second'
FROM generate_series(1, 10000) AS i;

-- Seed 5K MCP calls
INSERT INTO task_run_mcp_calls (id, task_run_id, step_id, server_id, server_name, tool_name, response_type, duration_ms, success, created_at)
SELECT
  gen_random_uuid()::text,
  'YOUR_TASK_RUN_ID',
  'step-' || i,
  'server-1',
  'test-server',
  'tool_' || (i % 20),
  'json',
  (random() * 5000)::int,
  i % 8 != 0,
  NOW() - (5000 - i) * interval '1 second'
FROM generate_series(1, 5000) AS i;
```

## Test Procedure

### Baseline (before test)
1. Open Task Manager, note WebView2 process memory
2. Expected idle: 100-200MB

### Test 1: Process Output Viewer (highest risk — 240K rows)
1. Go to Processes page
2. Click on a process with extensive output
3. **PASS criteria:**
   - Page loads without "Out of Memory" error
   - WebView2 memory stays under 500MB
   - Scrolling is smooth (no jank)
   - "Scroll to bottom" button works
   - Search filtering works
   - Output updates in real-time

### Test 2: Task Run Events (8K+ rows)
1. Go to AI Data Viewer tab
2. Select a completed task run with 8K+ events
3. Click "Events" section
4. **PASS criteria:**
   - Initial load shows first page (200 events)
   - "Show more" button appears with count of remaining
   - Clicking "Show more" loads next page without freezing
   - Filter by event type works and resets pagination
   - Memory stays under 400MB

### Test 3: API Requests (4K+ rows)
1. In AI Data Viewer, click "API Requests" section
2. **PASS criteria:**
   - Initial load shows first 50 requests
   - "Show more" loads next 50
   - Expanding a request shows details (headers, body, response)
   - No memory spike when expanding rows with large response bodies
   - Memory stays under 400MB

### Test 4: MCP Calls (1K+ rows)
1. In AI Data Viewer, click "MCP Calls" section
2. **PASS criteria:**
   - Initial load shows first 50 calls
   - "Show more" works
   - Expanding calls shows arguments and response
   - Memory stays under 300MB

### Test 5: Pipeline Events Timeline (if >50 events)
1. Go to Reflection Dashboard, click "Pipeline" tab
2. **PASS criteria:**
   - Shows "50 of N" count
   - "Load more" button works
   - Virtualized scrolling is smooth

### Test 6: Task Run Output (Active page)
1. Go to Active page during a running task
2. **PASS criteria:**
   - Output preview renders without freezing
   - "Load more" button works
   - Memory stays under 300MB

### Test 7: Concurrent stress
1. Open multiple tabs simultaneously:
   - Process output viewer with large logs
   - Task run events (8K+)
   - API requests (4K+)
2. **PASS criteria:**
   - WebView2 memory stays under 800MB total
   - No "Out of Memory" crash
   - All pages remain responsive

### Test 8: Dead-pane scrollback trim (the always-on leak)
This guards the regression behind the most common "Error code: out of memory"
crash during normal multi-session use: finished terminal panes stay mounted
(only an explicit close disposes the backend) and each retains ~20MB of xterm
cell storage at the live `scrollback: 10000` cap, so they accumulate until the
renderer dies. On process exit `TerminalInstance` now trims the pane to
`DEAD_TERMINAL_SCROLLBACK` (2000 lines).
1. Open a terminal, run a command that emits >10K lines (e.g. `seq 1 200000`),
   let it fill the scrollback, then let the shell process exit (`exit`).
2. Take a heap snapshot before and after the exit message appears.
3. **PASS criteria:**
   - After exit, the pane's retained buffer drops to ~2000 lines worth of cells
     (DevTools → Memory shows the large xterm buffer arrays shrink).
   - The tail of the output is still scrollable/readable in the dead pane.
   - Spawning + exiting many panes over a long session does NOT monotonically
     grow WebView2 memory the way it did pre-fix.

## Failure Diagnosis

If OOM occurs:
1. Check which page/component triggered it
2. Open DevTools (F12) → Memory → Take heap snapshot
3. Look for large arrays (>10K elements) in retained objects
4. Check Network tab for large IPC payloads (>5MB)
5. Verify the `limit` param is being sent in the invoke call

## Metrics to Record

| Test | WebView2 RAM (MB) | Load time (s) | Scroll FPS | OOM? |
|---|---|---|---|---|
| Test 1: Process output | | | | |
| Test 2: Events | | | | |
| Test 3: API requests | | | | |
| Test 4: MCP calls | | | | |
| Test 5: Pipeline events | | | | |
| Test 6: Task output | | | | |
| Test 7: Concurrent | | | | |
