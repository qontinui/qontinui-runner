#!/usr/bin/env pwsh
# Creates demo workflows in the Qontinui runner via API.
# Run this after starting the runner to add 3 sample demo workflows.
#
# Usage: powershell.exe -File create_demo_workflows.ps1
$ErrorActionPreference = "Stop"
$baseUrl = "http://localhost:9876"

function Create-Workflow {
    param([string]$Name, [hashtable]$Body)

    $json = $Body | ConvertTo-Json -Depth 10
    try {
        $response = Invoke-WebRequest -Uri "$baseUrl/unified-workflows" `
            -Method POST `
            -ContentType "application/json" `
            -Body $json `
            -UseBasicParsing
        $data = $response.Content | ConvertFrom-Json
        Write-Host "Created: $Name -> ID: $($data.data.id)" -ForegroundColor Green
        return $data.data.id
    } catch {
        Write-Host "FAILED: $Name -> $($_.Exception.Message)" -ForegroundColor Red
        return $null
    }
}

$setupScriptDir = "C:\Users\jspin\Documents\qontinui_parent\qontinui-runner\examples\demo-workflows"

# ============================================================
# Demo 1: Fix Buggy Calculator
# ============================================================
$calculator = @{
    name = "Demo: Fix Buggy Calculator"
    description = "A Python calculator has 3 bugs in its arithmetic functions (subtract, divide, modulo). The AI reads test failures and fixes the bugs until all tests pass. Quick demo (~1 iteration)."
    category = "demo"
    tags = @("demo", "python", "bug-fix", "beginner")
    setup_steps = @(
        @{
            type = "command"
            mode = "shell"
            name = "Reset demo workspace"
            phase = "setup"
            shell_command = "python setup_calculator.py"
            shell_command_working_directory = $setupScriptDir
            shell_command_fail_on_error = $true
        }
    )
    verification_steps = @(
        @{
            type = "command"
            mode = "check"
            name = "Run calculator tests"
            phase = "verification"
            check_type = "custom_command"
            check_command = "python test_calculator.py"
            check_working_directory = "C:\temp\qontinui-demo\calculator"
        }
    )
    agentic_steps = @(
        @{
            type = "prompt"
            name = "Fix calculator bugs"
            phase = "agentic"
            content = @"
Fix the bugs in the Python calculator module so that all tests pass.

**Workspace:** C:\temp\qontinui-demo\calculator
**File to fix:** calculator.py
**Test file:** test_calculator.py (do NOT modify this file)

The test file is correct. Only modify calculator.py to fix the failing tests.
Read the test failures carefully - they tell you exactly which functions have bugs and what the expected output should be.
"@
        }
    )
    completion_steps = @()
    max_iterations = 3
    health_check_enabled = $false
    preflight_check_enabled = $false
    log_watch_enabled = $false
    reflection_mode = $false
    skip_ai_summary = $true
}

# ============================================================
# Demo 2: Implement from Tests (TDD)
# ============================================================
$tdd = @{
    name = "Demo: Implement from Tests (TDD)"
    description = "Test-driven development demo. A test file defines 5 string utility functions that don't exist yet. The AI must implement all functions from scratch to make the tests pass."
    category = "demo"
    tags = @("demo", "python", "tdd", "implementation")
    setup_steps = @(
        @{
            type = "command"
            mode = "shell"
            name = "Reset demo workspace"
            phase = "setup"
            shell_command = "python setup_tdd.py"
            shell_command_working_directory = $setupScriptDir
            shell_command_fail_on_error = $true
        }
    )
    verification_steps = @(
        @{
            type = "command"
            mode = "check"
            name = "Run string utils tests"
            phase = "verification"
            check_type = "custom_command"
            check_command = "python test_string_utils.py"
            check_working_directory = "C:\temp\qontinui-demo\string-utils"
        }
    )
    agentic_steps = @(
        @{
            type = "prompt"
            name = "Implement string utils"
            phase = "agentic"
            content = @"
Implement the missing string utility functions so that all tests pass.

**Workspace:** C:\temp\qontinui-demo\string-utils
**File to implement:** string_utils.py (currently empty - you need to write all 5 functions)
**Test file:** test_string_utils.py (do NOT modify this file)

Read the test file to understand what each function should do:
- reverse_words(s): reverse the order of words in a string
- title_case(s): capitalize first letter of each word, lowercase the rest
- count_vowels(s): count vowels (a,e,i,o,u) case-insensitive
- is_palindrome(s): check palindrome ignoring case and spaces
- truncate(s, max_length): truncate to max_length with "..." suffix if truncated
"@
        }
    )
    completion_steps = @()
    max_iterations = 3
    health_check_enabled = $false
    preflight_check_enabled = $false
    log_watch_enabled = $false
    reflection_mode = $false
    skip_ai_summary = $true
}

# ============================================================
# Demo 3: Fix Data Pipeline
# ============================================================
$pipeline = @{
    name = "Demo: Fix Data Pipeline"
    description = "A sales data processing pipeline has bugs in its revenue calculation and aggregation logic. The AI must fix the pipeline so the output matches expected values. Shows real-world data processing debugging."
    category = "demo"
    tags = @("demo", "python", "data", "pipeline", "debugging")
    setup_steps = @(
        @{
            type = "command"
            mode = "shell"
            name = "Reset demo workspace"
            phase = "setup"
            shell_command = "python setup_data_pipeline.py"
            shell_command_working_directory = $setupScriptDir
            shell_command_fail_on_error = $true
        }
    )
    verification_steps = @(
        @{
            type = "command"
            mode = "check"
            name = "Validate pipeline output"
            phase = "verification"
            check_type = "custom_command"
            check_command = "python validate_pipeline.py"
            check_working_directory = "C:\temp\qontinui-demo\data-pipeline"
        }
    )
    agentic_steps = @(
        @{
            type = "prompt"
            name = "Fix pipeline bugs"
            phase = "agentic"
            content = @"
Fix the bugs in the data processing pipeline so that all validation checks pass.

**Workspace:** C:\temp\qontinui-demo\data-pipeline
**File to fix:** pipeline.py (the data processing pipeline)
**Input data:** sales_data.csv (do NOT modify)
**Validation script:** validate_pipeline.py (do NOT modify)

The pipeline reads sales_data.csv, calculates revenue per row, aggregates by product and region, and outputs a JSON report. The validation script checks the report against manually calculated expected values.

Focus on: the calculate_revenue function has a bug with type conversion and arithmetic, and find_top_product returns the wrong result.
"@
        }
    )
    completion_steps = @()
    max_iterations = 5
    health_check_enabled = $false
    preflight_check_enabled = $false
    log_watch_enabled = $false
    reflection_mode = $false
    skip_ai_summary = $true
}

Write-Host "`n=== Creating Demo Workflows ===" -ForegroundColor Cyan
Write-Host ""

$id1 = Create-Workflow -Name "Fix Buggy Calculator" -Body $calculator
$id2 = Create-Workflow -Name "Implement from Tests" -Body $tdd
$id3 = Create-Workflow -Name "Fix Data Pipeline" -Body $pipeline

Write-Host ""
Write-Host "=== Done ===" -ForegroundColor Cyan
Write-Host "Workflow IDs:"
Write-Host "  Calculator: $id1"
Write-Host "  TDD:        $id2"
Write-Host "  Pipeline:   $id3"
