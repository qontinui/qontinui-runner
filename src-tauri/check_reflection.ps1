# Check reflection task run output and reflection fixes
$resp = Invoke-WebRequest -Uri 'http://localhost:9876/task-runs' -UseBasicParsing
$content = $resp.Content | ConvertFrom-Json

# Find all runs
$runs = $content
if ($content.data) { $runs = $content.data }
if ($content.PSObject.Properties['runs']) { $runs = $content.runs }

Write-Host "Total task runs: $($runs.Count)" -ForegroundColor Cyan

# Find today's reflection runs
$today = $runs | Where-Object {
    $_.workflow_name -like "Reflection:*" -and $_.created_at -like "2026-03-04*"
}

Write-Host "`nToday's reflection runs: $($today.Count)" -ForegroundColor Cyan
foreach ($r in $today) {
    $id = $r.id
    Write-Host "  ID: $id" -ForegroundColor Yellow
    Write-Host "  Name: $($r.workflow_name)"
    Write-Host "  Status: $($r.status)"
    Write-Host "  Created: $($r.created_at)"

    # Get output
    try {
        $outResp = Invoke-WebRequest -Uri "http://localhost:9876/task-runs/$id/output?tail_chars=2000" -UseBasicParsing
        $output = $outResp.Content
        if ($output.Length -gt 500) {
            Write-Host "  Output (last 500 chars):" -ForegroundColor DarkGray
            Write-Host $output.Substring($output.Length - 500)
        } else {
            Write-Host "  Output:" -ForegroundColor DarkGray
            Write-Host $output
        }
    } catch {
        Write-Host "  Output unavailable: $($_.Exception.Message)" -ForegroundColor Red
    }
    Write-Host ""
}

# Also check ALL reflection runs (not just today)
Write-Host "=== All reflection runs ===" -ForegroundColor Cyan
$allReflections = $runs | Where-Object { $_.workflow_name -like "Reflection:*" } | Select-Object -First 5
foreach ($r in $allReflections) {
    $name = if ($r.workflow_name.Length -gt 70) { $r.workflow_name.Substring(0,70) } else { $r.workflow_name }
    Write-Host "  $($r.id.Substring(0,8)) | $($r.status) | $($r.created_at.Substring(0,19)) | $name"
}
