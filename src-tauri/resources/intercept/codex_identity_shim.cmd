@echo off
setlocal EnableDelayedExpansion
rem qontinui session-restore READ-BACK identity shim (Windows cmd / PowerShell).
rem
rem Wraps a CLI whose profile reads its session id back (Codex): the CLI mints
rem its own id, so this wrapper never changes the user's argv. Its one job is
rem to SIGNAL the runner that a session is starting in this terminal - POST
rem /control/session-open with NO session_id - so the runner's read-back
rem capture can find the session's rollout file. Ported from qontinui-runner
rem PR #651. The pinning counterpart is identity_shim.cmd.
rem
rem TEMPLATE materialized by the runner; it substitutes the @@...@@
rem placeholders. Do NOT run it raw. UNTESTED on Windows since the port
rem (the Codex profile records Windows as UNKNOWN).
rem
rem Hard invariants (mirrored from identity_shim.cmd):
rem   * FAIL-OPEN: any failure -> run the REAL CLI unchanged.
rem   * TRANSPARENT: stdio inherited; the user sees the real exit code.
rem   * RECURSION-GUARD: QONTINUI_INSTALL_INTERCEPT_GUARD=1 -> pure passthrough.
rem   * ARGS UNCHANGED - never inject a flag.
rem   * NEVER set or relocate CODEX_HOME; a user-set value is only reported.
rem
rem Placeholders:
rem   @@TOOL@@      the wrapped program name
rem   @@SHIM_DIR@@  absolute path of this shim's own bin dir

set "TOOL=@@TOOL@@"
set "SHIM_DIR=@@SHIM_DIR@@"

rem ---- resolve the REAL CLI: first PATH match not inside SHIM_DIR -----------
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

rem ---- recursion guard: a nested invocation never re-signals ----------------
if "%QONTINUI_INSTALL_INTERCEPT_GUARD%"=="1" goto :passthrough

rem ---- only a launch that can start a session signals (see the bash twin) ---
for %%S in (login logout mcp plugin app-server remote-control completion update doctor sandbox debug apply a queue archive delete migrate-rollouts unarchive cloud exec-server features agents help -h --help -V --version) do (
  if /I "%~1"=="%%S" goto :passthrough
)

rem ---- best-effort start signal (NO session_id). Never load-bearing. --------
if defined QONTINUI_INSTALL_INTERCEPT_PORT (
  where curl >nul 2>nul
  if not errorlevel 1 (
    set "CWDJSON=%CD:\=\\%"
    set "HOMEJSON="
    if defined CODEX_HOME set "HOMEJSON=,\"config_dir\":\"!CODEX_HOME:\=\\!\""
    curl -fsS --connect-timeout 3 --max-time 10 -X POST "http://127.0.0.1:%QONTINUI_INSTALL_INTERCEPT_PORT%/control/session-open" -H "Content-Type: application/json" -d "{\"terminal_id\":\"%QONTINUI_TERMINAL_ID%\",\"provider\":\"%TOOL%\",\"source\":\"startup\",\"cwd\":\"!CWDJSON!\"!HOMEJSON!}" >nul 2>nul
  )
)

:passthrough
rem Run the REAL CLI with the user's args UNCHANGED.
set "QONTINUI_INSTALL_INTERCEPT_GUARD=1"
if defined REAL (
  call "%REAL%" %*
) else (
  call %TOOL% %*
)
endlocal & exit /b %ERRORLEVEL%
