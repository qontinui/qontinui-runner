@echo off
setlocal EnableDelayedExpansion
rem qontinui session-restore IDENTITY shim (Windows cmd / PowerShell).
rem
rem Always-on (NOT gated by QONTINUI_INSTALL_INTERCEPT_ENABLED — the out-of-box
rem session-restore guarantee, plan SECTION 3b). TEMPLATE materialized
rem per-terminal by the runner; the runner substitutes the @@...@@ placeholders.
rem Do NOT run it raw.
rem
rem Job: append --session-id %QONTINUI_PINNED_SESSION_ID% to a hand-started
rem claude/gemini so it pins to the runner-known id. The runner already recorded
rem the session authoritatively at spawn; the confirmation POST is liveness only.
rem
rem Hard invariants (mirrored from the install shim, plan SECTION 6):
rem   * FAIL-OPEN: any failure -> run the REAL provider unchanged.
rem   * TRANSPARENT: stdio inherited; the user sees the real exit code.
rem   * RECURSION-GUARD: QONTINUI_INSTALL_INTERCEPT_GUARD=1 -> pure passthrough.
rem   * DON'T DOUBLE-PIN: user already passed --session-id/--resume -> passthrough.
rem
rem Placeholders:
rem   @@TOOL@@      the wrapped provider program name (claude/gemini)
rem   @@SHIM_DIR@@  absolute path of this shim's own bin dir

set "TOOL=@@TOOL@@"
set "SHIM_DIR=@@SHIM_DIR@@"

rem ---- resolve the REAL provider: first PATH match not inside SHIM_DIR -------
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

rem ---- recursion guard: a nested invocation never re-pins -------------------
if "%QONTINUI_INSTALL_INTERCEPT_GUARD%"=="1" goto :passthrough

set "PINNED=%QONTINUI_PINNED_SESSION_ID%"

<<<<<<< HEAD
rem ---- Claude SessionStart hook delivery (plan SECTION 4, Phase 2) ----------
rem For TOOL=claude ONLY, append --settings <runner-app-data hook file> so the
rem confirmation hook is delivered ADDITIVELY (Claude merges it on top of any
rem ~/.claude config WITHOUT writing to it). Empty/unset or non-claude => none.
set "SETTINGS_ARGS="
if /I "%TOOL%"=="claude" if defined QONTINUI_CLAUDE_HOOK_SETTINGS (
  if exist "%QONTINUI_CLAUDE_HOOK_SETTINGS%" set "SETTINGS_ARGS=--settings "%QONTINUI_CLAUDE_HOOK_SETTINGS%""
)

=======
>>>>>>> 21acbfae (feat(session-restore): Phase 1 — provider-agnostic core, origin migration, /control/session-open, always-on identity seam, SessionProviderAdapter trait)
rem ---- did the user already choose a session? then don't double-pin --------
set "USER_CHOSE=0"
for %%A in (%*) do (
  set "TOK=%%~A"
  if /I "!TOK!"=="--session-id" set "USER_CHOSE=1"
  if /I "!TOK!"=="--resume" set "USER_CHOSE=1"
  if /I "!TOK!"=="-r" set "USER_CHOSE=1"
  if /I "!TOK!"=="resume" set "USER_CHOSE=1"
  if /I "!TOK!"=="--continue" set "USER_CHOSE=1"
  if /I "!TOK!"=="-c" set "USER_CHOSE=1"
)

rem ---- best-effort confirmation/liveness POST (never load-bearing) ----------
if not "%USER_CHOSE%"=="1" if defined PINNED if defined QONTINUI_INSTALL_INTERCEPT_PORT (
  where curl >nul 2>nul
  if not errorlevel 1 (
    set "CWDJSON=%CD:\=\\%"
    curl -fsS --connect-timeout 3 --max-time 10 -X POST "http://127.0.0.1:%QONTINUI_INSTALL_INTERCEPT_PORT%/control/session-open" -H "Content-Type: application/json" -d "{\"terminal_id\":\"%QONTINUI_TERMINAL_ID%\",\"session_id\":\"%PINNED%\",\"source\":\"startup\",\"provider\":\"%TOOL%\",\"cwd\":\"!CWDJSON!\"}" >nul 2>nul
  )
)

set "QONTINUI_INSTALL_INTERCEPT_GUARD=1"
if "%USER_CHOSE%"=="1" goto :passthrough
if not defined PINNED goto :passthrough

<<<<<<< HEAD
rem ---- append the runner-pinned session id (+ claude --settings hook) -------
if defined REAL (
  call "%REAL%" %* %SETTINGS_ARGS% --session-id %PINNED%
) else (
  call %TOOL% %* %SETTINGS_ARGS% --session-id %PINNED%
=======
rem ---- append the runner-pinned session id and run the real provider --------
if defined REAL (
  call "%REAL%" %* --session-id %PINNED%
) else (
  call %TOOL% %* --session-id %PINNED%
>>>>>>> 21acbfae (feat(session-restore): Phase 1 — provider-agnostic core, origin migration, /control/session-open, always-on identity seam, SessionProviderAdapter trait)
)
endlocal & exit /b %ERRORLEVEL%

:passthrough
<<<<<<< HEAD
rem User chose their own session (or no pin) — still deliver the claude
rem --settings hook so a --resume/--continue confirms via SessionStart.
set "QONTINUI_INSTALL_INTERCEPT_GUARD=1"
if defined REAL (
  call "%REAL%" %* %SETTINGS_ARGS%
) else (
  call %TOOL% %* %SETTINGS_ARGS%
=======
set "QONTINUI_INSTALL_INTERCEPT_GUARD=1"
if defined REAL (
  call "%REAL%" %*
) else (
  call %TOOL% %*
>>>>>>> 21acbfae (feat(session-restore): Phase 1 — provider-agnostic core, origin migration, /control/session-open, always-on identity seam, SessionProviderAdapter trait)
)
endlocal & exit /b %ERRORLEVEL%
