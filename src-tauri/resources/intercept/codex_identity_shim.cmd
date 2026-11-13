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

rem ---- resolve the REAL CLI: first PATH match not inside any qontinui shim dir
set "REAL="
for %%X in (cmd exe bat) do (
  if not defined REAL (
    for %%D in ("%PATH:;=" "%") do (
      if not defined REAL (
        set "DENTRY=%%~D"
        call :is_qontinui_shim_dir
        if "!DSKIP!"=="0" (
          if exist "!DENTRY!\%TOOL%.%%X" set "REAL=!DENTRY!\%TOOL%.%%X"
        )
      )
    )
  )
)

rem ---- recursion guard: a nested invocation never re-signals ----------------
if "%QONTINUI_INSTALL_INTERCEPT_GUARD%"=="1" goto :passthrough

rem ---- only a launch that can start a session signals (see the bash twin) ---
rem The first argument is taken through a FOR variable, never as "%~1" inside
rem an IF: a first argument carrying its own quotes (--config="a b") unbalances
rem "%~1" and breaks the IF, while %%~A is substituted after the line is
rem parsed. Compared with delayed expansion for the same reason.
set "ARG1="
for %%A in (%1) do if not defined ARG1 set "ARG1=%%~A"
for %%S in (login logout mcp plugin app-server remote-control completion update doctor sandbox debug apply a queue archive delete migrate-rollouts unarchive cloud exec-server features agents help -h --help -V --version) do (
  if /I "!ARG1!"=="%%S" goto :passthrough
)

rem ---- `codex resume [<id>]` continues an EXISTING rollout: say so, with
rem the argv id when one is given (taken through a FOR variable like ARG1;
rem quotes stripped so it cannot unbalance the JSON; the runner validates it).
set "SRCJSON=startup"
set "RESUMEJSON="
if /I "!ARG1!"=="resume" (
  set "SRCJSON=resume"
  set "ARG2="
  for %%B in (%2) do if not defined ARG2 set "ARG2=%%~B"
  if defined ARG2 set "ARG2=!ARG2:"=!"
  if defined ARG2 if not "!ARG2:~0,1!"=="-" set "RESUMEJSON=,\"resume_id\":\"!ARG2!\""
)
rem ---- best-effort start signal (NO session_id). Never load-bearing. --------
if defined QONTINUI_INSTALL_INTERCEPT_PORT (
  where curl >nul 2>nul
  if not errorlevel 1 (
    set "CWDJSON=%CD:\=\\%"
    set "HOMEJSON="
    if defined CODEX_HOME set "HOMEJSON=,\"config_dir\":\"!CODEX_HOME:\=\\!\""
    curl -fsS --connect-timeout 3 --max-time 10 -X POST "http://127.0.0.1:%QONTINUI_INSTALL_INTERCEPT_PORT%/control/session-open" -H "Content-Type: application/json" -d "{\"terminal_id\":\"%QONTINUI_TERMINAL_ID%\",\"provider\":\"%TOOL%\",\"source\":\"!SRCJSON!\",\"cwd\":\"!CWDJSON!\"!HOMEJSON!!RESUMEJSON!}" >nul 2>nul
  )
)

:passthrough
rem Run the REAL CLI with the user's args UNCHANGED.
rem
rem Leave the delayed-expansion scope FIRST. With delayed expansion on, the
rem line that expands %* strips every "!" from the user's arguments (and a
rem caret before one), and CALL doubles their carets. REAL crosses ENDLOCAL
rem through a FOR variable (the R prefix keeps an unresolved REAL non-empty);
rem the guard is set in a fresh SETLOCAL so it reaches the child without
rem leaking into the caller's cmd session.
for /f "delims=" %%R in ("R!REAL!") do (
  endlocal
  setlocal DisableDelayedExpansion
  set "QSHIM_REAL=%%R"
)
set "QSHIM_REAL=%QSHIM_REAL:~1%"
set "QONTINUI_INSTALL_INTERCEPT_GUARD=1"
if not defined QSHIM_REAL goto :run_by_name
rem No CALL, for any target: CALL doubles the arguments' carets. An .exe
rem returns here with its exit code. A .cmd/.bat target takes control
rem instead (batch chaining): nothing after this line runs, and the
rem target's exit code is the shim's.
"%QSHIM_REAL%" %*
exit /b %ERRORLEVEL%
:run_by_name
rem Nothing launchable resolved: chain to the tool by name. Without CALL, a
rem name that resolves back to a qontinui shim would re-enter it for ever, so
rem a by-name run that finds this marker already set is the loop, and fails.
if defined QSHIM_BY_NAME (
  >&2 echo qontinui shim: @@TOOL@@ could not be resolved on PATH outside the qontinui shim dirs
  exit /b 9009
)
set "QSHIM_BY_NAME=1"
@@TOOL@@ %*
exit /b %ERRORLEVEL%

rem ---- is_qontinui_shim_dir: DSKIP=1 when DENTRY is ANY qontinui shim dir ---
rem Not only our own SHIM_DIR: a PATH inherited from another runner session
rem carries THAT session's qontinui-identity-*/qontinui-shim-* dir too, and
rem resolving its shim as the "real" tool made two shims dispatch to each other
rem for ever (plan 2026-11-04-identity-shim-foreign-shim-ping-pong-and-
rem secondary-runner-pty-socket-overflow, Phase 1). The same predicate as
rem intercept_core::shim_dir::is_qontinui_shim_dir: the dir's final name starts
rem with qontinui-identity- or qontinui-shim-, or it holds the identity-build
rem marker. Trailing separators are ignored. An EMPTY entry (a doubled or
rem trailing `;`) is skipped, as the bash templates and the native stub skip
rem it: probing it would test `\<tool>.cmd` at the root of the current drive.
:is_qontinui_shim_dir
set "DSKIP=0"
if not defined DENTRY (
  set "DSKIP=1"
  exit /b 0
)
if /I "!DENTRY!"=="%SHIM_DIR%" set "DSKIP=1"
set "DTRIM=!DENTRY!"
:is_qontinui_shim_dir_trim
if not defined DTRIM exit /b 0
if "!DTRIM:~-1!"=="\" (
  set "DTRIM=!DTRIM:~0,-1!"
  goto :is_qontinui_shim_dir_trim
)
if "!DTRIM:~-1!"=="/" (
  set "DTRIM=!DTRIM:~0,-1!"
  goto :is_qontinui_shim_dir_trim
)
rem A drive root (C:\ trimmed to C:) has no final name; %%~nx of C: would
rem name the CURRENT directory on that drive instead.
if "!DTRIM:~1!"==":" goto :is_qontinui_shim_dir_marker
set "DNAME="
for %%N in ("!DTRIM!") do set "DNAME=%%~nxN"
if /I "!DNAME:~0,18!"=="qontinui-identity-" set "DSKIP=1"
if /I "!DNAME:~0,14!"=="qontinui-shim-" set "DSKIP=1"
:is_qontinui_shim_dir_marker
if exist "!DTRIM!\.qontinui-identity-build" set "DSKIP=1"
exit /b 0
