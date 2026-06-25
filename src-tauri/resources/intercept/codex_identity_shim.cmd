@echo off
setlocal EnableDelayedExpansion
rem qontinui session-restore CODEX IDENTITY shim (Windows cmd / PowerShell).
rem
rem DISTINCT from identity_shim.cmd. Codex has NO session-id flag — it
rem GENERATES its own session id (written into CODEX_HOME\sessions\...\
rem rollout-*.jsonl line-1 session_meta). Appending any identity flag would
rem BREAK codex. So this wrapper NEVER mutates the user's argv and NEVER pins an
rem id. Its only job: SIGNAL the runner that a codex session is starting in this
rem terminal, so the runner begins its read-back capture (POST
rem /control/session-open -> session::codex_capture::capture_and_record).
rem
rem Always-on (NOT gated by QONTINUI_INSTALL_INTERCEPT_ENABLED — the out-of-box
rem session-restore guarantee). TEMPLATE materialized per-terminal by the runner;
rem the runner substitutes the @@...@@ placeholders. Do NOT run it raw.
rem
rem Hard invariants (mirrored from identity_shim.cmd):
rem   * FAIL-OPEN: any failure -> run the REAL codex unchanged.
rem   * TRANSPARENT: stdio inherited; the user sees the real exit code.
rem   * RECURSION-GUARD: QONTINUI_INSTALL_INTERCEPT_GUARD=1 -> pure passthrough.
rem   * ARGS UNCHANGED — never inject a flag.
rem   * NEVER touch/relocate CODEX_HOME (auth lives there).
rem
rem Placeholders:
rem   @@TOOL@@      the wrapped program name (always `codex` here)
rem   @@SHIM_DIR@@  absolute path of this shim's own bin dir

set "TOOL=@@TOOL@@"
set "SHIM_DIR=@@SHIM_DIR@@"

rem ---- resolve the REAL codex: first PATH match not inside SHIM_DIR ----------
set "REAL="
for %%X in (cmd exe bat) do (
  if not defined REAL (
    for %%D in ("%PATH:;=" "%") do (
      if not defined REAL (
        set "DENTRY=%%~D"
        if /I not "!DENTRY!"=="%SHIM_DIR%" (
          if exist "!DENTRY!\%TOOL%.%%X" set "REAL=!DENTRY!\%TOOL%.%%X"
        )
      )
    )
  )
)

rem ---- recursion guard: a nested invocation never re-signals -----------------
if "%QONTINUI_INSTALL_INTERCEPT_GUARD%"=="1" goto :passthrough

rem ---- best-effort signal to the runner loopback (NO session_id; codex
rem generates its own and the runner reads it back). Never load-bearing. -------
if defined QONTINUI_INSTALL_INTERCEPT_PORT (
  where curl >nul 2>nul
  if not errorlevel 1 (
    set "CWDJSON=%CD:\=\\%"
    curl -fsS --connect-timeout 3 --max-time 10 -X POST "http://127.0.0.1:%QONTINUI_INSTALL_INTERCEPT_PORT%/control/session-open" -H "Content-Type: application/json" -d "{\"terminal_id\":\"%QONTINUI_TERMINAL_ID%\",\"provider\":\"%TOOL%\",\"source\":\"startup\",\"cwd\":\"!CWDJSON!\"}" >nul 2>nul
  )
)

set "QONTINUI_INSTALL_INTERCEPT_GUARD=1"

:passthrough
rem Exec the REAL codex with the user's args UNCHANGED — no flag injection.
set "QONTINUI_INSTALL_INTERCEPT_GUARD=1"
if defined REAL (
  call "%REAL%" %*
) else (
  call %TOOL% %*
)
endlocal & exit /b %ERRORLEVEL%
