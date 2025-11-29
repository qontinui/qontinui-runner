# PowerShell script to run extraction test
# Run from Windows PowerShell (not WSL)

$ErrorActionPreference = "Stop"

# Change to the python-bridge directory
Set-Location $PSScriptRoot

Write-Host "Running Web Extraction Test..." -ForegroundColor Cyan
Write-Host "================================" -ForegroundColor Cyan

# Activate the virtual environment
if (Test-Path ".venv\Scripts\Activate.ps1") {
    Write-Host "Activating virtual environment..."
    . .\.venv\Scripts\Activate.ps1
} else {
    Write-Host "Warning: Virtual environment not found, using system Python" -ForegroundColor Yellow
}

# Run the test
python test_extraction.py

Write-Host ""
Write-Host "Test completed." -ForegroundColor Cyan
