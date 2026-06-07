@echo off
setlocal EnableDelayedExpansion
rem qontinui install-interception shim (Windows cmd / PowerShell).
rem
rem TEMPLATE materialized per-terminal by the runner. The runner substitutes
rem the @@...@@ placeholders at write time; do NOT run it raw.
rem
rem Hard invariants (plan SECTION 6):
rem   * FAIL-OPEN: any runner-contact failure -> run the REAL tool unchanged.
rem   * TRANSPARENT: stdio is inherited; the agent sees the real exit code.
rem   * ZERO-OVERHEAD non-install: non-install verbs run the real tool with no
rem     runner round-trip.
rem   * RECURSION-GUARD: QONTINUI_INSTALL_INTERCEPT_GUARD=1 already set -> pure
rem     passthrough.
rem
rem Placeholders:
rem   @@TOOL@@           the shadowed program name (npm/npx/pnpm/yarn/cargo/pip/pip3)
rem   @@PM_WIRE_NAME@@   the wire package_manager (npx->npm, pip3->pip)
rem   @@SHIM_DIR@@       absolute path of this shim's own bin dir (skipped in
rem                      the real-tool PATH scan)
rem   @@INSTALL_VERBS@@  space-separated install-shaped verbs
rem
rem KNOWN GAP: cargo/pip/pip3 ship as <name>.exe, which a .cmd cannot shadow
rem (.EXE precedes .CMD in PATHEXT). Under PowerShell/cmd those three are NOT
rem intercepted by this .cmd; the extensionless Git-Bash shim covers them. npx
rem and npx-family classify is coarse here too (see note below).

set "TOOL=@@TOOL@@"
set "PM_WIRE_NAME=@@PM_WIRE_NAME@@"
set "SHIM_DIR=@@SHIM_DIR@@"
set "INSTALL_VERBS=@@INSTALL_VERBS@@"
set "NEVER_GATE=@@NEVER_GATE@@"
set "MODE=%QONTINUI_INSTALL_INTERCEPT_MODE%"
if not defined MODE set "MODE=observe"
set "OVERRIDE=%QONTINUI_INSTALL_OVERRIDE%"

rem ---- resolve the REAL tool: first PATH match not inside SHIM_DIR ----------
set "REAL="
for %%E in (%TOOL%.cmd %TOOL%.exe %TOOL%.bat) do (
  for %%P in ("%%~$PATH:E") do rem noop
)
rem Robust scan: walk PATH entries, skip SHIM_DIR, take first existing
rem TOOL.cmd/TOOL.exe/TOOL.bat.
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

rem ---- recursion guard -----------------------------------------------------
if "%QONTINUI_INSTALL_INTERCEPT_GUARD%"=="1" goto :passthrough

rem ---- classify: first non-flag token is the verb --------------------------
set "VERB="
for %%A in (%*) do (
  if not defined VERB (
    set "TOK=%%~A"
    set "FC=!TOK:~0,1!"
    if not "!FC!"=="-" set "VERB=!TOK!"
  )
)

set "IS_INSTALL=0"
for %%V in (%INSTALL_VERBS%) do (
  if /I "%VERB%"=="%%V" set "IS_INSTALL=1"
)

if "%IS_INSTALL%"=="0" goto :passthrough

rem ---- install-shaped: best-effort pre-call, run real tool, post-call ------
rem The .cmd variant does NOT parse package specifics (that lives in the bash
rem payload, which agent terminals predominantly use). It still declares an
rem observe pre/post pass with empty packages so an install typed under
rem cmd/PowerShell is at least signature-visible, and ALWAYS runs the real
rem tool (observe mode, fail-open). Known gaps under cmd: no per-package parse,
rem no dev flag, and the npx detection is coarse (npx's verb is the package
rem name, not in INSTALL_VERBS, so npx classifies as passthrough here). The
rem wire package_manager is PM_WIRE_NAME (npx->npm, pip3->pip).
rem Override path: QONTINUI_INSTALL_OVERRIDE=1 -> override_escalation:true on
rem the pre-call (producer records +overridden), still run the real tool.
set "OVERRIDE_JSON=false"
if "%OVERRIDE%"=="1" set "OVERRIDE_JSON=true"

set "CORR="
set "GATE="
set "PWDJSON=%CD:\=\\%"
rem FAIL-OPEN one-time notice (plan §4 Phase 4): emitted at most once, ONLY on the
rem pre-call failure path (absent PORT / no curl / empty response) — never on the
rem post-call. A single shim process makes <=1 pre-call.
if not defined QONTINUI_INSTALL_INTERCEPT_PORT (
  >&2 echo qontinui: install interception unavailable -- running normally
) else (
  where curl >nul 2>nul
  if errorlevel 1 (
    >&2 echo qontinui: install interception unavailable -- running normally
  ) else (
    rem Pre-call: SHORT connect timeout (3s); bounded total (20s).
    for /f "usebackq delims=" %%R in (`curl -fsS --connect-timeout 3 --max-time 20 -X POST "http://127.0.0.1:%QONTINUI_INSTALL_INTERCEPT_PORT%/install-effects/run" -H "Content-Type: application/json" -d "{\"mode\":\"intercept\",\"repo_path\":\"!PWDJSON!\",\"package_manager\":\"%PM_WIRE_NAME%\",\"packages\":[],\"dev\":false,\"override_escalation\":%OVERRIDE_JSON%}" 2^>nul`) do (
      set "PRERESP=%%R"
    )
    if defined PRERESP (
      for /f "tokens=2 delims=:," %%C in ('echo !PRERESP! ^| findstr /C:"correlation_id"') do (
        set "CORR=%%~C"
        set "CORR=!CORR:"=!"
        set "CORR=!CORR: =!"
      )
      rem Robust gate-field check: the producer controls the response shape and
      rem serializes "gate":"escalate" — a substring find is sufficient.
      echo !PRERESP! | findstr /C:"\"gate\":\"escalate\"" >nul 2>nul
      if not errorlevel 1 set "GATE=escalate"
    ) else (
      rem connect-refused / timeout / non-2xx / empty -> fail open once.
      >&2 echo qontinui: install interception unavailable -- running normally
    )
  )
)

rem ---- GATE DECISION (Phase 3) -------------------------------------------
rem BLOCK iff MODE==gate AND GATE==escalate AND OVERRIDE!=1 AND NOT a never-gate
rem tool. (The .cmd variant declares empty packages, so lockfile-only is implied
rem for the coarse path; the bash shim owns the precise per-package gate.) A
rem block prints a simplified blocked message to STDERR + exit 1, no real tool.
if /I "%MODE%"=="gate" if "%GATE%"=="escalate" if not "%OVERRIDE%"=="1" if not "%NEVER_GATE%"=="1" (
  >&2 echo Warning: qontinui: this install is predicted RISKY and was blocked.
  >&2 echo   risks: see runner log ^(predicted escalation^)
  >&2 echo   To override ^(record an audited +overridden install^), re-run with:
  >&2 echo       set QONTINUI_INSTALL_OVERRIDE=1 ^&^& %TOOL% %*
  endlocal ^& exit /b 1
)

set "QONTINUI_INSTALL_INTERCEPT_GUARD=1"
if defined REAL (
  call "%REAL%" %*
) else (
  call %TOOL% %*
)
set "RC=%ERRORLEVEL%"

if defined CORR if defined QONTINUI_INSTALL_INTERCEPT_PORT (
  where curl >nul 2>nul
  if not errorlevel 1 (
    curl -fsS --max-time 30 --connect-timeout 3 -X POST "http://127.0.0.1:%QONTINUI_INSTALL_INTERCEPT_PORT%/install-effects/observe-verify" -H "Content-Type: application/json" -d "{\"correlation_id\":\"!CORR!\",\"install_exit_code\":!RC!}" >nul 2>nul
  )
)

endlocal & exit /b %RC%

:passthrough
set "QONTINUI_INSTALL_INTERCEPT_GUARD=1"
if defined REAL (
  call "%REAL%" %*
) else (
  call %TOOL% %*
)
endlocal & exit /b %ERRORLEVEL%
