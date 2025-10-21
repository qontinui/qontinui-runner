@echo off
REM Batch script to run Python scripts using Poetry environment
REM This ensures the qontinui library and all dependencies are available

cd /d C:\Users\jspin\Documents\qontinui_parent\qontinui
poetry run python %*
