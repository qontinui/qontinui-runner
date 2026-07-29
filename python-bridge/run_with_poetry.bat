@echo off
REM Batch script to run Python scripts using Poetry environment
REM This ensures the qontinui library and all dependencies are available

REM The qontinui library sits beside qontinui-runner under the workspace root.
REM Derive it from this script's own location (%~dp0 =
REM <workspace-root>\qontinui-runner\python-bridge\) instead of a literal user
REM profile, which resolved on exactly one machine. %QONTINUI_ROOT% overrides.
if defined QONTINUI_ROOT (
    cd /d "%QONTINUI_ROOT%\qontinui"
) else (
    cd /d "%~dp0..\..\qontinui"
)
poetry run python %*
